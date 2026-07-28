pub mod handler;

use std::{
    convert::Infallible,
    io::{self, Read as _, Write as _},
    os::unix::{ffi::OsStrExt as _, net::UnixStream},
    sync::{Arc, Mutex},
};

use futures_util::{
    future::{Either, select},
    pin_mut,
};
pub use handler::PtraceHandler;
use handler::Syscall;
use nix::{
    errno::Errno,
    sys::{
        ptrace::{self, Event, Options},
        signal::Signal,
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::Pid,
};
use rustc_hash::FxHashSet;
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
use tokio::{net::UnixListener, sync::oneshot, task::JoinHandle};
use tracing::{Level, span};

use crate::payload::{Filter, PtracePayload};

pub struct Supervisor<H> {
    payload: PtracePayload,
    cancel_tx: oneshot::Sender<Infallible>,
    accept_loop_task: JoinHandle<io::Result<()>>,
    handler: Arc<Mutex<H>>,
    trace_error: Arc<Mutex<Option<io::Error>>>,
}

impl<H: Default> Supervisor<H> {
    #[must_use]
    pub const fn payload(&self) -> &PtracePayload {
        &self.payload
    }

    /// Stops the supervisor and returns all handler instances.
    ///
    /// # Panics
    /// Panics if the accept loop task has panicked or an internal mutex was poisoned.
    ///
    /// # Errors
    /// Returns an error if the accept loop or a tracer thread failed with an I/O error.
    pub async fn stop(self) -> io::Result<Vec<H>> {
        drop(self.cancel_tx);
        self.accept_loop_task.await.expect("accept loop task panicked")?;
        let trace_error = self.trace_error.lock().expect("trace error mutex poisoned").take();
        if let Some(error) = trace_error {
            return Err(error);
        }
        // Tracer threads are intentionally detached. A descendant may outlive the
        // process fspy was asked to wait for, but detaching it would make every
        // SECCOMP_RET_TRACE syscall fail with ENOSYS. Snapshot the accesses seen
        // through the root's exit and let detached threads service descendants
        // into the fresh default handler until those descendants exit.
        let handler =
            std::mem::take(&mut *self.handler.lock().expect("ptrace handler mutex poisoned"));
        Ok(vec![handler])
    }
}

/// Creates a new supervisor that traces only the syscalls selected by its seccomp filter.
///
/// # Panics
/// Panics if the seccomp filter cannot be compiled or the target architecture is unsupported.
///
/// # Errors
/// Returns an error if the temporary IPC socket cannot be created.
pub fn supervise<H: PtraceHandler + Default + Send + 'static>() -> io::Result<Supervisor<H>> {
    let attach_listener = tempfile::Builder::new()
        .prefix("fspy_seccomp_ptrace")
        .make(|path| UnixListener::bind(path))?;

    let seccomp_filter = SeccompFilter::new(
        H::syscalls().iter().map(|sysno| (sysno.id().into(), vec![])).collect(),
        SeccompAction::Allow,
        SeccompAction::Trace(0),
        std::env::consts::ARCH.try_into().unwrap(),
    )
    .unwrap();

    let bpf_filter =
        Filter(BpfProgram::try_from(seccomp_filter).unwrap().into_iter().map(Into::into).collect());

    let payload = PtracePayload {
        ipc_path: attach_listener.path().as_os_str().as_bytes().to_vec(),
        filter: bpf_filter,
    };

    let (cancel_tx, mut cancel_rx) = oneshot::channel::<Infallible>();
    let handler = Arc::new(Mutex::new(H::default()));
    let accept_handler = Arc::clone(&handler);
    let trace_error = Arc::new(Mutex::new(None));
    let accept_trace_error = Arc::clone(&trace_error);

    let accept_loop = async move {
        loop {
            let accept_future = attach_listener.as_file().accept();
            pin_mut!(accept_future);
            let (incoming_stream, _) = match select(&mut cancel_rx, accept_future).await {
                Either::Left((Err(_), _)) => break,
                Either::Right((incoming, _)) => incoming?,
            };
            let incoming_stream = incoming_stream.into_std()?;
            incoming_stream.set_nonblocking(false)?;
            let thread_handler = Arc::clone(&accept_handler);
            let thread_trace_error = Arc::clone(&accept_trace_error);
            // ptrace ownership is thread-specific, so the thread that seizes this
            // root must also wait for and resume every event in its process tree.
            std::thread::Builder::new().name("fspy-ptrace".into()).spawn(move || {
                if let Err(error) = trace(incoming_stream, &thread_handler) {
                    let mut trace_error =
                        thread_trace_error.lock().expect("trace error mutex poisoned");
                    if trace_error.is_none() {
                        *trace_error = Some(error);
                    }
                }
            })?;
        }
        Ok(())
    };

    Ok(Supervisor {
        payload,
        cancel_tx,
        accept_loop_task: tokio::spawn(accept_loop),
        handler,
        trace_error,
    })
}

fn trace<H: PtraceHandler>(mut stream: UnixStream, handler: &Mutex<H>) -> io::Result<()> {
    let mut tid_bytes = [0; std::mem::size_of::<libc::pid_t>()];
    stream.read_exact(&mut tid_bytes)?;
    let root = Pid::from_raw(libc::pid_t::from_ne_bytes(tid_bytes));

    let options = Options::PTRACE_O_TRACESECCOMP
        | Options::PTRACE_O_TRACEFORK
        | Options::PTRACE_O_TRACEVFORK
        | Options::PTRACE_O_TRACECLONE
        | Options::PTRACE_O_TRACEEXEC
        | Options::PTRACE_O_TRACEEXIT;
    let attach_result = ptrace::seize(root, options);
    let attach_errno = attach_result.as_ref().err().map_or(0, |error| *error as libc::c_int);
    stream.write_all(&attach_errno.to_ne_bytes())?;
    attach_result?;
    drop(stream);

    let mut tracees = FxHashSet::from_iter([root]);
    while !tracees.is_empty() {
        let status = match waitpid(None, Some(WaitPidFlag::__WALL | WaitPidFlag::__WNOTHREAD)) {
            Ok(status) => status,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        };
        handle_status(status, &mut tracees, handler)?;
    }

    Ok(())
}

fn handle_status<H: PtraceHandler>(
    status: WaitStatus,
    tracees: &mut FxHashSet<Pid>,
    handler: &Mutex<H>,
) -> io::Result<()> {
    match status {
        WaitStatus::Exited(pid, _) | WaitStatus::Signaled(pid, _, _) => {
            tracees.remove(&pid);
        }
        WaitStatus::PtraceEvent(pid, Signal::SIGTRAP, event)
            if event == Event::PTRACE_EVENT_SECCOMP as libc::c_int =>
        {
            let _span = span!(Level::TRACE, "seccomp ptrace tick");
            if let Ok(syscall) = Syscall::read(pid) {
                let _handle_result =
                    handler.lock().expect("ptrace handler mutex poisoned").handle_syscall(&syscall);
            }
            continue_tracee(pid, None)?;
        }
        WaitStatus::PtraceEvent(pid, Signal::SIGTRAP, event)
            if event == Event::PTRACE_EVENT_FORK as libc::c_int
                || event == Event::PTRACE_EVENT_VFORK as libc::c_int
                || event == Event::PTRACE_EVENT_CLONE as libc::c_int =>
        {
            let new_pid = Pid::from_raw(
                ptrace::getevent(pid)?
                    .try_into()
                    .map_err(|_| io::Error::other("ptrace child PID does not fit pid_t"))?,
            );
            tracees.insert(new_pid);
            continue_tracee(pid, None)?;
        }
        WaitStatus::PtraceEvent(pid, Signal::SIGTRAP, event)
            if event == Event::PTRACE_EVENT_EXEC as libc::c_int =>
        {
            let old_pid = Pid::from_raw(
                ptrace::getevent(pid)?
                    .try_into()
                    .map_err(|_| io::Error::other("ptrace exec TID does not fit pid_t"))?,
            );
            tracees.remove(&old_pid);
            tracees.insert(pid);
            continue_tracee(pid, None)?;
        }
        WaitStatus::PtraceEvent(pid, Signal::SIGTRAP, event)
            if event == Event::PTRACE_EVENT_EXIT as libc::c_int =>
        {
            tracees.remove(&pid);
            detach_tracee(pid)?;
        }
        WaitStatus::PtraceEvent(pid, signal, event)
            if event == Event::PTRACE_EVENT_STOP as libc::c_int =>
        {
            if signal == Signal::SIGTRAP {
                continue_tracee(pid, None)?;
            } else {
                listen(pid)?;
            }
        }
        WaitStatus::PtraceEvent(pid, _, _) | WaitStatus::PtraceSyscall(pid) => {
            continue_tracee(pid, None)?;
        }
        WaitStatus::Stopped(pid, signal) => {
            continue_tracee(pid, signal)?;
        }
        WaitStatus::Continued(_) | WaitStatus::StillAlive => {}
    }
    Ok(())
}

fn continue_tracee(pid: Pid, signal: impl Into<Option<Signal>>) -> io::Result<()> {
    match ptrace::cont(pid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn detach_tracee(pid: Pid) -> io::Result<()> {
    // Detach at PTRACE_EVENT_EXIT so the tracee's original parent, rather than
    // this tracer thread, consumes the terminal wait status.
    match ptrace::detach(pid, None) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn listen(pid: Pid) -> io::Result<()> {
    // SAFETY: PTRACE_LISTEN takes no address or data argument, and `pid` is a
    // seized tracee currently stopped at PTRACE_EVENT_STOP.
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_LISTEN,
            pid.as_raw(),
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_void>(),
        )
    };
    Errno::result(result).map(drop).map_err(Into::into)
}
