//! Unix shared memory backed by a sparse file at a caller-provided path.

use std::{
    ffi::OsStr,
    io,
    num::NonZeroUsize,
    os::unix::ffi::OsStrExt as _,
    ptr::{self, NonNull},
};

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
/// shared memory's path.
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
/// Creates `size` bytes of zero-initialized shared memory at `path`.
///
/// Returns an already opened [`ShmHandle`], so the creating process never has
/// to go through [`open`]. The caller owns the backing path and must eventually
/// pass it to [`remove`].
///
/// Only pages that are actually written occupy memory or disk, so a large
/// capacity is cheap.
///
/// # Errors
///
/// Returns an error if the shared memory cannot be created or sized.
pub fn create(path: &OsStr, size: usize) -> io::Result<ShmHandle> {
    let size = NonZeroUsize::new(size).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size must be nonzero")
    })?;
    let size_u64 = u64::try_from(size.get()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size exceeds u64")
    })?;
    let file = open_file(
        path,
        sigsafe::fs::OFlags::RDWR
            | sigsafe::fs::OFlags::CREATE
            | sigsafe::fs::OFlags::EXCL
            | sigsafe::fs::OFlags::CLOEXEC,
        sigsafe::fs::Mode::RUSR | sigsafe::fs::Mode::WUSR,
    )?;
    // Every byte reads as zero because the file is all holes.
    if let Err(error) = sigsafe::fs::ftruncate(&file, size_u64) {
        drop(file);
        let _ = remove(path);
        return Err(errno_to_io(error));
    }

    Ok(ShmHandle { file, size })
}

/// Opens the shared memory at `path`.
///
/// # Errors
///
/// Returns an error if the shared memory is unavailable, including after its
/// backing-file name has been removed.
pub fn open(path: &OsStr) -> io::Result<ShmHandle> {
    let file = open_file(
        path,
        sigsafe::fs::OFlags::RDWR | sigsafe::fs::OFlags::CLOEXEC,
        sigsafe::fs::Mode::empty(),
    )?;
    // If another process shrinks the file before `map`, mapping fails. If it
    // resizes afterwards, nothing here touches the mapped pages. A concurrent
    // resize cannot make a mapping access invalid memory.
    let size = usize::try_from(sigsafe::fs::fstat(&file).map_err(errno_to_io)?.st_size)
        .map_err(|_| io::ErrorKind::InvalidData)?;
    let size = NonZeroUsize::new(size).ok_or(io::ErrorKind::InvalidData)?;
    Ok(ShmHandle { file, size })
}

/// Removes the shared memory's backing-file name.
///
/// Existing handles and mappings remain usable, but later calls to [`open`]
/// fail once removal succeeds.
///
/// # Errors
///
/// Returns an error if the backing-file name cannot be removed.
pub fn remove(path: &OsStr) -> io::Result<()> {
    let mut path_buf = [0_u8; sigsafe::fs::PATH_MAX];
    let path = copy_path(path, &mut path_buf)?;
    sigsafe::fs::unlinkat(sigsafe::CWD, path, sigsafe::fs::AtFlags::empty()).map_err(errno_to_io)
}

fn open_file(
    path: &OsStr,
    flags: sigsafe::fs::OFlags,
    mode: sigsafe::fs::Mode,
) -> io::Result<sigsafe::OwnedFd> {
    let mut path_buf = [0_u8; sigsafe::fs::PATH_MAX];
    let path = copy_path(path, &mut path_buf)?;
    sigsafe::fs::openat(sigsafe::CWD, path, flags, mode).map_err(errno_to_io)
}

fn copy_path<'buf>(
    path: &OsStr,
    buf: &'buf mut [u8; sigsafe::fs::PATH_MAX],
) -> io::Result<sigsafe::CStr<'buf, sigsafe::Fat>> {
    let path_bytes = path.as_bytes();
    let len_with_nul = path_bytes.len().checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let path = buf
        .get_mut(..len_with_nul)
        .ok_or_else(|| io::Error::from_raw_os_error(sigsafe::Errno::NAMETOOLONG.raw_os_error()))?;
    path[..path_bytes.len()].copy_from_slice(path_bytes);
    sigsafe::CStr::from_bytes_with_nul(path).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn errno_to_io(errno: sigsafe::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

impl ShmHandle {
    /// Maps the shared bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the mapping cannot be established.
    pub fn map(&self) -> io::Result<Mapping> {
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
        }
        .map_err(errno_to_io)?;
        let Some(ptr) = NonNull::new(mapped.cast()) else {
            // A non-fixed mapping should not be placed at address zero,
            // which Rust cannot represent as a non-null allocation.
            // SAFETY: release the successful mapping before rejecting it.
            let _ = unsafe { sigsafe::mm::munmap(mapped, self.size.get()) };
            return Err(io::ErrorKind::Other.into());
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
