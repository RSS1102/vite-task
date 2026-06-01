//! The in-memory capture store: a Loro CRDT of file contents over time, plus an
//! ordered list of *writes* and a raw terminal-output byte log.
//!
//! A **write** is one open→…→close lifecycle of a descriptor opened for
//! writing. Its *before* is the file's content snapshotted in the open
//! callback; its *after* is the content snapshotted in the close callback of
//! the same descriptor. Both are stored as points in the Loro history (per-path
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
    /// Frontier capturing the file's content at the matching open.
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
/// seccomp backend can't report the target's fd).
#[derive(Clone, PartialEq, Eq, Hash)]
enum FdKey {
    Fd(u32, i64),
    Path(u32, Str),
}

fn fd_key(pid: u32, raw_fd: i64, path: &Str) -> FdKey {
    if raw_fd >= 0 { FdKey::Fd(pid, raw_fd) } else { FdKey::Path(pid, path.clone()) }
}

struct State {
    doc: LoroDoc,
    text_files: LoroMap,
    bin_files: LoroMap,
    /// Per-path text/binary classification, to clean up the other map on a flip.
    flavor: FxHashMap<Str, Flavor>,
    /// Open descriptors awaiting their close: key -> the open's before-frontier.
    open: FxHashMap<FdKey, Vec<OpId>>,
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
    /// # Panics
    ///
    /// Panics if a Loro container operation fails (a corrupted invariant).
    pub fn record_open(&self, pid: u32, raw_fd: i64, path: &str, content: &[u8]) {
        let mut guard = self.lock();
        let before = set_content(&mut guard, path, content);
        let key = fd_key(pid, raw_fd, &Str::from(path));
        guard.open.insert(key, before);
    }

    /// Record the post-write snapshot taken just before `path` is closed on
    /// descriptor `raw_fd` of process `pid`, emitting a [`Write`]. Its `before`
    /// is the matching open's snapshot (or the file's prior content if the open
    /// wasn't observed).
    ///
    /// # Panics
    ///
    /// Panics if a Loro container operation fails (a corrupted invariant).
    pub fn record_close(&self, pid: u32, raw_fd: i64, path: &str, content: &[u8]) {
        let path_key = Str::from(path);
        let mut guard = self.lock();
        // The file's content before this close, for the fallback `before`.
        let prior = serialize_frontiers(&guard.doc.state_frontiers());
        let after = set_content(&mut guard, path, content);
        let before = guard.open.remove(&fd_key(pid, raw_fd, &path_key)).unwrap_or(prior);
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
