//! Run a program under fspy, snapshotting its file writes and terminal output.
//!
//! Transport is chosen automatically: an interactive terminal on glibc/macOS
//! Unix gets a real PTY (see the `pty_unix` module); everything else —
//! non-interactive stdio, Windows, and musl (whose PTY internals are
//! concurrency-unsafe, per `pty_terminal`) — uses pipes (see the `pipe`
//! module).

mod pipe;
#[cfg(all(unix, not(target_env = "musl")))]
mod pty_unix;

use std::{ffi::OsString, io::IsTerminal as _, sync::Arc};

use tokio_util::sync::CancellationToken;
use vite_path::AbsolutePathBuf;
use vite_str::Str;

use crate::{
    capture::{CapturedData, Snapshotter},
    ignore::IgnoreSet,
};

/// Inputs for a worldline run.
pub struct RunOptions {
    /// Program to execute.
    pub program: OsString,
    /// Arguments passed to the program.
    pub args: Vec<OsString>,
    /// Working directory the program runs in (also the snapshot root).
    pub cwd: AbsolutePathBuf,
    /// Resolved ignore rules.
    pub ignore: IgnoreSet,
}

/// Metadata about a completed run, served alongside the timeline.
#[derive(Debug)]
pub struct Meta {
    /// Full command line (`program` followed by its args).
    pub argv: Vec<Str>,
    /// Working directory the program ran in.
    pub cwd: Str,
    /// Process exit code, or `None` if it was terminated by a signal.
    pub exit_code: Option<i32>,
}

/// The result of a run: metadata plus the captured timeline.
pub struct Captured {
    /// Run metadata.
    pub meta: Meta,
    /// The captured Loro timeline, events, and output log.
    pub data: CapturedData,
}

/// Run the program to completion, capturing its file-write timeline and
/// terminal output.
///
/// # Errors
///
/// Returns an error if the program cannot be spawned or the transport fails to
/// initialize.
pub async fn run(options: RunOptions) -> anyhow::Result<Captured> {
    let RunOptions { program, args, cwd, ignore } = options;
    let snap = Snapshotter::new();

    let mut cmd = fspy::Command::new(&program);
    cmd.args(&args);
    cmd.envs(std::env::vars_os());
    cmd.current_dir(&cwd);
    crate::capture::install_callback(&mut cmd, snap.clone(), Arc::new(ignore));

    let cancel = CancellationToken::new();
    {
        // Ctrl-C cancels the run (mainly relevant on the piped path; on the PTY
        // path the ETX byte reaches the child through the line discipline).
        // `set_handler` errors if already installed in this process — ignored.
        let cancel = cancel.clone();
        let _ = ctrlc::set_handler(move || cancel.cancel());
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let use_pty = cfg!(all(unix, not(target_env = "musl"))) && interactive;

    let status = if use_pty {
        #[cfg(all(unix, not(target_env = "musl")))]
        {
            pty_unix::run_pty(cmd, snap.clone(), cancel).await?
        }
        #[cfg(not(all(unix, not(target_env = "musl"))))]
        {
            unreachable!("interactive PTY path is glibc/macOS-Unix only")
        }
    } else {
        pipe::run_piped(cmd, snap.clone(), cancel).await?
    };

    // The program and its descendants have exited; capture the final on-disk
    // state of every file we saw (catches writes with no observable close).
    snap.finalize();

    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(Str::from(program.to_string_lossy().as_ref()));
    for arg in &args {
        argv.push(Str::from(arg.to_string_lossy().as_ref()));
    }

    let meta = Meta {
        argv,
        cwd: vite_str::format!("{}", cwd.as_absolute_path()),
        exit_code: status.code(),
    };

    Ok(Captured { meta, data: snap.finish() })
}
