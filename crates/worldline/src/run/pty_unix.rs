//! Interactive transport on Unix: run the child on a real PTY.
//!
//! fspy owns the spawn, so we hand it three dups of the PTY slave as
//! stdin/stdout/stderr. The parent terminal is put in raw mode (restored on
//! drop) and a single async task pumps: master output → capture log + parent
//! stdout, parent stdin → master, and SIGWINCH → master resize. Ctrl-C flows
//! through as the ETX byte, so the slave's line discipline delivers SIGINT to
//! the child — no extra handler needed here.

use std::{
    io,
    os::fd::{AsRawFd as _, BorrowedFd, OwnedFd},
    process::{ExitStatus, Stdio},
};

use nix::{
    pty::{Winsize, openpty},
    sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, unix::AsyncFd},
    signal::unix::{SignalKind, signal},
};
use tokio_util::sync::CancellationToken;

use crate::capture::Snapshotter;

// SAFETY of the ioctl wrappers: the kernel reads/writes a `Winsize` through the
// pointer; callers pass a valid `&mut Winsize` / `&Winsize`.
nix::ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);
nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

const BUF: usize = 64 * 1024;

/// Query the parent terminal's window size, defaulting to 80x24.
fn parent_winsize() -> Winsize {
    let mut ws = Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: `ws` is a valid, owned `Winsize`; fd 0 is the parent's stdin.
    unsafe {
        let _ = tiocgwinsz(libc::STDIN_FILENO, &raw mut ws);
    }
    ws
}

/// Restores the parent terminal's original termios on drop (including on panic
/// or Ctrl-C unwinding).
struct RawModeGuard {
    original: Termios,
}

impl RawModeGuard {
    /// Put the parent terminal (fd 0) into raw mode, returning a guard that
    /// restores it. Returns `None` if fd 0 is not a terminal.
    fn enter() -> Option<Self> {
        // SAFETY: fd 0 is the process's stdin for the lifetime of the borrow.
        let stdin = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        let original = tcgetattr(stdin).ok()?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr(stdin, SetArg::TCSADRAIN, &raw).ok()?;
        Some(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // SAFETY: fd 0 is the process's stdin for the lifetime of the borrow.
        let stdin = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        let _ = tcsetattr(stdin, SetArg::TCSADRAIN, &self.original);
    }
}

/// Make an owned fd non-blocking (required for `AsyncFd`).
fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

/// The libuv `FD_CLOEXEC` workaround (mirrors `vite_task`): Node marks fds 0-2
/// close-on-exec and reopens them as `/dev/null` after exec otherwise.
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature matches Command::pre_exec's FnMut() -> io::Result<()> contract"
)]
fn clear_stdio_cloexec() -> io::Result<()> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    for raw in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: fds 0-2 are valid in the post-fork child.
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        if let Ok(flags) = fcntl(fd, FcntlArg::F_GETFD) {
            let mut fd_flags = FdFlag::from_bits_retain(flags);
            if fd_flags.contains(FdFlag::FD_CLOEXEC) {
                fd_flags.remove(FdFlag::FD_CLOEXEC);
                let _ = fcntl(fd, FcntlArg::F_SETFD(fd_flags));
            }
        }
    }
    Ok(())
}

/// Spawn `cmd` on a PTY, pump I/O until it exits, and return its status.
///
/// # Errors
///
/// Returns an error if the PTY cannot be opened or the child cannot be spawned.
pub async fn run_pty(
    mut cmd: fspy::Command,
    snap: Snapshotter,
    cancel: CancellationToken,
) -> anyhow::Result<ExitStatus> {
    let winsize = parent_winsize();
    let pty = openpty(Some(&winsize), None)?;
    let master = pty.master;
    let slave = pty.slave;

    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?));
    // SAFETY: the closure only performs fcntl on fds 0-2 in the post-fork child.
    unsafe {
        cmd.pre_exec(clear_stdio_cloexec);
    }

    let tracked = cmd.spawn(cancel.clone()).await?;
    let wait = tracked.wait_handle;

    // Hold the original slave open until the child has exited so the PTY stays
    // connected (mirrors pty_terminal's macOS data-loss guard).
    let _slave_holder = slave;

    // Raw mode for the parent terminal, restored on drop.
    let _raw_guard = RawModeGuard::enter();

    set_nonblocking(&master)?;
    let pump = tokio::spawn(pump_master(master, snap, cancel.clone()));

    let termination = wait.await;
    // Child has exited: stop the pump and let the master drain/close.
    cancel.cancel();
    let _ = pump.await;

    Ok(termination?.status)
}

/// Pump master output → capture + parent stdout, parent stdin → master, and
/// SIGWINCH → master resize, until cancelled or the master closes.
async fn pump_master(master: OwnedFd, snap: Snapshotter, cancel: CancellationToken) {
    let Ok(async_master) = AsyncFd::new(master) else {
        return;
    };
    let mut parent_stdin = tokio::io::stdin();
    let mut parent_stdout = tokio::io::stdout();
    let mut sigwinch = signal(SignalKind::window_change()).ok();
    let mut in_buf = vec![0u8; 8 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut stdin_done = false;

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            guard = async_master.readable() => {
                let Ok(mut guard) = guard else { break };
                let mut buf = vec![0u8; BUF];
                match guard.try_io(|inner| read_fd(inner.get_ref().as_raw_fd(), &mut buf)) {
                    Ok(Ok(0) | Err(_)) => break, // EOF or EIO: child is gone
                    Ok(Ok(n)) => {
                        snap.append_output(&buf[..n]);
                        let _ = parent_stdout.write_all(&buf[..n]).await;
                        let _ = parent_stdout.flush().await;
                    }
                    Err(_would_block) => {}
                }
            }
            read = parent_stdin.read(&mut in_buf), if pending.is_empty() && !stdin_done => match read {
                Ok(0) | Err(_) => stdin_done = true,
                Ok(n) => pending.extend_from_slice(&in_buf[..n]),
            },
            guard = async_master.writable(), if !pending.is_empty() => {
                let Ok(mut guard) = guard else { break };
                match guard.try_io(|inner| write_fd(inner.get_ref().as_raw_fd(), &pending)) {
                    Ok(Ok(n)) => drop(pending.drain(..n)),
                    Ok(Err(_)) => break,
                    Err(_would_block) => {}
                }
            }
            Some(()) = recv_signal(sigwinch.as_mut()) => {
                let ws = parent_winsize();
                // SAFETY: `ws` is a valid `Winsize`; the master fd is open.
                unsafe {
                    let _ = tiocswinsz(async_master.get_ref().as_raw_fd(), &raw const ws);
                }
            }
        }
    }
}

/// Await the next SIGWINCH, or never if the signal stream is unavailable.
async fn recv_signal(sig: Option<&mut tokio::signal::unix::Signal>) -> Option<()> {
    match sig {
        Some(sig) => sig.recv().await,
        None => std::future::pending().await,
    }
}

fn read_fd(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is a valid writable slice; `fd` is the open master.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(usize::try_from(n).unwrap_or(0)) }
}

fn write_fd(fd: i32, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: `buf` is a valid readable slice; `fd` is the open master.
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(usize::try_from(n).unwrap_or(0)) }
}

// fspy's blocking callbacks are compiled out on musl, so the PTY capture path
// can only be exercised off-musl.
#[cfg(all(test, not(target_env = "musl")))]
#[expect(clippy::disallowed_types, reason = "test glue uses std String/Path types")]
mod tests {
    use std::sync::Arc;

    use subprocess_test::command_for_fn;
    use tokio_util::sync::CancellationToken;
    use vite_path::AbsolutePathBuf;

    use super::run_pty;
    use crate::{
        capture::{Snapshotter, install_callback},
        ignore::IgnoreSet,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pty_captures_output_and_file_writes() {
        let dir = tempfile::tempdir().unwrap();
        let dir_arg = dir.path().to_str().unwrap().to_owned();

        let cmd = command_for_fn!(dir_arg, |dir: String| {
            use std::io::Write as _;
            let dir = std::path::Path::new(&dir);
            std::fs::write(dir.join("pty.txt"), b"pty-content").unwrap();
            let mut out = std::io::stdout();
            out.write_all(b"PTY-HELLO\n").unwrap();
            out.flush().unwrap();
        });

        let cwd = AbsolutePathBuf::new(dir.path().to_path_buf()).unwrap();
        let ignore = Arc::new(IgnoreSet::new(cwd, true, &[]).unwrap());
        let snap = Snapshotter::new();
        let mut fspy_cmd = fspy::Command::new(cmd.program);
        fspy_cmd.args(cmd.args);
        fspy_cmd.envs(cmd.envs);
        fspy_cmd.current_dir(cmd.cwd);
        install_callback(&mut fspy_cmd, snap.clone(), ignore);

        let status = run_pty(fspy_cmd, snap.clone(), CancellationToken::new()).await.unwrap();
        assert!(status.success(), "child should exit cleanly");

        snap.finalize();
        let data = snap.finish();

        assert!(
            data.output.windows(9).any(|w| w == b"PTY-HELLO"),
            "raw PTY output not captured: {:?}",
            std::str::from_utf8(&data.output).unwrap_or("<non-utf8>"),
        );
        assert!(
            data.events.iter().any(|e| e.path.ends_with("pty.txt")),
            "pty.txt write not captured: {:?}",
            data.events.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        );
    }
}
