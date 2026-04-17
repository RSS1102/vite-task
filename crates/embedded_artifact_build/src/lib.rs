use std::{fs, io, path::Path};

/// Compute the `xxh3_128` hash of `bytes`. Useful for verifying a downloaded
/// artifact against an expected hash before calling [`write_artifact`].
#[must_use]
pub fn hash(bytes: &[u8]) -> u128 {
    xxhash_rust::xxh3::xxh3_128(bytes)
}

/// Write an artifact produced by a build script so `embedded_artifact`'s
/// `artifact!` macro can load it from `OUT_DIR` at compile time.
///
/// Creates two files in `out_dir`: `{name}` holding `bytes`, and
/// `{name}.hash` holding the hex-formatted `xxh3_128` hash.
///
/// # Errors
///
/// Returns the first I/O error from either write.
pub fn write_artifact(out_dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    fs::write(out_dir.join(name), bytes)?;
    fs::write(out_dir.join(format!("{name}.hash")), format!("{:x}", hash(bytes)))?;
    Ok(())
}
