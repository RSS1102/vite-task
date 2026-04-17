use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// An artifact (e.g., a DLL or shared library) whose content is embedded and needs to be written to disk.
pub struct Artifact {
    pub name: &'static str,
    pub content: &'static [u8],
    pub hash: &'static str,
}

/// Construct an [`Artifact`] from a `&'static [u8]` expression; the hash is
/// computed from the bytes at compile time.
///
/// Only use this for small artifacts — const-time hashing of large payloads
/// significantly slows compilation. For large artifacts, pre-compute the hash
/// in a build script and use [`artifact!`].
#[macro_export]
macro_rules! artifact_of {
    ($name: literal, $content: expr) => {
        $crate::Artifact::new(
            $name,
            $content,
            $crate::__private::formatcp!("{:x}", $crate::__private::xxh3_128($content)),
        )
    };
}

/// Construct an [`Artifact`] from a file in the caller's `OUT_DIR`.
///
/// The build script is expected to have written `$OUT_DIR/{name}` (the raw
/// bytes) and `$OUT_DIR/{name}.hash` (the hex hash) — see the
/// `embedded_artifact_build` crate for a helper that does both in one call.
#[macro_export]
macro_rules! artifact {
    ($name: literal) => {
        $crate::Artifact::new(
            $name,
            ::core::include_bytes!(::core::concat!(::core::env!("OUT_DIR"), "/", $name)),
            ::core::include_str!(::core::concat!(::core::env!("OUT_DIR"), "/", $name, ".hash")),
        )
    };
}

#[doc(hidden)]
pub mod __private {
    pub use const_format::formatcp;
    pub use xxhash_rust::const_xxh3::xxh3_128;
}

impl Artifact {
    #[must_use]
    pub const fn new(name: &'static str, content: &'static [u8], hash: &'static str) -> Self {
        Self { name, content, hash }
    }

    /// Write the artifact's content to `dir` under a content-addressed filename.
    ///
    /// Returns the final path. If a file with the same hash already exists at
    /// the target path, it is reused without rewriting.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory can't be read/written, or if the
    /// temp-file rename fails and the destination still doesn't exist.
    pub fn write_to(&self, dir: impl AsRef<Path>, suffix: &str) -> io::Result<PathBuf> {
        let dir = dir.as_ref();
        let path = dir.join(format!("{}_{}{}", self.name, self.hash, suffix));

        if fs::exists(&path)? {
            return Ok(path);
        }
        let tmp_path = dir.join(format!("{:x}", rand::random::<u128>()));
        let mut tmp_file_open_options = OpenOptions::new();
        tmp_file_open_options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut tmp_file_open_options, 0o755); // executable
        let mut tmp_file = tmp_file_open_options.open(&tmp_path)?;
        tmp_file.write_all(self.content)?;
        drop(tmp_file);

        if let Err(err) = fs::rename(&tmp_path, &path) {
            if !fs::exists(&path)? {
                return Err(err);
            }
            fs::remove_file(&tmp_path)?;
        }
        Ok(path)
    }
}
