//! Optional remote cache tier sitting behind the local `SQLite` cache.
//!
//! The local cache ([`super::ExecutionCache`]) is authoritative for a single
//! machine. A remote cache lets independent machines (CI runners, teammates)
//! share task results: a cache miss locally can still be a hit remotely, and a
//! freshly computed result is pushed up so the next machine skips the work.
//!
//! # Storage model
//!
//! The remote tier mirrors the local one. Locally there are two `SQLite`
//! tables — `cache_entries` (cache key → [`super::CacheEntryValue`]) and
//! `task_fingerprints` (execution key → cache key) — plus output archive files
//! on disk. The remote tier exposes the same three stores over HTTP:
//!
//! - two key→blob namespaces ([`RemoteNamespace`]) for the fingerprint tables,
//! - a blob store for output archives ([`RemoteCacheBackend::get_artifact`]).
//!
//! The reference implementation ([`HttpRemoteCache`]) talks to a Cloudflare
//! Worker that keeps the fingerprint namespaces in Workers KV and the archives
//! in R2, but the [`RemoteCacheBackend`] trait deliberately hides that: any
//! endpoint speaking the same small HTTP protocol works, and tests substitute
//! an in-memory backend.

use std::{ffi::OsStr, sync::Arc, time::Duration};

use anyhow::Context as _;
use base64::Engine as _;
use rustc_hash::FxHashMap;
use sha2::{Digest as _, Sha256};
use vite_path::AbsolutePath;
use vite_str::Str;

/// Environment variable holding the base URL of the remote cache endpoint
/// (e.g. `https://cache.example.workers.dev` or `http://localhost:8787` for
/// local development). Setting it enables the remote cache tier.
pub const REMOTE_CACHE_URL_ENV: &str = "VITE_REMOTE_CACHE_URL";

/// Environment variable holding the bearer token sent with every remote cache
/// request. Optional: a local development endpoint may not require auth.
pub const REMOTE_CACHE_TOKEN_ENV: &str = "VITE_REMOTE_CACHE_TOKEN";

/// Dotenv file, in the workspace root, that supplies [`REMOTE_CACHE_URL_ENV`] /
/// [`REMOTE_CACHE_TOKEN_ENV`] when they are absent from the process environment.
const DOTENV_FILE: &str = ".env";

/// One of the two key→blob namespaces in the remote fingerprint store, mirroring
/// the local cache's `cache_entries` and `task_fingerprints` `SQLite` tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteNamespace {
    /// Cache key → serialized [`super::CacheEntryValue`] (local `cache_entries`).
    Entry,
    /// Execution key → serialized [`super::CacheEntryKey`] (local
    /// `task_fingerprints`).
    Fingerprint,
}

impl RemoteNamespace {
    /// URL path segment identifying this namespace in the HTTP protocol.
    #[must_use]
    pub const fn path_segment(self) -> &'static str {
        match self {
            Self::Entry => "entries",
            Self::Fingerprint => "fingerprints",
        }
    }
}

/// Hash a serialized cache key into a fixed-length, URL-safe remote key.
///
/// Serialized cache keys (commands, glob configs, env fingerprints) have no
/// length bound and can exceed limits on URL path segments and KV keys, so the
/// remote store is keyed on the SHA-256 digest of the blob instead. Collisions
/// are cryptographically negligible, and the stored value still carries
/// everything needed to validate a hit against the current inputs.
#[must_use]
pub fn remote_key(key_blob: &[u8]) -> Str {
    let digest = Sha256::digest(key_blob);
    Str::from(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
}

/// Configuration for the remote cache tier, resolved from the environment.
#[derive(Clone)]
pub struct RemoteCacheConfig {
    /// Base URL of the remote cache endpoint, without a trailing slash.
    pub base_url: Str,
    /// Optional bearer token sent with every request.
    pub token: Option<Str>,
}

impl std::fmt::Debug for RemoteCacheConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token: it is a secret and `ExecutionCache` is
        // `#[derive(Debug)]`, which traces may surface.
        f.debug_struct("RemoteCacheConfig")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RemoteCacheConfig {
    /// Resolve remote cache config. Each value is taken from the process
    /// environment ([`REMOTE_CACHE_URL_ENV`] / [`REMOTE_CACHE_TOKEN_ENV`]) or,
    /// when unset there, from a [`DOTENV_FILE`] in the workspace root (the
    /// process env wins). Returns `None` (remote cache disabled) when no base
    /// URL can be resolved; the token is optional.
    #[must_use]
    pub fn from_envs(
        envs: &FxHashMap<Arc<OsStr>, Arc<OsStr>>,
        workspace_root: &AbsolutePath,
    ) -> Option<Self> {
        let dotenv = load_dotenv(workspace_root);
        let resolve = |name: &str| env_value(envs, name).or_else(|| dotenv.get(name).cloned());

        let base_url = Str::from(resolve(REMOTE_CACHE_URL_ENV)?.trim_end_matches('/'));
        let token = resolve(REMOTE_CACHE_TOKEN_ENV);
        Some(Self { base_url, token })
    }
}

/// Read a non-empty UTF-8 env value, or `None` if unset/empty/non-UTF-8.
fn env_value(envs: &FxHashMap<Arc<OsStr>, Arc<OsStr>>, name: &str) -> Option<Str> {
    let value = envs.get(OsStr::new(name))?.to_str()?;
    (!value.is_empty()).then(|| Str::from(value))
}

/// Parse the workspace `.env` file (via `dotenvy`, without mutating the process
/// environment) into a map of its non-empty entries. A missing or unreadable
/// file yields an empty map, so remote caching simply stays disabled.
fn load_dotenv(workspace_root: &AbsolutePath) -> FxHashMap<Str, Str> {
    let mut map = FxHashMap::default();
    let path = workspace_root.join(DOTENV_FILE);
    let Ok(iter) = dotenvy::from_path_iter(path.as_path()) else {
        return map;
    };
    for (key, value) in iter.flatten() {
        if !value.is_empty() {
            map.insert(Str::from(key), Str::from(value));
        }
    }
    map
}

/// A remote cache tier: three stores (two key→blob namespaces and a blob store)
/// addressed by opaque string keys.
///
/// All methods are best-effort from the caller's perspective: the local cache
/// stays authoritative, so callers degrade gracefully on `Err` (treating a
/// failed lookup as a miss and a failed store as skipped) rather than failing
/// the build. The trait is `?Send` to match the single-threaded task scheduler.
#[async_trait::async_trait(?Send)]
pub trait RemoteCacheBackend: std::fmt::Debug {
    /// Fetch a fingerprint blob, or `None` if absent.
    async fn get_kv(&self, ns: RemoteNamespace, key: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Store a fingerprint blob, overwriting any existing value.
    async fn put_kv(&self, ns: RemoteNamespace, key: &str, value: &[u8]) -> anyhow::Result<()>;
    /// Fetch an output archive by id, or `None` if absent.
    async fn get_artifact(&self, id: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Store an output archive under the given id, overwriting any existing one.
    async fn put_artifact(&self, id: &str, bytes: &[u8]) -> anyhow::Result<()>;
}

/// HTTP-backed [`RemoteCacheBackend`] speaking the Vite+ remote cache protocol.
///
/// # Protocol
///
/// Every request carries `Authorization: Bearer <token>` when a token is
/// configured. The body is the raw blob (no framing).
///
/// | Method  | Path                       | Behavior                              |
/// | ------- | -------------------------- | ------------------------------------- |
/// | `GET`   | `/v1/{namespace}/{key}`    | 200 + blob, or 404 if absent          |
/// | `PUT`   | `/v1/{namespace}/{key}`    | 2xx on success                        |
/// | `GET`   | `/v1/artifacts/{id}`       | 200 + bytes, or 404 if absent         |
/// | `PUT`   | `/v1/artifacts/{id}`       | 2xx on success                        |
///
/// `{namespace}` is [`RemoteNamespace::path_segment`].
pub struct HttpRemoteCache {
    client: reqwest::Client,
    base_url: Str,
    token: Option<Str>,
}

impl std::fmt::Debug for HttpRemoteCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the token (see `RemoteCacheConfig`'s Debug impl).
        f.debug_struct("HttpRemoteCache").field("base_url", &self.base_url).finish_non_exhaustive()
    }
}

impl HttpRemoteCache {
    /// Build an HTTP remote cache client from resolved config.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(config: RemoteCacheConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build remote cache HTTP client")?;
        Ok(Self { client, base_url: config.base_url, token: config.token })
    }

    /// Attach the bearer token to a request when one is configured.
    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => builder.bearer_auth(token.as_str()),
            None => builder,
        }
    }

    /// `GET {url}`, returning the body on 200, `None` on 404, error otherwise.
    async fn get_bytes(&self, url: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let response = self
            .authed(self.client.get(url))
            .send()
            .await
            .with_context(|| vite_str::format!("remote cache GET {url} failed"))?;
        match response.status() {
            reqwest::StatusCode::OK => {
                let bytes = response
                    .bytes()
                    .await
                    .with_context(|| vite_str::format!("reading remote cache body from {url}"))?;
                Ok(Some(bytes.to_vec()))
            }
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status => Err(anyhow::anyhow!("remote cache GET {url} returned status {status}")),
        }
    }

    /// `PUT {url}` with `body`, erroring on any non-2xx status.
    async fn put_bytes(&self, url: &str, body: Vec<u8>) -> anyhow::Result<()> {
        let response = self
            .authed(self.client.put(url))
            .body(body)
            .send()
            .await
            .with_context(|| vite_str::format!("remote cache PUT {url} failed"))?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("remote cache PUT {url} returned status {status}"))
        }
    }
}

#[async_trait::async_trait(?Send)]
impl RemoteCacheBackend for HttpRemoteCache {
    async fn get_kv(&self, ns: RemoteNamespace, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let url = vite_str::format!("{}/v1/{}/{key}", self.base_url, ns.path_segment());
        self.get_bytes(url.as_str()).await
    }

    async fn put_kv(&self, ns: RemoteNamespace, key: &str, value: &[u8]) -> anyhow::Result<()> {
        let url = vite_str::format!("{}/v1/{}/{key}", self.base_url, ns.path_segment());
        self.put_bytes(url.as_str(), value.to_vec()).await
    }

    async fn get_artifact(&self, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let url = vite_str::format!("{}/v1/artifacts/{id}", self.base_url);
        self.get_bytes(url.as_str()).await
    }

    async fn put_artifact(&self, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let url = vite_str::format!("{}/v1/artifacts/{id}", self.base_url);
        self.put_bytes(url.as_str(), bytes.to_vec()).await
    }
}

/// In-memory [`RemoteCacheBackend`] for tests: three maps standing in for the
/// remote KV namespaces and R2 blob store, so the two-tier cache logic can be
/// exercised without a network or a real endpoint.
#[cfg(test)]
#[derive(Debug, Default)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "shared across the crate's test modules (cache::tests uses it too)"
)]
pub(crate) struct InMemoryRemoteCache {
    entries: std::sync::Mutex<FxHashMap<Str, Vec<u8>>>,
    fingerprints: std::sync::Mutex<FxHashMap<Str, Vec<u8>>>,
    artifacts: std::sync::Mutex<FxHashMap<Str, Vec<u8>>>,
}

#[cfg(test)]
impl InMemoryRemoteCache {
    fn namespace(&self, ns: RemoteNamespace) -> &std::sync::Mutex<FxHashMap<Str, Vec<u8>>> {
        match ns {
            RemoteNamespace::Entry => &self.entries,
            RemoteNamespace::Fingerprint => &self.fingerprints,
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait(?Send)]
impl RemoteCacheBackend for InMemoryRemoteCache {
    async fn get_kv(&self, ns: RemoteNamespace, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.namespace(ns).lock().expect("lock poisoned").get(key).cloned())
    }

    async fn put_kv(&self, ns: RemoteNamespace, key: &str, value: &[u8]) -> anyhow::Result<()> {
        self.namespace(ns).lock().expect("lock poisoned").insert(Str::from(key), value.to_vec());
        Ok(())
    }

    async fn get_artifact(&self, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.artifacts.lock().expect("lock poisoned").get(id).cloned())
    }

    async fn put_artifact(&self, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.artifacts.lock().expect("lock poisoned").insert(Str::from(id), bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::TempDir;
    use vite_path::AbsolutePathBuf;

    use super::*;

    fn envs(pairs: &[(&str, &str)]) -> FxHashMap<Arc<OsStr>, Arc<OsStr>> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    Arc::from(OsString::from(*k).as_os_str()),
                    Arc::from(OsString::from(*v).as_os_str()),
                )
            })
            .collect()
    }

    /// A temp workspace dir, optionally seeded with a `.env` file.
    fn workspace(dotenv: Option<&str>) -> (TempDir, AbsolutePathBuf) {
        let tmp = TempDir::new().unwrap();
        if let Some(content) = dotenv {
            std::fs::write(tmp.path().join(DOTENV_FILE), content).unwrap();
        }
        let root = AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        (tmp, root)
    }

    #[test]
    fn config_disabled_without_url() {
        let (_tmp, root) = workspace(None);
        assert!(RemoteCacheConfig::from_envs(&envs(&[]), &root).is_none());
        assert!(
            RemoteCacheConfig::from_envs(&envs(&[(REMOTE_CACHE_URL_ENV, "")]), &root).is_none()
        );
    }

    #[test]
    fn config_trims_trailing_slash_and_reads_token() {
        let (_tmp, root) = workspace(None);
        let config = RemoteCacheConfig::from_envs(
            &envs(&[
                (REMOTE_CACHE_URL_ENV, "https://cache.example.com/"),
                (REMOTE_CACHE_TOKEN_ENV, "secret"),
            ]),
            &root,
        )
        .expect("config enabled");
        assert_eq!(config.base_url, "https://cache.example.com");
        assert_eq!(config.token.as_deref(), Some("secret"));
    }

    #[test]
    fn config_reads_url_and_token_from_dotenv() {
        let (_tmp, root) = workspace(Some(
            "# remote cache\nVITE_REMOTE_CACHE_URL=http://127.0.0.1:9999/\nVITE_REMOTE_CACHE_TOKEN=dotenv-token\n",
        ));
        let config =
            RemoteCacheConfig::from_envs(&envs(&[]), &root).expect("config enabled via .env");
        assert_eq!(config.base_url, "http://127.0.0.1:9999");
        assert_eq!(config.token.as_deref(), Some("dotenv-token"));
    }

    #[test]
    fn config_env_wins_over_dotenv() {
        let (_tmp, root) = workspace(Some(
            "VITE_REMOTE_CACHE_URL=http://from-dotenv\nVITE_REMOTE_CACHE_TOKEN=dotenv-token\n",
        ));
        let config = RemoteCacheConfig::from_envs(
            &envs(&[
                (REMOTE_CACHE_URL_ENV, "http://from-env"),
                (REMOTE_CACHE_TOKEN_ENV, "env-token"),
            ]),
            &root,
        )
        .expect("config enabled");
        assert_eq!(config.base_url, "http://from-env");
        assert_eq!(config.token.as_deref(), Some("env-token"));
    }

    #[test]
    fn config_disabled_without_dotenv_or_env() {
        let (_tmp, root) = workspace(None);
        assert!(RemoteCacheConfig::from_envs(&envs(&[]), &root).is_none());
    }

    #[test]
    fn config_token_optional() {
        let (_tmp, root) = workspace(None);
        let config = RemoteCacheConfig::from_envs(
            &envs(&[(REMOTE_CACHE_URL_ENV, "http://localhost:8787")]),
            &root,
        )
        .expect("config enabled");
        assert_eq!(config.token, None);
    }

    #[test]
    fn debug_redacts_token() {
        let config = RemoteCacheConfig {
            base_url: Str::from("https://cache.example.com"),
            token: Some(Str::from("super-secret")),
        };
        let rendered = vite_str::format!("{config:?}");
        assert!(!rendered.contains("super-secret"), "token leaked in Debug: {rendered}");
    }

    #[test]
    fn remote_key_is_stable_and_url_safe() {
        let key = remote_key(b"hello world");
        assert_eq!(key, remote_key(b"hello world"));
        assert_ne!(key, remote_key(b"hello world!"));
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[tokio::test]
    async fn in_memory_backend_round_trips_each_store() {
        let remote = InMemoryRemoteCache::default();

        // Distinct namespaces and the artifact store are independent.
        remote.put_kv(RemoteNamespace::Entry, "k", b"entry-value").await.unwrap();
        remote.put_kv(RemoteNamespace::Fingerprint, "k", b"fp-value").await.unwrap();
        remote.put_artifact("k", b"artifact-bytes").await.unwrap();

        assert_eq!(
            remote.get_kv(RemoteNamespace::Entry, "k").await.unwrap().as_deref(),
            Some(b"entry-value".as_slice())
        );
        assert_eq!(
            remote.get_kv(RemoteNamespace::Fingerprint, "k").await.unwrap().as_deref(),
            Some(b"fp-value".as_slice())
        );
        assert_eq!(
            remote.get_artifact("k").await.unwrap().as_deref(),
            Some(b"artifact-bytes".as_slice())
        );
        assert_eq!(remote.get_kv(RemoteNamespace::Entry, "absent").await.unwrap(), None);
        assert_eq!(remote.get_artifact("absent").await.unwrap(), None);
    }
}
