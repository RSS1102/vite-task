#[cfg(target_os = "linux")]
mod sigsys;

#[cfg(target_os = "macos")]
mod macos_artifacts;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::{io, path::Path};

#[cfg(target_os = "macos")]
use fspy_shared::ipc::NativeStr;
use fspy_shared::ipc::{PathAccess, channel::channel};
#[cfg(target_os = "macos")]
use fspy_shared_unix::payload::Artifacts;
use fspy_shared_unix::{
    exec::ExecResolveConfig,
    payload::{Payload, encode_payload},
    spawn::handle_exec,
};
use futures_util::FutureExt;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;

use crate::{
    ChildTermination, Command, TrackedChild,
    arena::PathAccessArena,
    error::SpawnError,
    ipc::{OwnedReceiverLockGuard, SHM_CAPACITY},
};

#[derive(Debug)]
pub struct SpyImpl {
    #[cfg(target_os = "macos")]
    artifacts: Artifacts,

    #[cfg(target_os = "macos")]
    preload_path: Box<NativeStr>,
}

impl SpyImpl {
    /// Initializes platform artifacts. Linux injects its handler at exec and
    /// does not materialize a preload library.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "keeps initialization uniform with the fallible macOS backend"
    )]
    pub const fn init_in(_dir: &Path) -> io::Result<Self> {
        Ok(Self {})
    }

    #[cfg(target_os = "macos")]
    pub fn init_in(dir: &Path) -> io::Result<Self> {
        let preload_path = {
            use materialized_artifact::{Artifact, artifact};

            const PRELOAD_CDYLIB: Artifact = artifact!("fspy_preload");

            let preload_cdylib_path = PRELOAD_CDYLIB.materialize().suffix(".dylib").at(dir)?;
            preload_cdylib_path.as_path().into()
        };

        Ok(Self {
            #[cfg(target_os = "macos")]
            preload_path,
            #[cfg(target_os = "macos")]
            artifacts: {
                let coreutils_path =
                    macos_artifacts::COREUTILS_BINARY.materialize().executable().at(dir)?;
                let bash_path = macos_artifacts::OILS_BINARY.materialize().executable().at(dir)?;
                Artifacts {
                    bash_path: bash_path.as_path().into(),
                    coreutils_path: coreutils_path.as_path().into(),
                }
            },
        })
    }

    pub(crate) async fn spawn(
        &self,
        mut command: Command,
        cancellation_token: CancellationToken,
    ) -> Result<TrackedChild, SpawnError> {
        #[cfg(target_os = "macos")]
        let (ipc_channel_conf, ipc_receiver) =
            channel(SHM_CAPACITY).map_err(SpawnError::ChannelCreation)?;
        #[cfg(target_os = "linux")]
        let (_, ipc_receiver) = channel(SHM_CAPACITY).map_err(SpawnError::ChannelCreation)?;

        let payload = Payload {
            #[cfg(target_os = "macos")]
            ipc_channel_conf,

            #[cfg(target_os = "macos")]
            artifacts: self.artifacts.clone(),

            #[cfg(target_os = "macos")]
            preload_path: self.preload_path.clone(),
        };

        let encoded_payload = encode_payload(payload);

        let mut exec = command.get_exec();
        let mut exec_resolve_accesses = PathAccessArena::default();
        let pre_exec = handle_exec(
            &mut exec,
            ExecResolveConfig::search_path_enabled(None),
            &encoded_payload,
            |mode, path| {
                exec_resolve_accesses.add(PathAccess { mode, path: path.into() });
            },
        )
        .map_err(|err| SpawnError::Injection(err.into()))?;
        #[cfg(target_os = "linux")]
        debug_assert!(pre_exec.is_none());
        command.set_exec(exec);
        command.env("FSPY", "1");

        let mut tokio_command = command.into_tokio_command();

        #[cfg(target_os = "linux")]
        let shm_fd = ipc_receiver.shm_fd().as_raw_fd();
        #[cfg(target_os = "linux")]
        let shm_len = ipc_receiver.shm_len();

        // SAFETY: both platform hooks are restricted to async-signal-safe raw
        // operations in the post-fork child.
        unsafe {
            tokio_command.pre_exec(move || {
                #[cfg(target_os = "linux")]
                sigsys::prepare(shm_fd)?;
                #[cfg(target_os = "macos")]
                if let Some(pre_exec) = pre_exec.as_ref() {
                    pre_exec.run()?;
                }
                Ok(())
            });
        }

        // Spawn and the post-exec ptrace handshake are blocking operations.
        let mut child = spawn_blocking(move || {
            let child = tokio_command.spawn().map_err(SpawnError::OsSpawn)?;
            #[cfg(target_os = "linux")]
            let child = {
                let mut child = child;
                let pid = child.id().ok_or_else(|| {
                    SpawnError::Injection(io::Error::other("spawned child has no process id"))
                })?;
                if let Err(error) = sigsys::inject(pid, shm_fd, shm_len) {
                    let _ = child.start_kill();
                    return Err(SpawnError::Injection(error));
                }
                child
            };
            Ok(child)
        })
        .await
        .map_err(|err| SpawnError::OsSpawn(err.into()))??;

        Ok(TrackedChild {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            // Keep polling for the child to exit in the background even if `wait_handle` is not awaited,
            // because we need to stop the supervisor and lock the channel as soon as the child exits.
            wait_handle: tokio::spawn(async move {
                let status = tokio::select! {
                    status = child.wait() => status?,
                    () = cancellation_token.cancelled() => {
                        child.start_kill()?;
                        child.wait().await?
                    }
                };

                let arenas = vec![exec_resolve_accesses];

                // Lock the ipc channel after the child has exited.
                // We are not interested in path accesses from descendants after the main child has exited.
                let ipc_receiver_lock_guard =
                    OwnedReceiverLockGuard::lock_async(ipc_receiver).await?;
                let path_accesses = PathAccessIterable { arenas, ipc_receiver_lock_guard };

                io::Result::Ok(ChildTermination { status, path_accesses })
            })
            .map(|f| f?) // flatten JoinError and io::Result
            .boxed(),
        })
    }
}

pub struct PathAccessIterable {
    arenas: Vec<PathAccessArena>,
    ipc_receiver_lock_guard: OwnedReceiverLockGuard,
}

impl PathAccessIterable {
    pub fn iter(&self) -> impl Iterator<Item = PathAccess<'_>> {
        let accesses_in_arena =
            self.arenas.iter().flat_map(|arena| arena.borrow_accesses().iter()).copied();

        let accesses_in_shm = self.ipc_receiver_lock_guard.iter_path_accesses();
        accesses_in_shm.chain(accesses_in_arena)
    }
}
