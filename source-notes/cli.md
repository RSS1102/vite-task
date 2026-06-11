## Complete CLI Surface of vp run / vite_task

### 1. FLAGS AND ENVIRONMENT VARIABLES

#### CLI Flags (crates/vite_task/src/cli/mod.rs)

**Package Selection Flags** (flattened from PackageQueryArgs at line 33):
- `-r, --recursive`: Select all packages in the workspace (false by default)
- `-t, --transitive`: Select the current package and its transitive dependencies (false by default)
- `-w, --workspace-root`: Select the workspace root package (false by default)
- `-F, --filter <PATTERN>`: Match packages by name, directory, or glob pattern (multiple allowed; line 264-277)
- `--fail-if-no-match`: Exit with non-zero status if a filter expression matches no packages (false by default; line 285)

**Task Execution Flags** (RunFlags struct, lines 31-62):
- `--ignore-depends-on`: Do not run dependencies specified in `dependsOn` fields (line 36-37)
- `-v, --verbose`: Show full detailed summary after execution (line 40-41)
- `--cache`: Force caching on for all tasks and scripts (line 44-45)
- `--no-cache`: Force caching off for all tasks and scripts (line 48-49; conflicts with --cache)
- `--log <MODE>`: How task output is displayed (line 52-53, default: "interleaved")
  - Valid values: `interleaved`, `labeled`, `grouped` (crate::cli::LogMode enum, lines 12-19)
- `--concurrency-limit <N>`: Maximum number of tasks to run concurrently (line 56-57; defaults to 4 if not specified; see DEFAULT_CONCURRENCY_LIMIT at crates/vite_task_plan/src/execution_graph.rs:165)
- `--parallel`: Run tasks without dependency ordering; sets concurrency to unlimited unless --concurrency-limit is also specified (line 61-62)
- `--last-details`: Display the saved detailed summary of the last run (line 101-102; exclusive with task execution)

**Task Specifier and Additional Args** (lines 104-109):
- Positional `TASK_SPECIFIER`: `packageName#taskName` or just `taskName` (or omitted to show task selector)
- `ADDITIONAL_ARGS`: Any arguments after the task name are forwarded to the task process verbatim (trailing_var_arg=true prevents flag interception)

#### Environment Variables:
- `VITE_CACHE_PATH`: Override default cache location (crates/vite_task/src/session/mod.rs:176); defaults to `node_modules/.vite/task-cache` relative to workspace root
- `NO_COLOR` / `FORCE_COLOR`: Control ANSI color output (detected via `supports_color::Stream` crate; see crates/vite_task/src/session/mod.rs:841-854)

No VP_RUN_* prefixed environment variables detected in codebase.

#### Cache Subcommand:
- `vp cache clean`: Clean up all the cache (crates/vite_task/src/cli/mod.rs:23-25)

---

### 2. ADVANCED FEATURES

**Watch Mode**: NO. No watch/continuous mode detected. Codebase contains only one-shot execution.

**Dry-Run**: NO. No dry-run or plan-preview flag in RunFlags. Tasks are executed directly.

**Task Graph Print/Visualize**: NO. No graph visualization, tree printing, or DAG export command detected.

**List Tasks Mode**: IMPLICIT. Running `vp run` without a task name shows an interactive task selector (lines 426-579 of crates/vite_task/src/session/mod.rs); in non-TTY mode, prints the flat task list and exits with SUCCESS (or FAILURE if a task name was provided but not found; lines 533-542).

**Git-Based Change Detection (--affected equivalent)**: NO. No `--affected`, `--since`, `--until`, or git-based filtering detected. No integration with git refs in filter parsing.

---

### 3. FILTER GRAMMAR (--filter)

**Complete Syntax** (crates/vite_workspace/src/package_filter.rs, module docs lines 9-30, and parsing logic lines 460-618):

The filter syntax follows pnpm's `--filter` specification:

**Name/Glob Selectors** (lines 13-14):
- `foo` → exact package name match
- `@scope/*` → glob pattern (supports `*` and `?` wildcards only, pnpm semantics)

**Directory Selectors** (lines 15-16):
- `./path` → packages whose root is at or under this directory
- `{./path}` → same, brace syntax (allows traversal suffixes)
- `.` → current directory (exact match)

**Compound Selectors** (lines 17):
- `name{./dir}` → name AND directory intersection (both must match)

**Graph Traversal Suffixes** (lines 18-22):
- `foo...` → foo + its transitive dependencies (line 18)
- `...foo` → foo + its transitive dependents (line 19)
- `foo^...` → foo's dependencies only (exclude foo itself; line 20)
- `...^foo` → foo's dependents only (exclude foo itself; line 21)
- `...foo...` → foo + dependencies + dependents (line 22)

**Exclusion** (line 23):
- `!foo` → exclude packages matching the filter from results

**Note on Path Traversal**: Unbraced dot-prefix selectors (e.g., `./path`) do NOT support `...` traversal (pnpm ambiguity with `..` parent dir; line 504-507). Braced paths (`{./path}`) support traversal.

**Whitespace Handling**: Multiple filters can be combined using whitespace within a single `--filter` value, or by using multiple `--filter` arguments (line 313-328).

**Multiple Filters Composition** (line 175-195): Inclusions are unioned, exclusions are subtracted at the PackageQuery level.

---

### 4. REMOTE/SHARED CACHE

**Remote Cache Upload/Download**: NO. No code for remote cache upload, download, cache import/export, or CI cache priming detected.

**Local Cache Only**: ExecutionCache (crates/vite_task/src/session/cache/mod.rs:118-120) uses only local SQLite database and archives stored in `VITE_CACHE_PATH` (default: `node_modules/.vite/task-cache`).

**Cache Subcommands**: Only `cache clean` is supported (crates/vite_task/src/cli/mod.rs:23-25).

---

### 5. ADDITIONAL ARGS DISTRIBUTION

**Semantics** (crates/vite_task_plan/src/plan.rs:745-813):

Only the explicitly requested tasks (matched by the query) receive additional args (extra_args). Tasks reached solely via `dependsOn` expansion receive an empty arg slice (line 806-807).

**Distribution When Multiple Packages Selected**:
- All explicitly queried tasks in all selected packages receive the same extra_args
- Dependency-pulled tasks receive empty args

**Combination with --parallel**:
- Extra args are passed to all explicitly queried tasks regardless of execution order (serial vs. parallel does not affect arg distribution)

---

### 6. NON-TTY / CI BEHAVIOR

**TTY Detection** (crates/vite_task/src/session/mod.rs:330-356):
- Interactive mode is enabled when both stdin AND stdout are terminals (line 330-331: `std::io::stdin().is_terminal() && std::io::stdout().is_terminal()`)
- Non-interactive mode: task selector prints flat list; no fuzzy search

**Reporter Selection** (lines 332-356):
- Default reporter: `InterleavedReporterBuilder` (line 343)
- User-selected via `--log` flag (line 341-356):
  - `Interleaved`: streams output directly as tasks produce it (line 343)
  - `Labeled`: prefixes each line with `[packageName#taskName]` (line 348)
  - `Grouped`: buffers output per task, prints as block after task completes (line 352)

All three reporters are wrapped with `SummaryReporterBuilder` (line 359) which adds summary tracking and final summary output.

**Color Support Detection** (lines 841-854):
- Uses `supports_color::on(Stream::Stdout)` and `supports_color::Stream::Stderr`
- Honors `NO_COLOR` and `FORCE_COLOR` environment variables
- Cached per process lifetime
- Per-stream detection: non-TTY stdout does not strip colors from TTY stderr (line 334-335)

**Output Verbosity Control**:
- `-v, --verbose` flag: shows full detailed summary (line 40-41, passed to SummaryReporterBuilder line 363)
- Without `-v`: compact summary displayed (crates/vite_task/src/session/reporter/summary_reporter.rs:134-137)

**Summary Persistence**:
- Last run summary saved to `last-summary.json` via `WriteSummaryFn` callback (crates/vite_task/src/session/reporter/summary_reporter.rs:25-26, 140-141)

---

### 7. EXIT CODE SEMANTICS AND FAST-FAIL

**Exit Code Mapping** (crates/vite_task/src/session/reporter/summary_reporter.rs:104-125):

1. **No failures, no infra errors**: exit 0 (SUCCESS)
2. **Single task failed**: exit with that task's exit code clamped to 1-255 (line 122: `exit_code.clamp(1, 255)`)
3. **Multiple tasks failed OR any infra error**: exit 1 (FAILURE)

**Task-Level Exit Codes**:
- 0 = success
- non-zero = failure
- Cached hits bypass execution; no exit code generated (SpawnOutcome::CacheHit; line 107)

**Infra Errors** (tasks with error != None):
- Cache lookup/update failures
- Fingerprint computation errors
- Post-run tracking errors
Result: exit 1 regardless of individual task exit codes (line 124)

**Fast-Fail Behavior** (crates/vite_task/src/session/execute/mod.rs:782-784):
- When any task fails (non-zero exit status), `fast_fail_token.cancel()` is called (line 259)
- Cancelled tasks: remaining queued tasks are skipped; in-flight tasks complete but their results are marked as Cancelled (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::Cancelled); line 616)
- Cancelled tasks do NOT contribute to the exit code (line 107: only SpawnOutcome::Failed contributes)

**Ctrl-C Interrupt** (crates/vite_task/src/session/mod.rs:390-394):
- Separate `interrupt_token` distinct from `fast_fail_token`
- On Ctrl-C, interrupt_token is cancelled; task runner halts scheduling new tasks
- In-flight task processes receive SIGTERM (via `kill(-pid, SIGTERM)` on Unix) or are terminated on Windows
- Exit code determined by last task before cancellation (same logic as fast-fail)

---

### 8. DETAILS AND EDGE CASES

**Task Specifier Format** (crates/vite_task/src/cli/mod.rs:107):
- `packageName#taskName` or `taskName` (scoped package names like `@scope/pkg#task` are supported; pnpm-compatible)
- Parser: TaskSpecifier::parse_raw (vite_task_graph crate)

**Implicit CWD Selection** (crates/vite_workspace/src/package_filter.rs:428-436):
- When no flags, filters, or explicit package name provided: the package containing the current working directory is selected
- is_cwd_only=true returned to signal implicit cwd fallback (line 436)

**Workspace Root as Fallback** (crates/vite_workspace/src/package_filter.rs:372-390):
- `-w` (workspace-root) can be combined with `--transitive` to select root and all dependencies

**Scoped Auto-Completion** (crates/vite_workspace/src/package_filter.rs:79-84):
- If exact name "bar" has no match but exactly one "@*/bar" package exists, that package is matched automatically (pnpm semantics)

**Concurrency Defaults** (crates/vite_task_plan/src/plan.rs:729, 737; crates/vite_task_plan/src/execution_graph.rs:165):
- Default: 4
- `--parallel` sets unlimited concurrency (unless `--concurrency-limit` also specified)
- Configured via PlanOptions.concurrency_limit or ExecutionGraph.concurrency_limit

**Cache Configuration Precedence** (crates/vite_task_plan/src/plan.rs:749-752):
1. Explicit `--cache` / `--no-cache` flags
2. vite-task.json / vite.config.ts per-task cache configuration
3. User cache configuration
4. Defaults (caching enabled unless task explicitly disables it)

**Dependency Handling** (crates/vite_task/src/cli/mod.rs:35-37):
- `--ignore-depends-on`: skips tasks listed in the task's `dependsOn` field
- Tasks are still executed in dependency order if they appear in the query result directly

**Extra Args Only on Queried Tasks** (crates/vite_task_plan/src/plan.rs:802-808):
- If `vp run build --foo` is run with `-r` (recursive), ALL packages' build tasks receive `--foo`
- If only specific packages match, ALL those packages' tasks receive `--foo`
- But dependency-only tasks (pulled in via dependsOn) receive empty args

## Gotchas
- Trailing arguments after task name are passed verbatim to tasks; vp flags must appear BEFORE the task name (trailing_var_arg=true enforces this per clap issue #285)
- Unbraced directory selectors like './path' do NOT support traversal suffixes (...); use braces {./path} if you need traversal
- Extra args are distributed only to explicitly queried tasks, NOT to their dependents via dependsOn; this prevents CLI args from polluting dependency tasks
- When multiple tasks fail, exit code is always 1 (not the first or last exit code); only single-failure-per-run scenarios preserve the original exit code
- Cancelled tasks (due to sibling failure or Ctrl-C) do not update the cache and do not contribute to exit code determination
- Interactive task selection only works when both stdin and stdout are TTYs; in CI with redirected streams, a flat task list is printed instead
- Color detection is per-stream (stdout vs stderr) to allow piping one while keeping colors on the other; NO_COLOR and FORCE_COLOR are respected globally
- Remote/shared cache does not exist; cache is entirely local to VITE_CACHE_PATH (typically node_modules/.vite/task-cache)
- No watch mode, dry-run, graph visualization, or git-based affected filtering; vp run is single-shot execution only
- The --parallel flag sets unlimited concurrency unless --concurrency-limit is also specified; default concurrency without flags is 4