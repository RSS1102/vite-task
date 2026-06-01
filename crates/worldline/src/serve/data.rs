//! Reconstruct the served JSON payload from a captured run.
//!
//! For each write we `checkout` its before/after frontiers and read the file's
//! content at each, deduplicating identical contents into a content-addressed
//! blob table. The browser renders straight from this — no Loro/WASM on the
//! client side.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use loro::{LoroDoc, LoroValue};
use serde::Serialize;
use vite_str::Str;

use crate::{capture::rebuild_frontier, run::Captured};

/// How a blob's bytes are encoded for transport.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BlobEncoding {
    /// `data` is the file content verbatim (the file was valid UTF-8).
    Utf8,
    /// `data` is standard base64 of the raw bytes (non-UTF-8 file).
    Base64,
}

/// One unique file content, shared by every write that references it.
#[derive(Serialize)]
pub struct Blob {
    /// Encoding of `data`.
    pub enc: BlobEncoding,
    /// The (possibly encoded) content.
    pub data: Str,
}

/// One write as served to the UI: the file's content before (open) and after
/// (close), each a blob id into [`ApiData::blobs`].
#[derive(Serialize)]
pub struct ApiWrite {
    /// Monotonic sequence number.
    pub seq: u64,
    /// The written file's path.
    pub path: Str,
    /// Blob id of the content before the write (empty file if newly created).
    pub before: Str,
    /// Blob id of the content after the write.
    pub after: Str,
    /// Byte offset into the output log at this write (`output[0..offset]`).
    pub output_offset: usize,
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
    /// Ordered writes.
    pub writes: Vec<ApiWrite>,
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
    let mut writes = Vec::with_capacity(captured.data.writes.len());

    for write in &captured.data.writes {
        let before = blob_at(&doc, &write.before, write.path.as_str(), &mut blobs);
        let after = blob_at(&doc, &write.after, write.path.as_str(), &mut blobs);
        writes.push(ApiWrite {
            seq: write.seq,
            path: write.path.clone(),
            before,
            after,
            output_offset: write.output_offset,
        });
    }
    doc.checkout_to_latest();

    ApiData {
        argv: captured.meta.argv.clone(),
        cwd: captured.meta.cwd.clone(),
        exit_code: captured.meta.exit_code,
        output_len: captured.data.output.len(),
        writes,
        blobs,
    }
}

/// Checkout `frontier`, read `path`'s content there, intern it into `blobs`,
/// and return its blob id. An absent file interns the empty blob.
fn blob_at(
    doc: &LoroDoc,
    frontier: &[crate::capture::OpId],
    path: &str,
    blobs: &mut BTreeMap<Str, Blob>,
) -> Str {
    let _ = doc.checkout(&rebuild_frontier(frontier));
    let (id, blob) = read_content(doc, path);
    blobs.entry(id.clone()).or_insert(blob);
    id
}

/// Read `path`'s content at the doc's current version as a `(blob id, blob)`.
fn read_content(doc: &LoroDoc, path: &str) -> (Str, Blob) {
    if let LoroValue::Map(map) = doc.get_map("text_files").get_deep_value()
        && let Some(LoroValue::String(text)) = map.get(path)
    {
        let data = text.to_string();
        return (
            blob_id(data.as_bytes()),
            Blob { enc: BlobEncoding::Utf8, data: Str::from(data.as_str()) },
        );
    }
    if let LoroValue::Map(map) = doc.get_map("bin_files").get_deep_value()
        && let Some(LoroValue::Binary(bytes)) = map.get(path)
    {
        return (
            blob_id(bytes.as_slice()),
            Blob {
                enc: BlobEncoding::Base64,
                data: Str::from(BASE64.encode(bytes.as_slice()).as_str()),
            },
        );
    }
    // Absent at this version (e.g. before a file was first created).
    (blob_id(b""), Blob { enc: BlobEncoding::Utf8, data: Str::default() })
}

#[cfg(test)]
mod tests {
    use super::reconstruct;
    use crate::{
        capture::Snapshotter,
        run::{Captured, Meta},
    };

    #[test]
    fn reconstructs_writes_with_before_after() {
        let snap = Snapshotter::new();
        snap.append_output(b"out");
        // First write to a.txt: created empty, closed with "hello".
        snap.record_open(1, 3, "/w/a.txt", b"");
        snap.record_close(1, 3, "/w/a.txt", b"hello");
        // Second write to a.txt: opened with "hello", closed with "hello world".
        snap.record_open(1, 3, "/w/a.txt", b"hello");
        snap.record_close(1, 3, "/w/a.txt", b"hello world");

        let captured = Captured {
            meta: Meta { argv: vec!["p".into()], cwd: "/w".into(), exit_code: Some(0) },
            data: snap.finish(),
        };
        let api = reconstruct(&captured);

        assert_eq!(api.writes.len(), 2, "one write per close");
        let blob = |id: &str| api.blobs[id].data.as_str().to_owned();

        // Write 0: "" -> "hello".
        assert_eq!(blob(api.writes[0].before.as_str()), "");
        assert_eq!(blob(api.writes[0].after.as_str()), "hello");
        // Write 1: "hello" -> "hello world".
        assert_eq!(blob(api.writes[1].before.as_str()), "hello");
        assert_eq!(blob(api.writes[1].after.as_str()), "hello world");

        // "hello" is shared between write 0's after and write 1's before.
        assert_eq!(api.writes[0].after.as_str(), api.writes[1].before.as_str());
    }
}
