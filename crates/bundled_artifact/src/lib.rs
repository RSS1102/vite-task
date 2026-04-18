use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// An artifact (e.g., a DLL or shared library) whose content is embedded and needs to be written to disk.
pub struct Artifact {
    name: &'static str,
    content: &'static [u8],
    hash: &'static str,
}

/// Construct an [`Artifact`] from the env vars published by a build script
/// via `bundled_artifact_build::register`. Must match the `ENV_PREFIX`
/// constant in `bundled_artifact_build`.
#[macro_export]
macro_rules! artifact {
    ($name:literal) => {
        $crate::Artifact::__new(
            $name,
            ::core::include_bytes!(::core::env!(::core::concat!(
                "BUNDLED_ARTIFACT_",
                $name,
                "_PATH"
            ))),
            ::core::env!(::core::concat!("BUNDLED_ARTIFACT_", $name, "_HASH")),
        )
    };
}

impl Artifact {
    #[doc(hidden)]
    #[must_use]
    pub const fn __new(name: &'static str, content: &'static [u8], hash: &'static str) -> Self {
        Self { name, content, hash }
    }

    /// Ensure the artifact is materialized in `dir` under a content-addressed
    /// filename, writing it if missing.
    ///
    /// Returns the final path. If a file with the same hash already exists at
    /// the target path, it is reused without rewriting.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory can't be read/written, or if the
    /// temp-file rename fails and the destination still doesn't exist.
    pub fn ensure_in(&self, dir: impl AsRef<Path>, suffix: &str) -> io::Result<PathBuf> {
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
