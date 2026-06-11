**END-TO-END CACHE INTERNALS MAPPING**

**1. FINGERPRINT COMPOSITION**

The cache key structure is defined in `/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:42-52` as `CacheEntryKey` containing:
- `spawn_fingerprint: SpawnFingerprint` - The primary cache key component
- `input_config: ResolvedGlobConfig` - Input glob patterns and auto-inference flag
- `output_config: ResolvedGlobConfig` - Output glob patterns for archive restoration

**Spawn Fingerprint Composition** (`/Volumes/d/code/vite-task/crates/vite_task_plan/src/cache_metadata.rs:76-82`):
- `cwd: RelativePathBuf` - Workspace-relative working directory
- `program_fingerprint: ProgramFingerprint` - Either `OutsideWorkspace { program_name }` (e.g., "node", "npm") or `InsideWorkspace { relative_program_path }` 
- `args: Arc<[Str]>` - Command arguments, including extra args appended to the last `&&`-separated command item
- `env_fingerprints: EnvFingerprints` - Fingerprinted and untracked environment variables

**Environment Handling** (`/Volumes/d/code/vite-task/crates/vite_task_plan/src/envs.rs:47-59`):
- `fingerprinted_envs: BTreeMap<Str, Arc<str>>` - Explicitly declared env vars; values are hashed if they match sensitive patterns (e.g., `*_KEY`, `*_TOKEN`, `AWS_*`). Hash uses SHA256 with "sha256:" prefix.
- `untracked_env_config: Arc<[Str]>` - Names of env vars that are passed through but values NOT fingerprinted (e.g., PATH, CI). Names themselves ARE included in fingerprint so changes to which env vars are untracked invalidates cache.
- `FORCE_COLOR=1` is auto-inserted as fallback if no one else set it (for consistent terminal coloring)

**Cache Entry Value** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:101-115`):
- `post_run_fingerprint: PostRunFingerprint` - Path fingerprints from fspy inferred reads
- `std_outputs: Arc<[StdOutput]>` - Captured stdout/stderr for replay, coalesced by stream kind
- `duration: Duration` - Execution time (for replay feedback)
- `globbed_inputs: BTreeMap<RelativePathBuf, u64>` - Workspace-relative paths → xxHash3_64 of file content, from positive globs (negative-filtered)
- `output_archive: Option<Str>` - UUID-named tar.zst archive file name (e.g., "{uuid}.tar.zst")

**Hit-Time Validation** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:249-310`):
1. Lookup by `CacheEntryKey` (spawn fingerprint + input/output config)
2. Validate explicit globbed inputs: compare stored xxHash3_64 against current file hashes. If any missing/added/modified → `InputChanged { kind, path }`
3. Validate inferred inputs (fspy-tracked, from `post_run_fingerprint`): re-fingerprint each tracked path, detect added/removed/modified entries in directories
4. If both lookups miss, check `task_fingerprints` table to find prior cache key; report which field changed (spawn, input_config, or output_config)

---

**2. FSPY MECHANICS**

**macOS Implementation** (`/Volumes/d/code/vite-task/crates/fspy/src/unix/mod.rs`):
- **Preload library approach**: LD_PRELOAD-style library (`fspy_preload_unix`) compiled as cdylib, materialized to disk via `materialized_artifact` crate at runtime (content-addressed filename)
- Before spawn, `ExecResolveConfig::search_path_enabled(None)` resolves program from PATH, records accesses during exec phase
- Payload passed via environment variable, preload library hooks libc calls to capture path accesses
- Also requires Coreutils binary (cp, rm, etc.) and Oils shell binary (bash alternative) materialized to disk

**Linux Implementation** (`/Volumes/d/code/vite-task/crates/fspy/src/unix/mod.rs` with `/Volumes/d/code/vite-task/crates/fspy_seccomp_unotify`):
- **seccomp_unotify supervisor model**: Installs seccomp filter with user-mode notification (`seccomp(..., SECCOMP_FILTER_FLAG_NEW_LISTENER)`)
- Supervisor runs in separate task (`fspy_seccomp_unotify::supervisor::supervise::<SyscallHandler>()`)
- Child process blocked on watched syscalls; supervisor unblocks after recording access
- On musl builds (no preload), seccomp alone handles tracking

**Tracked Syscalls** (`/Volumes/d/code/vite-task/crates/fspy/src/unix/syscall_handler/mod.rs:81-103`):
- `open`, `openat`, `openat2` - File opens (flags determine READ/WRITE mode)
- `stat`, `lstat`, `fstatat`, `statx` - Stat calls
- `access`, `faccessat`, `faccessat2` - Permission checks
- `getdents`, `getdents64` - Directory listing
- `execve`, `execveat` - Program execution (resolved in parent context)

Path accesses are recorded as `PathAccess { mode: AccessMode, path: OsStr }` where AccessMode flags:
- `READ` (file read)
- `WRITE` (file write, flags include O_WRONLY or O_RDWR)
- `READ_DIR` (opendir/getdents)

**Access Mode Determination** (`/Volumes/d/code/vite-task/crates/fspy/src/unix/syscall_handler/open.rs` logic):
- From open flags: `O_ACCMODE` extracts access bits → `O_RDWR` = READ|WRITE, `O_WRONLY` = WRITE, else READ
- Relative paths resolved using caller's fd context (via `/proc/{pid}/fd/{fd}`)

**Post-Execution Normalization** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/tracked_accesses.rs:26-67`):
1. Strip workspace root prefix
2. Normalize `..` components (e.g., `packages/sub/../shared/dist/out.js` → `packages/shared/dist/out.js`)
3. Skip `.git` directory accesses (workaround for tools like oxlint)
4. Filter against negative glob patterns
5. Coalesce by path: if path appears in multiple accesses, set flags (read_dir_entries = true if any READ_DIR mode)

**FspyUnsupported Condition** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:630-634`):
- Task requests fspy auto-inference (`input_config.includes_auto == true`)
- Binary built with `cfg(not(fspy))` (e.g., cross-compiled to musl or Windows)
- Task executed successfully but no path accesses available → Cache entry cannot be created soundly

**Known Limitations**:
- **Network access**: Not tracked (system calls made by kernel, not accessible to fspy)
- **Child processes**: Only direct syscalls tracked. Shell scripts spawning subprocesses: preload+IPC tracks shell's calls, but subprocesses (e.g., node via .cmd shim) not fully tracked unless explicitly supervised. Windows Job Object used to kill descendants but does not track their accesses.
- **File watching**: Not tracked (inotify/kqueue not hooked)
- **mmap access**: Reads via memory-mapped files not captured (kernel handles page faults)
- **Statically-linked binaries**: On Linux, seccomp-unotify path works; on macOS preload fails (no libc hooks). Tests at `/Volumes/d/code/vite-task/crates/fspy/tests/static_executable.rs` verify seccomp path.
- **Scripts reading env vars in build process**: Env-influenced outputs aren't automatically tracked; only env vars explicitly declared in task config affect cache key
- **Nondeterministic output**: fspy cannot detect; tasks producing different output for identical inputs will still cache

---

**3. OUTPUT ARCHIVING**

**Archive Creation** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:713-740`):
1. Collect output files matching positive globs (filtered by negative globs) via `glob::collect_glob_paths()`
2. Files not found during glob walk are skipped (task may delete temp files between glob and archive)
3. Create tar.zst archive with UUID filename (e.g., `{uuid}.tar.zst`) in cache directory (`node_modules/.vite/task-cache`)
4. Archive stored only if positive globs exist AND files match

**Archive Extraction** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:408-437`):
- On cache hit, extract tar.zst archive to workspace root
- Parent directories created automatically
- **Existing files overwritten** (no skip, no merge)
- Archive missing/corrupted → Surface recovery instruction: "Run `vp cache clean` to clear cache"

**Stale Output Cleanup** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:318-342`):
- When updating cache entry with new execution, check if previous entry exists with different `output_archive` filename
- If old archive name differs from new: best-effort delete old file from cache directory
- Missing old file is silently ignored (e.g., after crash or manual clear)

**Symlinks Handling** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/archive.rs:33-39`):
- Only regular files included in archive (`metadata.is_file()` check)
- Symlinks skipped during archiving

---

**4. CACHE STORAGE & CONCURRENCY**

**Cache Location**: `node_modules/.vite/task-cache`
- Created at `/Volumes/d/code/vite-task/crates/vite_task/src/session/mod.rs` (grep: `workspace_root.join("node_modules/.vite/task-cache")`)
- Contains: `cache.db` (SQLite with WAL mode), `db_open.lock`, output archive files ({uuid}.tar.zst)

**SQLite Schema** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:205-234`):
- `cache_entries`: key=CacheEntryKey (BLOB), value=CacheEntryValue (BLOB)
- `task_fingerprints`: key=ExecutionCacheKey (BLOB), value=CacheEntryKey (BLOB)
- Schema version 13; versions 1-12 trigger full reset; 14+ rejected with upgrade instruction
- PRAGMA `journal_mode=WAL` for write concurrency

**Concurrency Safety**:
- File-level lock: `db_open.lock` created before opening connection (line 199-200). Lock held across DB operations but released when file is dropped.
- Multiple `vp run` processes can open the DB simultaneously (WAL mode allows readers + one writer)
- Race condition during cache.db initialization: first process to acquire `db_open.lock` initializes; others wait on lock, re-check version on acquisition
- No application-level locking for individual cache entry updates → Two simultaneous executions of same task may both update cache; SQLite's `INSERT ... ON CONFLICT(key) DO UPDATE` serializes writes, last writer wins

**Size Limits & Eviction**:
- No built-in eviction; cache grows unbounded (archive files accumulate)
- Stale archives cleaned only if same cache key is executed again with different output
- User cleanup: `vp cache clean` command (not detailed in source, but referenced in error messages)

---

**5. READ-WRITE OVERLAP DETECTION**

**Exact Rule** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:618-629`):
- After successful execution, extract read/write sets from fspy (`TrackedPathAccesses`)
- Check if any path appears in BOTH `path_reads` AND `path_writes`
- If overlap found: skip cache update, report `InputModified { path }`, exit normally (no error)
- Only applies to **inferred inputs** (fspy-tracked); explicit glob inputs are NOT checked against writes

**Why**: Prerun glob hashing cannot detect modifications task made to its own inputs. If task reads file A at time T1 (hash X), then writes file A at time T2, cache entry stores old hash X. Next run: file A has new content (from previous task write), but prerun hash is still X → miss misattributed to input change, not to old code running on new input → cache correctness violated.

**Workarounds**:
1. **Negative patterns**: Exclude files task both reads and writes from fspy tracking (but not from glob inputs):
   ```json
   { "inputs": [{ "auto": true }, "!**/*.tsbuildinfo"] }
   ```
   Excludes `*.tsbuildinfo` from fspy tracking, so overlap not detected, but task still detects file absence on cache hit. Risk: if task modifies the file in a way cache doesn't restore correctly, misses will cascade.

2. **Explicit `inputs` only (no auto)**: Define explicit globs for true inputs, avoid fspy entirely:
   ```json
   { "inputs": ["src/**/*.ts", "tsconfig.json"] }
   ```
   Task can write freely; cache updated only if explicit inputs change.

3. **Cache disable**: Set `cache: false` on task if read-write overlap is unavoidable and caching creates correctness issues.

**Files in node_modules or $HOME**: 
- Task writes to `.next/cache` or eslint `--cache`: fspy will record WRITE access. If task also reads same file → overlap detected. Cache skipped.
- Task writes to `$HOME/.npm-cache` or similar: similar behavior if task reads from there.
- Mitigation: negative patterns to exclude these from tracking, or explicit inputs only.

---

**6. TERMINAL OUTPUT REPLAY**

**Capture Format** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/pipe.rs:20-34`):
- Raw bytes captured (not decoded to UTF-8), organized by stream kind:
```rust
pub struct StdOutput {
    pub kind: OutputKind,  // StdOut or StdErr
    pub content: Vec<u8>,  // Raw bytes
}
```
- Adjacent chunks of same kind coalesced during drain (line 96-104)

**Drain Process** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/pipe.rs:47-94`):
- Concurrent read loop on stdout/stderr with 8192-byte buffers
- Bytes immediately written to user-facing writers (live output)
- If capture enabled (cached execution), also appended to `Vec<StdOutput>`, with adjacent same-kind chunks merged
- Cancellation: return Ok without killing child (caller's wait future handles kill)

**Replay** (`/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:408-416`):
- On cache hit, iterate stored `std_outputs`
- Write each chunk to corresponding writer (stdout_writer or stderr_writer)
- Write + flush per chunk (may be buffered, but flush called)

**ANSI Handling**:
- Raw bytes preserved during capture (no ANSI stripping at capture time)
- Reporter (grouped/labeled/summary mode) may strip ANSI when displaying, but cached bytes are unchanged
- Replay outputs ANSI codes as-is; terminal decides color support

**Large Outputs**:
- No truncation or size limit on captured output
- Stored in `Arc<[StdOutput]>` (heap-allocated, variable-length)
- Serialized with `wincode` binary codec (efficient)
- Very large outputs increase cache entry value size; no practical limit enforced, but disk usage grows

---

**7. TOOLS THAT ARE UNCACHEABLE OR FLAKY-CACHED**

**Jest/Vitest Workers**:
- Worker processes spawn child processes that fspy may not fully track (platform-dependent; seccomp-unotify on Linux tracks syscalls, but macOS preload in worker child may not intercept all)
- Output interleaving between workers → cached output captured sequentially, but live output concurrent → test output ordering differs between cached and non-cached runs
- Workaround: Negative patterns to exclude Jest cache dirs; explicit inputs only for test source files

**esbuild/SWC Daemons**:
- Daemon process persists between builds; accesses served from daemon's cache, not tracked by fspy if daemon is outside workspace
- Cache entries reflect "empty" inputs if daemon served the build
- Workaround: Disable daemons (`--no-bundle` for esbuild, etc.) or use explicit inputs for known source files

**Next.js Telemetry**:
- Next.js may make network requests for telemetry (not tracked by fspy)
- Output deterministic, but cache key doesn't reflect telemetry disabled → two runs with different network state produce same cached output (acceptable if telemetry output not in stdout/stderr)
- Workaround: Set `NEXT_TELEMETRY_DISABLED=1` in task env

**Timestamps in Output**:
- Commands like `echo "Build at $(date)"` produce different outputs each run
- fspy tracks read of system time (clock_gettime via libc), but timestamp value not hashed into cache key
- Cached output replays with old timestamp → Output differs
- Workaround: Use explicit inputs only and ensure task command is deterministic; or use negative patterns to exclude timestamp-generating tools from tracking

**Version Checks**:
- Tools may read version files (package.json, .nvmrc) and include version in output
- fspy tracks the reads; if version file unchanged, cache hits and replays old version string
- Acceptable if version remains stable; breaks if tool upgraded

**Incremental Builds (TypeScript, Webpack)**:
- `.tsbuildinfo` files: TypeScript reads and writes incrementally
- Read-write overlap detected if same file both read and written
- Workaround: Negative pattern `!**/*.tsbuildinfo` to exclude from fspy (cache still updates on source change)
- Webpack cache: Similar; webpack stores cache in dist, reads from it next run → overlap if webpack caches in same dir as outputs

---

**SUMMARY OF KEY CITATIONS**:
- Cache key & entry: `/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:42-115`
- Spawn fingerprint: `/Volumes/d/code/vite-task/crates/vite_task_plan/src/cache_metadata.rs:76-82`
- Environment handling: `/Volumes/d/code/vite-task/crates/vite_task_plan/src/envs.rs:47-175`
- Hit validation: `/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:249-310`
- fspy macOS/Linux: `/Volumes/d/code/vite-task/crates/fspy/src/unix/mod.rs`, `/Volumes/d/code/vite-task/crates/fspy/src/unix/syscall_handler/mod.rs:81-103`
- Output archiving: `/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:713-740` and `/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/archive.rs`
- Read-write overlap: `/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:599-629`
- Output capture/replay: `/Volumes/d/code/vite-task/crates/vite_task/src/session/execute/pipe.rs`
- Env filtering: `/Volumes/d/code/vite-task/crates/vite_task_plan/src/envs.rs:84-156`
- Database: `/Volumes/d/code/vite-task/crates/vite_task/src/session/cache/mod.rs:191-238`


## Gotchas
- Cache key includes extra_args appended to the LAST &&-separated command only; other && items do not receive extra args, but they still appear in ExecutionCacheKey so parallel runs with different extra_args have separate cache entries.
- Environment variables: ONLY explicitly declared fingerprinted envs + untracked env names (not values) affect the cache key. Untracked env NAMES are fingerprinted; changes to which envs are untracked invalidate cache. Changes to untracked env VALUES do NOT invalidate cache even though all values are still passed to the child process.
- Read-write overlap detection ONLY applies to fspy-inferred inputs, NOT explicit glob inputs. A task that reads glob-matched file A and then writes to it will produce cache misses forever (not a correctness bug, but perpetual misses); only inferred reads trigger the InputModified rejection.
- Program fingerprint: if program is INSIDE workspace (relative path), it's fingerprinted by that path. If the file is modified or moved, cache misses. But if program is outside workspace (like 'node' from PATH), only the program name is fingerprinted; installing a different Node version does NOT invalidate cache automatically.
- Archive extraction OVERWRITES existing files without checking; if task output overlaps with source files, cache hit restores old output on top of current source (risk if task output is meant to be regenerated each run).
- Output archives only created if output_config.positive_globs is non-empty AND files match. If task has outputs but no output globs configured, outputs are NOT restored on cache hit (cache hit only replays stdout/stderr).
- FspyUnsupported happens only when binary compiled without cfg(fspy) (musl, Windows cross-compile, etc.) AND task requests auto-inference (inputs includes {auto: true} or omitted). On such builds, tasks with explicit inputs only are cacheable, but auto-inference tasks are not.
- Symlinks in output archives: skipped entirely (only regular files included). If task outputs a symlink, it won't be restored.
- Cancellation (Ctrl-C): fast-fail token (task sibling failure) kills child; interrupt token (Ctrl-C) does NOT kill child, only prevents caching and scheduling new tasks. Child left to handle SIGINT naturally.
- Database concurrency: WAL mode allows reader + writer concurrency, but two simultaneous executions of the same task may both try to update the cache. SQLite ON CONFLICT serializes writes; last writer wins. No application-level optimistic locking.
- Negative patterns filter BOTH explicit globs AND fspy inferred accesses. Using negative patterns to exclude files from fspy tracking is a workaround for read-write overlap, but those files are still written by the task and must be restored on cache hit via archive or assumed not needed.
- If a task requires fspy auto-inference but the binary is built without cfg(fspy), and the task succeeds, cache is NOT updated (FspyUnsupported status). Same task on a binary WITH cfg(fspy) will cache normally. This can cause inconsistent caching across deployments.
- FORCE_COLOR=1 is auto-inserted if no fingerprinted/untracked env explicitly set FORCE_COLOR. This ensures cached output is colored by default, but if task explicitly opts into FORCE_COLOR via env config, user's choice (even FORCE_COLOR=0) wins.
- Extra args are part of ExecutionCacheKey (for UserTask) and appended only to the LAST &&-separated command. Different extra args → different cache entries. This prevents 'vp build' and 'vp build --prod' from sharing cache.