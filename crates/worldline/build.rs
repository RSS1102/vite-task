//! Embed the built web UI (`ui/dist`) into the binary.
//!
//! Walks `ui/dist` and generates a slice of `(url_path, bytes)` pairs via
//! `include_bytes!`. `ui/dist` is committed to the repository; rebuild it with
//! `just build-ui`. A test (`tests/dist_up_to_date.rs`) verifies the committed
//! dist matches the UI source.
#![expect(
    clippy::disallowed_types,
    reason = "build scripts use std path/string types; vite_path is not a build dependency"
)]

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dist = PathBuf::from(&manifest_dir).join("ui").join("dist");
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut entries = Vec::new();
    if dist.is_dir() {
        collect(&dist, &dist, &mut entries);
    } else {
        println!(
            "cargo:warning=worldline: {} is missing; the web UI will be empty. Run `just build-ui`.",
            dist.display()
        );
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut generated = String::from(
        "/// Embedded web UI assets: (url path relative to dist, file bytes).\n\
         pub static ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for (url_path, abs_path) in &entries {
        // `{:?}` emits a valid, escaped Rust string literal on every platform.
        writeln!(generated, "    ({url_path:?}, include_bytes!({abs_path:?})),")
            .expect("write to String");
    }
    generated.push_str("];\n");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_assets.rs");
    fs::write(&out, generated).expect("write embedded_assets.rs");
}

/// Recursively collect files under `dir`, keyed by their `/`-joined path
/// relative to `root`. Dotfiles (e.g. `.source-hash`) are skipped.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let read = fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in read {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).expect("strip dist prefix");
            let url_path = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((url_path, path.to_string_lossy().into_owned()));
        }
    }
}
