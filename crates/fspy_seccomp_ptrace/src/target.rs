use std::{
    ffi::OsStr,
    io::{Read as _, Write as _},
    os::unix::{ffi::OsStrExt, net::UnixStream},
};

use libc::sock_filter;
use nix::sys::prctl::set_no_new_privs;

use crate::{bindings::install_filter, payload::PtracePayload};

/// Attaches the current thread to the supervisor and installs its seccomp trace filter.
///
/// # Errors
/// Returns an error if the supervisor cannot attach, setting no-new-privs fails,
/// the filter cannot be installed, or the IPC socket communication fails.
pub fn install_target(payload: &PtracePayload) -> nix::Result<()> {
    let ipc_path = OsStr::from_bytes(&payload.ipc_path);
    let mut stream = UnixStream::connect(ipc_path).map_err(io_error_to_errno)?;
    // SAFETY: gettid takes no arguments and returns the caller's Linux thread ID.
    #[expect(clippy::cast_possible_truncation, reason = "Linux thread IDs use pid_t")]
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
    stream.write_all(&tid.to_ne_bytes()).map_err(io_error_to_errno)?;

    let mut response = [0; std::mem::size_of::<libc::c_int>()];
    stream.read_exact(&mut response).map_err(io_error_to_errno)?;
    let attach_errno = libc::c_int::from_ne_bytes(response);
    if attach_errno != 0 {
        return Err(nix::Error::from_raw(attach_errno));
    }

    set_no_new_privs()?;
    let sock_filters =
        payload.filter.0.iter().copied().map(sock_filter::from).collect::<Vec<sock_filter>>();
    install_filter(&sock_filters)?;
    Ok(())
}

fn io_error_to_errno(error: std::io::Error) -> nix::Error {
    nix::Error::try_from(error).unwrap_or(nix::Error::UnknownErrno)
}
