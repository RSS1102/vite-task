//! Kernel-backed random bytes with no allocation or hidden initialization.

use crate::Result;

/// Fills `bytes` with random data supplied by the kernel.
///
/// Linux uses the `getrandom` syscall directly. macOS uses libSystem's
/// `getentropy` system-call wrapper in chunks within its 256-byte limit.
///
/// # Errors
///
/// Returns the error reported by the platform random-data call.
pub fn fill(bytes: &mut [u8]) -> Result<()> {
    imp::fill(bytes)
}

#[cfg(target_os = "linux")]
mod imp {
    use crate::{Errno, Result};

    pub(super) fn fill(mut bytes: &mut [u8]) -> Result<()> {
        while !bytes.is_empty() {
            // SAFETY: `bytes` is writable for its complete length and the
            // kernel initializes only the count it returns.
            let initialized = unsafe {
                syscalls::syscall3(
                    syscalls::Sysno::getrandom,
                    bytes.as_mut_ptr().addr(),
                    bytes.len(),
                    0,
                )
            }
            .map_err(|errno| Errno::from_raw_os_error(errno.into_raw()))?;
            if initialized == 0 {
                return Err(Errno::IO);
            }
            let (_, remaining) = bytes.split_at_mut_checked(initialized).ok_or(Errno::IO)?;
            bytes = remaining;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use crate::{Errno, Result};

    pub(super) fn fill(bytes: &mut [u8]) -> Result<()> {
        for chunk in bytes.chunks_mut(256) {
            // SAFETY: `chunk` is writable for the supplied length, which is
            // within getentropy's 256-byte limit.
            let result = unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
            if result == -1 {
                // SAFETY: libSystem stored this call's error before returning.
                return Err(Errno::from_raw_os_error(unsafe { *libc::__error() }));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_empty_and_nonempty_buffers() {
        fill(&mut []).unwrap();
        fill(&mut [0; 32]).unwrap();
    }
}
