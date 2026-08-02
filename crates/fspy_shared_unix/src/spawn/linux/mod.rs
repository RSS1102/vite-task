use std::convert::Infallible;

use crate::{
    exec::Exec,
    payload::{EncodedPayload, PAYLOAD_ENV_NAME},
};

pub struct PreExec(Infallible);
impl PreExec {
    /// Linux command preparation is performed by fspy's ptrace/SIGSYS path.
    ///
    /// # Errors
    ///
    /// This function is unreachable because Linux never constructs `PreExec`.
    pub const fn run(&self) -> nix::Result<()> {
        match self.0 {}
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "keeps the platform-specific command preparation signature uniform"
)]
pub fn handle_exec(
    command: &mut Exec,
    _encoded_payload: &EncodedPayload,
) -> nix::Result<Option<PreExec>> {
    // Do not leak a payload from an outer fspy session into this command. A
    // user-supplied LD_PRELOAD is left untouched; this backend never adds one.
    command.envs.retain(|(name, _)| name != PAYLOAD_ENV_NAME);
    Ok(None)
}
