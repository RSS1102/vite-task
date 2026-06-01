//! The in-memory capture store: a Loro CRDT of file contents over time, plus an
//! ordered list of *writes* and a raw terminal-output byte log.
//!
//! A **write** is one open→…→close lifecycle of a descriptor opened for
//! writing. Its *after* is the content snapshotted in the close callback of
//! that descriptor; its *before* is the file's content just prior to the write
//! — the open-callback snapshot for a fresh or non-truncating open, or, when a
//! truncating open (`O_TRUNC`, as `writeFileSync` uses) has already emptied the
//! file before the snapshot could run, the content last recorded for that path.
//! Both are stored as points in the Loro history (per-path
//! `LoroText`/binary, so repeated near-identical writes are delta-stored), and
//! each write records the before/after [`Frontiers`](loro::Frontiers) the
//! server later `checkout`s to render them.

use std::sync::{Arc, Mutex, PoisonError};

use loro::{ExportMode, LoroDoc, LoroMap, LoroText, UpdateOptions};
use rustc_hash::FxHashMap;
use serde::Serialize;
use vite_str::Str;

/// A Loro operation id, serialized as a `{peer, counter}` pair so it can be
/// round-tripped through JSON and rebuilt into a [`loro::Frontiers`].
#[derive(Clone, Debug, Serialize)]
pub struct OpId {
    /// The peer id as a decimal string (Loro peer ids are `u64`).
    pub peer: Str,
    /// The op counter within that peer.
    pub counter: i32,
}

/// One write: a file's content just before it was opened for writing (`before`)
/// and just after it was closed (`after`), as Loro history frontiers.
#[derive(Clone, Debug, Serialize)]
pub struct Write {
    /// Monotonic sequence number, starting at 0.
    pub seq: u64,
    /// Absolute path of the written file.
    pub path: Str,
    /// Frontier capturing the file's content just before this write (the open
    /// snapshot, or the last-recorded content when a truncating open emptied it
    /// first).
    pub before: Vec<OpId>,
    /// Frontier capturing the file's content at the close.
    pub after: Vec<OpId>,
    /// Length of the raw output log when this write was recorded; the UI renders
    /// `output[0..output_offset]` for this write.
    pub output_offset: usize,
}

/// The fully captured run, ready to be served or dumped.
pub struct CapturedData {
    /// A full Loro snapshot (history + state) of the file contents.
    pub snapshot: Vec<u8>,
    /// The ordered list of writes.
    pub writes: Vec<Write>,
    /// The raw terminal-output byte log.
    pub output: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Text,
    Binary,
}

/// Correlation key pairing an open with its close. Uses the descriptor when the
/// backend reports it (`raw_fd >= 0`); otherwise falls back to the path (the
/// seccomp backend can't report the target's fd). Several opens can share a key
/// — the path fallback collapses every open of one path, and a descriptor
/// number is reused after close — so each key maps to a *stack* of pending
/// opens, paired LIFO and validated by path on close.
#[derive(Clone, PartialEq, Eq, Hash)]
enum FdKey {
    Fd(u32, i64),
    Path(u32, Str),
}

fn fd_key(pid: u32, raw_fd: i64, path: &Str) -> FdKey {
    if raw_fd >= 0 { FdKey::Fd(pid, raw_fd) } else { FdKey::Path(pid, path.clone()) }
}

/// A file's identity on disk: device + inode on Unix, volume serial + file index
/// on Windows.
///
/// Used to tell an in-place truncating rewrite (same file) apart from a
/// replacement of the same path — a rename-over or delete-and-recreate (a
/// *different* file at that path).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    dev: u64,
    ino: u64,
}

impl FileId {
    /// Construct from a `(device, inode)`-style identity pair.
    #[must_use]
    pub const fn new(dev: u64, ino: u64) -> Self {
        Self { dev, ino }
    }
}

/// One pending open awaiting its close: the path it was opened on (to validate
/// the pairing) and the before-frontier captured for it.
struct OpenSlot {
    path: Str,
    before: Vec<OpId>,
}

struct State {
    doc: LoroDoc,
    text_files: LoroMap,
    bin_files: LoroMap,
    /// Per-path text/binary classification, to clean up the other map on a flip.
    flavor: FxHashMap<Str, Flavor>,
    /// Pending opens awaiting their close: key -> a LIFO stack of open slots
    /// (several opens can share a key; see [`FdKey`]).
    open: FxHashMap<FdKey, Vec<OpenSlot>>,
    /// Per-path on-disk identity as of the last recorded write, to tell an
    /// in-place rewrite from a replacement of the same path.
    identity: FxHashMap<Str, FileId>,
    writes: Vec<Write>,
    output: Vec<u8>,
    next_seq: u64,
}

/// A cheap-to-clone handle to the shared capture state.
///
/// Cloned into the fspy callback (open/close snapshots) and the output pump
/// (terminal bytes); both lock the same mutex so the write list and output
/// offsets stay consistent.
#[derive(Clone)]
pub struct Snapshotter {
    inner: Arc<Mutex<State>>,
}

impl Default for Snapshotter {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshotter {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        let doc = LoroDoc::new();
        let text_files = doc.get_map("text_files");
        let bin_files = doc.get_map("bin_files");
        Self {
            inner: Arc::new(Mutex::new(State {
                doc,
                text_files,
                bin_files,
                flavor: FxHashMap::default(),
                open: FxHashMap::default(),
                identity: FxHashMap::default(),
                writes: Vec::new(),
                output: Vec::new(),
                next_seq: 0,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Append raw terminal-output bytes to the log.
    pub fn append_output(&self, bytes: &[u8]) {
        self.lock().output.extend_from_slice(bytes);
    }

    /// Record the pre-write snapshot taken when `path` was opened for writing on
    /// descriptor `raw_fd` of process `pid`. Pairs with the matching
    /// [`Self::record_close`].
    ///
    /// The open-time `content` is read in the supervisor *after* the open
    /// syscall, so a truncating open (`O_TRUNC` — what `writeFileSync` and most
    /// whole-file writes use) has already emptied the file by the time we read
    /// it. To avoid losing the real pre-write content, the open read is only
    /// adopted as the `before` when it is non-empty (a non-truncating open, or a
    /// pre-existing file). An empty read for a path we've already recorded is
    /// treated as a truncating open and the last-recorded content is kept —
    /// unless `id` shows the file was replaced since (rename-over or
    /// delete-and-recreate), in which case it's treated as a fresh file so stale
    /// content isn't resurrected.
    ///
    /// # Panics
    ///
    /// Panics if a Loro container operation fails (a corrupted invariant).
    pub fn record_open(
        &self,
        pid: u32,
        raw_fd: i64,
        path: &str,
        content: &[u8],
        id: Option<FileId>,
    ) {
        let mut guard = self.lock();
        let key = Str::from(path);
        let before = if !content.is_empty() {
            // Non-truncating open or a pre-existing file: the read reflects the
            // real current content (including any external modification).
            set_content(&mut guard, path, content)
        } else if guard.flavor.contains_key(&key) && !replaced(&guard, &key, id) {
            // Empty read of a path we already track whose on-disk identity is
            // unchanged: a truncating open emptied the file before we could read
            // it. Keep what we last recorded (the previous write's `after`).
            serialize_frontiers(&guard.doc.state_frontiers())
        } else {
            // Empty read of a genuinely new file, or a path whose identity
            // changed since we last saw it (rename-over / delete-and-recreate):
            // treat it as fresh rather than resurrecting stale content.
            set_content(&mut guard, path, content)
        };
        guard
            .open
            .entry(fd_key(pid, raw_fd, &key))
            .or_default()
            .push(OpenSlot { path: key, before });
    }

    /// Record the post-write snapshot taken just before `path` is closed on
    /// descriptor `raw_fd` of process `pid`, emitting a [`Write`]. Its `before`
    /// is the matching open's snapshot (or the file's prior content if the open
    /// wasn't observed). `id` is the file's on-disk identity, remembered to
    /// detect a later replacement of this path.
    ///
    /// # Panics
    ///
    /// Panics if a Loro container operation fails (a corrupted invariant).
    pub fn record_close(
        &self,
        pid: u32,
        raw_fd: i64,
        path: &str,
        content: &[u8],
        id: Option<FileId>,
    ) {
        let path_key = Str::from(path);
        let mut guard = self.lock();
        // The file's content before this close, for the fallback `before`.
        let prior = serialize_frontiers(&guard.doc.state_frontiers());
        let after = set_content(&mut guard, path, content);
        if let Some(id) = id {
            guard.identity.insert(path_key.clone(), id);
        }
        let before = pop_open(&mut guard, pid, raw_fd, &path_key).unwrap_or(prior);
        let seq = guard.next_seq;
        guard.next_seq += 1;
        let output_offset = guard.output.len();
        guard.writes.push(Write { seq, path: path_key, before, after, output_offset });
    }

    /// Export the captured run: a full Loro snapshot plus the write list and raw
    /// output log.
    ///
    /// # Panics
    ///
    /// Panics if the Loro snapshot export fails (should not happen for a
    /// well-formed in-memory document).
    #[must_use]
    pub fn finish(&self) -> CapturedData {
        let guard = self.lock();
        let snapshot =
            guard.doc.export(ExportMode::snapshot()).expect("loro snapshot export should not fail");
        CapturedData { snapshot, writes: guard.writes.clone(), output: guard.output.clone() }
    }
}

/// Whether the file at `key` has been replaced since we last recorded it — its
/// on-disk identity differs from the stored one. Unknown identities (either side
/// `None`) are treated as "not replaced", so the truncating-rewrite heuristic
/// still applies when identity is unavailable.
fn replaced(state: &State, key: &Str, current: Option<FileId>) -> bool {
    matches!((state.identity.get(key), current), (Some(old), Some(new)) if *old != new)
}

/// Pop the most recent pending open for this `(pid, raw_fd/path)` key whose path
/// matches the closing `path`, returning its before-frontier. A mismatched top
/// slot — a descriptor reused for a different file (e.g. via `dup2`) — is
/// discarded so it can't be mispaired. `None` means no matching open was
/// observed, and the caller falls back to the file's prior content.
fn pop_open(state: &mut State, pid: u32, raw_fd: i64, path: &Str) -> Option<Vec<OpId>> {
    let key = fd_key(pid, raw_fd, path);
    let stack = state.open.get_mut(&key)?;
    let slot = stack.pop()?;
    if stack.is_empty() {
        state.open.remove(&key);
    }
    (slot.path == *path).then_some(slot.before)
}

/// Set `path`'s content in the doc, committing, and return the new frontier.
/// Identical content is a no-op (Loro dedups), so the frontier is unchanged.
fn set_content(state: &mut State, path: &str, content: &[u8]) -> Vec<OpId> {
    let key = Str::from(path);
    if let Ok(text) = std::str::from_utf8(content) {
        if state.flavor.get(&key) == Some(&Flavor::Binary) {
            let _ = state.bin_files.delete(path);
        }
        let container = state
            .text_files
            .get_or_create_container(path, LoroText::new())
            .expect("get_or_create text container");
        container.update(text, UpdateOptions::default()).expect("loro text update");
        state.flavor.insert(key, Flavor::Text);
    } else {
        if state.flavor.get(&key) == Some(&Flavor::Text) {
            let _ = state.text_files.delete(path);
        }
        state.bin_files.insert(path, content.to_vec()).expect("loro binary insert");
        state.flavor.insert(key, Flavor::Binary);
    }
    state.doc.commit();
    serialize_frontiers(&state.doc.state_frontiers())
}

/// Serialize a Loro frontier into JSON-friendly `{peer, counter}` pairs.
fn serialize_frontiers(frontiers: &loro::Frontiers) -> Vec<OpId> {
    frontiers
        .iter()
        .map(|id| OpId { peer: vite_str::format!("{}", id.peer), counter: id.counter })
        .collect()
}

/// Rebuild a Loro frontier from serialized `{peer, counter}` pairs (the inverse
/// of `serialize_frontiers`). Pairs with an unparsable peer are skipped.
#[must_use]
pub fn rebuild_frontier(ops: &[OpId]) -> loro::Frontiers {
    let mut frontiers = loro::Frontiers::default();
    for op in ops {
        if let Ok(peer) = op.peer.as_str().parse::<u64>() {
            frontiers.push(loro::ID::new(peer, op.counter));
        }
    }
    frontiers
}
