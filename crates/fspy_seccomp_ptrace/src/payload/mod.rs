mod filter;
pub use filter::Filter;
use wincode::{SchemaRead, SchemaWrite};

#[derive(Debug, SchemaWrite, SchemaRead, Clone)]
pub struct PtracePayload {
    pub(crate) ipc_path: Vec<u8>,
    pub(crate) filter: Filter,
}
