//! I/O calls with caller-owned buffers.
//!
//! On Linux these go straight to the kernel. On macOS they use the
//! async-signal-safe libc calls.

use core::fmt;

use crate::{BorrowedFd, Error, Result};

/// Reads into `buf` with a single `read(2)` and returns the initialized byte
/// count.
///
/// # Errors
///
/// Returns the error reported by `read`.
pub fn read(fd: BorrowedFd<'_>, buf: &mut [u8]) -> Result<usize> {
    #[cfg(any(target_os = "linux", target_os = "none"))]
    {
        // SAFETY: `fd` stays borrowed and `buf` is writable for its whole length.
        unsafe {
            syscalls::syscall!(syscalls::Sysno::read, fd.as_raw_fd(), buf.as_mut_ptr(), buf.len())
        }
        .map_err(Error::from)
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `fd` stays borrowed and `buf` is writable for its whole length.
        let read = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if read < 0 { Err(Error::last_os_error()) } else { Ok(read.cast_unsigned()) }
    }
}

/// Writes `buf` to `fd` with a single `write(2)` and returns the number of
/// bytes the kernel accepted.
///
/// A short write is reported as-is, exactly like the syscall; the caller
/// decides whether to write the remainder.
///
/// # Errors
///
/// Returns the error reported by `write`.
pub fn write(fd: BorrowedFd<'_>, buf: &[u8]) -> Result<usize> {
    #[cfg(any(target_os = "linux", target_os = "none"))]
    {
        // SAFETY: `fd` stays borrowed for the call and `buf` is readable for
        // its whole length.
        unsafe {
            syscalls::syscall!(syscalls::Sysno::write, fd.as_raw_fd(), buf.as_ptr(), buf.len())
        }
        .map_err(Error::from)
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `fd` stays borrowed for the call and `buf` is readable for
        // its whole length.
        let written = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if written < 0 { Err(Error::last_os_error()) } else { Ok(written.cast_unsigned()) }
    }
}

impl fmt::Write for BorrowedFd<'_> {
    fn write_str(&mut self, string: &str) -> fmt::Result {
        write_all(string.as_bytes(), |remaining| write(*self, remaining))
    }
}

fn write_all(mut remaining: &[u8], mut write: impl FnMut(&[u8]) -> Result<usize>) -> fmt::Result {
    while !remaining.is_empty() {
        match write(remaining) {
            Ok(0) => return Err(fmt::Error),
            Ok(written) => remaining = remaining.get(written..).ok_or(fmt::Error)?,
            Err(Error::INTR) => {}
            Err(_) => return Err(fmt::Error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_write_retries_interrupts_and_short_writes() {
        let mut steps = [Err(Error::INTR), Ok(2), Ok(3)].into_iter();
        let mut lengths = Vec::new();

        write_all(b"hello", |remaining| {
            lengths.push(remaining.len());
            steps.next().unwrap()
        })
        .unwrap();

        assert_eq!(lengths, [5, 5, 3]);
        assert!(write_all(b"x", |_| Ok(0)).is_err());
        assert!(write_all(b"x", |_| Err(Error::BADF)).is_err());
    }
}
