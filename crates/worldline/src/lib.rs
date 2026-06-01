//! worldline — run a program under fspy, snapshot every file write into a Loro
//! CRDT timeline, capture terminal output, then serve a scrubbable web UI.

// Temporary loro API probe to validate the dependency and method names before
// the real modules land. Replaced in the next step.
#[doc(hidden)]
#[must_use]
pub fn __loro_probe() -> usize {
    use loro::{LoroDoc, LoroText};

    let doc = LoroDoc::new();
    let files = doc.get_map("text_files");
    let text: LoroText =
        files.get_or_create_container("a.txt", LoroText::new()).expect("create container");
    text.update("hello", loro::UpdateOptions::default()).expect("update");
    doc.commit();
    let _frontiers = doc.state_frontiers();
    let snapshot = doc.export(loro::ExportMode::snapshot()).expect("export");
    snapshot.len()
}
