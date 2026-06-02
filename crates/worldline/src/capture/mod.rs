//! Capture: wires the fspy write-open/write-close callbacks into the
//! [`Snapshotter`] timeline.

mod store;

use std::sync::Arc;

use fspy::{AccessMode, FileEvent, FileEventKind};
pub use store::{CapturedData, FileId, OpId, Snapshotter, Write, rebuild_frontier};
use vite_path::AbsolutePath;

use crate::ignore::IgnoreSet;

/// Register the write-snapshot callback on `cmd`.
///
/// The `WRITE` mask means the callback fires only on write opens and write
/// closes — readonly opens/closes are filtered out in the supervisor at zero
/// cost. On a write-open we snapshot the file's pre-write content; on the
/// matching write-close (paired by `(pid, raw_fd)`) we snapshot its post-write
/// content and record one [`Write`]. Ignored and non-UTF-8 paths are skipped.
///
/// The descriptor handed to the supervisor is the traced process's own fd,
/// which is write-only for a write open and therefore unreadable. We instead
/// read the file's content by path: the supervisor process opens it `O_RDONLY`
/// itself. The traced process is blocked in the open/close hook while we do
/// this, so the content is a consistent point-in-time read. A truncating open
/// reads as empty here (it already ran); the store recovers the pre-write
/// content from the last recorded write, using the file's identity to avoid
/// resurrecting stale content after a replacement (see [`Snapshotter`]).
pub fn install_callback(cmd: &mut fspy::Command, snap: Snapshotter, ignore: Arc<IgnoreSet>) {
    cmd.on_file_event(AccessMode::WRITE, move |event: FileEvent<'_>| {
        let Some(path) = event.path.get() else {
            // Closing events may have an unresolvable path (deleted/anonymous).
            return;
        };
        // Canonicalize so a file maps to a single key regardless of how fspy
        // reported it: on macOS an open event carries the path as passed to
        // `open` (e.g. `/tmp/x`) while a close event resolves it via `F_GETPATH`
        // (e.g. `/private/tmp/x`).
        let canonical = canonicalize_robust(path);

        // A rename relabels an already-captured write (its content lives under
        // the source path) to its destination — following an atomic
        // write-temp-then-rename to the final name. No content to read here.
        if event.kind == FileEventKind::Renamed {
            let Some(to) = event.to_path else {
                return;
            };
            let to_canonical = canonicalize_robust(to);
            // Only relabel when the destination is in scope; otherwise the file
            // moved out of interest and the temp write is left as-is.
            let Some(to_abs) = AbsolutePath::new(&to_canonical) else {
                return;
            };
            if ignore.is_ignored(to_abs) {
                return;
            }
            if let (Some(from_str), Some(to_str)) = (canonical.to_str(), to_canonical.to_str()) {
                snap.record_rename(from_str, to_str);
            }
            return;
        }

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

        // Open once and read both the content and the file's on-disk identity
        // from the same handle — the traced process is blocked in the hook while
        // we do this, so avoiding a second path walk + stat keeps it short.
        let Ok(mut file) = std::fs::File::open(&canonical) else {
            return;
        };
        // The identity (used to tell an in-place rewrite from a replacement of
        // the same path — rename-over / delete-and-recreate) comes from an
        // `fstat` on the open handle, not a second path lookup.
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            file.metadata().ok().map(|meta| FileId::new(meta.dev(), meta.ino()))
        };
        // A stable on-disk identity isn't available off Unix without unstable
        // APIs (`windows_by_handle`), so replacement detection is skipped there:
        // a truncating rewrite keeps the prior content (correct for in-place
        // rewrites; a rename-over or delete-and-recreate may show stale content).
        #[cfg(not(unix))]
        let identity: Option<FileId> = None;
        let mut content = Vec::new();
        if std::io::Read::read_to_end(&mut file, &mut content).is_err() {
            return;
        }
        let (pid, fd) = (event.pid, event.raw_fd);
        match event.kind {
            FileEventKind::Opened => snap.record_open(pid, fd, path_str, &content, identity),
            FileEventKind::Closing => snap.record_close(pid, fd, path_str, &content, identity),
            // Handled above (before the content read).
            FileEventKind::Renamed => {}
        }
    });
}

/// Canonicalize `path`, falling back to canonicalizing its parent and
/// re-appending the file name when the file itself no longer exists (e.g. a
/// rename source that has already moved away). This keeps the key consistent
/// with how the file was recorded while it existed — on macOS canonicalization
/// resolves `/tmp` to `/private/tmp`, so a raw fallback would not match.
#[expect(
    clippy::disallowed_types,
    reason = "interfacing with std::fs::canonicalize requires std::path types"
)]
fn canonicalize_robust(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = std::fs::canonicalize(parent)
    {
        return parent.join(name);
    }
    path.to_path_buf()
}
