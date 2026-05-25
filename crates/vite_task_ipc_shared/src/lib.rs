use std::collections::BTreeMap;

use native_str::NativeStr;
use wincode::{SchemaRead, SchemaWrite};

pub const IPC_ENV_NAME: &str = "VP_RUN_IPC_NAME";

/// Path to the Node client module that JS/TS tools `require()` to talk to
/// the runner.
///
/// Implementation-detail leakage (`napi`, `.node`, `addon`) is intentionally
/// kept out of the name: from the consumer's point of view this is just a
/// path they can `require()`. The `NODE_` scope reserves room for a future
/// C-ABI client library advertised via its own env var for non-Node
/// consumers.
pub const NODE_CLIENT_PATH_ENV_NAME: &str = "VP_RUN_NODE_CLIENT_PATH";

#[derive(Debug, SchemaWrite, SchemaRead)]
pub enum Request<'a> {
    IgnoreInput(&'a NativeStr),
    IgnoreOutput(&'a NativeStr),
    GetEnv { name: &'a NativeStr, tracked: bool },
    GetEnvs { pattern: &'a str, tracked: bool },
    DisableCache,
}

#[derive(Debug, SchemaWrite, SchemaRead)]
pub struct GetEnvResponse<'a> {
    pub env_value: Option<&'a NativeStr>,
}

#[derive(Debug, SchemaWrite, SchemaRead)]
pub struct GetEnvsResponse<'a> {
    /// Match snapshot for the glob pattern, sorted by name. `BTreeMap` is used
    /// over a `Vec` to make ordering and key-uniqueness part of the type.
    pub entries: BTreeMap<&'a NativeStr, &'a NativeStr>,
}

/// Ack body for `IgnoreInput`, `IgnoreOutput`, `DisableCache`.
///
/// Carries no payload. The wire byte just confirms the server has
/// processed the request, so the client can treat any subsequent runtime
/// action — a `readFileSync`, process exit, etc. — as happening after
/// the runner already knows. Without this the client would have to
/// trust the OS pipe layer to deliver buffered writes through a closing
/// handle, which isn't reliable on Windows named pipes when the writer
/// process is mid-teardown.
#[derive(Debug, SchemaWrite, SchemaRead)]
pub struct Ack;
