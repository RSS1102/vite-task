use std::{io, os::fd::RawFd};

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn fspy_sigsys_prepare(shm_fd: libc::c_int) -> libc::c_int;
    fn fspy_sigsys_inject(
        pid: libc::pid_t,
        shm_fd: libc::c_int,
        shm_len: libc::size_t,
    ) -> libc::c_int;
}

/// Marks the child traceable and installs the selective TRAP filter.
///
/// # Safety
///
/// This must only run in the post-fork child immediately before exec.
pub unsafe fn prepare(shm_fd: RawFd) -> io::Result<()> {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: the caller guarantees the pre-exec child context and the fd
        // is the live channel memfd inherited from the parent.
        if unsafe { fspy_sigsys_prepare(shm_fd) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = shm_fd;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the experimental SIGSYS injector supports Linux x86-64 only",
        ))
    }
}

/// Waits for the child's post-exec ptrace stop, maps the existing IPC shared
/// memory, installs the in-process handler, and detaches.
pub fn inject(pid: u32, shm_fd: RawFd, shm_len: usize) -> io::Result<()> {
    #[cfg(target_arch = "x86_64")]
    {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "child pid exceeds pid_t"))?;
        // SAFETY: `pid` is the freshly spawned TRACEME child and the fd/length
        // identify the channel mapping inherited across its exec.
        if unsafe { fspy_sigsys_inject(pid, shm_fd, shm_len) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (pid, shm_fd, shm_len);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the experimental SIGSYS injector supports Linux x86-64 only",
        ))
    }
}
