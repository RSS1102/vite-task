//! Reconstruct the served JSON payload from a captured Loro timeline.
//!
//! For each timeline event we `checkout` the recorded frontier and read the
//! file set at that version, deduplicating identical file contents into a
//! content-addressed blob table. The browser renders straight from this — no
//! Loro/WASM on the client side.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use loro::{LoroDoc, LoroValue};
use serde::Serialize;
use vite_str::Str;

use crate::{
    capture::{Event, EventKind, rebuild_frontier},
    run::Captured,
};

/// How a blob's bytes are encoded for transport.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BlobEncoding {
    /// `data` is the file content verbatim (the file was valid UTF-8).
    Utf8,
    /// `data` is standard base64 of the raw bytes (non-UTF-8 file).
    Base64,
}

/// One unique file content, shared by every event/path that references it.
#[derive(Serialize)]
pub struct Blob {
    /// Encoding of `data`.
    pub enc: BlobEncoding,
    /// The (possibly encoded) content.
    pub data: Str,
}

/// One timeline event as served to the UI.
#[derive(Serialize)]
pub struct ApiEvent {
    /// Monotonic sequence number.
    pub seq: u64,
    /// Whether this was a pre-write or post-write capture.
    pub kind: EventKind,
    /// The file whose version was recorded at this event.
    pub path: Str,
    /// Byte offset into the output log at this event (`output[0..offset]`).
    pub output_offset: usize,
    /// The full filesystem state at this event: path -> blob id.
    pub files: BTreeMap<Str, Str>,
}

/// The complete payload served at `/api/data`.
#[derive(Serialize)]
pub struct ApiData {
    /// Full command line.
    pub argv: Vec<Str>,
    /// Working directory.
    pub cwd: Str,
    /// Process exit code, or `null` if terminated by a signal.
    pub exit_code: Option<i32>,
    /// Total length of the raw output log (served separately at `/api/output`).
    pub output_len: usize,
    /// Ordered timeline events.
    pub events: Vec<ApiEvent>,
    /// Content-addressed blob table: blob id -> content.
    pub blobs: BTreeMap<Str, Blob>,
}

fn blob_id(bytes: &[u8]) -> Str {
    vite_str::format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
}

/// Build the served payload from a captured run.
///
/// # Panics
///
/// Panics if the captured Loro snapshot cannot be imported, which would
/// indicate a corrupted in-memory document.
#[must_use]
pub fn reconstruct(captured: &Captured) -> ApiData {
    let doc = LoroDoc::new();
    doc.import(&captured.data.snapshot).expect("import worldline loro snapshot");

    let mut blobs: BTreeMap<Str, Blob> = BTreeMap::new();
    let mut events = Vec::with_capacity(captured.data.events.len());

    for event in &captured.data.events {
        // Reconstruct the filesystem at this event's version.
        let _ = doc.checkout(&rebuild_frontier(&event.frontier));
        let mut files: BTreeMap<Str, Str> = BTreeMap::new();
        collect_text_files(&doc, &mut blobs, &mut files);
        collect_binary_files(&doc, &mut blobs, &mut files);
        events.push(api_event(event, files));
    }
    doc.checkout_to_latest();

    ApiData {
        argv: captured.meta.argv.clone(),
        cwd: captured.meta.cwd.clone(),
        exit_code: captured.meta.exit_code,
        output_len: captured.data.output.len(),
        events,
        blobs,
    }
}

fn api_event(event: &Event, files: BTreeMap<Str, Str>) -> ApiEvent {
    ApiEvent {
        seq: event.seq,
        kind: event.kind,
        path: event.path.clone(),
        output_offset: event.output_offset,
        files,
    }
}

fn collect_text_files(
    doc: &LoroDoc,
    blobs: &mut BTreeMap<Str, Blob>,
    files: &mut BTreeMap<Str, Str>,
) {
    let LoroValue::Map(map) = doc.get_map("text_files").get_deep_value() else {
        return;
    };
    for (path, value) in map.iter() {
        if let LoroValue::String(content) = value {
            let id = blob_id(content.as_bytes());
            blobs.entry(id.clone()).or_insert_with(|| Blob {
                enc: BlobEncoding::Utf8,
                data: Str::from(content.as_str()),
            });
            files.insert(Str::from(path.as_str()), id);
        }
    }
}

fn collect_binary_files(
    doc: &LoroDoc,
    blobs: &mut BTreeMap<Str, Blob>,
    files: &mut BTreeMap<Str, Str>,
) {
    let LoroValue::Map(map) = doc.get_map("bin_files").get_deep_value() else {
        return;
    };
    for (path, value) in map.iter() {
        if let LoroValue::Binary(bytes) = value {
            let id = blob_id(bytes.as_slice());
            blobs.entry(id.clone()).or_insert_with(|| Blob {
                enc: BlobEncoding::Base64,
                data: Str::from(BASE64.encode(bytes.as_slice()).as_str()),
            });
            files.insert(Str::from(path.as_str()), id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::reconstruct;
    use crate::{
        capture::{EventKind, Snapshotter},
        run::{Captured, Meta},
    };

    #[test]
    fn reconstructs_deduped_timeline() {
        let snap = Snapshotter::new();
        snap.append_output(b"step1 ");
        snap.record_write("/w/a.txt", b"v1", EventKind::WriteOpen);
        snap.append_output(b"step2 ");
        snap.record_write("/w/a.txt", b"v2", EventKind::WriteClose);
        // A second file with the SAME content as a.txt v2 must share a blob.
        snap.record_write("/w/b.txt", b"v2", EventKind::WriteClose);

        let captured = Captured {
            meta: Meta {
                argv: vec!["prog".into(), "arg".into()],
                cwd: "/w".into(),
                exit_code: Some(0),
            },
            data: snap.finish(),
        };

        let api = reconstruct(&captured);
        assert_eq!(api.events.len(), 3);
        assert_eq!(api.output_len, 12);

        // First event: only a.txt = v1.
        let e0 = &api.events[0];
        assert_eq!(e0.output_offset, 6);
        assert_eq!(e0.files.len(), 1);
        let a_v1 = &e0.files["/w/a.txt"];
        assert_eq!(api.blobs[a_v1].data.as_str(), "v1");

        // Last event: a.txt = v2 and b.txt = v2 share one blob id.
        let last = api.events.last().unwrap();
        assert_eq!(last.files["/w/a.txt"], last.files["/w/b.txt"]);
        assert_eq!(api.blobs[&last.files["/w/a.txt"]].data.as_str(), "v2");
        // v1 and v2 are distinct blobs.
        assert_ne!(a_v1, &last.files["/w/a.txt"]);
    }
}
