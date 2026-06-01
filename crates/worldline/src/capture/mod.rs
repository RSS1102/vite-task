//! Capture: wires the fspy write-open/write-close callbacks into the
//! [`Snapshotter`] timeline.

mod store;

use std::sync::Arc;

use fspy::{AccessMode, FileEvent, FileEventKind};
pub use store::{CapturedData, Event, EventKind, OpId, Snapshotter, rebuild_frontier};
use vite_path::AbsolutePath;

use crate::ignore::IgnoreSet;

/// Register the write-snapshot callback on `cmd`.
///
/// The `WRITE` mask means the callback fires only on write opens and write
/// closes — readonly opens/closes are filtered out in the supervisor at zero
/// cost. On a write-open we record the file's pre-write content; on a
/// write-close we record its post-write content (the authoritative final
/// state). Ignored paths and non-UTF-8 paths are skipped.
///
/// The descriptor handed to the supervisor is the traced process's own fd,
/// which is write-only for a write open and therefore unreadable. We instead
/// read the file's content by path: the supervisor process opens it `O_RDONLY`
/// itself. The traced process is blocked in the open/close hook while we do
/// this, so the content is a consistent point-in-time read. (For a truncating
/// open the pre-write content reads as empty, which is the file's observable
/// state at that instant — the post-write content arrives on the close event.)
pub fn install_callback(cmd: &mut fspy::Command, snap: Snapshotter, ignore: Arc<IgnoreSet>) {
    cmd.on_file_event(AccessMode::WRITE, move |event: FileEvent<'_>| {
        let Some(path) = event.path.get() else {
            // Closing events may have an unresolvable path (deleted/anonymous).
            return;
        };
        // Canonicalize so a file maps to a single key regardless of how fspy
        // reported it: on macOS an open event carries the path as passed to
        // `open` (e.g. `/tmp/x`) while a close event resolves it via `F_GETPATH`
        // (e.g. `/private/tmp/x`). Falling back to the raw path keeps deleted
        // files (whose canonicalization fails) addressable.
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let Some(abs) = AbsolutePath::new(&canonical) else {
            return;
        };
        if ignore.is_ignored(abs) {
            return;
        }
        let Some(path_str) = canonical.to_str() else {
            // Loro keys must be UTF-8; skip the rare non-UTF-8 path.
            return;
        };

        let kind = match event.kind {
            FileEventKind::Opened => EventKind::WriteOpen,
            FileEventKind::Closing => EventKind::WriteClose,
        };

        let Ok(content) = std::fs::read(&canonical) else {
            return;
        };
        snap.record_write(path_str, &content, kind);
    });
}
