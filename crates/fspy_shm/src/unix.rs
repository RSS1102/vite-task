//! Unix shared memory backed by a sparse file at a caller-provided path.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    os::unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _, io::FromRawFd as _},
    path::PathBuf,
};

use memmap2::{MmapOptions, MmapRaw};

/// Keeps the shared memory's backing-file name alive and removes it on drop.
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
    file: File,
    size: usize,
}

/// The mapped shared bytes.
///
/// A `Mapping` keeps the bytes alive until it is dropped and cannot affect the
/// shared memory's path.
pub struct Mapping {
    raw: MmapRaw,
}

/// Creates `size` bytes of zero-initialized shared memory at `path`.
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
pub fn create(path: &OsStr, size: usize) -> io::Result<(ShmKeeper, ShmHandle)> {
    if size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared-memory size must be nonzero",
        ));
    }
    let size_u64 = u64::try_from(size).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size exceeds u64")
    })?;
    let path = PathBuf::from(path);

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        // Only the creating user may open the mapping.
        .mode(0o600)
        .open(&path)?;
    // The keeper exists from here on, so every error path below cleans up.
    let keeper = ShmKeeper { path };

    // Every byte reads as zero because the file is all holes.
    file.set_len(size_u64)?;

    Ok((keeper, ShmHandle { file, size }))
}

/// Opens the shared memory at `path`.
///
/// # Errors
///
/// Returns an error if the shared memory is unavailable, which is the common
/// case once its keeper has been dropped.
pub fn open(path: &OsStr) -> io::Result<ShmHandle> {
    let file = open_file(path)?;
    // If another process shrinks the file before `map`, mapping fails. If it
    // resizes afterwards, nothing here touches the mapped pages. A concurrent
    // resize cannot make a mapping access invalid memory.
    let size = usize::try_from(file.metadata()?.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid shared-memory size"))?;
    if size == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "shared-memory size is zero"));
    }
    Ok(ShmHandle { file, size })
}

fn open_file(path: &OsStr) -> io::Result<File> {
    let path_bytes = path.as_bytes();
    let len_with_nul = path_bytes.len().checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let mut path_buf = [0_u8; sigsafe::fs::PATH_MAX];
    let path = path_buf
        .get_mut(..len_with_nul)
        .ok_or_else(|| io::Error::from_raw_os_error(sigsafe::Errno::NAMETOOLONG.raw_os_error()))?;
    path[..path_bytes.len()].copy_from_slice(path_bytes);
    let path = sigsafe::CStr::from_bytes_with_nul(path).map_err(|_| io::ErrorKind::InvalidInput)?;
    let fd = sigsafe::fs::openat(
        sigsafe::CWD,
        path,
        sigsafe::fs::OFlags::RDWR | sigsafe::fs::OFlags::CLOEXEC,
        sigsafe::fs::Mode::empty(),
    )
    .map_err(errno_to_io)?;
    let fd = sigsafe::IntoRawFd::into_raw_fd(fd);
    // SAFETY: ownership of the descriptor returned by `openat` transfers to
    // this `File` without closing or duplicating it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn errno_to_io(errno: sigsafe::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

impl Drop for ShmKeeper {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl ShmHandle {
    /// Maps the shared bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the mapping cannot be established.
    pub fn map(&self) -> io::Result<Mapping> {
        Ok(Mapping { raw: MmapOptions::new().len(self.size).map_raw(&self.file)? })
    }
}

#[expect(clippy::len_without_is_empty, reason = "shared-memory mappings are always non-empty")]
impl Mapping {
    /// Returns the mapped length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Returns a raw pointer to the first mapped byte.
    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.raw.as_mut_ptr()
    }

    /// Returns the mapped bytes as a shared slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure that no process or thread mutates the mapping for
    /// the lifetime of the returned slice.
    #[must_use]
    pub unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: The mapping is valid for its full length, and the caller
        // guarantees that it is not mutated while the slice is borrowed.
        unsafe { std::slice::from_raw_parts(self.as_ptr().cast_const(), self.len()) }
    }
}
