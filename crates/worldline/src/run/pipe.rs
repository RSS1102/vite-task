//! Piped transport: used for non-interactive stdio and on all of Windows.
//!
//! The child's stdin/stdout/stderr are pipes owned by fspy. We forward the
//! parent's stdin to the child, and copy the child's stdout/stderr both to the
//! parent and into the capture's raw-output log (in arrival order, which is how
//! a terminal would interleave them).

use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

use crate::capture::Snapshotter;

const BUF: usize = 8 * 1024;

/// Spawn `cmd` with piped stdio, pump until it exits, and return its status.
pub async fn run_piped(
    mut cmd: fspy::Command,
    snap: Snapshotter,
    cancel: CancellationToken,
) -> anyhow::Result<ExitStatus> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut tracked = cmd.spawn(cancel.clone()).await?;
    let mut child_stdin = tracked.stdin.take();
    let mut child_stdout = tracked.stdout.take().expect("piped stdout");
    let mut child_stderr = tracked.stderr.take().expect("piped stderr");
    let wait = tracked.wait_handle;

    let pump = async {
        let mut parent_stdin = tokio::io::stdin();
        let mut parent_stdout = tokio::io::stdout();
        let mut parent_stderr = tokio::io::stderr();
        let mut out_buf = vec![0u8; BUF];
        let mut err_buf = vec![0u8; BUF];
        let mut in_buf = vec![0u8; BUF];
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut stdin_done = child_stdin.is_none();

        while !(stdout_done && stderr_done) {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                read = child_stdout.read(&mut out_buf), if !stdout_done => match read {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => {
                        snap.append_output(&out_buf[..n]);
                        let _ = parent_stdout.write_all(&out_buf[..n]).await;
                        let _ = parent_stdout.flush().await;
                    }
                },
                read = child_stderr.read(&mut err_buf), if !stderr_done => match read {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => {
                        snap.append_output(&err_buf[..n]);
                        let _ = parent_stderr.write_all(&err_buf[..n]).await;
                        let _ = parent_stderr.flush().await;
                    }
                },
                read = parent_stdin.read(&mut in_buf), if !stdin_done => match read {
                    Ok(0) | Err(_) => {
                        stdin_done = true;
                        // Closing the child's stdin signals EOF to it.
                        child_stdin = None;
                    }
                    Ok(n) => {
                        let failed = match child_stdin.as_mut() {
                            Some(sink) => sink.write_all(&in_buf[..n]).await.is_err(),
                            None => true,
                        };
                        if failed {
                            stdin_done = true;
                            child_stdin = None;
                        }
                    }
                },
            }
        }
    };

    let (_pump, termination) = tokio::join!(pump, wait);
    Ok(termination?.status)
}
