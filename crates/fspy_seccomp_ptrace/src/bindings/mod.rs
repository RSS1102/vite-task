use std::os::raw::c_int;

use libc::syscall;

/// # Safety
/// The `args` pointer must be valid for the given `operation`, or null if the operation
/// does not require arguments.
unsafe fn seccomp(
    operation: libc::c_uint,
    flags: libc::c_uint,
    args: *mut libc::c_void,
) -> nix::Result<libc::c_int> {
    // SAFETY: caller guarantees `args` is valid for the given seccomp operation
    let ret = unsafe { syscall(libc::SYS_seccomp, operation, flags, args) };
    if ret < 0 {
        return Err(nix::Error::last());
    }
    Ok(c_int::try_from(ret).unwrap())
}

/// Installs a seccomp filter for the current thread.
///
/// # Errors
/// Returns an error if the seccomp syscall fails (e.g., invalid filter program or
/// insufficient privileges).
pub fn install_filter(prog: &[libc::sock_filter]) -> nix::Result<()> {
    let mut filter = libc::sock_fprog {
        len: prog.len().try_into().unwrap(),
        filter: prog.as_ptr().cast_mut().cast(),
    };

    // SAFETY: `filter` is a valid `sock_fprog` pointing to the BPF program slice.
    unsafe { seccomp(libc::SECCOMP_SET_MODE_FILTER, 0, (&raw mut filter).cast()) }?;
    Ok(())
}
