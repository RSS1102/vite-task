//! Unix shared memory backed by a sparse temporary file and identified by its
//! path.

use std::{
    env::temp_dir,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    num::NonZeroUsize,
    os::unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _, io::IntoRawFd as _},
    path::PathBuf,
};

use memmap2::{MmapOptions, MmapRaw};
use uuid::Uuid;

use crate::BACKING_PREFIX;

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
    file: fspy_nostd::OwnedFd,
    size: NonZeroUsize,
}

/// The mapped shared bytes.
///
/// A `Mapping` keeps the bytes alive until it is dropped and cannot affect the
/// shared memory's identifier.
pub struct Mapping {
    raw: MmapRaw,
}

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
    let file = into_nostd_fd(file);

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
pub fn open(id: &OsStr) -> io::Result<ShmHandle> {
    let file = open_file(id)?;
    // If another process shrinks the file before `map`, mapping fails. If it
    // resizes afterwards, nothing here touches the mapped pages. A concurrent
    // resize cannot make a mapping access invalid memory.
    let size = usize::try_from(fspy_nostd::fs::fstat(&file).map_err(error_to_io)?.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid shared-memory size"))?;
    let size = NonZeroUsize::new(size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "shared-memory size is zero"))?;
    Ok(ShmHandle { file, size })
}

fn open_file(path: &OsStr) -> io::Result<fspy_nostd::OwnedFd> {
    let mut buf = [0_u8; fspy_nostd::fs::PATH_MAX];
    let path = copy_path(path, &mut buf)?;
    fspy_nostd::fs::openat(
        fspy_nostd::CWD,
        path,
        fspy_nostd::fs::OFlags::RDWR | fspy_nostd::fs::OFlags::CLOEXEC,
        fspy_nostd::fs::Mode::empty(),
    )
    .map_err(error_to_io)
}

fn copy_path<'buf>(
    path: &OsStr,
    buf: &'buf mut [u8; fspy_nostd::fs::PATH_MAX],
) -> io::Result<fspy_nostd::CStr<'buf, fspy_nostd::Fat>> {
    let bytes = path.as_bytes();
    if bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"));
    }

    let len_with_nul = bytes.len().checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let initialized = buf.get_mut(..len_with_nul).ok_or_else(|| {
        io::Error::from_raw_os_error(fspy_nostd::Error::NAMETOOLONG.raw_os_error())
    })?;
    initialized[..bytes.len()].copy_from_slice(bytes);
    initialized[bytes.len()] = 0;

    // SAFETY: the copied path contains no NUL, followed by the terminator set
    // above, and the returned view borrows the initialized buffer prefix.
    Ok(unsafe { fspy_nostd::CStr::from_units_with_nul_unchecked(initialized) })
}

fn error_to_io(error: fspy_nostd::Error) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn into_nostd_fd(file: File) -> fspy_nostd::OwnedFd {
    let fd = file.into_raw_fd();
    // SAFETY: ownership of `file`'s descriptor transfers without closing or
    // duplicating it.
    unsafe { fspy_nostd::FromRawFd::from_raw_fd(fd) }
}

impl Drop for ShmKeeper {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
    pub fn map(&self) -> io::Result<Mapping> {
        let file = fspy_nostd::AsRawFd::as_raw_fd(&self.file);
        Ok(Mapping { raw: MmapOptions::new().len(self.size.get()).map_raw(file)? })
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
