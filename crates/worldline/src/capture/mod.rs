//! Capture: wires the fspy write-open/write-close callbacks into the
//! [`Snapshotter`] timeline.

mod store;

use std::{fs::File, sync::Arc};

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
pub fn install_callback(cmd: &mut fspy::Command, snap: Snapshotter, ignore: Arc<IgnoreSet>) {
    cmd.on_file_event(AccessMode::WRITE, move |event: FileEvent<'_>| {
        let Some(path) = event.path.get() else {
            // Closing events may have an unresolvable path (deleted/anonymous).
            return;
        };
        let Some(abs) = AbsolutePath::new(path) else {
            return;
        };
        if ignore.is_ignored(abs) {
            return;
        }
        let Some(path_str) = path.to_str() else {
            // Loro keys must be UTF-8; skip the rare non-UTF-8 path.
            return;
        };

        let kind = match event.kind {
            FileEventKind::Opened => EventKind::WriteOpen,
            FileEventKind::Closing => EventKind::WriteClose,
        };
        // Right after a write-open the offset is 0; restore it afterwards so we
        // don't disturb the offset the traced process will use. A closing file
        // needs no restore.
        let restore = matches!(event.kind, FileEventKind::Opened);

        let mut file = event.fd.as_file();
        let Ok(content) = read_full(&mut file, restore) else {
            return;
        };
        snap.record_write(path_str, &content, kind);
    });
}

/// Read a file's full content from offset 0 without disturbing the descriptor's
/// shared file offset.
///
/// On Unix `pread` (`read_at`) is positional and never touches the offset. On
/// Windows `seek_read` is positional too, but to be safe for write-opens we
/// rewind the pointer to 0 when `restore` is set.
#[cfg_attr(
    not(windows),
    expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the descriptor needs a mutable borrow on Windows to rewind via seek"
    )
)]
fn read_full(file: &mut File, restore: bool) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut offset: u64 = 0;
    loop {
        #[cfg(unix)]
        let read = {
            use std::os::unix::fs::FileExt as _;
            file.read_at(&mut chunk, offset)?
        };
        #[cfg(windows)]
        let read = {
            use std::os::windows::fs::FileExt as _;
            file.seek_read(&mut chunk, offset)?
        };
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        offset += read as u64;
    }

    #[cfg(windows)]
    if restore {
        use std::io::{Seek as _, SeekFrom};
        let _ = file.seek(SeekFrom::Start(0));
    }
    #[cfg(not(windows))]
    let _ = restore;

    Ok(buf)
}
