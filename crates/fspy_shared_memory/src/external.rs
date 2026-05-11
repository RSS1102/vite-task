use std::{io, num::NonZeroUsize};

use shared_memory::ShmemError;

pub struct Inner {
    shm: shared_memory::Shmem,
}

impl Inner {
    pub fn create(size: NonZeroUsize) -> io::Result<Self> {
        let shm = shared_memory::ShmemConf::new().size(size.get()).create().map_err(to_io_error)?;
        Ok(Self { shm })
    }

    pub fn open(os_id: &str, size: NonZeroUsize) -> io::Result<Self> {
        let shm = shared_memory::ShmemConf::new()
            .size(size.get())
            .os_id(os_id)
            .open()
            .map_err(to_io_error)?;
        Ok(Self { shm })
    }

    pub fn os_id(&self) -> &str {
        self.shm.get_os_id()
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.shm.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.shm.len()
    }
}

/// Translate `shared_memory`'s opaque error into an `io::Error`, preserving
/// the OS error code when present so callers can dispatch on `ErrorKind`
/// (e.g. `NotFound` for "creator dropped the region").
fn to_io_error(err: ShmemError) -> io::Error {
    match err {
        ShmemError::MapCreateFailed(code) | ShmemError::MapOpenFailed(code) =>
        {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "OS error codes are small positive integers; the cast is exact"
            )]
            io::Error::from_raw_os_error(code as i32)
        }
        other => io::Error::other(other),
    }
}
