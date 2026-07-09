#[cfg(target_os = "linux")]
mod syscall_handler;

#[cfg(target_os = "macos")]
mod macos_artifacts;

#[cfg(not(target_env = "musl"))]
use std::ptr;
use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::Path,
};

use bstr::BString;
#[cfg(target_os = "linux")]
use fspy_seccomp_unotify::supervisor::supervise;
use fspy_shared::ipc::{AccessMode, PathAccess};
#[cfg(not(target_env = "musl"))]
use fspy_shared::ipc::{NativeStr, channel::channel};
#[cfg(target_os = "macos")]
use fspy_shared_unix::payload::Artifacts;
use fspy_shared_unix::{
    exec::{Exec, ExecResolveConfig},
    payload::{Payload, encode_payload},
    spawn::handle_exec,
};
use futures_util::FutureExt;
#[cfg(target_os = "linux")]
use syscall_handler::SyscallHandler;
use tokio::{process::Command, task::spawn_blocking};
use tokio_util::sync::CancellationToken;

#[cfg(not(target_env = "musl"))]
use crate::ipc::{OwnedReceiverLockGuard, SHM_CAPACITY};
use crate::{ChildTermination, TrackedChild, arena::PathAccessArena, error::SpawnError};

#[derive(Debug)]
pub struct SpyImpl {
    #[cfg(target_os = "macos")]
    artifacts: Artifacts,

    #[cfg(not(target_env = "musl"))]
    preload_path: Box<NativeStr>,
}

impl SpyImpl {
    /// Initialize the fs access spy by writing the preload library on disk.
    ///
    /// On musl targets, we don't build a preload library —
    /// only seccomp-based tracking is used.
    pub fn init_in(#[cfg_attr(target_env = "musl", allow(unused))] dir: &Path) -> io::Result<Self> {
        #[cfg(not(target_env = "musl"))]
        let preload_path = {
            use materialized_artifact::{Artifact, artifact};

            const PRELOAD_CDYLIB: Artifact = artifact!("fspy_preload");

            let preload_cdylib_path = PRELOAD_CDYLIB.materialize().suffix(".dylib").at(dir)?;
            preload_cdylib_path.as_path().into()
        };

        Ok(Self {
            #[cfg(not(target_env = "musl"))]
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

    pub(crate) async fn spawn<F>(
        &self,
        command: Command,
        cancellation_token: CancellationToken,
        configure: F,
    ) -> Result<TrackedChild, SpawnError>
    where
        F: FnOnce(&mut Command),
    {
        #[cfg(target_os = "linux")]
        let supervisor = supervise::<SyscallHandler>().map_err(SpawnError::Supervisor)?;

        #[cfg(not(target_env = "musl"))]
        let (ipc_channel_conf, ipc_receiver) =
            channel(SHM_CAPACITY).map_err(SpawnError::ChannelCreation)?;

        let payload = Payload {
            #[cfg(not(target_env = "musl"))]
            ipc_channel_conf,

            #[cfg(target_os = "macos")]
            artifacts: self.artifacts.clone(),

            #[cfg(not(target_env = "musl"))]
            preload_path: self.preload_path.clone(),

            #[cfg(target_os = "linux")]
            seccomp_payload: supervisor.payload().clone(),
        };

        let encoded_payload = encode_payload(payload);

        let cwd = command.as_std().get_current_dir().map(Path::to_path_buf);
        let kill_on_drop = command.get_kill_on_drop();
        let mut exec_resolve_accesses = PathAccessArena::default();
        let mut exec = command_to_exec(&command, |mode, path| {
            exec_resolve_accesses.add(PathAccess { mode, path: path.into() });
        })?;
        let pre_exec = handle_exec(
            &mut exec,
            ExecResolveConfig::search_path_disabled(),
            &encoded_payload,
            |mode, path| {
                exec_resolve_accesses.add(PathAccess { mode, path: path.into() });
            },
        )
        .map_err(|err| SpawnError::Injection(err.into()))?;
        set_exec_env(&mut exec, b"FSPY", b"1");
        let mut command = exec_to_command(exec, cwd, kill_on_drop);
        configure(&mut command);

        if let Some(pre_exec) = pre_exec {
            // SAFETY: the pre_exec closure only calls pre_exec.run(), which is
            // safe to call in a post-fork context.
            unsafe {
                command.pre_exec(move || {
                    pre_exec.run()?;
                    Ok(())
                });
            }
        }

        // command.spawn blocks while executing the `pre_exec` closure.
        // Run it inside spawn_blocking to avoid blocking the tokio runtime, especially the supervisor loop,
        // which needs to accept incoming connections while `pre_exec` is connecting to it.
        let mut child = spawn_blocking(move || command.spawn())
            .await
            .map_err(|err| SpawnError::OsSpawn(err.into()))?
            .map_err(SpawnError::OsSpawn)?;

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

                let arenas = std::iter::once(exec_resolve_accesses);
                // Stop the supervisor and collect path accesses from it.
                #[cfg(target_os = "linux")]
                let arenas = arenas.chain(
                    supervisor
                        .stop()
                        .await?
                        .into_iter()
                        .map(syscall_handler::SyscallHandler::into_arena),
                );
                let arenas = arenas.collect::<Vec<_>>();

                // Lock the ipc channel after the child has exited.
                // We are not interested in path accesses from descendants after the main child has exited.
                #[cfg(not(target_env = "musl"))]
                let ipc_receiver_lock_guard =
                    OwnedReceiverLockGuard::lock_async(ipc_receiver).await?;
                let path_accesses = PathAccessIterable {
                    arenas,
                    #[cfg(not(target_env = "musl"))]
                    ipc_receiver_lock_guard,
                };

                io::Result::Ok(ChildTermination { status, path_accesses })
            })
            .map(|f| f?) // flatten JoinError and io::Result
            .boxed(),
        })
    }
}

fn exec_to_command(mut exec: Exec, cwd: Option<std::path::PathBuf>, kill_on_drop: bool) -> Command {
    let mut command = Command::new(OsString::from_vec(exec.program.into()));
    command.arg0(OsString::from_vec(exec.args.remove(0).into()));
    command.args(exec.args.into_iter().map(|arg| OsString::from_vec(arg.into())));
    command.env_clear();
    command.envs(exec.envs.into_iter().map(|(name, value)| {
        (OsString::from_vec(name.into()), OsString::from_vec(value.unwrap_or_default().into()))
    }));
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.kill_on_drop(kill_on_drop);
    command
}

fn command_to_exec(
    command: &Command,
    mut on_path_access: impl FnMut(AccessMode, &Path),
) -> Result<Exec, SpawnError> {
    let command = command.as_std();
    let resolved_envs = command.get_resolved_envs().collect::<Vec<_>>();
    let configured_path = resolved_envs
        .iter()
        .find_map(|(name, value)| (name == "PATH").then_some(value.as_os_str()));

    let cwd = command.get_current_dir().map_or_else(
        || std::env::current_dir().expect("failed to get current dir"),
        Path::to_path_buf,
    );
    let cwd = std::path::absolute(cwd).expect("failed to resolve current dir");
    let search_path = configured_path.map_or_else(default_search_path, OsStr::to_os_string);
    let program = resolve_program(command.get_program(), &search_path, &cwd, &mut on_path_access)
        .map_err(|cause| SpawnError::Which {
        program: command.get_program().to_os_string(),
        path: Some(search_path),
        cwd,
        cause,
    })?;

    let args = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| BString::from(arg.as_bytes().to_vec()))
        .collect();
    let envs = resolved_envs
        .into_iter()
        .map(|(name, value)| {
            (BString::from(name.into_vec()), Some(BString::from(value.into_vec())))
        })
        .collect();

    Ok(Exec { program: BString::from(program.into_os_string().into_vec()), args, envs })
}

fn resolve_program(
    program: &OsStr,
    search_path: &OsStr,
    cwd: &Path,
    mut on_path_access: impl FnMut(AccessMode, &Path),
) -> Result<std::path::PathBuf, which::Error> {
    if program.as_bytes().contains(&b'/') {
        return which::which_in(program, Option::<&OsStr>::None, cwd);
    }

    for directory in std::env::split_paths(search_path) {
        let directory = if directory.is_absolute() { directory } else { cwd.join(directory) };
        let candidate = directory.join(program);
        on_path_access(AccessMode::READ, &candidate);
        if let Ok(program) = which::which_in(candidate, Option::<&OsStr>::None, cwd) {
            return Ok(program);
        }
    }
    Err(which::Error::CannotFindBinaryPath)
}

#[cfg(target_env = "musl")]
fn default_search_path() -> OsString {
    OsString::from("/usr/local/bin:/bin:/usr/bin")
}

#[cfg(not(target_env = "musl"))]
fn default_search_path() -> OsString {
    // SAFETY: A null buffer asks `confstr` for the required allocation size.
    let size = unsafe { libc::confstr(libc::_CS_PATH, ptr::null_mut(), 0) };
    if size == 0 {
        return OsString::from("/bin:/usr/bin");
    }

    let mut bytes = vec![0; size];
    // SAFETY: `bytes` has the size returned by the preceding `confstr` call.
    let written = unsafe { libc::confstr(libc::_CS_PATH, bytes.as_mut_ptr().cast(), bytes.len()) };
    if written == 0 {
        return OsString::from("/bin:/usr/bin");
    }
    bytes.truncate(written.saturating_sub(1));
    OsString::from_vec(bytes)
}

fn set_exec_env(exec: &mut Exec, name: &[u8], value: &[u8]) {
    if let Some((_, existing_value)) = exec.envs.iter_mut().find(|(env_name, _)| env_name == name) {
        *existing_value = Some(BString::from(value.to_vec()));
    } else {
        exec.envs.push((BString::from(name.to_vec()), Some(BString::from(value.to_vec()))));
    }
}

pub struct PathAccessIterable {
    arenas: Vec<PathAccessArena>,
    #[cfg(not(target_env = "musl"))]
    ipc_receiver_lock_guard: OwnedReceiverLockGuard,
}

impl PathAccessIterable {
    pub fn iter(&self) -> impl Iterator<Item = PathAccess<'_>> {
        let accesses_in_arena =
            self.arenas.iter().flat_map(|arena| arena.borrow_accesses().iter()).copied();

        #[cfg(not(target_env = "musl"))]
        {
            let accesses_in_shm = self.ipc_receiver_lock_guard.iter_path_accesses();
            accesses_in_shm.chain(accesses_in_arena)
        }
        #[cfg(target_env = "musl")]
        {
            accesses_in_arena
        }
    }
}
