use std::{
    env::temp_dir, fs::OpenOptions, io, num::NonZeroUsize, os::fd::AsFd, path::PathBuf,
    ptr::NonNull,
};

use nix::sys::mman::{MapFlags, ProtFlags, mmap, munmap};
use uuid::Uuid;

pub struct Inner {
    os_id: Box<str>,
    path: PathBuf,
    addr: NonNull<u8>,
    len: NonZeroUsize,
    owns_file: bool,
}

impl Inner {
    pub fn create(size: NonZeroUsize) -> io::Result<Self> {
        let path = temp_dir().join(format!("fspy_shm_{}", Uuid::new_v4().simple()));
        let os_id: Box<str> = path
            .to_str()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "shared memory backing path must be valid UTF-8",
                )
            })?
            .into();

        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        file.set_len(size.get() as u64)?;

        let addr = map(&file, size).inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;

        Ok(Self { os_id, path, addr, len: size, owns_file: true })
    }

    pub fn open(os_id: &str, size: NonZeroUsize) -> io::Result<Self> {
        let path = PathBuf::from(os_id);
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let addr = map(&file, size)?;
        Ok(Self { os_id: os_id.into(), path, addr, len: size, owns_file: false })
    }

    pub fn os_id(&self) -> &str {
        &self.os_id
    }

    #[expect(
        clippy::missing_const_for_fn,
        reason = "Mirrors the non-const macOS/Windows wrapper for API parity"
    )]
    pub fn as_ptr(&self) -> *mut u8 {
        self.addr.as_ptr()
    }

    #[expect(
        clippy::missing_const_for_fn,
        reason = "Mirrors the non-const macOS/Windows wrapper for API parity"
    )]
    pub fn len(&self) -> usize {
        self.len.get()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: `self.addr` came from `mmap` with `self.len` bytes; the
        // mapping is dropped exactly once because `Inner` owns it.
        let _ = unsafe { munmap(self.addr.cast(), self.len.get()) };
        if self.owns_file {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn map<F: AsFd>(file: F, size: NonZeroUsize) -> io::Result<NonNull<u8>> {
    // SAFETY: `mmap` with a valid fd, non-zero length, and `MAP_SHARED` is
    // sound. Treating the returned region as `*mut u8` is fine — it's plain
    // bytes — and cross-process synchronization is the caller's responsibility
    // per the `Shmem::as_slice` contract.
    let ptr = unsafe {
        mmap(
            None,
            size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            file,
            0,
        )
    }
    .map_err(io::Error::from)?;
    Ok(ptr.cast())
}
