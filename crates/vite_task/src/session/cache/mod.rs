//! Execution cache for storing and retrieving cached command results.

pub mod archive;
pub mod display;

use std::{collections::BTreeMap, fmt::Display, fs::File, io::Write, sync::Arc, time::Duration};

// Re-export display functions for convenience
pub use display::format_cache_status_inline;
pub use display::{
    SpawnFingerprintChange, detect_spawn_fingerprint_changes, format_input_change_str,
    format_spawn_change,
};
use rusqlite::{Connection, OptionalExtension as _, types::Value};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use vite_path::{AbsolutePath, RelativePathBuf};
use vite_str::Str;
use vite_task_graph::config::ResolvedGlobConfig;
use vite_task_plan::cache_metadata::{CacheMetadata, ExecutionCacheKey, SpawnFingerprint};
use wincode::{
    SchemaRead, SchemaReadOwned, SchemaWrite,
    config::{ConfigCore, Configuration},
    error::{ReadResult, WriteResult},
    io::{Reader, Writer},
};

use super::execute::{
    fingerprint::{PostRunFingerprint, TrackedEnvQuery},
    pipe::StdOutput,
};

const TASK_CACHE_PREALLOCATION_SIZE_LIMIT: usize = 256 * 1024 * 1024;
type TaskCacheConfig = Configuration<true, TASK_CACHE_PREALLOCATION_SIZE_LIMIT>;
const TASK_CACHE_CONFIG: TaskCacheConfig =
    Configuration::default().with_preallocation_size_limit::<TASK_CACHE_PREALLOCATION_SIZE_LIMIT>();

fn serialize_cache<T>(value: &T) -> WriteResult<Vec<u8>>
where
    T: SchemaWrite<TaskCacheConfig, Src = T> + ?Sized,
{
    wincode::config::serialize(value, TASK_CACHE_CONFIG)
}

fn deserialize_cache<T>(bytes: &[u8]) -> ReadResult<T>
where
    T: SchemaReadOwned<TaskCacheConfig, Dst = T>,
{
    wincode::config::deserialize_exact(bytes, TASK_CACHE_CONFIG)
}

#[derive(Debug, Clone, Copy)]
enum CacheTable {
    CacheEntries,
    TaskFingerprints,
}

impl CacheTable {
    const fn name(self) -> &'static str {
        match self {
            Self::CacheEntries => "cache_entries",
            Self::TaskFingerprints => "task_fingerprints",
        }
    }

    const fn select_sql(self) -> &'static str {
        match self {
            Self::CacheEntries => "SELECT value FROM cache_entries WHERE key=?1",
            Self::TaskFingerprints => "SELECT value FROM task_fingerprints WHERE key=?1",
        }
    }

    const fn upsert_sql(self) -> &'static str {
        match self {
            Self::CacheEntries => {
                "INSERT INTO cache_entries (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value=?2"
            }
            Self::TaskFingerprints => {
                "INSERT INTO task_fingerprints (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value=?2"
            }
        }
    }

    const fn delete_if_unchanged_sql(self) -> &'static str {
        match self {
            Self::CacheEntries => {
                "DELETE FROM cache_entries \
                 WHERE key=?1 AND typeof(value)=typeof(?2) AND value IS ?2"
            }
            Self::TaskFingerprints => {
                "DELETE FROM task_fingerprints \
                 WHERE key=?1 AND typeof(value)=typeof(?2) AND value IS ?2"
            }
        }
    }

    const fn list_sql(self) -> &'static str {
        match self {
            Self::CacheEntries => "SELECT key, value FROM cache_entries",
            Self::TaskFingerprints => "SELECT key, value FROM task_fingerprints",
        }
    }
}

struct CacheRow<T> {
    value: T,
    key_blob: Vec<u8>,
    stored_value: Value,
}

enum CacheRead<T> {
    Found(CacheRow<T>),
    Missing,
    Unreadable,
}

/// Cache lookup key identifying a task's execution configuration.
///
/// # Key vs value design
///
/// Put a field in the **key** if each distinct value should have its own
/// cache entry (e.g., different env values → different entries, so
/// reverting an env change can still hit the old entry).
///
/// Put a field in the **value** ([`CacheEntryValue`]) if changes should
/// overwrite the existing entry (e.g., input file hashes — there's no
/// reason to keep the old hash around, and storing them in the value
/// lets us report exactly *which file* changed).
#[derive(Debug, SchemaWrite, SchemaRead, Serialize, PartialEq, Eq, Clone)]
pub struct CacheEntryKey {
    /// The spawn fingerprint (command, args, cwd, envs)
    pub spawn_fingerprint: SpawnFingerprint,
    /// Resolved input configuration that affects cache behavior.
    /// Glob patterns are workspace-root-relative.
    pub input_config: ResolvedGlobConfig,
    /// Resolved output configuration that affects cache restoration.
    /// Glob patterns are workspace-root-relative.
    pub output_config: ResolvedGlobConfig,
}

impl CacheEntryKey {
    fn from_metadata(cache_metadata: &CacheMetadata) -> Self {
        Self {
            spawn_fingerprint: cache_metadata.spawn_fingerprint.clone(),
            input_config: cache_metadata.input_config.clone(),
            output_config: cache_metadata.output_config.clone(),
        }
    }
}

/// wincode schema adapter for `Duration`.
struct DurationSchema;

// SAFETY: Writes exactly `size_of::<u64>() + size_of::<u32>()` bytes matching size_of.
unsafe impl<C: ConfigCore> SchemaWrite<C> for DurationSchema {
    type Src = Duration;

    fn size_of(_src: &Self::Src) -> WriteResult<usize> {
        Ok(size_of::<u64>() + size_of::<u32>())
    }

    fn write(mut writer: impl Writer, src: &Self::Src) -> WriteResult<()> {
        <u64 as SchemaWrite<C>>::write(writer.by_ref(), &src.as_secs())?;
        <u32 as SchemaWrite<C>>::write(writer.by_ref(), &src.subsec_nanos())?;
        Ok(())
    }
}

// SAFETY: Reads u64 + u32, matching the write format; dst is initialized on Ok.
unsafe impl<'de, C: ConfigCore> SchemaRead<'de, C> for DurationSchema {
    type Dst = Duration;

    fn read(
        mut reader: impl Reader<'de>,
        dst: &mut std::mem::MaybeUninit<Self::Dst>,
    ) -> ReadResult<()> {
        let secs = <u64 as SchemaRead<'de, C>>::get(&mut reader)?;
        let nanos = <u32 as SchemaRead<'de, C>>::get(&mut reader)?;
        dst.write(Duration::new(secs, nanos));
        Ok(())
    }
}

/// Cached execution result for a task.
///
/// Contains the post-run fingerprint (from fspy), captured outputs,
/// execution duration, and explicit input file hashes.
#[derive(Debug, SchemaWrite, SchemaRead, Serialize)]
pub struct CacheEntryValue {
    pub post_run_fingerprint: PostRunFingerprint,
    pub std_outputs: Arc<[StdOutput]>,
    #[wincode(with = "DurationSchema")]
    pub duration: Duration,
    /// Hashes of explicit input files computed from positive globs.
    /// Files matching negative globs are already filtered out.
    /// Path is relative to workspace root, value is `xxHash3_64` of file content.
    /// Stored in the value (not the key) so changes can be detected and reported.
    pub globbed_inputs: BTreeMap<RelativePathBuf, u64>,
    /// Filename of the output archive (e.g. `{uuid}.tar.zst`) stored alongside
    /// `cache.db` in the cache directory. `None` if no output files were produced.
    pub output_archive: Option<Str>,
}

#[derive(Debug)]
pub struct ExecutionCache {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "FingerprintMismatch contains SpawnFingerprint which is intentionally large; boxing would add unnecessary indirection for a short-lived enum"
)]
pub enum CacheMiss {
    NotFound,
    /// A malformed or incompatible local cache row was ignored.
    Unreadable,
    FingerprintMismatch(FingerprintMismatch),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum InputChangeKind {
    /// File content changed but path is the same
    ContentModified,
    /// New file or folder added
    Added,
    /// Existing file or folder removed
    Removed,
}

/// A single env var difference between a stored fingerprint and the current
/// environment.
///
/// The canonical shape for an env change wherever one is detected and
/// reported. The [`Display`] impl is the single source of the user-facing
/// wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvMismatch {
    /// Set now, but absent from the stored fingerprint.
    Added { name: Str },
    /// In the stored fingerprint, but unset now.
    Removed { name: Str },
    /// Present on both sides with different values.
    Changed { name: Str },
}

impl EnvMismatch {
    /// The name of the env var that diverged.
    #[must_use]
    pub const fn name(&self) -> &Str {
        match self {
            Self::Added { name } | Self::Removed { name } | Self::Changed { name } => name,
        }
    }

    /// Compare a stored env value against the current one, returning the
    /// mismatch if they differ. `None` on either side means the env is unset
    /// there; two unset or two equal values are not a mismatch.
    #[must_use]
    pub fn compare<T: Eq>(name: &Str, stored: Option<&T>, current: Option<&T>) -> Option<Self> {
        match (stored, current) {
            (None, Some(_)) => Some(Self::Added { name: name.clone() }),
            (Some(_), None) => Some(Self::Removed { name: name.clone() }),
            (Some(old_value), Some(new_value)) if old_value != new_value => {
                Some(Self::Changed { name: name.clone() })
            }
            _ => None,
        }
    }
}

impl Display for EnvMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added { name } => write!(f, "env '{name}' added"),
            Self::Removed { name } => write!(f, "env '{name}' removed"),
            Self::Changed { name } => write!(f, "env '{name}' changed"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum FingerprintMismatch {
    /// Found a previous cache entry key for the same task, but the spawn fingerprint differs.
    /// This happens when the command itself or an env changes.
    SpawnFingerprint {
        /// The fingerprint from the cached entry
        old: SpawnFingerprint,
        /// The fingerprint of the current execution
        new: SpawnFingerprint,
    },
    /// Found a previous cache entry key for the same task, but `input_config` differs.
    InputConfig,
    /// Found a previous cache entry key for the same task, but `output_config` differs.
    OutputConfig,

    InputChanged {
        kind: InputChangeKind,
        path: RelativePathBuf,
    },
    /// A runner-aware tool-tracked env var changed between runs.
    TrackedEnvChanged(EnvMismatch),
    /// A runner-aware tool-tracked bulk env query's match-set changed between runs.
    TrackedEnvQueryChanged {
        query: TrackedEnvQuery,
        mismatch: EnvMismatch,
    },
}

impl From<crate::session::execute::fingerprint::PostRunMismatch> for FingerprintMismatch {
    fn from(mismatch: crate::session::execute::fingerprint::PostRunMismatch) -> Self {
        use crate::session::execute::fingerprint::PostRunMismatch;
        match mismatch {
            PostRunMismatch::Input { kind, path } => Self::InputChanged { kind, path },
            PostRunMismatch::TrackedEnv(mismatch) => Self::TrackedEnvChanged(mismatch),
            PostRunMismatch::TrackedEnvQuery { query, mismatch } => {
                Self::TrackedEnvQueryChanged { query, mismatch }
            }
        }
    }
}

/// Split a relative path into `(parent_dir, filename)`.
/// Returns `None` for the parent if the path has no `/` separator.
pub fn split_path(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((parent, filename)) => (Some(parent), filename),
        None => (None, path),
    }
}

/// On-disk schema version of the cache database.
///
/// Bump this whenever the database layout (table structure, serialization
/// format, or fingerprint semantics) changes in an incompatible way.
///
/// The version is encoded *only* in the cache directory name (see
/// [`cache_schema_dir_name`], e.g. `v14`); there is no in-database version
/// marker. Keying the storage location on this version means Vite+ builds that
/// pin different schema versions never open each other's database: each keeps
/// its own cache warm across branch switches, and a cache from a different
/// version is simply ignored (it lives in a directory this build never looks
/// at) rather than aborting the run. Bumping the version starts a fresh cache.
const CACHE_SCHEMA_VERSION: u32 = 17;

/// Name of the per-version subdirectory (e.g. `v14`) under the task-cache
/// directory that holds the database and output archives for the current
/// [`CACHE_SCHEMA_VERSION`].
pub fn cache_schema_dir_name() -> Str {
    vite_str::format!("v{CACHE_SCHEMA_VERSION}")
}

impl ExecutionCache {
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn load_from_path(path: &AbsolutePath) -> anyhow::Result<Self> {
        tracing::info!("Creating task cache directory at {:?}", path);
        std::fs::create_dir_all(path)?;

        // Use file lock to prevent race conditions when multiple processes initialize the database
        let lock_path = path.join("db_open.lock");
        let lock_file = File::create(lock_path.as_path())?;
        lock_file.lock()?;

        let db_path = path.join("cache.db");
        let conn = Connection::open(db_path.as_path())?;
        // The schema version is encoded in the directory name (see
        // `cache_schema_dir_name`), so any database in this directory already has
        // the current schema: there is nothing to migrate or version-check. Set
        // WAL mode and ensure the tables exist in a single round-trip. On an
        // existing database the `IF NOT EXISTS` creates are near-free no-ops (a
        // schema lookup, no write); on a fresh one they create the tables. This
        // runs once per process (the cache is `OnceCell`-initialized).
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS cache_entries (key BLOB PRIMARY KEY, value BLOB);
             CREATE TABLE IF NOT EXISTS task_fingerprints (key BLOB PRIMARY KEY, value BLOB);",
        )?;
        // Lock is released when lock_file is dropped
        Ok(Self { conn: Mutex::new(conn) })
    }

    #[tracing::instrument]
    pub async fn save(self) -> anyhow::Result<()> {
        // do some cleanup in the future
        Ok(())
    }

    /// Try to hit cache by looking up the cache entry key and validating inputs.
    /// Returns `Ok(Ok(cache_value))` on cache hit, `Ok(Err(cache_miss))` on miss.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn try_hit(
        &self,
        cache_metadata: &CacheMetadata,
        globbed_inputs: &BTreeMap<RelativePathBuf, u64>,
        workspace_root: &AbsolutePath,
    ) -> anyhow::Result<Result<CacheEntryValue, CacheMiss>> {
        let spawn_fingerprint = &cache_metadata.spawn_fingerprint;
        let execution_cache_key = &cache_metadata.execution_cache_key;

        let cache_key = CacheEntryKey::from_metadata(cache_metadata);

        // Try to find the cache entry by key (spawn fingerprint + input config)
        let cache_value = match self.get_by_cache_key(&cache_key).await? {
            CacheRead::Found(row) => Some(row.value),
            CacheRead::Missing => None,
            CacheRead::Unreadable => return Ok(Err(CacheMiss::Unreadable)),
        };
        if let Some(cache_value) = cache_value {
            // Validate explicit globbed inputs against the stored values
            if let Some(mismatch) =
                detect_globbed_input_change(&cache_value.globbed_inputs, globbed_inputs)
            {
                return Ok(Err(CacheMiss::FingerprintMismatch(mismatch)));
            }

            // Validate post-run fingerprint (inferred inputs + tracked envs)
            if let Some(mismatch) = cache_value
                .post_run_fingerprint
                .validate(workspace_root, &cache_metadata.unfiltered_envs)?
            {
                return Ok(Err(CacheMiss::FingerprintMismatch(mismatch.into())));
            }
            // Associate the execution key to the cache entry key if not already,
            // so that next time we can find it and report what changed
            self.upsert_task_fingerprint(execution_cache_key, &cache_key).await?;
            return Ok(Ok(cache_value));
        }

        // No cache found with the current cache entry key,
        // check if execution key maps to a different cache entry key
        match self.get_cache_key_by_execution_key(execution_cache_key).await? {
            CacheRead::Found(row) => {
                let old_cache_key = row.value;
                if old_cache_key == cache_key {
                    self.discard_dangling_fingerprint(row.key_blob, row.stored_value).await;
                    return Ok(Err(CacheMiss::NotFound));
                }

                // Destructure to ensure we handle all fields when new ones are added.
                // `get_by_cache_key` above returned None for the *current* cache key,
                // so at least one field on `old_cache_key` must differ from the
                // current metadata — checked in priority order (spawn → input → output).
                let CacheEntryKey {
                    spawn_fingerprint: old_spawn_fingerprint,
                    input_config: old_input_config,
                    output_config: old_output_config,
                } = old_cache_key;
                let mismatch = if old_spawn_fingerprint != *spawn_fingerprint {
                    FingerprintMismatch::SpawnFingerprint {
                        old: old_spawn_fingerprint,
                        new: spawn_fingerprint.clone(),
                    }
                } else if old_input_config != cache_metadata.input_config {
                    FingerprintMismatch::InputConfig
                } else {
                    debug_assert_ne!(old_output_config, cache_metadata.output_config);
                    FingerprintMismatch::OutputConfig
                };
                return Ok(Err(CacheMiss::FingerprintMismatch(mismatch)));
            }
            CacheRead::Unreadable => return Ok(Err(CacheMiss::Unreadable)),
            CacheRead::Missing => {}
        }

        Ok(Err(CacheMiss::NotFound))
    }

    /// Update cache after successful execution.
    ///
    /// If a previous entry exists for the same cache key with a different
    /// `output_archive`, the stale archive file in `cache_dir` is removed
    /// (best-effort) so it doesn't accumulate on disk.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn update(
        &self,
        cache_metadata: &CacheMetadata,
        cache_value: CacheEntryValue,
        cache_dir: &AbsolutePath,
    ) -> anyhow::Result<()> {
        let execution_cache_key = &cache_metadata.execution_cache_key;

        let cache_key = CacheEntryKey::from_metadata(cache_metadata);

        // If a previous entry exists with a stale output archive, delete the
        // old file so the cache directory doesn't accumulate orphaned archives.
        if let CacheRead::Found(row) = self.get_by_cache_key(&cache_key).await?
            && let old_value = row.value
            && let Some(old_archive) = old_value.output_archive
            && cache_value.output_archive.as_ref() != Some(&old_archive)
        {
            let old_archive_path = cache_dir.join(old_archive.as_str());
            // Best-effort cleanup: a missing file (e.g. after a crash or manual
            // cache clear) is fine, so we ignore the error.
            let _ = std::fs::remove_file(old_archive_path.as_path());
        }

        self.upsert_cache_entry(&cache_key, &cache_value).await?;
        self.upsert_task_fingerprint(execution_cache_key, &cache_key).await?;
        Ok(())
    }
}

/// Compare stored and current globbed inputs, returning the first changed path.
/// Both maps are `BTreeMap` so we iterate them in sorted lockstep.
fn detect_globbed_input_change(
    stored: &BTreeMap<RelativePathBuf, u64>,
    current: &BTreeMap<RelativePathBuf, u64>,
) -> Option<FingerprintMismatch> {
    let mut stored_iter = stored.iter();
    let mut current_iter = current.iter();
    let mut s = stored_iter.next();
    let mut c = current_iter.next();

    loop {
        match (s, c) {
            (None, None) => return None,
            (Some((sp, _)), None) => {
                return Some(FingerprintMismatch::InputChanged {
                    kind: InputChangeKind::Removed,
                    path: sp.clone(),
                });
            }
            (None, Some((cp, _))) => {
                return Some(FingerprintMismatch::InputChanged {
                    kind: InputChangeKind::Added,
                    path: cp.clone(),
                });
            }
            (Some((sp, sh)), Some((cp, ch))) => match sp.cmp(cp) {
                std::cmp::Ordering::Equal => {
                    if sh != ch {
                        return Some(FingerprintMismatch::InputChanged {
                            kind: InputChangeKind::ContentModified,
                            path: sp.clone(),
                        });
                    }
                    s = stored_iter.next();
                    c = current_iter.next();
                }
                std::cmp::Ordering::Less => {
                    return Some(FingerprintMismatch::InputChanged {
                        kind: InputChangeKind::Removed,
                        path: sp.clone(),
                    });
                }
                std::cmp::Ordering::Greater => {
                    return Some(FingerprintMismatch::InputChanged {
                        kind: InputChangeKind::Added,
                        path: cp.clone(),
                    });
                }
            },
        }
    }
}

// Basic database operations
impl ExecutionCache {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock guard cannot be dropped earlier because prepared statement borrows connection"
    )]
    async fn get_key_by_value<
        K: SchemaWrite<TaskCacheConfig, Src = K>,
        V: SchemaReadOwned<TaskCacheConfig, Dst = V>,
    >(
        &self,
        table: CacheTable,
        key: &K,
    ) -> anyhow::Result<CacheRead<V>> {
        let key_blob = serialize_cache(key)?;
        let stored_value = {
            let conn = self.conn.lock().await;
            let mut select_stmt = conn.prepare_cached(table.select_sql())?;
            select_stmt.query_row::<Value, _, _>([&key_blob], |row| row.get(0)).optional()?
        };
        let Some(stored_value) = stored_value else {
            return Ok(CacheRead::Missing);
        };
        let Value::Blob(value_blob) = &stored_value else {
            let value_type = stored_value.data_type();
            self.discard_unreadable_row(table, &key_blob, &stored_value, &value_type).await;
            return Ok(CacheRead::Unreadable);
        };
        match deserialize_cache(value_blob) {
            Ok(value) => Ok(CacheRead::Found(CacheRow { value, key_blob, stored_value })),
            Err(error) => {
                self.discard_unreadable_row(table, &key_blob, &stored_value, &error).await;
                Ok(CacheRead::Unreadable)
            }
        }
    }

    async fn get_by_cache_key(
        &self,
        cache_key: &CacheEntryKey,
    ) -> anyhow::Result<CacheRead<CacheEntryValue>> {
        self.get_key_by_value(CacheTable::CacheEntries, cache_key).await
    }

    async fn get_cache_key_by_execution_key(
        &self,
        execution_cache_key: &ExecutionCacheKey,
    ) -> anyhow::Result<CacheRead<CacheEntryKey>> {
        self.get_key_by_value(CacheTable::TaskFingerprints, execution_cache_key).await
    }

    async fn delete_if_unchanged(
        &self,
        table: CacheTable,
        key_blob: &[u8],
        stored_value: &Value,
    ) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(table.delete_if_unchanged_sql(), rusqlite::params![key_blob, stored_value])
    }

    async fn discard_unreadable_row(
        &self,
        table: CacheTable,
        key_blob: &[u8],
        stored_value: &Value,
        reason: &(impl Display + Sync),
    ) {
        let value_size = match stored_value {
            Value::Blob(bytes) => Some(bytes.len()),
            Value::Null | Value::Integer(_) | Value::Real(_) | Value::Text(_) => None,
        };
        match self.delete_if_unchanged(table, key_blob, stored_value).await {
            Ok(1) => tracing::warn!(
                table = table.name(),
                key_size = key_blob.len(),
                ?value_size,
                reason = %reason,
                "Discarded unreadable task cache row"
            ),
            Ok(_) => tracing::warn!(
                table = table.name(),
                key_size = key_blob.len(),
                ?value_size,
                reason = %reason,
                "Ignored unreadable task cache row that changed concurrently"
            ),
            Err(error) => tracing::warn!(
                table = table.name(),
                key_size = key_blob.len(),
                ?value_size,
                reason = %reason,
                ?error,
                "Ignored unreadable task cache row but failed to discard it"
            ),
        }
    }

    async fn discard_dangling_fingerprint(&self, key_blob: Vec<u8>, stored_value: Value) {
        match self.delete_if_unchanged(CacheTable::TaskFingerprints, &key_blob, &stored_value).await
        {
            Ok(1) => tracing::warn!("Discarded dangling task cache fingerprint"),
            Ok(_) => {
                tracing::warn!("Ignored dangling task cache fingerprint that changed concurrently");
            }
            Err(error) => tracing::warn!(
                ?error,
                "Ignored dangling task cache fingerprint but failed to discard it"
            ),
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock guard must be held while executing the prepared statement"
    )]
    async fn upsert<
        K: SchemaWrite<TaskCacheConfig, Src = K>,
        V: SchemaWrite<TaskCacheConfig, Src = V>,
    >(
        &self,
        table: CacheTable,
        key: &K,
        value: &V,
    ) -> anyhow::Result<()> {
        let key_blob = serialize_cache(key)?;
        let value_blob = serialize_cache(value)?;
        let conn = self.conn.lock().await;
        let mut update_stmt = conn.prepare_cached(table.upsert_sql())?;
        update_stmt.execute([key_blob, value_blob])?;
        Ok(())
    }

    async fn upsert_cache_entry(
        &self,
        cache_key: &CacheEntryKey,
        cache_value: &CacheEntryValue,
    ) -> anyhow::Result<()> {
        self.upsert(CacheTable::CacheEntries, cache_key, cache_value).await
    }

    async fn upsert_task_fingerprint(
        &self,
        execution_cache_key: &ExecutionCacheKey,
        cache_entry_key: &CacheEntryKey,
    ) -> anyhow::Result<()> {
        self.upsert(CacheTable::TaskFingerprints, execution_cache_key, cache_entry_key).await
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock guard must be held while iterating over query rows"
    )]
    async fn list_table<
        K: SchemaReadOwned<TaskCacheConfig, Dst = K> + Serialize,
        V: SchemaReadOwned<TaskCacheConfig, Dst = V> + Serialize,
    >(
        &self,
        table: CacheTable,
        out: &mut impl Write,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        let mut select_stmt = conn.prepare_cached(table.list_sql())?;
        let mut rows = select_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let key_blob: Vec<u8> = row.get(0)?;
            let value_blob: Vec<u8> = row.get(1)?;
            let key: K = deserialize_cache(&key_blob)?;
            let value: V = deserialize_cache(&value_blob)?;
            writeln!(
                out,
                "{} => {}",
                serde_json::to_string_pretty(&key)?,
                serde_json::to_string_pretty(&value)?
            )?;
        }
        Ok(())
    }

    pub async fn list(&self, mut out: impl Write) -> anyhow::Result<()> {
        out.write_all(b"------- task_fingerprints -------\n")?;
        self.list_table::<ExecutionCacheKey, CacheEntryKey>(CacheTable::TaskFingerprints, &mut out)
            .await?;
        out.write_all(b"------- cache_entries -------\n")?;
        self.list_table::<CacheEntryKey, CacheEntryValue>(CacheTable::CacheEntries, &mut out)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, sync::Arc};

    use rusqlite::Connection;
    use rustc_hash::FxHashMap;
    use tempfile::TempDir;
    use vite_path::AbsolutePathBuf;
    use vite_task_graph::config::user::{EnabledCacheConfig, UserCacheConfig};
    use vite_task_plan::{plan_request::SyntheticPlanRequest, plan_synthetic};
    use wincode::{config::DEFAULT_PREALLOCATION_SIZE_LIMIT, error::WriteError};

    use super::*;
    use crate::{collections::HashMap, session::execute::fingerprint::PathFingerprint};

    fn temp_dir() -> (TempDir, AbsolutePathBuf) {
        let tmp = TempDir::new().unwrap();
        let dir = AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        (tmp, dir)
    }

    fn open_raw(db: &AbsolutePath) -> Connection {
        Connection::open(db.as_path()).unwrap()
    }

    fn synthetic_cache_metadata(workspace: &Arc<AbsolutePath>) -> CacheMetadata {
        let program = Arc::<OsStr>::from(std::env::current_exe().unwrap().into_os_string());
        plan_synthetic(
            workspace,
            workspace,
            SyntheticPlanRequest {
                program,
                args: Arc::from([]),
                cache_config: UserCacheConfig::with_config(EnabledCacheConfig {
                    env: None,
                    untracked_env: None,
                    input: None,
                    output: None,
                }),
                envs: Arc::new(FxHashMap::default()),
            },
            Arc::from([Str::from("cache-regression-test")]),
        )
        .unwrap()
        .cache_metadata
        .unwrap()
    }

    #[tokio::test]
    async fn cache_entry_larger_than_default_preallocation_limit_roundtrips() {
        let (_tmp, dir) = temp_dir();
        let cache = ExecutionCache::load_from_path(&dir).unwrap();
        let entry_size = size_of::<(RelativePathBuf, PathFingerprint)>();
        let entry_count = DEFAULT_PREALLOCATION_SIZE_LIMIT / entry_size + 1;
        let needed = entry_count * entry_size;

        let mut inferred_inputs = HashMap::with_capacity(entry_count);
        for index in 0..entry_count {
            inferred_inputs.insert(
                RelativePathBuf::new(vite_str::format!("input-{index}")).unwrap(),
                PathFingerprint::FileContentHash(index as u64),
            );
        }
        let value = CacheEntryValue {
            post_run_fingerprint: PostRunFingerprint {
                inferred_inputs,
                ..PostRunFingerprint::default()
            },
            std_outputs: Arc::from([]),
            duration: Duration::ZERO,
            globbed_inputs: BTreeMap::new(),
            output_archive: None,
        };

        assert!(needed > DEFAULT_PREALLOCATION_SIZE_LIMIT);
        assert_eq!(
            <TaskCacheConfig as ConfigCore>::PREALLOCATION_SIZE_LIMIT,
            Some(TASK_CACHE_PREALLOCATION_SIZE_LIMIT),
        );
        assert!(matches!(
            wincode::serialize(&value),
            Err(WriteError::PreallocationSizeLimit {
                needed: error_needed,
                limit: DEFAULT_PREALLOCATION_SIZE_LIMIT,
            }) if error_needed == needed
        ));

        cache.upsert(CacheTable::CacheEntries, &0_u8, &value).await.unwrap();
        let CacheRead::Found(row) = cache
            .get_key_by_value::<u8, CacheEntryValue>(CacheTable::CacheEntries, &0_u8)
            .await
            .unwrap()
        else {
            panic!("large cache entry did not round-trip");
        };
        assert_eq!(row.value.post_run_fingerprint.inferred_inputs.len(), entry_count);
        assert_eq!(
            row.value.post_run_fingerprint.inferred_inputs.get(
                &RelativePathBuf::new(vite_str::format!("input-{}", entry_count - 1)).unwrap(),
            ),
            Some(&PathFingerprint::FileContentHash((entry_count - 1) as u64)),
        );
    }

    #[tokio::test]
    async fn unreadable_cache_entry_is_a_miss_and_only_that_row_is_deleted() {
        let (_tmp, dir) = temp_dir();
        let cache = ExecutionCache::load_from_path(&dir).unwrap();
        let key = serialize_cache(&0_u8).unwrap();
        let neighbor_key = serialize_cache(&1_u8).unwrap();
        {
            let conn = cache.conn.lock().await;
            conn.execute(
                "INSERT INTO cache_entries (key, value) VALUES (?1, ?2), (?3, ?4)",
                rusqlite::params![key, vec![0xff_u8], neighbor_key, vec![0xfe_u8]],
            )
            .unwrap();
        }

        assert!(matches!(
            cache
                .get_key_by_value::<u8, CacheEntryValue>(CacheTable::CacheEntries, &0_u8)
                .await
                .unwrap(),
            CacheRead::Unreadable
        ));

        let rows: Vec<Vec<u8>> = {
            let conn = cache.conn.lock().await;
            conn.prepare("SELECT key FROM cache_entries ORDER BY key")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(rows, vec![neighbor_key]);
    }

    #[tokio::test]
    async fn unreadable_task_fingerprint_is_a_miss_and_is_deleted() {
        let (_tmp, dir) = temp_dir();
        let cache = ExecutionCache::load_from_path(&dir).unwrap();
        let key = serialize_cache(&0_u8).unwrap();
        {
            let conn = cache.conn.lock().await;
            conn.execute(
                "INSERT INTO task_fingerprints (key, value) VALUES (?1, 'not a blob')",
                [&key],
            )
            .unwrap();
        }

        assert!(matches!(
            cache
                .get_key_by_value::<u8, CacheEntryKey>(CacheTable::TaskFingerprints, &0_u8)
                .await
                .unwrap(),
            CacheRead::Unreadable
        ));
        let remaining: u32 = cache
            .conn
            .lock()
            .await
            .query_one("SELECT COUNT(*) FROM task_fingerprints", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn cleanup_does_not_delete_a_concurrently_replaced_value() {
        let (_tmp, dir) = temp_dir();
        let cache = ExecutionCache::load_from_path(&dir).unwrap();
        let key = serialize_cache(&0_u8).unwrap();
        {
            let conn = cache.conn.lock().await;
            conn.execute(
                "INSERT INTO cache_entries (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, Value::Real(1.0)],
            )
            .unwrap();
        }

        let deleted = cache
            .delete_if_unchanged(CacheTable::CacheEntries, &key, &Value::Integer(1))
            .await
            .unwrap();
        assert_eq!(deleted, 0);
        let value: Value = cache
            .conn
            .lock()
            .await
            .query_one("SELECT value FROM cache_entries WHERE key=?1", [&key], |row| row.get(0))
            .unwrap();
        assert_eq!(value, Value::Real(1.0));
    }

    #[tokio::test]
    async fn unreadable_entry_and_dangling_fingerprint_do_not_block_execution() {
        let (_tmp, dir) = temp_dir();
        let workspace = Arc::<AbsolutePath>::from(dir.clone());
        let cache = ExecutionCache::load_from_path(&dir).unwrap();
        let metadata = synthetic_cache_metadata(&workspace);
        let cache_key = CacheEntryKey::from_metadata(&metadata);
        let key_blob = serialize_cache(&cache_key).unwrap();
        {
            let conn = cache.conn.lock().await;
            conn.execute("INSERT INTO cache_entries (key, value) VALUES (?1, X'FF')", [&key_blob])
                .unwrap();
        }
        cache.upsert_task_fingerprint(&metadata.execution_cache_key, &cache_key).await.unwrap();

        assert!(matches!(
            cache.try_hit(&metadata, &BTreeMap::new(), &dir).await.unwrap(),
            Err(CacheMiss::Unreadable)
        ));
        assert!(matches!(
            cache.try_hit(&metadata, &BTreeMap::new(), &dir).await.unwrap(),
            Err(CacheMiss::NotFound)
        ));

        let conn = cache.conn.lock().await;
        let entries: u32 =
            conn.query_one("SELECT COUNT(*) FROM cache_entries", [], |row| row.get(0)).unwrap();
        let fingerprints: u32 =
            conn.query_one("SELECT COUNT(*) FROM task_fingerprints", [], |row| row.get(0)).unwrap();
        drop(conn);
        assert_eq!((entries, fingerprints), (0, 0));
    }

    /// Reopening the same cache directory keeps existing entries: the tables are
    /// created with `IF NOT EXISTS`, so a second open never wipes the database.
    #[test]
    fn reopening_preserves_existing_entries() {
        let (_tmp, dir) = temp_dir();

        drop(ExecutionCache::load_from_path(&dir).unwrap());
        {
            let conn = open_raw(&dir.join("cache.db"));
            conn.execute("INSERT INTO cache_entries (key, value) VALUES (X'01', X'02')", ())
                .unwrap();
        }

        // Reopening must not recreate or clear the tables.
        drop(ExecutionCache::load_from_path(&dir).unwrap());

        let count: u32 = open_raw(&dir.join("cache.db"))
            .query_one("SELECT COUNT(*) FROM cache_entries", (), |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Regression test for vite-plus#1785: two different schema-version
    /// directories under the same cache base are fully independent, so caches
    /// from different Vite+ versions never collide (each version reads and
    /// writes only its own directory).
    #[test]
    fn version_directories_are_isolated() {
        let (_tmp, base) = temp_dir();

        let dir_a = base.join("v13");
        let dir_b = base.join("v14");

        drop(ExecutionCache::load_from_path(&dir_a).unwrap());
        drop(ExecutionCache::load_from_path(&dir_b).unwrap());

        assert!(dir_a.join("cache.db").as_path().exists());
        assert!(dir_b.join("cache.db").as_path().exists());

        // A row written into A is invisible to B.
        {
            let conn = open_raw(&dir_a.join("cache.db"));
            conn.execute("INSERT INTO cache_entries (key, value) VALUES (X'01', X'02')", ())
                .unwrap();
        }
        let count_b: u32 = open_raw(&dir_b.join("cache.db"))
            .query_one("SELECT COUNT(*) FROM cache_entries", (), |r| r.get(0))
            .unwrap();
        assert_eq!(count_b, 0);
    }
}
