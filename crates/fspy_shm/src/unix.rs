//! Unix shared memory backed by a sparse temporary file and identified by its
//! path.

use std::{
    env::temp_dir,
    ffi::OsStr,
    io,
    num::NonZeroUsize,
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
    ptr::{self, NonNull},
};

use uuid::Uuid;

use crate::BACKING_PREFIX;

/// Error returned by shared-memory operations on Unix.
pub type Error = sigsafe::Error;
/// Result returned by shared-memory operations on Unix.
pub type Result<T> = sigsafe::Result<T>;

/// Keeps the shared memory's identifier alive and removes it on drop.
///
/// Removal is cleanup, not a stop signal: later opens fail, but existing
/// [`ShmHandle`]s and [`Mapping`]s keep reading and writing. To stop them,
/// store a flag in the shared bytes, as the fspy channel's close gate does.
pub struct ShmKeeper {
    path: PathBuf,
}

/// Opened shared memory that is not mapped yet.
///
/// [`map`](Self::map) can be called more than once; every call returns another
/// view of the same bytes. Drop the handle once the mappings exist.
pub struct ShmHandle {
    file: sigsafe::OwnedFd,
    size: NonZeroUsize,
}

/// The mapped shared bytes.
///
/// A `Mapping` keeps the bytes alive until it is dropped and cannot affect the
/// shared memory's identifier.
pub struct Mapping {
    ptr: NonNull<u8>,
    len: NonZeroUsize,
}

// SAFETY: a mapping owns no thread-affine state; access synchronization is
// supplied by the fspy channel built on top of it.
unsafe impl Send for Mapping {}
// SAFETY: sharing a `Mapping` does not itself access its bytes, and all actual
// concurrent access is synchronized by the fspy channel.
unsafe impl Sync for Mapping {}

/// Creates `size` bytes of zero-initialized shared memory.
///
/// Returns its [`ShmKeeper`] and an already opened [`ShmHandle`], so the
/// creating process never has to go through [`open`].
///
/// Only pages that are actually written occupy memory or disk, so a large
/// capacity is cheap.
///
/// # Errors
///
/// Returns an error if the shared memory cannot be created or sized.
pub fn create(size: usize) -> io::Result<(ShmKeeper, ShmHandle)> {
    let size = NonZeroUsize::new(size).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size must be nonzero")
    })?;
    let size_u64 = u64::try_from(size.get()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size exceeds u64")
    })?;

    // `temp_dir` reflects `TMPDIR` verbatim, which may be relative. The
    // identifier travels to processes with other working directories, so
    // resolve it against the creator's current directory first.
    let path = std::path::absolute(temp_dir())?
        .join(format!("{BACKING_PREFIX}{}.shm", Uuid::new_v4().simple()));

    let file = open_file(
        path.as_os_str(),
        sigsafe::fs::OFlags::RDWR
            | sigsafe::fs::OFlags::CREATE
            | sigsafe::fs::OFlags::EXCL
            | sigsafe::fs::OFlags::CLOEXEC,
        sigsafe::fs::Mode::RUSR | sigsafe::fs::Mode::WUSR,
    )
    .map_err(errno_to_io)?;
    // The keeper exists from here on, so every error path below cleans up.
    let keeper = ShmKeeper { path };

    // Every byte reads as zero because the file is all holes.
    sigsafe::fs::ftruncate(&file, size_u64).map_err(errno_to_io)?;

    Ok((keeper, ShmHandle { file, size }))
}

/// Opens the shared memory identified by `id`.
///
/// The identifier works from any process, regardless of the process's working
/// directory or environment.
///
/// # Errors
///
/// Returns an error if the shared memory is unavailable, which is the common
/// case once its keeper has been dropped.
pub fn open(id: &OsStr) -> Result<ShmHandle> {
    let file = open_file(
        id,
        sigsafe::fs::OFlags::RDWR | sigsafe::fs::OFlags::CLOEXEC,
        sigsafe::fs::Mode::empty(),
    )?;
    // If another process shrinks the file before `map`, mapping fails. If it
    // resizes afterwards, nothing here touches the mapped pages. A concurrent
    // resize cannot make a mapping access invalid memory.
    let size = usize::try_from(sigsafe::fs::fstat(&file)?.st_size)
        .map_err(|_| sigsafe::Errno::OVERFLOW)?;
    let size = NonZeroUsize::new(size).ok_or(sigsafe::Errno::INVAL)?;
    Ok(ShmHandle { file, size })
}

fn open_file(
    id: &OsStr,
    flags: sigsafe::fs::OFlags,
    mode: sigsafe::fs::Mode,
) -> Result<sigsafe::OwnedFd> {
    let mut path = [0_u8; sigsafe::fs::PATH_MAX];
    let path = copy_path(id, &mut path)?;
    sigsafe::fs::openat(sigsafe::CWD, path, flags, mode)
}

fn copy_path<'buf>(
    id: &OsStr,
    buf: &'buf mut [u8; sigsafe::fs::PATH_MAX],
) -> Result<sigsafe::CStr<'buf, sigsafe::Fat>> {
    let id = id.as_bytes();
    let len_with_nul = id.len().checked_add(1).ok_or(sigsafe::Errno::NAMETOOLONG)?;
    let path = buf.get_mut(..len_with_nul).ok_or(sigsafe::Errno::NAMETOOLONG)?;
    path[..id.len()].copy_from_slice(id);
    sigsafe::CStr::from_bytes_with_nul(path).map_err(|_| sigsafe::Errno::INVAL)
}

fn errno_to_io(errno: sigsafe::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

impl Drop for ShmKeeper {
    fn drop(&mut self) {
        let mut path = [0_u8; sigsafe::fs::PATH_MAX];
        let Ok(path) = copy_path(self.path.as_os_str(), &mut path) else {
            return;
        };
        let _ = sigsafe::fs::unlinkat(sigsafe::CWD, path, sigsafe::fs::AtFlags::empty());
    }
}

impl ShmKeeper {
    /// Returns the shared memory's opaque identifier, which any process passes
    /// to [`open`].
    #[must_use]
    pub fn id(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl ShmHandle {
    /// Maps the shared bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the mapping cannot be established.
    pub fn map(&self) -> Result<Mapping> {
        // SAFETY: the address is only a hint, the nonzero length is the
        // validated backing-file size, the descriptor remains borrowed,
        // and the resulting shared mapping is owned by `Mapping`.
        let mapped = unsafe {
            sigsafe::mm::mmap(
                ptr::null_mut(),
                self.size.get(),
                sigsafe::mm::ProtFlags::READ | sigsafe::mm::ProtFlags::WRITE,
                sigsafe::mm::MapFlags::SHARED,
                &self.file,
                0,
            )
        }?;
        let Some(ptr) = NonNull::new(mapped.cast()) else {
            // A non-fixed mapping should not be placed at address zero,
            // which Rust cannot represent as a non-null allocation.
            // SAFETY: release the successful mapping before rejecting it.
            let _ = unsafe { sigsafe::mm::munmap(mapped, self.size.get()) };
            return Err(sigsafe::Errno::INVAL);
        };
        Ok(Mapping { ptr, len: self.size })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this is the complete mapping owned by `self`, and dropping
        // it proves that no safe borrow through `self` remains.
        let _ = unsafe { sigsafe::mm::munmap(self.ptr.as_ptr().cast(), self.len.get()) };
    }
}

#[expect(clippy::len_without_is_empty, reason = "shared-memory mappings are always non-empty")]
impl Mapping {
    /// Returns the mapped length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len.get()
    }

    /// Returns a raw pointer to the first mapped byte.
    #[must_use]
    pub const fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Returns the mapped bytes as a shared slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no process or thread mutates the mapping for
    /// the lifetime of the returned slice.
    #[must_use]
    pub const unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: The mapping is valid for its full length, and the caller
        // guarantees that it is not mutated while the slice is borrowed.
        unsafe { std::slice::from_raw_parts(self.as_ptr().cast_const(), self.len()) }
    }
}
