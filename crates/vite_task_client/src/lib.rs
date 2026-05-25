use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsStr,
    io::{self, Read, Write},
    sync::Arc,
};

use interprocess::local_socket::{Stream, prelude::*};
use native_str::NativeStr;
use vite_path::{self, AbsolutePath};
use vite_task_ipc_shared::{Ack, GetEnvResponse, GetEnvsResponse, IPC_ENV_NAME, Request};

pub struct Client {
    stream: RefCell<Stream>,
    scratch: RefCell<Vec<u8>>,
}

/// Windows-only: flush pending writes through to the runner ourselves,
/// then bypass `interprocess`'s named-pipe limbo so dropping the stream
/// doesn't spawn its linger-pool thread.
///
/// Why: every send marks the pipe as "dirty" and `interprocess`'s `Drop`
/// hands dirty pipes to a background thread that calls
/// `FlushFileBuffers` for graceful close. That thread is created lazily,
/// and on locked-down Windows runners (Node worker threads inside CI
/// containers) the `CreateThread` from the napi finalizer can return
/// ACCESS_DENIED. The crate panics on that path with `failed to start
/// the persistent thread of the Interprocess linger pool`, killing the
/// Node child.
///
/// `flush()` calls the real `FlushFileBuffers`, which blocks until the
/// runner has read every byte we sent — this is what the linger pool
/// would have done off-thread. `assume_flushed()` then clears the dirty
/// flag so the inner `Drop` closes the handle directly instead of
/// detouring through the linger pool. Together they preserve the
/// protocol guarantee that fire-and-forget calls (`ignoreInput`,
/// `ignoreOutput`, `disableCache`) reach the runner before the child
/// exits.
#[cfg(windows)]
impl Drop for Client {
    fn drop(&mut self) {
        let interprocess::local_socket::Stream::NamedPipe(np_stream) = self.stream.get_mut();
        let _ = np_stream.inner().flush();
        np_stream.inner().assume_flushed();
    }
}

impl Client {
    /// Scans `envs` for the runner's IPC connection info and connects if
    /// present. Typical callers pass `std::env::vars_os()`.
    ///
    /// Returns `Ok(None)` if the IPC env is absent (running outside the runner).
    /// `Err(..)` if the env is set but connecting fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the env var is set but the server cannot be reached.
    pub fn from_envs(
        envs: impl Iterator<Item = (impl AsRef<OsStr>, impl AsRef<OsStr>)>,
    ) -> io::Result<Option<Self>> {
        for (name, value) in envs {
            if name.as_ref() == IPC_ENV_NAME {
                let stream = Stream::connect(resolve_name(value.as_ref())?)?;
                return Ok(Some(Self::from_stream(stream)));
            }
        }
        Ok(None)
    }

    const fn from_stream(stream: Stream) -> Self {
        Self { stream: RefCell::new(stream), scratch: RefCell::new(Vec::new()) }
    }

    /// `path` can be a file or a directory; for a directory, all files inside
    /// it are ignored. Relative paths are resolved against the current working
    /// directory before being sent to the runner.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails to send, or (for a relative
    /// `path`) if the current working directory cannot be read.
    pub fn ignore_input(&self, path: &OsStr) -> io::Result<()> {
        let ns = resolve_path(path)?;
        self.send(&Request::IgnoreInput(&ns))?;
        self.recv_ack()
    }

    /// `path` can be a file or a directory; for a directory, all files inside
    /// it are ignored. Relative paths are resolved against the current working
    /// directory before being sent to the runner.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails to send, or (for a relative
    /// `path`) if the current working directory cannot be read.
    pub fn ignore_output(&self, path: &OsStr) -> io::Result<()> {
        let ns = resolve_path(path)?;
        self.send(&Request::IgnoreOutput(&ns))?;
        self.recv_ack()
    }

    /// # Errors
    ///
    /// Returns an error if the request fails to send.
    pub fn disable_cache(&self) -> io::Result<()> {
        self.send(&Request::DisableCache)?;
        self.recv_ack()
    }

    fn recv_ack(&self) -> io::Result<()> {
        self.recv_with(|bytes| {
            let _: Ack = wincode::deserialize_exact(bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            Ok(())
        })
    }

    /// Requests an env value from the runner. Returns `None` if the runner reports
    /// the env is not available.
    ///
    /// # Errors
    ///
    /// Returns an error if the request or response fails.
    pub fn get_env(&self, name: &OsStr, tracked: bool) -> io::Result<Option<Arc<OsStr>>> {
        let name = Box::<NativeStr>::from(name);

        self.send(&Request::GetEnv { name: &name, tracked })?;
        self.recv_with(|bytes| {
            let response: GetEnvResponse<'_> = wincode::deserialize_exact(bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            Ok(response
                .env_value
                .map(|env_value| Arc::<OsStr>::from(env_value.to_cow_os_str().as_ref())))
        })
    }

    /// Requests every env whose name matches `pattern` from the runner. The
    /// returned map is keyed by env name (sorted) with its value.
    ///
    /// Unlike [`Self::get_env`], this always round-trips to the server — the
    /// client has no way to know in advance which names the pattern matches.
    /// Env names that aren't valid UTF-8 are silently dropped at the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the request or response fails, or if the server
    /// rejected the pattern as an invalid glob.
    pub fn get_envs(
        &self,
        pattern: &str,
        tracked: bool,
    ) -> io::Result<BTreeMap<Arc<OsStr>, Arc<OsStr>>> {
        self.send(&Request::GetEnvs { pattern, tracked })?;
        self.recv_with(|bytes| {
            let response: GetEnvsResponse<'_> = wincode::deserialize_exact(bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            Ok(response
                .entries
                .iter()
                .map(|(name, value)| {
                    (
                        Arc::<OsStr>::from(name.to_cow_os_str().as_ref()),
                        Arc::<OsStr>::from(value.to_cow_os_str().as_ref()),
                    )
                })
                .collect())
        })
    }

    fn send(&self, request: &Request<'_>) -> io::Result<()> {
        let bytes = wincode::serialize(request)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request too large"))?;
        let mut stream = self.stream.borrow_mut();
        stream.write_all(&len.to_le_bytes())?;
        stream.write_all(&bytes)?;
        stream.flush()?;
        Ok(())
    }

    fn recv_with<T>(&self, extract: impl FnOnce(&[u8]) -> io::Result<T>) -> io::Result<T> {
        let mut stream = self.stream.borrow_mut();
        let mut scratch = self.scratch.borrow_mut();
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes)?;
        let len = u32::from_le_bytes(len_bytes) as usize;
        scratch.clear();
        scratch.resize(len, 0);
        stream.read_exact(&mut scratch)?;
        extract(&scratch)
    }
}

#[cfg(unix)]
fn resolve_name(name: &OsStr) -> io::Result<interprocess::local_socket::Name<'_>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    name.to_fs_name::<GenericFilePath>()
}

#[cfg(windows)]
fn resolve_name(name: &OsStr) -> io::Result<interprocess::local_socket::Name<'_>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    name.to_ns_name::<GenericNamespaced>()
}

#[expect(
    clippy::disallowed_types,
    reason = "std::path::PathBuf is needed to round-trip through std::fs::canonicalize on Windows below"
)]
fn resolve_path(path: &OsStr) -> io::Result<Box<NativeStr>> {
    let absolute: std::path::PathBuf = if let Some(abs) = AbsolutePath::new(path) {
        abs.as_path().to_path_buf()
    } else {
        let mut buf = vite_path::current_dir()?;
        buf.push(path);
        buf.as_path().to_path_buf()
    };

    // On Windows, canonicalize so the path uses the exact on-disk casing
    // and resolves substitute drives / junctions the same way `fspy`'s
    // `GetFinalPathNameByHandleW`-reported paths do. Without this, an
    // `ignoreInput("cache_like")` whose `current_dir()` prefix differs in
    // case or symlink shape from the fspy-reported reads won't filter
    // them out, and the runner sees a read/write overlap. Strip the
    // `\\?\` namespace prefix because `fspy_shared::NativePath::
    // strip_path_prefix` does the same on the runner side; if the
    // canonical form starts with `\\?\UNC\`, fall back to the
    // non-canonical form so we don't accidentally rewrite a UNC path
    // (where dropping `\\?\` would change meaning).
    #[cfg(windows)]
    let absolute = match std::fs::canonicalize(&absolute) {
        Ok(canonical) => {
            use std::{
                ffi::OsString,
                os::windows::ffi::{OsStrExt, OsStringExt},
            };
            let wide: Vec<u16> = canonical.as_os_str().encode_wide().collect();
            let unc_prefix: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
            let nt_prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
            if wide.starts_with(&unc_prefix) {
                // UNC path — keep canonical form (still has \\?\UNC\ for fspy parity).
                canonical
            } else if let Some(rest) = wide.strip_prefix(nt_prefix.as_slice()) {
                std::path::PathBuf::from(OsString::from_wide(rest))
            } else {
                canonical
            }
        }
        Err(_) => absolute,
    };

    Ok(Box::<NativeStr>::from(absolute.as_os_str()))
}
