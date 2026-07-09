use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{SPY_IMPL, TrackedChild, error::SpawnError};

/// Spawn the command with file system access tracking.
///
/// # Errors
///
/// Returns [`SpawnError`] if program resolution fails or the process cannot be spawned.
///
/// On Unix, use [`spawn_with`] to configure stdio and other settings that `Command`
/// does not expose through getters.
/// On Windows, fspy replaces configured process creation flags with `CREATE_SUSPENDED`,
/// which is required for injection before the process starts.
pub async fn spawn(
    command: Command,
    cancellation_token: CancellationToken,
) -> Result<TrackedChild, SpawnError> {
    spawn_with(command, cancellation_token, |_| {}).await
}

/// Spawn the command with file system access tracking, configuring the final command before spawn.
///
/// On Unix, fspy rebuilds the command after resolving shebangs and injection. Use `configure`
/// only for settings that do not affect resolution, including stdio, custom `arg0`, and
/// `pre_exec` hooks that do not change the cwd or filesystem view. Configure the program,
/// arguments, environment, and cwd on the input command.
///
/// # Errors
///
/// Returns [`SpawnError`] if program resolution fails or the process cannot be spawned.
pub async fn spawn_with<F>(
    command: Command,
    cancellation_token: CancellationToken,
    configure: F,
) -> Result<TrackedChild, SpawnError>
where
    F: FnOnce(&mut Command),
{
    SPY_IMPL.spawn(command, cancellation_token, configure).await
}
