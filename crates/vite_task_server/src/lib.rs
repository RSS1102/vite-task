use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    sync::Arc,
};

use futures::{FutureExt, StreamExt, future::LocalBoxFuture, stream::FuturesUnordered};
use interprocess::local_socket::{
    ListenerOptions,
    tokio::{Listener, Stream, prelude::*},
};
use native_str::NativeStr;
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use vite_path::AbsolutePath;
use vite_task_ipc_shared::{Ack, GetEnvResponse, GetEnvsResponse, IPC_ENV_NAME, Request};
use wincode::{SchemaWrite, config::DefaultConfig};

pub trait Handler {
    fn ignore_input(&mut self, path: &Arc<AbsolutePath>);
    fn ignore_output(&mut self, path: &Arc<AbsolutePath>);
    fn disable_cache(&mut self);
    fn get_env(&mut self, name: &OsStr, tracked: bool) -> Option<Arc<OsStr>>;
    /// Returns the subset of the env map whose names match `pattern` as a
    /// wax/glob pattern, recording the match-set for the post-run fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error if `pattern` fails to parse as a glob.
    fn get_envs(
        &mut self,
        pattern: &str,
        tracked: bool,
    ) -> Result<BTreeMap<Arc<OsStr>, Arc<OsStr>>, vite_glob::Error>;
}

/// A protocol-level failure observed while servicing a client.
///
/// The driver retains only the first such error across all clients, then
/// completes gracefully (existing clients drain, new connections are rejected).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read request frame from client")]
    ReadFrame(#[source] io::Error),

    #[error("failed to deserialize request from client")]
    InvalidRequest(#[source] wincode::ReadError),

    #[error("non-absolute path from client: {path:?}")]
    NonAbsolutePath { path: OsString },

    #[error("invalid glob pattern from client: {:?}", .0.pattern)]
    InvalidGlob(Box<InvalidGlob>),

    #[error("failed to write response to client")]
    WriteResponse(#[source] io::Error),
}

/// Payload for [`Error::InvalidGlob`]. Boxed so the `Error` enum stays small
/// — `vite_glob::Error` wraps `wax::BuildError` which is over 100 bytes on
/// its own.
#[derive(Debug)]
pub struct InvalidGlob {
    pub pattern: Box<str>,
    pub source: vite_glob::Error,
}

/// A [`Handler`] that records every report and resolves `get_env` against
/// a provided env map.
///
/// Call [`Recorder::into_reports`] after the driver future completes to
/// recover the collected [`Reports`].
pub struct Recorder {
    ignored_inputs: FxHashSet<Arc<AbsolutePath>>,
    ignored_outputs: FxHashSet<Arc<AbsolutePath>>,
    cache_disabled: bool,
    env_records: FxHashMap<Arc<OsStr>, EnvRecord>,
    env_glob_records: FxHashMap<Arc<str>, EnvGlobRecord>,
    env_map: FxHashMap<Arc<OsStr>, Arc<OsStr>>,
}

/// A record of an env value requested via `get_env`.
///
/// `tracked` is the monotonic OR of every `tracked` flag sent for this name
/// — once `true`, it stays `true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRecord {
    pub tracked: bool,
    pub value: Option<Arc<OsStr>>,
}

/// A record of a glob-pattern env query made via `get_envs`.
///
/// `matches` is captured on the first call and reused on repeat queries —
/// the server's `env_map` is immutable for the task's lifetime, so the set
/// is stable. `tracked` is monotonic like `EnvRecord::tracked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvGlobRecord {
    pub tracked: bool,
    pub matches: BTreeMap<Arc<OsStr>, Arc<OsStr>>,
}

/// The data collected by a [`Recorder`] over the server's lifetime.
#[derive(Debug, Default)]
pub struct Reports {
    pub ignored_inputs: FxHashSet<Arc<AbsolutePath>>,
    pub ignored_outputs: FxHashSet<Arc<AbsolutePath>>,
    pub cache_disabled: bool,
    pub env_records: FxHashMap<Arc<OsStr>, EnvRecord>,
    pub env_glob_records: FxHashMap<Arc<str>, EnvGlobRecord>,
}

impl Recorder {
    #[must_use]
    pub fn new(env_map: FxHashMap<Arc<OsStr>, Arc<OsStr>>) -> Self {
        Self {
            ignored_inputs: FxHashSet::default(),
            ignored_outputs: FxHashSet::default(),
            cache_disabled: false,
            env_records: FxHashMap::default(),
            env_glob_records: FxHashMap::default(),
            env_map,
        }
    }

    #[must_use]
    pub fn into_reports(self) -> Reports {
        Reports {
            ignored_inputs: self.ignored_inputs,
            ignored_outputs: self.ignored_outputs,
            cache_disabled: self.cache_disabled,
            env_records: self.env_records,
            env_glob_records: self.env_glob_records,
        }
    }
}

impl Handler for Recorder {
    fn ignore_input(&mut self, path: &Arc<AbsolutePath>) {
        self.ignored_inputs.insert(Arc::clone(path));
    }

    fn ignore_output(&mut self, path: &Arc<AbsolutePath>) {
        self.ignored_outputs.insert(Arc::clone(path));
    }

    fn disable_cache(&mut self) {
        self.cache_disabled = true;
    }

    fn get_env(&mut self, name: &OsStr, tracked: bool) -> Option<Arc<OsStr>> {
        if let Some(existing) = self.env_records.get_mut(name) {
            existing.tracked |= tracked;
            return existing.value.clone();
        }
        let value = self.env_map.get(name).cloned();
        self.env_records.insert(name.into(), EnvRecord { tracked, value: value.clone() });
        value
    }

    fn get_envs(
        &mut self,
        pattern: &str,
        tracked: bool,
    ) -> Result<BTreeMap<Arc<OsStr>, Arc<OsStr>>, vite_glob::Error> {
        if let Some(existing) = self.env_glob_records.get_mut(pattern) {
            existing.tracked |= tracked;
            return Ok(existing.matches.clone());
        }
        let set = vite_glob::GlobPatternSet::new(std::iter::once(pattern))?;
        let matches: BTreeMap<Arc<OsStr>, Arc<OsStr>> = self
            .env_map
            .iter()
            .filter_map(|(name, value)| {
                // Env names that aren't valid UTF-8 can't be matched against a
                // glob (wax patterns are UTF-8), so they're silently dropped.
                // Consistent with how `collect_tracked_envs` drops non-UTF-8
                // names when building the post-run fingerprint.
                let name_str = name.to_str()?;
                if set.is_match(name_str) {
                    Some((Arc::clone(name), Arc::clone(value)))
                } else {
                    None
                }
            })
            .collect();
        self.env_glob_records
            .insert(Arc::from(pattern), EnvGlobRecord { tracked, matches: matches.clone() });
        Ok(matches)
    }
}

/// Handle to a running IPC server.
///
/// `driver` must be polled to accept clients and handle messages. It resolves
/// only after [`StopAccepting::signal`] has been called AND all in-flight
/// per-client tasks have drained, returning the owned handler.
///
/// The driver resolves to `Err(Error)` if any client triggered a protocol
/// violation (see [`Error`]). The first such error is retained; subsequent
/// errors during drain are discarded. On `Err`, the handler is not returned.
///
/// Dropping `driver` before it resolves tears everything down immediately —
/// listener closed, per-client tasks cancelled, handler discarded.
pub struct ServerHandle<'h, H> {
    pub driver: LocalBoxFuture<'h, Result<H, Error>>,
    pub stop_accepting: StopAccepting,
}

/// Signal that tells the server to stop accepting new clients. Existing
/// clients continue until they naturally close the connection; the driver
/// future resolves once that drain completes.
///
/// [`signal`](Self::signal) takes `&self` and the underlying cancellation
/// is idempotent, so calling it twice or from a shared borrow is safe.
pub struct StopAccepting {
    token: CancellationToken,
}

impl StopAccepting {
    pub fn signal(&self) {
        self.token.cancel();
    }
}

/// Starts an IPC server.
///
/// Returns the env entries that a child process must inherit to find and
/// connect to this server, plus a handle bundling the driver future and the
/// `StopAccepting` signal. See [`ServerHandle`] for driver semantics.
///
/// # Errors
///
/// Returns an error if creating the listener fails (on Unix, this includes
/// creating the temp socket path).
pub fn serve<'h, H: Handler + 'h>(
    handler: H,
) -> io::Result<(impl Iterator<Item = (&'static OsStr, OsString)>, ServerHandle<'h, H>)> {
    let stop_token = CancellationToken::new();
    let (name, bound) = bind_listener()?;

    let run_stop = stop_token.clone();
    let driver = async move {
        // Multiple per-client futures coexist inside `FuturesUnordered` and each
        // calls `&mut self` handler methods. `RefCell` provides the interior
        // mutability that makes these shared-access method calls compile; at
        // runtime the `borrow_mut()` never conflicts because we're on a
        // single-threaded runtime and handler methods are synchronous (no
        // awaits, so no borrow spans a yield point).
        let handler = RefCell::new(handler);
        let first_err = run(bound, &handler, run_stop).await;
        first_err.map_or_else(|| Ok(handler.into_inner()), Err)
    }
    .boxed_local();

    Ok((
        std::iter::once((OsStr::new(IPC_ENV_NAME), name)),
        ServerHandle { driver, stop_accepting: StopAccepting { token: stop_token } },
    ))
}

#[cfg(unix)]
type Bound = tempfile::NamedTempFile<Listener>;
#[cfg(windows)]
type Bound = Listener;

#[cfg(unix)]
fn bind_listener() -> io::Result<(OsString, Bound)> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};

    let bound = tempfile::Builder::new().prefix("vite_task_ipc_").make(|path| {
        let name = path.to_fs_name::<GenericFilePath>()?;
        ListenerOptions::new().name(name).create_tokio()
    })?;
    let name = bound.path().as_os_str().to_owned();
    Ok((name, bound))
}

#[cfg(windows)]
fn bind_listener() -> io::Result<(OsString, Bound)> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};

    #[expect(
        clippy::disallowed_macros,
        reason = "socket name always exceeds Str inline capacity; format! is the simplest construction"
    )]
    let name = OsString::from(format!("vite_task_ipc_{}", uuid::Uuid::new_v4()));

    let ns_name = name.as_os_str().to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new().name(ns_name).create_tokio()?;
    Ok((name, listener))
}

#[cfg(unix)]
fn listener_of(bound: &Bound) -> &Listener {
    bound.as_file()
}

#[cfg(windows)]
const fn listener_of(bound: &Bound) -> &Listener {
    bound
}

async fn run<H: Handler>(
    bound: Bound,
    handler: &RefCell<H>,
    shutdown: CancellationToken,
) -> Option<Error> {
    let mut clients = FuturesUnordered::new();
    let mut first_err: Option<Error> = None;

    // Accept phase: accept new clients until shutdown fires.
    loop {
        let listener = listener_of(&bound);
        tokio::select! {
            () = shutdown.cancelled() => break,
            accept_result = listener.accept() => {
                match accept_result {
                    Ok(stream) => {
                        clients.push(handle_client(stream, handler).boxed_local());
                    }
                    Err(err) => {
                        tracing::warn!(?err, "vite_task_server: accept failed");
                    }
                }
            }
            Some(result) = clients.next(), if !clients.is_empty() => {
                if let Err(err) = result
                    && first_err.is_none()
                {
                    first_err = Some(err);
                    shutdown.cancel();
                }
            }
        }
    }

    // Stop accepting: drop the listener (and on Unix unlink the socket file).
    // Existing client streams continue to work.
    drop(bound);

    // Drain phase: wait for all in-flight per-client tasks to finish.
    while let Some(result) = clients.next().await {
        if let Err(err) = result
            && first_err.is_none()
        {
            first_err = Some(err);
        }
    }

    first_err
}

async fn handle_client<H: Handler>(mut stream: Stream, handler: &RefCell<H>) -> Result<(), Error> {
    let mut buf = Vec::new();
    loop {
        match read_frame(&mut stream, &mut buf).await {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(Error::ReadFrame(err)),
        }

        let request: Request<'_> =
            wincode::deserialize_exact(&buf).map_err(Error::InvalidRequest)?;

        match request {
            Request::IgnoreInput(ns) => {
                let path = native_str_to_abs_path(ns)?;
                handler.borrow_mut().ignore_input(&path);
                write_response(&mut stream, &Ack).await.map_err(Error::WriteResponse)?;
            }
            Request::IgnoreOutput(ns) => {
                let path = native_str_to_abs_path(ns)?;
                handler.borrow_mut().ignore_output(&path);
                write_response(&mut stream, &Ack).await.map_err(Error::WriteResponse)?;
            }
            Request::DisableCache => {
                handler.borrow_mut().disable_cache();
                write_response(&mut stream, &Ack).await.map_err(Error::WriteResponse)?;
            }
            Request::GetEnv { name, tracked } => {
                let value = handler.borrow_mut().get_env(name.to_cow_os_str().as_ref(), tracked);
                let boxed: Option<Box<NativeStr>> = value.as_deref().map(Into::into);
                let response = GetEnvResponse { env_value: boxed.as_deref() };
                write_response(&mut stream, &response).await.map_err(Error::WriteResponse)?;
            }
            Request::GetEnvs { pattern, tracked } => {
                let matches =
                    handler.borrow_mut().get_envs(pattern, tracked).map_err(|source| {
                        Error::InvalidGlob(Box::new(InvalidGlob {
                            pattern: Box::<str>::from(pattern),
                            source,
                        }))
                    })?;
                // Borrow the name/value OsStrs into NativeStr refs for the
                // outgoing frame; `boxed_entries` owns the NativeStr boxes so
                // their refs stay valid while `response` is serialized.
                let boxed_entries: Vec<(Box<NativeStr>, Box<NativeStr>)> = matches
                    .into_iter()
                    .map(|(k, v)| (Box::<NativeStr>::from(&*k), Box::<NativeStr>::from(&*v)))
                    .collect();
                let entries: BTreeMap<&NativeStr, &NativeStr> =
                    boxed_entries.iter().map(|(k, v)| (&**k, &**v)).collect();
                let response = GetEnvsResponse { entries };
                write_response(&mut stream, &response).await.map_err(Error::WriteResponse)?;
            }
        }
    }
}

fn native_str_to_abs_path(ns: &NativeStr) -> Result<Arc<AbsolutePath>, Error> {
    let os_str = ns.to_cow_os_str();
    AbsolutePath::new(&*os_str)
        .map(Arc::from)
        .ok_or_else(|| Error::NonAbsolutePath { path: os_str.into_owned() })
}

async fn read_frame(stream: &mut Stream, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes).await?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    buf.clear();
    buf.resize(len, 0);
    stream.read_exact(buf).await?;
    Ok(())
}

async fn write_response<T>(stream: &mut Stream, response: &T) -> io::Result<()>
where
    T: SchemaWrite<DefaultConfig, Src = T> + ?Sized,
{
    let bytes = wincode::serialize(response)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response too large"))?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
