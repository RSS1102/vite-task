//! The demo's minimal `openat` seccomp filter.

use std::io;

use fspy_preload_linux::OPENAT_COOKIE;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};

/// Compiles a filter that traps `openat` unless argument slot six contains the
/// injected handler's cookie.
pub fn compile() -> Result<BpfProgram, seccompiler::BackendError> {
    let cookie_mismatch = SeccompRule::new(vec![SeccompCondition::new(
        5,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::Ne,
        OPENAT_COOKIE,
    )?])?;
    let filter = SeccompFilter::new(
        std::collections::BTreeMap::from([(libc::SYS_openat, vec![cookie_mismatch])]),
        SeccompAction::Allow,
        SeccompAction::Trap,
        std::env::consts::ARCH.try_into()?,
    )?;
    filter.try_into()
}

/// Applies a previously compiled filter to the calling thread.
pub fn apply(filter: &BpfProgram) -> io::Result<()> {
    seccompiler::apply_filter(filter).map_err(io::Error::other)
}
