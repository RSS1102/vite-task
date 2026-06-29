#[cfg(target_os = "linux")]
mod syscall_handler;

#[cfg(target_os = "macos")]
mod macos_artifacts;

use std::{
    ffi::{CString, OsStr, c_char},
    io,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::Path,
    ptr,
};

use bstr::BString;
#[cfg(target_os = "linux")]
use fspy_seccomp_unotify::supervisor::supervise;
use fspy_shared::ipc::PathAccess;
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
use tokio::{process::Command as TokioCommand, task::spawn_blocking};
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

    pub(crate) async fn spawn(
        &self,
        mut command: TokioCommand,
        cancellation_token: CancellationToken,
    ) -> Result<TrackedChild, SpawnError> {
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

        let mut exec = command_to_exec(&command)?;
        let mut exec_resolve_accesses = PathAccessArena::default();
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
        let execve_command = ExecveCommand::new(exec).map_err(SpawnError::OsSpawn)?;

        // SAFETY: caller-provided pre_exec hooks (if any) run first. This hook
        // then installs fspy's process-local state and execs the resolved
        // program with the resolved environment.
        unsafe {
            command.pre_exec(move || {
                if let Some(pre_exec) = pre_exec.as_ref() {
                    pre_exec.run()?;
                }
                execve_command.exec()
            });
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

fn command_to_exec(command: &TokioCommand) -> Result<Exec, SpawnError> {
    let command = command.as_std();
    let resolved_envs = command.get_resolved_envs().collect::<Vec<_>>();
    let path_env = resolved_envs.iter().find_map(|(name, value)| {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("path"))
            .then_some(value.as_os_str())
    });

    let cwd = command.get_current_dir().map_or_else(
        || std::env::current_dir().expect("failed to get current dir"),
        Path::to_path_buf,
    );
    let program = which::which_in(command.get_program(), path_env, &cwd).map_err(|err| {
        SpawnError::Which {
            program: command.get_program().to_os_string(),
            path: path_env.map(OsStr::to_owned),
            cwd,
            cause: err,
        }
    })?;

    let args = std::iter::once(program.as_os_str())
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

fn set_exec_env(exec: &mut Exec, name: &[u8], value: &[u8]) {
    if let Some((_, existing_value)) = exec.envs.iter_mut().find(|(env_name, _)| env_name == name) {
        *existing_value = Some(BString::from(value.to_vec()));
    } else {
        exec.envs.push((BString::from(name.to_vec()), Some(BString::from(value.to_vec()))));
    }
}

struct ExecveCommand {
    program: CString,
    args: Vec<CString>,
    envs: Vec<CString>,
    arg_ptrs: Vec<*const c_char>,
    env_ptrs: Vec<*const c_char>,
}

// SAFETY: The raw pointers point into immutable `CString` allocations owned by
// the same struct. They are only read in the child-side pre_exec hook.
unsafe impl Send for ExecveCommand {}
// SAFETY: See the `Send` impl. The struct is captured by a pre_exec closure
// requiring `Sync`, but it is not accessed concurrently after fork.
unsafe impl Sync for ExecveCommand {}

impl ExecveCommand {
    fn new(exec: Exec) -> io::Result<Self> {
        let program = CString::new(Vec::<u8>::from(exec.program)).map_err(nul_error)?;
        let args = exec
            .args
            .into_iter()
            .map(|arg| CString::new(Vec::<u8>::from(arg)).map_err(nul_error))
            .collect::<io::Result<Vec<_>>>()?;
        let envs = exec
            .envs
            .into_iter()
            .map(|(name, value)| {
                let mut env = Vec::<u8>::from(name);
                if let Some(value) = value {
                    env.push(b'=');
                    env.extend(Vec::<u8>::from(value));
                }
                CString::new(env).map_err(nul_error)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut arg_ptrs = args.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
        arg_ptrs.push(ptr::null());
        let mut env_ptrs = envs.iter().map(|env| env.as_ptr()).collect::<Vec<_>>();
        env_ptrs.push(ptr::null());

        Ok(Self { program, args, envs, arg_ptrs, env_ptrs })
    }

    fn exec(&self) -> io::Result<()> {
        // Keep the backing allocations visibly live for the raw argv/envp pointers.
        let _ = (&self.args, &self.envs);
        // SAFETY: `program`, `argv`, and `envp` are null-terminated C strings
        // with trailing null pointer arrays. On success this does not return.
        unsafe {
            libc::execve(self.program.as_ptr(), self.arg_ptrs.as_ptr(), self.env_ptrs.as_ptr());
        }
        Err(io::Error::last_os_error())
    }
}

fn nul_error(err: std::ffi::NulError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, err)
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
