use tokio::process::Command as TokioCommand;
use tokio_util::sync::CancellationToken;

use crate::{SPY_IMPL, TrackedChild, error::SpawnError};

/// Spawn the command with file system access tracking.
///
/// # Errors
///
/// Returns [`SpawnError`] if program resolution fails or the process cannot be spawned.
pub async fn spawn(
    command: TokioCommand,
    cancellation_token: CancellationToken,
) -> Result<TrackedChild, SpawnError> {
    SPY_IMPL.spawn(command, cancellation_token).await
}
