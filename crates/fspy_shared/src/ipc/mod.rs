#[cfg(not(target_env = "musl"))]
pub mod channel;
mod native_path;
use std::fmt::Debug;

use bitflags::bitflags;
pub use native_path::NativePath;
pub use native_str::NativeStr;
use wincode::{SchemaRead, SchemaWrite};

#[derive(SchemaWrite, SchemaRead, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct AccessMode(u16);

bitflags! {
    /// What a process did, or tried to do, to one path.
    ///
    /// `READ`, `WRITE` and `READ_DIR` describe the *intent* of the call. The
    /// remaining flags qualify it, and consumers need them to tell a real
    /// dependency from a probe and a real mutation from mere write capability:
    ///
    /// - a `READ` that also has `FAILED` named a path that was not there, so it
    ///   is an absence check rather than a dependency;
    /// - a `WRITE` on its own is only capability. Truncating, exclusively
    ///   creating, or being a rename destination is what makes it a mutation.
    impl AccessMode: u16 {
        /// Opened for reading, or its metadata was inspected.
        const READ = 1;
        /// Opened for writing. On its own this is capability, not mutation.
        const WRITE = 1 << 1;
        /// Directory entries were listed.
        const READ_DIR = 1 << 2;
        /// The call failed. The path was still named, but nothing was read,
        /// written or listed.
        const FAILED = 1 << 3;
        /// The open carried `O_CREAT`.
        const CREATE = 1 << 4;
        /// The open carried `O_TRUNC`, so any previous contents are gone.
        const TRUNCATE = 1 << 5;
        /// The open carried `O_EXCL`, so the path is newly created and had no
        /// previous contents to read.
        const EXCLUSIVE = 1 << 6;
        /// The path was the source of a successful rename and no longer exists
        /// under this name.
        const RENAME_FROM = 1 << 7;
        /// The path was the destination of a successful rename, which replaced
        /// whatever was there.
        const RENAME_TO = 1 << 8;
        /// The path was successfully removed by `unlink`, `remove` or `rmdir`.
        const DELETED = 1 << 9;
        /// A directory was successfully created at this path.
        const CREATED_DIR = 1 << 10;
        /// The path is a directory. Set on rename events so a consumer can
        /// re-attribute writes recorded beneath a renamed directory.
        const IS_DIR = 1 << 11;
    }
}

impl AccessMode {
    /// Whether this access changed the path's contents.
    ///
    /// Write capability alone does not qualify: a descriptor opened `O_RDWR` and
    /// never written leaves the file untouched, which is what Biome does to a
    /// clean source file on a warm run.
    #[must_use]
    pub const fn is_mutation(self) -> bool {
        if self.contains(Self::FAILED) {
            return false;
        }
        if self.intersects(Self::RENAME_TO.union(Self::DELETED).union(Self::CREATED_DIR)) {
            return true;
        }
        self.contains(Self::WRITE)
            && self.intersects(Self::TRUNCATE.union(Self::CREATE.union(Self::EXCLUSIVE)))
    }

    /// Whether this access observed existing content, so the path is a genuine
    /// dependency.
    ///
    /// A failed call observed nothing. An exclusive create observed nothing
    /// either, because the path is new by definition, which is how Go's
    /// `os.CreateTemp` opens atomic-write temporaries.
    #[must_use]
    pub const fn is_content_read(self) -> bool {
        if self.contains(Self::FAILED) || self.contains(Self::EXCLUSIVE) {
            return false;
        }
        self.intersects(Self::READ.union(Self::READ_DIR))
    }
}

impl Debug for AccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct InternalAccessMode(AccessMode);
        impl Debug for InternalAccessMode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                bitflags::parser::to_writer(&self.0, f)
            }
        }
        f.debug_tuple("AccessMode").field(&InternalAccessMode(*self)).finish()
    }
}

#[derive(SchemaWrite, SchemaRead, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathAccess<'a> {
    pub mode: AccessMode,
    pub path: &'a NativePath,
    // TODO: add follow_symlinks (O_NOFOLLOW)
}

impl<'a> PathAccess<'a> {
    pub fn read(path: impl Into<&'a NativePath>) -> Self {
        Self { mode: AccessMode::READ, path: path.into() }
    }

    pub fn read_dir(path: impl Into<&'a NativePath>) -> Self {
        Self { mode: AccessMode::READ_DIR, path: path.into() }
    }
}
