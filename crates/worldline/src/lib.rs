//! worldline — run a program under fspy, snapshot every file write into a Loro
//! CRDT timeline, capture terminal output, then serve a scrubbable web UI.

// `MetadataExt::volume_serial_number` / `file_index` (used to compute a file's
// on-disk identity in `capture`) are still nightly-gated behind this feature.
#![cfg_attr(windows, feature(windows_by_handle))]

pub mod capture;
pub mod cli;
pub mod ignore;
pub mod run;
pub mod serve;
