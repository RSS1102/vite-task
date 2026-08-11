//! Windows shared memory backed by a sparse file at a caller-provided path.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _},
};

use memmap2::{MmapOptions, MmapRaw};
#[cfg(test)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    },
    System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE},
};

/// Borrowed path accepted by shared-memory operations on Windows.
pub type Path<'a> = &'a OsStr;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const TEMPORARY: u32 = FILE_ATTRIBUTE_TEMPORARY;
const DELETE_ON_CLOSE: u32 = FILE_FLAG_DELETE_ON_CLOSE;
const DELETE_ACCESS: u32 = windows_sys::Win32::Storage::FileSystem::DELETE;

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
/// Returns an already opened [`ShmHandle`], so the creating process never has
/// to go through [`open`]. The caller owns the backing path and must eventually
/// pass it to [`remove`].
///
/// Only pages that are actually written occupy memory or disk, so a large
/// capacity is cheap.
///
/// # Errors
///
/// Returns an error if the shared memory cannot be created or sized, or the
/// containing volume does not support sparse files.
pub fn create(path: Path<'_>, size: usize) -> io::Result<ShmHandle> {
    if size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared-memory size must be nonzero",
        ));
    }
    let size_u64 = u64::try_from(size).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "shared-memory size exceeds u64")
    })?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(SHARE_ALL)
        // Ask Windows to keep the data in memory when it can.
        .attributes(TEMPORARY)
        .open(path)?;

    // NTFS allocates clusters for the whole logical size unless the file is
    // marked sparse first, which would turn the capacity into real disk usage.
    // Volumes without sparse-file support fail here.
    let initialized = set_sparse(&file).and_then(|()| {
        // Every byte reads as zero because the file is all holes.
        file.set_len(size_u64)
    });
    if let Err(error) = initialized {
        drop(file);
        let _ = remove(path);
        return Err(error);
    }

    Ok(ShmHandle { file, size })
}

/// Opens the shared memory at `path`.
///
/// # Errors
///
/// Returns an error if the shared memory is unavailable, including after its
/// backing-file name has been removed.
pub fn open(path: Path<'_>) -> io::Result<ShmHandle> {
    // Rust handles are non-inheritable, and its default share mode permits
    // concurrent read, write and delete access.
    let file = OpenOptions::new().read(true).write(true).open(path)?;
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

/// Removes the shared memory's backing-file name.
///
/// Existing handles and mappings remain usable, but later calls to [`open`]
/// fail once removal succeeds.
///
/// # Errors
///
/// Returns an error if removal cannot be performed or scheduled.
pub fn remove(path: Path<'_>) -> io::Result<()> {
    if fs::remove_file(path).is_ok() {
        return Ok(());
    }

    // Windows versions without POSIX delete refuse to remove the name of a
    // mapped file. Arm the deferred delete instead: closing this handle deletes
    // the file once every other handle to it is closed.
    OpenOptions::new()
        .access_mode(DELETE_ACCESS)
        .share_mode(SHARE_ALL)
        .custom_flags(DELETE_ON_CLOSE)
        .open(path)
        .map(drop)
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

/// Marks `file` sparse so that setting its length reserves no clusters.
fn set_sparse(file: &File) -> io::Result<()> {
    let mut bytes_returned = 0;
    // SAFETY: `file` supplies a valid synchronous file handle. FSCTL_SET_SPARSE
    // requires no input or output buffers, and `bytes_returned` is writable for
    // the duration of the call.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &raw mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Returns the backing file's logical size and allocated byte count.
#[cfg(test)]
pub fn file_sizes(file: &File) -> io::Result<(u64, u64)> {
    let mut info = FILE_STANDARD_INFO::default();
    let info_size = u32::try_from(std::mem::size_of::<FILE_STANDARD_INFO>())
        .map_err(|_| io::Error::other("file size information is too large"))?;
    // SAFETY: `file` supplies a valid handle and `info` is a writable
    // FILE_STANDARD_INFO buffer of exactly `info_size` bytes.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileStandardInfo,
            (&raw mut info).cast(),
            info_size,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    let logical_size = u64::try_from(info.EndOfFile)
        .map_err(|_| io::Error::other("file has a negative logical size"))?;
    let allocated_size = u64::try_from(info.AllocationSize)
        .map_err(|_| io::Error::other("file has a negative allocated size"))?;
    Ok((logical_size, allocated_size))
}
