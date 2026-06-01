//! worldline — run a program under fspy, snapshot every file write into a Loro
//! CRDT timeline, capture terminal output, then serve a scrubbable web UI.

pub mod capture;
pub mod cli;
pub mod ignore;
pub mod run;
pub mod serve;
