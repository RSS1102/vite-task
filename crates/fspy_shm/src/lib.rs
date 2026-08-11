#![cfg_attr(all(unix, not(test)), no_std)]
#![doc = include_str!("../README.md")]

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use platform::{Error, Mapping, Path, Result, ShmHandle, create, open, remove};
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::fs::File;
    use std::{
        env::temp_dir,
        ffi::{OsStr, OsString},
        fs,
        mem::align_of,
        path::Path,
        process::Command,
    };
    #[cfg(unix)]
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    use subprocess_test::command_for_fn;
    use uuid::Uuid;

    use super::{Mapping, ShmHandle};

    const TEST_BACKING_PREFIX: &str = "vite-task-fspy-test-";
    // Page-aligned on all supported targets.
    const SIZE: usize = 64 * 1024;
    // Use one byte more than 64 KiB to test multiple pages and a partial last page.
    const ZERO_INITIALIZED_SIZE: usize = SIZE + 1;

    #[test]
    fn new_mapping_is_zero_initialized_in_all_views() {
        let (id, _cleanup, handle) = create(ZERO_INITIALIZED_SIZE);
        let first = handle.map().unwrap();
        let second = with_path(&id, super::open).unwrap().map().unwrap();

        assert_zero_initialized(&first);
        assert_zero_initialized(&second);
    }

    #[test]
    fn mappings_of_one_backing_file_are_shared() {
        let (id, _cleanup, handle) = create(SIZE);
        let first = handle.map().unwrap();
        assert_eq!(first.len(), SIZE);
        assert_eq!(first.as_ptr() as usize % align_of::<usize>(), 0);

        let second = with_path(&id, super::open).unwrap().map().unwrap();
        assert_eq!(second.len(), SIZE);

        write_byte(&first, 0, 17);
        assert_eq!(read_byte(&second, 0), 17);
        write_byte(&second, SIZE - 1, 29);
        assert_eq!(read_byte(&first, SIZE - 1), 29);
    }

    #[test]
    fn one_handle_maps_repeatedly() {
        let (_id, _cleanup, handle) = create(SIZE);
        let first = handle.map().unwrap();
        let second = handle.map().unwrap();

        write_byte(&first, 0, 17);
        assert_eq!(read_byte(&second, 0), 17);
    }

    #[test]
    fn mapping_is_visible_across_processes() {
        let (id, _cleanup, handle) = create(SIZE);
        let mapping = handle.map().unwrap();
        write_byte(&mapping, 0, 17);

        let id = id.to_str().expect("test temp dir is UTF-8").to_owned();
        let command = command_for_fn!(id, |id: String| {
            let opened = with_path(OsStr::new(&id), super::open).unwrap().map().unwrap();
            assert_eq!(read_byte(&opened, 0), 17);
            write_byte(&opened, SIZE - 1, 29);
        });
        assert!(Command::from(command).status().unwrap().success());
        assert_eq!(read_byte(&mapping, SIZE - 1), 29);
    }

    #[test]
    fn subprocess_open_ignores_changed_temp_and_working_directory() {
        let (id, _cleanup, handle) = create(SIZE);
        let mapping = handle.map().unwrap();
        let changed_cwd =
            temp_dir().join(format!("{TEST_BACKING_PREFIX}changed-cwd-{}", Uuid::new_v4()));
        fs::create_dir(&changed_cwd).unwrap();
        write_byte(&mapping, 0, 17);

        let id = id.to_str().expect("test temp dir is UTF-8").to_owned();
        let mut command = command_for_fn!(id, |id: String| {
            let opened = with_path(OsStr::new(&id), super::open).unwrap().map().unwrap();
            assert_eq!(read_byte(&opened, 0), 17);
            write_byte(&opened, SIZE - 1, 29);
        });
        command.cwd = changed_cwd.clone();
        // The shared-memory path is absolute, so a relative temporary directory
        // in the child must make no difference on any platform.
        for name in ["TMPDIR", "TMP", "TEMP"] {
            command.envs.insert(OsString::from(name), OsString::from("changed-relative-tmp"));
        }
        let succeeded = Command::from(command).status().unwrap().success();
        fs::remove_dir(changed_cwd).unwrap();

        assert!(succeeded);
        assert_eq!(read_byte(&mapping, SIZE - 1), 29);
    }

    #[test]
    fn remove_prevents_new_opens() {
        let (id, _cleanup, handle) = create(SIZE);
        drop(handle);
        with_path(&id, super::remove).unwrap();

        assert!(with_path(&id, super::open).is_err());
    }

    #[test]
    fn opened_mapping_survives_remove() {
        let (id, _cleanup, handle) = create(SIZE);
        let opened = with_path(&id, super::open).unwrap().map().unwrap();
        write_byte(&opened, 0, 17);
        drop(handle);
        with_path(&id, super::remove).unwrap();

        assert!(with_path(&id, super::open).is_err());
        assert_eq!(read_byte(&opened, 0), 17);
        write_byte(&opened, SIZE - 1, 29);
        assert_eq!(read_byte(&opened, SIZE - 1), 29);
    }

    /// Removal semantics, part one: a mapping alone (no handle) keeps the
    /// bytes alive after removal of the name.
    #[test]
    fn remove_backing_file_preserves_existing_mappings() {
        let (id, _cleanup, handle) = create(SIZE);
        let path = Path::new(&id).to_owned();
        let opened = with_path(&id, super::open).unwrap().map().unwrap();
        drop(handle);
        assert!(path.exists());

        with_path(&id, super::remove).unwrap();

        assert!(!path.exists());
        assert!(with_path(&id, super::open).is_err());
        write_byte(&opened, 0, 17);
        assert_eq!(read_byte(&opened, 0), 17);
    }

    /// Removal semantics, part two: the name goes away even while a handle is
    /// still open, and that handle keeps mapping the same bytes afterwards.
    #[test]
    fn remove_with_open_handle_preserves_handle() {
        let (id, _cleanup, handle) = create(SIZE);
        let before = handle.map().unwrap();
        write_byte(&before, 0, 17);

        with_path(&id, super::remove).unwrap();

        assert!(!Path::new(&id).exists());
        assert!(with_path(&id, super::open).is_err());

        let after = handle.map().unwrap();
        assert_eq!(read_byte(&after, 0), 17);
        write_byte(&after, SIZE - 1, 29);
        assert_eq!(read_byte(&before, SIZE - 1), 29);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn four_gib_mapping_is_sparse_and_supports_endpoint_access() {
        const PRODUCTION_SIZE: usize = 4 * 1024 * 1024 * 1024;
        #[cfg(windows)]
        const MAX_ENDPOINT_ALLOCATION: u64 = 16 * 1024 * 1024;

        let (id, _cleanup, handle) = create(PRODUCTION_SIZE);
        #[cfg(windows)]
        {
            let (logical_size, initial_allocation) = backing_file_sizes(&id);
            assert_eq!(logical_size, PRODUCTION_SIZE as u64);
            assert!(initial_allocation < MAX_ENDPOINT_ALLOCATION);
        }

        let first = handle.map().unwrap();
        let opened = with_path(&id, super::open).unwrap().map().unwrap();
        write_byte(&first, 0, 17);
        write_byte(&first, PRODUCTION_SIZE - 1, 29);
        assert_eq!(read_byte(&opened, 0), 17);
        assert_eq!(read_byte(&opened, PRODUCTION_SIZE - 1), 29);

        // Touching both endpoints must not have allocated the range between them.
        #[cfg(windows)]
        {
            let (logical_size, endpoint_allocation) = backing_file_sizes(&id);
            assert_eq!(logical_size, PRODUCTION_SIZE as u64);
            assert!(endpoint_allocation < MAX_ENDPOINT_ALLOCATION);
        }
    }

    #[cfg(windows)]
    fn backing_file_sizes(id: &OsStr) -> (u64, u64) {
        let file = File::open(id).unwrap();
        super::windows::file_sizes(&file).unwrap()
    }

    struct Cleanup(OsString);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = with_path(&self.0, super::remove);
        }
    }

    fn create(size: usize) -> (OsString, Cleanup, ShmHandle) {
        let path = std::path::absolute(temp_dir())
            .unwrap()
            .join(format!("{TEST_BACKING_PREFIX}{}.shm", Uuid::new_v4().simple()));
        let id = path.into_os_string();
        let handle = with_path(&id, |path| super::create(path, size)).unwrap();
        let cleanup = Cleanup(id.clone());
        (id, cleanup, handle)
    }

    #[cfg(unix)]
    fn with_path<T>(
        path: &OsStr,
        call: impl FnOnce(super::Path<'_>) -> super::Result<T>,
    ) -> super::Result<T> {
        let path = CString::new(path.as_bytes()).map_err(|_| sigsafe::Errno::INVAL)?;
        // SAFETY: `CString` owns one NUL-terminated string for the duration of
        // `call`.
        let path = unsafe { super::Path::from_ptr(path.as_ptr()) };
        call(path)
    }

    #[cfg(windows)]
    fn with_path<T>(
        path: &OsStr,
        call: impl FnOnce(super::Path<'_>) -> super::Result<T>,
    ) -> super::Result<T> {
        call(path)
    }

    fn read_byte(mapping: &Mapping, index: usize) -> u8 {
        assert!(index < mapping.len());
        // SAFETY: The index is in bounds and tests synchronize all accesses.
        unsafe { mapping.as_ptr().add(index).read() }
    }

    fn assert_zero_initialized(mapping: &Mapping) {
        assert!(mapping.len() >= ZERO_INITIALIZED_SIZE);
        assert!((0..mapping.len()).all(|index| read_byte(mapping, index) == 0));
    }

    fn write_byte(mapping: &Mapping, index: usize, value: u8) {
        assert!(index < mapping.len());
        // SAFETY: The index is in bounds and tests synchronize all accesses.
        unsafe { mapping.as_ptr().add(index).write(value) };
    }
}
