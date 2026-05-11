//! Shared-memory abstraction used by fspy's IPC channel.
//!
//! On Linux, backs the region with a regular file in `std::env::temp_dir()`
//! and `mmap`s it. This avoids the `/dev/shm` size cap that POSIX `shm_open`
//! is subject to (e.g. Docker's 64 MiB default), which the previous
//! `shared_memory`-backed implementation was vulnerable to.
//!
//! On macOS and Windows, defers to the upstream `shared_memory` crate —
//! neither platform exhibits the size-cap bug and reusing battle-tested code
//! is the safer choice.

use std::{io, num::NonZeroUsize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::Inner;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod external;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use external::Inner;

/// A shared memory region accessible across processes via [`os_id`](Self::os_id).
pub struct Shmem {
    inner: Inner,
}

impl Shmem {
    /// Creates a new shared memory region of `size` bytes. The returned
    /// [`os_id`](Self::os_id) can be passed to [`open`](Self::open) from
    /// another process to attach to the same region.
    ///
    /// # Errors
    /// Returns `io::ErrorKind::InvalidInput` if `size` is zero, or another
    /// `io::Error` if the backing resource cannot be allocated.
    pub fn create(size: usize) -> io::Result<Self> {
        Inner::create(non_zero_size(size)?).map(|inner| Self { inner })
    }

    /// Opens an existing region previously created in another process.
    ///
    /// # Errors
    /// Returns `io::ErrorKind::InvalidInput` if `size` is zero, or
    /// `io::ErrorKind::NotFound` if the region's creator has dropped its
    /// `Shmem` (and thus removed the backing). Other errors propagate from
    /// the underlying OS calls.
    pub fn open(os_id: &str, size: usize) -> io::Result<Self> {
        Inner::open(os_id, non_zero_size(size)?).map(|inner| Self { inner })
    }

    /// Returns an opaque identifier that another process can pass to
    /// [`open`](Self::open) to attach to this region.
    #[must_use]
    pub fn os_id(&self) -> &str {
        self.inner.os_id()
    }

    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.inner.as_ptr()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the region as a byte slice.
    ///
    /// # Safety
    /// The caller must ensure no concurrent mutators access the region for
    /// the lifetime of the returned slice. Matches the contract of the
    /// upstream `shared_memory::Shmem::as_slice`.
    #[must_use]
    pub unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: caller upholds "no concurrent mutators" precondition;
        // `as_ptr` and `len` describe a valid mmap region for `self`'s lifetime.
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len()) }
    }
}

fn non_zero_size(size: usize) -> io::Result<NonZeroUsize> {
    NonZeroUsize::new(size).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared memory size must be non-zero")
    })
}

#[cfg(test)]
mod tests {
    use std::io;

    use assert2::assert;
    use subprocess_test::command_for_fn;

    use super::*;

    const SIZE: usize = 64 * 1024;

    #[test]
    fn smoke_create_open_same_process() {
        let creator = Shmem::create(SIZE).expect("create");
        let opener = Shmem::open(creator.os_id(), SIZE).expect("open");

        // SAFETY: this test is single-threaded; no concurrent mutators.
        unsafe { creator.as_ptr().write(0x42) };
        // SAFETY: same as above.
        let observed = unsafe { *opener.as_ptr() };
        assert!(observed == 0x42);
    }

    #[test]
    fn smoke_create_open_across_processes() {
        let creator = Shmem::create(SIZE).expect("create");

        // SAFETY: nothing else has a handle yet.
        unsafe { creator.as_ptr().write(0xCD) };

        let os_id = creator.os_id().to_owned();
        let cmd = command_for_fn!(os_id, |os_id: String| {
            let opener = Shmem::open(&os_id, SIZE).expect("child open");
            // SAFETY: parent wrote then handed off via os_id; we read before writing.
            let observed = unsafe { *opener.as_ptr() };
            assert!(observed == 0xCD);
            // SAFETY: parent is blocked waiting for our exit, no concurrent reader.
            unsafe { opener.as_ptr().write(0xEF) };
        });
        let status = std::process::Command::from(cmd).status().expect("spawn");
        assert!(status.success());

        // SAFETY: child has exited (waited above); no concurrent writer.
        let observed = unsafe { *creator.as_ptr() };
        assert!(observed == 0xEF);
    }

    #[test]
    fn open_nonexistent_returns_not_found() {
        match Shmem::open("/tmp/fspy_shm_definitely_does_not_exist_xyz123", SIZE) {
            Err(err) => assert!(err.kind() == io::ErrorKind::NotFound),
            Ok(_) => panic!("expected NotFound error"),
        }
    }

    #[test]
    fn create_zero_size_returns_invalid_input() {
        match Shmem::create(0) {
            Err(err) => assert!(err.kind() == io::ErrorKind::InvalidInput),
            Ok(_) => panic!("expected InvalidInput error"),
        }
    }

    /// After the creator drops (removing the backing file on Linux), an
    /// already-attached opener keeps its mapping valid — matches the
    /// crash-resilience contract of the previous `shared_memory`-backed
    /// implementation.
    #[test]
    fn opener_mapping_survives_creator_drop() {
        let creator = Shmem::create(SIZE).expect("create");
        let opener = Shmem::open(creator.os_id(), SIZE).expect("open");
        let os_id = creator.os_id().to_owned();
        drop(creator);

        // SAFETY: only `opener` references the mapping now; no concurrent access.
        unsafe { opener.as_ptr().write(0x99) };
        // SAFETY: same.
        let observed = unsafe { *opener.as_ptr() };
        assert!(observed == 0x99);

        // A late opener arriving after the creator dropped sees the backing gone.
        match Shmem::open(&os_id, SIZE) {
            Err(err) => assert!(err.kind() == io::ErrorKind::NotFound),
            Ok(_) => panic!("expected NotFound for late opener"),
        }
    }
}
