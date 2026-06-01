//! End-to-end capture: run a child that writes files through the real runner
//! (piped transport, since the test's stdio is not a TTY), then verify the
//! timeline events and the Loro snapshot round-trip.
//!
//! Gated off musl, where fspy's blocking callbacks are compiled out (mirrors
//! `fspy`'s own `static_executable.rs`).
#![cfg(not(target_env = "musl"))]
#![expect(clippy::disallowed_types, reason = "test code uses std String/Path for convenience")]

use loro::{LoroDoc, LoroValue};
use subprocess_test::command_for_fn;
use vite_path::AbsolutePathBuf;
use worldline::{
    capture::rebuild_frontier,
    ignore::IgnoreSet,
    run::{RunOptions, run},
};

fn text_at(doc: &LoroDoc, suffix: &str) -> Option<String> {
    let LoroValue::Map(map) = doc.get_map("text_files").get_deep_value() else {
        return None;
    };
    map.iter().find(|(k, _)| k.ends_with(suffix)).and_then(|(_, v)| match v {
        LoroValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captures_file_writes_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap().to_owned();

    let cmd = command_for_fn!(dir_arg, |dir: String| {
        use std::io::Write as _;
        let dir = std::path::Path::new(&dir);
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("a.txt"), b"hello world").unwrap();
        std::fs::write(dir.join("img.bin"), [0u8, 159, 146, 150]).unwrap();
        // exit(0) (inside subprocess_test) won't flush, so write+flush explicitly.
        let mut out = std::io::stdout();
        out.write_all(b"done!").unwrap();
        out.flush().unwrap();
    });

    // Root the run (and ignore set) at the tempdir the child writes into.
    let cwd = AbsolutePathBuf::new(dir.path().to_path_buf()).unwrap();
    let ignore = IgnoreSet::new(cwd.clone(), true, &[]).unwrap();
    let captured = run(RunOptions { program: cmd.program, args: cmd.args, cwd, ignore })
        .await
        .expect("run worldline");

    assert_eq!(captured.meta.exit_code, Some(0), "child should exit cleanly");

    // Both files appear in the timeline.
    let paths: Vec<&str> = captured.data.events.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.ends_with("a.txt")), "a.txt missing from {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("img.bin")), "img.bin missing from {paths:?}");

    // The terminal output was captured.
    assert!(
        captured.data.output.windows(5).any(|w| w == b"done!"),
        "expected captured output to contain the child's stdout"
    );

    // Reconstruct the final filesystem state from the Loro snapshot.
    let doc = LoroDoc::new();
    doc.import(&captured.data.snapshot).expect("import snapshot");
    let last = captured.data.events.last().expect("at least one event");
    doc.checkout(&rebuild_frontier(&last.frontier)).expect("checkout last frontier");
    assert_eq!(
        text_at(&doc, "a.txt").as_deref(),
        Some("hello world"),
        "final a.txt content should be the last write"
    );
    doc.checkout_to_latest();
}
