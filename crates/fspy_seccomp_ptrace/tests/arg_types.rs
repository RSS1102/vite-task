#![cfg(target_os = "linux")]
use std::{
    env::{current_dir, set_current_dir},
    error::Error,
    ffi::{CString, OsString},
    io,
    os::unix::ffi::OsStringExt,
    process::ExitStatus,
    time::Duration,
};

use assertables::assert_contains;
use fspy_seccomp_ptrace::{
    impl_handler,
    supervisor::{
        handler::arg::{CStrPtr, Caller, Fd},
        supervise,
    },
    target::install_target,
};
use nix::{
    fcntl::{AT_FDCWD, OFlag, openat},
    sys::{stat::Mode, wait::waitpid},
    unistd::{ForkResult, fork},
};
use test_log::test;
use tokio::{process::Command, task::spawn_blocking, time::timeout};
use tracing::{Level, span, trace};

#[derive(Debug, PartialEq, Eq, Clone)]
enum Syscall {
    Openat { at_dir: OsString, path: Option<OsString> },
}

#[derive(Default, Clone, Debug)]
struct SyscallRecorder(Vec<Syscall>);
impl SyscallRecorder {
    fn openat(&mut self, caller: Caller<'_>, (fd, path): (Fd, CStrPtr)) -> io::Result<()> {
        let at_dir = fd.get_path(caller)?;
        let mut buf = vec![0u8; 40000];
        let path = path
            .read(caller, &mut buf)?
            .map(|null_pos| OsString::from_vec(buf[..null_pos].to_vec()));
        self.0.push(Syscall::Openat { at_dir, path });
        Ok(())
    }
}

impl_handler!(SyscallRecorder: openat,);

async fn run_in_pre_exec(
    f: impl FnMut() -> io::Result<()> + Send + Sync + 'static,
) -> Result<Vec<Syscall>, Box<dyn Error>> {
    let (exit_status, syscalls) = run_command(Command::new("/bin/echo"), f).await?;
    assert!(exit_status.success());
    Ok(syscalls)
}

async fn run_command(
    mut command: Command,
    mut after_install: impl FnMut() -> io::Result<()> + Send + Sync + 'static,
) -> Result<(ExitStatus, Vec<Syscall>), Box<dyn Error>> {
    Ok(timeout(Duration::from_secs(5), async move {
        let supervisor = supervise::<SyscallRecorder>()?;

        let payload = supervisor.payload().clone();

        // SAFETY: `pre_exec` closure runs in the forked child process before exec.
        // It attaches to the ptrace supervisor, installs the seccomp filter, and
        // runs the user-provided closure. None of these operations use async state.
        unsafe {
            command.pre_exec(move || {
                install_target(&payload)?;
                after_install()?;
                Ok(())
            });
        }
        let child_fut = spawn_blocking(move || {
            let _span = span!(Level::TRACE, "spawn test child process");
            command.spawn()
        });

        let exit_status = child_fut.await.unwrap()?.wait().await?;
        trace!("test child process exited with status: {:?}", exit_status);

        trace!("waiting for handler to finish and test child process to exit");

        let recorders = supervisor.stop().await?;
        trace!("{} recorders awaited", recorders.len());

        let syscalls = recorders.into_iter().flat_map(|recorder| recorder.0);
        io::Result::Ok((exit_status, syscalls.collect()))
    })
    .await??)
}

#[test(tokio::test)]
async fn fd_and_path() -> Result<(), Box<dyn Error>> {
    let syscalls = run_in_pre_exec(|| {
        set_current_dir("/")?;
        let home_fd = openat(AT_FDCWD, c"/home", OFlag::O_PATH, Mode::empty())?;
        let _ = openat(home_fd, c"open_at_home", OFlag::O_RDONLY, Mode::empty());
        let _ = openat(AT_FDCWD, c"openat_cwd", OFlag::O_RDONLY, Mode::empty());
        Ok(())
    })
    .await?;
    assert_contains!(syscalls, &Syscall::Openat { at_dir: "/".into(), path: Some("/home".into()) });
    assert_contains!(
        syscalls,
        &Syscall::Openat { at_dir: "/home".into(), path: Some("open_at_home".into()) }
    );
    assert_contains!(
        syscalls,
        &Syscall::Openat { at_dir: "/".into(), path: Some("openat_cwd".into()) }
    );
    Ok(())
}

#[tokio::test]
async fn path_long() -> Result<(), Box<dyn Error>> {
    let long_path = b"a".repeat(30000);
    let long_path_cstr = CString::new(long_path.as_slice()).unwrap();
    let syscalls = run_in_pre_exec(move || {
        let _ = openat(AT_FDCWD, long_path_cstr.as_c_str(), OFlag::O_RDONLY, Mode::empty());
        Ok(())
    })
    .await?;
    assert_contains!(
        syscalls,
        &Syscall::Openat {
            at_dir: current_dir().unwrap().into(),
            path: Some(OsString::from_vec(long_path)),
        }
    );
    Ok(())
}

#[tokio::test]
async fn path_overflow() -> Result<(), Box<dyn Error>> {
    let long_path = b"a".repeat(40000);
    let long_path_cstr = CString::new(long_path.as_slice()).unwrap();
    let syscalls = run_in_pre_exec(move || {
        let _ = openat(AT_FDCWD, long_path_cstr.as_c_str(), OFlag::O_RDONLY, Mode::empty());
        Ok(())
    })
    .await?;
    assert_contains!(
        syscalls,
        &Syscall::Openat { at_dir: current_dir().unwrap().into(), path: None }
    );
    Ok(())
}

#[tokio::test]
async fn follows_forked_processes() -> Result<(), Box<dyn Error>> {
    let syscalls = run_in_pre_exec(|| {
        // SAFETY: This closure already runs after `Command` has forked and before
        // exec. Both branches use only syscall wrappers before exiting or execing.
        match unsafe { fork()? } {
            ForkResult::Parent { child } => {
                waitpid(child, None)?;
            }
            ForkResult::Child => {
                let _ = openat(AT_FDCWD, c"/fspy-ptrace-child", OFlag::O_RDONLY, Mode::empty());
                // SAFETY: Exiting directly avoids running parent-side destructors
                // in the forked child.
                unsafe { libc::_exit(0) };
            }
        }
        Ok(())
    })
    .await?;
    assert!(syscalls.iter().any(|syscall| {
        matches!(
            syscall,
            Syscall::Openat { path: Some(path), .. } if path == "/fspy-ptrace-child"
        )
    }));
    Ok(())
}

#[tokio::test]
async fn forwards_signals() -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "trap 'exit 23' USR1; kill -USR1 $$; exit 0"]);
    let (exit_status, _) = run_command(command, || Ok(())).await?;
    assert_eq!(exit_status.code(), Some(23));
    Ok(())
}
