use std::{fs, io, path::Path};

/// Write an artifact produced by a build script so `embedded_artifact`'s
/// `artifact!` macro can load it from `OUT_DIR` at compile time.
///
/// Creates two files in `out_dir`: `{name}` holding `bytes`, and
/// `{name}.hash` holding the hex-formatted hash used by `artifact!` to
/// content-address the extracted file at runtime.
///
/// # Errors
///
/// Returns the first I/O error from either write.
pub fn write_artifact(out_dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    fs::write(out_dir.join(name), bytes)?;
    fs::write(out_dir.join(format!("{name}.hash")), format!("{:x}", hash(bytes)))?;
    Ok(())
}

fn hash(bytes: &[u8]) -> u128 {
    xxhash_rust::xxh3::xxh3_128(bytes)
}
