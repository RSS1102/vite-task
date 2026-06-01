//! Verifies the committed web UI bundle (`ui/dist`) is in sync with its source.
//!
//! The embedded `ui/dist` is built by `just build-ui` and committed. CI has no
//! Node/Vite step, so instead of rebuilding we hash the UI source and compare
//! against `ui/.source-hash` (written by the build, beside `dist` so Vite's
//! `emptyOutDir` can't clobber it). If the source changed without a rebuild,
//! this fails. Toolchain-only, runs on every platform.
//!
//! Update after changing UI source with `just build-ui`, or directly:
//! `WORLDLINE_UPDATE_UI_HASH=1 cargo test -p worldline --test dist_up_to_date`.
#![expect(
    clippy::disallowed_types,
    clippy::disallowed_macros,
    reason = "test glue uses std path/string types and format!"
)]

use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Top-level UI source files that must trigger a rebuild when changed.
const ROOT_FILES: &[&str] = &["index.html", "package.json", "vite.config.ts", "tsconfig.json"];

fn ui_dir() -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("ui")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Hash of all UI source files (relative path + line-ending-normalized content).
fn source_hash() -> String {
    let ui = ui_dir();
    let mut files = Vec::new();
    collect(&ui.join("src"), &mut files);
    for name in ROOT_FILES {
        let path = ui.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();

    let mut buf = Vec::new();
    for file in &files {
        // `/`-join the components so the hash is identical across platforms.
        let rel = file
            .strip_prefix(&ui)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        buf.extend_from_slice(rel.as_bytes());
        buf.push(0);
        // Normalize CRLF -> LF so Windows checkouts hash identically.
        buf.extend(fs::read(file).unwrap().into_iter().filter(|&b| b != b'\r'));
        buf.push(0);
    }
    format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&buf))
}

#[test]
fn dist_is_up_to_date() {
    let hash = source_hash();
    // Kept beside `dist` (not inside it) so Vite's `emptyOutDir` can't clobber it.
    let hash_path = ui_dir().join(".source-hash");

    if env::var("WORLDLINE_UPDATE_UI_HASH").as_deref() == Ok("1") {
        fs::create_dir_all(hash_path.parent().unwrap()).unwrap();
        fs::write(&hash_path, &hash).unwrap();
        return;
    }

    let committed = fs::read_to_string(&hash_path).unwrap_or_default();
    assert_eq!(
        hash,
        committed.trim(),
        "worldline UI source changed but {} is stale. Rebuild with `just build-ui` \
         (or WORLDLINE_UPDATE_UI_HASH=1 cargo test -p worldline --test dist_up_to_date).",
        hash_path.display(),
    );
}
