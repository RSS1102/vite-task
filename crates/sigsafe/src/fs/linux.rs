use core::{mem::MaybeUninit, slice};

use rustix::fd::FromRawFd as _;

use crate::{
    AsRawFd as _, BorrowedFd, CStr, Errno, Fat, OwnedFd, Result, Thin,
    fs::{Mode, OFlags},
};

// Linux UAPI `PATH_MAX`.
pub(super) const PATH_MAX: usize = 4096;

fn syscall_fd(fd: BorrowedFd<'_>) -> Result<usize> {
    let fd = isize::try_from(fd.as_raw_fd()).map_err(|_| Errno::OVERFLOW)?;
    Ok(fd.cast_unsigned())
}

#[expect(clippy::needless_pass_by_value, reason = "CStr is a borrowed value type")]
pub(super) fn openat<R>(
    dirfd: BorrowedFd<'_>,
    path: CStr<'_, R>,
    flags: OFlags,
    mode: Mode,
) -> Result<OwnedFd> {
    // SAFETY: `dirfd` remains borrowed and `path` is NUL-terminated. The
    // kernel reads no variadic arguments; all four syscall arguments are
    // passed explicitly.
    let fd = unsafe {
        syscalls::syscall4(
            syscalls::Sysno::openat,
            syscall_fd(dirfd)?,
            path.as_ptr().addr(),
            usize::try_from(flags.bits()).map_err(|_| Errno::INVAL)?,
            usize::try_from(mode.bits()).map_err(|_| Errno::INVAL)?,
        )
    }
    .map_err(|errno| Errno::from_raw_os_error(errno.into_raw()))?;
    let fd = i32::try_from(fd).map_err(|_| Errno::OVERFLOW)?;

    // SAFETY: a successful `openat` returns a new owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Reads the target of `path` relative to `dirfd` into `buf`.
///
/// The returned bytes borrow the initialized prefix of `buf`. As with the
/// underlying syscall, they do not include a terminating NUL, and a result
/// that fills `buf` may have been truncated.
///
/// This function performs one syscall and does not retry a full buffer.
///
/// # Errors
///
/// Returns the error reported by `readlinkat`.
pub fn readlinkat<'buf>(
    dirfd: BorrowedFd<'_>,
    path: CStr<'_, Thin>,
    buf: &'buf mut [MaybeUninit<u8>],
) -> Result<&'buf [u8]> {
    // SAFETY: `dirfd` remains borrowed, `path` is NUL-terminated, and `buf`
    // is writable for `buf.len()` bytes. `readlinkat` returns the initialized
    // byte count.
    let initialized = unsafe {
        syscalls::syscall4(
            syscalls::Sysno::readlinkat,
            syscall_fd(dirfd)?,
            path.as_ptr().addr(),
            buf.as_mut_ptr().addr(),
            buf.len(),
        )
    }
    .map_err(|errno| Errno::from_raw_os_error(errno.into_raw()))?;

    // SAFETY: the syscall initialized exactly this prefix.
    Ok(unsafe { slice::from_raw_parts(buf.as_ptr().cast(), initialized) })
}

pub(super) fn getcwd(buf: &mut [MaybeUninit<u8>]) -> Result<CStr<'_, Fat>> {
    // rustix exposes only an allocating `getcwd`, so use the raw syscall for
    // caller-owned storage.
    // SAFETY: `buf` is writable for exactly `buf.len()` bytes. The syscall
    // writes no more than that and returns the initialized length including
    // its terminating NUL.
    let initialized =
        unsafe { syscalls::syscall2(syscalls::Sysno::getcwd, buf.as_mut_ptr().addr(), buf.len()) }
            .map_err(|errno| Errno::from_raw_os_error(errno.into_raw()))?;

    // SAFETY: the syscall initialized this prefix through its terminating NUL.
    let bytes = unsafe { slice::from_raw_parts(buf.as_ptr().cast(), initialized) };
    // SAFETY: the syscall returned one NUL-terminated pathname.
    Ok(unsafe { CStr::from_bytes_with_nul_unchecked(bytes) })
}
