//! End-to-end capture: run a child that writes files through the real runner
//! (piped transport, since the test's stdio is not a TTY), then verify the
//! reconstructed list of writes — each pairing an open snapshot (`before`) with
//! the close snapshot (`after`) of the same descriptor.
//!
//! Gated off musl, where fspy's blocking callbacks are compiled out (mirrors
//! `fspy`'s own `static_executable.rs`).
#![cfg(not(target_env = "musl"))]
#![expect(clippy::disallowed_types, reason = "test code uses std String/Path for convenience")]

use subprocess_test::command_for_fn;
use vite_path::AbsolutePathBuf;
use worldline::{
    ignore::IgnoreSet,
    run::{RunOptions, run},
    serve::{ApiData, ApiWrite, BlobEncoding, reconstruct},
};

/// UTF-8 content of a served blob id, or `None` if it's binary/missing.
fn text(api: &ApiData, id: &str) -> Option<String> {
    api.blobs
        .get(id)
        .and_then(|b| matches!(b.enc, BlobEncoding::Utf8).then(|| b.data.as_str().to_owned()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_writes_with_before_and_after() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap().to_owned();

    let cmd = command_for_fn!(dir_arg, |dir: String| {
        use std::io::Write as _;
        let dir = std::path::Path::new(&dir);
        let a = dir.join("a.txt");
        // Create a.txt = "v1" (truncating write -> before is empty).
        std::fs::write(&a, b"v1").unwrap();
        // Append "v2" without truncating: the open snapshot is the existing
        // "v1", which must become this write's `before` (paired by fd).
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        f.write_all(b"v2").unwrap();
        drop(f);
        // A non-UTF-8 file.
        std::fs::write(dir.join("img.bin"), [0u8, 159, 146, 150]).unwrap();
        // exit(0) (inside subprocess_test) won't flush, so write+flush explicitly.
        let mut out = std::io::stdout();
        out.write_all(b"done!").unwrap();
        out.flush().unwrap();
    });

    let cwd = AbsolutePathBuf::new(dir.path().to_path_buf()).unwrap();
    let ignore = IgnoreSet::new(cwd.clone(), true, &[]).unwrap();
    let captured = run(RunOptions { program: cmd.program, args: cmd.args, cwd, ignore })
        .await
        .expect("run worldline");

    assert_eq!(captured.meta.exit_code, Some(0), "child should exit cleanly");
    assert!(
        captured.data.output.windows(5).any(|w| w == b"done!"),
        "expected captured output to contain the child's stdout"
    );

    let api = reconstruct(&captured);

    let a_writes: Vec<&ApiWrite> =
        api.writes.iter().filter(|w| w.path.ends_with("a.txt")).collect();
    assert!(a_writes.len() >= 2, "expected >= 2 writes to a.txt, got {}", a_writes.len());

    // The create: before empty -> after "v1".
    assert!(
        a_writes.iter().any(|w| {
            text(&api, w.before.as_str()).as_deref() == Some("")
                && text(&api, w.after.as_str()).as_deref() == Some("v1")
        }),
        "expected a create write ''->'v1'"
    );

    // The append: the open snapshot "v1" is paired (same fd) with the close
    // snapshot "v1v2". A non-empty `before` proves the open content was kept.
    assert!(
        a_writes.iter().any(|w| {
            text(&api, w.before.as_str()).as_deref() == Some("v1")
                && text(&api, w.after.as_str()).as_deref() == Some("v1v2")
        }),
        "expected an append write before='v1' after='v1v2' (open snapshot paired with close)"
    );

    // img.bin is recorded as a write whose after is a binary blob.
    let img = api.writes.iter().find(|w| w.path.ends_with("img.bin")).expect("img.bin write");
    assert!(
        matches!(api.blobs[img.after.as_str()].enc, BlobEncoding::Base64),
        "non-UTF-8 img.bin should be served as a base64 blob"
    );
}
