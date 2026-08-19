//! Borrowed standard-stream descriptors.

use crate::{BorrowedFd, RawFd};

#[cfg(any(target_os = "linux", target_os = "none"))]
const STDERR_FILENO: RawFd = linux_raw_sys::general::STDERR_FILENO.cast_signed();
#[cfg(target_os = "macos")]
const STDERR_FILENO: RawFd = libc::STDERR_FILENO;

/// Returns the process's standard-error descriptor.
///
/// # Safety
///
/// Descriptor 2 must remain open as standard error for the returned lifetime.
/// In a `no_std` process, it may instead have been closed and reused.
#[doc(alias = "STDERR_FILENO")]
#[inline]
#[must_use]
pub const unsafe fn stderr() -> BorrowedFd<'static> {
    // SAFETY: the caller guarantees that standard error remains valid.
    unsafe { BorrowedFd::borrow_raw(STDERR_FILENO) }
}
