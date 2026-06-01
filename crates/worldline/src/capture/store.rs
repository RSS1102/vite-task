//! The in-memory capture store: a Loro CRDT timeline of file versions plus a
//! raw terminal-output byte log and an ordered event list.
//!
//! Files are stored in two top-level [`LoroMap`]s keyed by absolute path:
//! `text_files` holds a [`LoroText`] per UTF-8 file (delta-stored, so repeated
//! near-identical writes are cheap), and `bin_files` holds the raw bytes of
//! each non-UTF-8 file (full replace). Each recorded version commits the doc
//! and snaps the current [`Frontiers`](loro::Frontiers); the server later
//! `checkout`s those frontiers to reconstruct the filesystem at any point.

use std::sync::{Arc, Mutex, PoisonError};

use loro::{ExportMode, LoroDoc, LoroMap, LoroText, UpdateOptions};
use rustc_hash::FxHashMap;
use serde::Serialize;
use vite_str::Str;
use xxhash_rust::xxh3::xxh3_64;

/// Whether a recorded version came from a write-open (pre-write content) or a
/// write-close (post-write content).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// Content read from the descriptor right after it was opened for writing
    /// (the file's state just before the program writes it).
    WriteOpen,
    /// Content read from the descriptor right before it is closed (the file's
    /// state just after the program finished writing it).
    WriteClose,
    /// Content re-read from disk after the program (and its descendants) exited.
    /// Captures the final state of writes that were flushed only when a child
    /// process exited (e.g. shell redirections), where no `close` is observable.
    Final,
}

/// A Loro operation id, serialized as a `{peer, counter}` pair so it can be
/// round-tripped through JSON and rebuilt into a [`loro::Frontiers`].
#[derive(Clone, Debug, Serialize)]
pub struct OpId {
    /// The peer id as a decimal string (Loro peer ids are `u64`).
    pub peer: Str,
    /// The op counter within that peer.
    pub counter: i32,
}

/// One point on the timeline: a file version was recorded.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// Monotonic sequence number, starting at 0.
    pub seq: u64,
    /// Whether this is a pre-write or post-write capture.
    pub kind: EventKind,
    /// Absolute path of the file whose version was recorded.
    pub path: Str,
    /// Length of the raw output log at the instant this event was recorded;
    /// the UI renders `output[0..output_offset]` for this event.
    pub output_offset: usize,
    /// The Loro state frontier right after this version was committed. The
    /// server `checkout`s to it to reconstruct the filesystem at this event.
    pub frontier: Vec<OpId>,
}

/// The fully captured run, ready to be served or dumped.
pub struct CapturedData {
    /// A full Loro snapshot (history + state) of the file timeline.
    pub snapshot: Vec<u8>,
    /// The ordered timeline events.
    pub events: Vec<Event>,
    /// The raw terminal-output byte log.
    pub output: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Text,
    Binary,
}

struct State {
    doc: LoroDoc,
    text_files: LoroMap,
    bin_files: LoroMap,
    /// Per-path content hash of the last recorded version, to skip no-op writes.
    last_seen: FxHashMap<Str, u64>,
    /// Per-path text/binary classification, to clean up the other map on a flip.
    flavor: FxHashMap<Str, Flavor>,
    events: Vec<Event>,
    output: Vec<u8>,
    next_seq: u64,
}

/// A cheap-to-clone handle to the shared capture state.
///
/// Cloned into the fspy callback (write snapshots) and the output pump
/// (terminal bytes); both lock the same mutex so the event list and output
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
    /// Create an empty timeline.
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
                last_seen: FxHashMap::default(),
                flavor: FxHashMap::default(),
                events: Vec::new(),
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

    /// Record a file version. No-op if `content` is unchanged from the last
    /// recorded version of `path` (so consecutive identical writes, and a
    /// write-open immediately following a write-close with the same bytes, cost
    /// nothing).
    ///
    /// # Panics
    ///
    /// Panics if a Loro container operation fails, which would indicate a
    /// corrupted internal invariant rather than a recoverable error.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the guard must span the whole record so the doc commit, frontier, last_seen, output offset and event push stay consistent"
    )]
    pub fn record_write(&self, path: &str, content: &[u8], kind: EventKind) {
        let hash = xxh3_64(content);
        let key = Str::from(path);

        let mut guard = self.lock();
        if guard.last_seen.get(&key) == Some(&hash) {
            return;
        }

        let state = &mut *guard;
        if let Ok(text) = std::str::from_utf8(content) {
            if state.flavor.get(&key) == Some(&Flavor::Binary) {
                let _ = state.bin_files.delete(path);
            }
            let container = state
                .text_files
                .get_or_create_container(path, LoroText::new())
                .expect("get_or_create text container");
            // `timeout_ms: None` (the default) means `update` never times out,
            // so this only errors on a poisoned internal invariant.
            container.update(text, UpdateOptions::default()).expect("loro text update");
            state.flavor.insert(key.clone(), Flavor::Text);
        } else {
            if state.flavor.get(&key) == Some(&Flavor::Text) {
                let _ = state.text_files.delete(path);
            }
            state.bin_files.insert(path, content.to_vec()).expect("loro binary insert");
            state.flavor.insert(key.clone(), Flavor::Binary);
        }

        state.doc.commit();
        let frontier = serialize_frontiers(&state.doc.state_frontiers());
        state.last_seen.insert(key.clone(), hash);
        let seq = state.next_seq;
        state.next_seq += 1;
        let output_offset = state.output.len();
        state.events.push(Event { seq, kind, path: key, output_offset, frontier });
    }

    /// Re-read every file seen during the run and record its final content.
    ///
    /// Call once after the traced program and all its descendants have exited.
    /// This captures writes whose `close` was never observable (e.g. a shell
    /// redirection whose fd is closed implicitly when a child process exits).
    /// Unchanged files are skipped by [`Self::record_write`]'s dedup.
    pub fn finalize(&self) {
        let paths: Vec<Str> = {
            let guard = self.lock();
            guard.last_seen.keys().cloned().collect()
        };
        for path in paths {
            if let Ok(content) = std::fs::read(path.as_str()) {
                self.record_write(path.as_str(), &content, EventKind::Final);
            }
        }
    }

    /// Export the captured timeline: a full Loro snapshot plus the event list
    /// and raw output log.
    ///
    /// # Panics
    ///
    /// Panics if the Loro snapshot export fails, which should not happen for a
    /// well-formed in-memory document.
    #[must_use]
    pub fn finish(&self) -> CapturedData {
        let guard = self.lock();
        let snapshot =
            guard.doc.export(ExportMode::snapshot()).expect("loro snapshot export should not fail");
        CapturedData { snapshot, events: guard.events.clone(), output: guard.output.clone() }
    }
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

#[cfg(test)]
mod tests {
    use loro::{LoroDoc, LoroValue};

    use super::{EventKind, Snapshotter, rebuild_frontier};

    fn text_at(doc: &LoroDoc, path: &str) -> Option<vite_str::Str> {
        match doc.get_map("text_files").get_deep_value() {
            LoroValue::Map(map) => match map.get(path) {
                Some(LoroValue::String(s)) => Some(vite_str::Str::from(s.to_string().as_str())),
                _ => None,
            },
            _ => None,
        }
    }

    fn bin_len_at(doc: &LoroDoc, path: &str) -> Option<usize> {
        match doc.get_map("bin_files").get_deep_value() {
            LoroValue::Map(map) => match map.get(path) {
                Some(LoroValue::Binary(b)) => Some(b.len()),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn records_versions_dedups_and_round_trips() {
        let snap = Snapshotter::new();
        snap.record_write("/w/a.txt", b"v1", EventKind::WriteOpen);
        // Identical content: must be skipped (no new event).
        snap.record_write("/w/a.txt", b"v1", EventKind::WriteClose);
        snap.record_write("/w/a.txt", b"v2", EventKind::WriteClose);
        // A non-UTF-8 file goes to bin_files.
        snap.record_write("/w/img.bin", &[0u8, 159, 146, 150], EventKind::WriteClose);

        let data = snap.finish();
        assert_eq!(data.events.len(), 3, "identical rewrite should be deduped");
        assert_eq!(data.events[0].kind, EventKind::WriteOpen);

        let doc = LoroDoc::new();
        doc.import(&data.snapshot).expect("import snapshot");

        // At the first event a.txt is "v1" and img.bin does not yet exist.
        doc.checkout(&rebuild_frontier(&data.events[0].frontier)).expect("checkout v1");
        assert_eq!(text_at(&doc, "/w/a.txt").as_deref(), Some("v1"));
        assert_eq!(bin_len_at(&doc, "/w/img.bin"), None);

        // At the last event a.txt is "v2" and img.bin is present with 4 bytes.
        doc.checkout(&rebuild_frontier(&data.events[2].frontier)).expect("checkout latest");
        assert_eq!(text_at(&doc, "/w/a.txt").as_deref(), Some("v2"));
        assert_eq!(bin_len_at(&doc, "/w/img.bin"), Some(4));

        doc.checkout_to_latest();
    }
}
