## Vite Task Execution Semantics Report

### 1. Long-running/Persistent Tasks

**Current behavior: Vite Task has NO persistent task concept.** Every task is expected to terminate (with success or failure). There is no mechanism equivalent to Turborepo's `persistent: true` flag. 

**Evidence:**
- /Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:102-158: The DAG scheduler (`execute_expanded_graph`) uses a completion-driven model. Tasks are scheduled when all dependencies complete; the scheduler waits for task completion (`futures.next().await` line 140) before proceeding.
- /Volumes/d/code/vite-task/docs/concurrency.md confirms tasks must terminate for scheduling to proceed.
- No `persistent` field exists in task configs or spawn execution structs.

**What happens with dependsOn chains where a task never exits:** Deadlock. If task A depends on task B, and B runs forever (e.g., a dev server), A will never be scheduled because the scheduler only advances after B completes (line 151-156). Sibling tasks will also be starved because the semaphore pool is exhausted by the blocked task.

**Workaround for dev servers:** Use `--parallel` flag (documented in /Volumes/d/code/vite-task/docs/concurrency.md:30-44) to ignore dependency order and remove concurrency limits. This allows multiple dev servers to run simultaneously without waiting for each other. However, this breaks the normal dependency chain semantics and is not a true "persistent task" model.

---

### 2. In-process Execution Path

**Builtin commands:** Only `echo` is implemented as in-process execution.

**Code path:** /Volumes/d/code/vite-task/crates/vite_task_plan/src/in_process.rs:55-84 defines `InProcessExecution::get_builtin_execution()` which returns `Some(...)` only for `"echo"` (with `-n` flag support). All other commands fall through to spawning.

**Consequences of in-process execution:**
- Caching is always disabled for in-process commands: /Volumes/d/code/vite-task/crates/vite_task/src/session/execute/mod.rs:218-233 reports `CacheStatus::Disabled(CacheDisabledReason::InProcessExecution)`.
- Output is captured and written synchronously without piping infrastructure.

**Shell interpreter for command strings:**
When a command string cannot be parsed as a sequence of simple commands via `try_parse_as_and_list()`:
- **Unix:** `/bin/sh -c <command>` is invoked. Hardcoded at /Volumes/d/code/vite-task/crates/vite_task_plan/src/plan.rs:349: `AbsolutePath::new("/bin/sh").unwrap().into()`
- **Windows:** `cmd.exe /d /s /c <command>` is invoked. Hardcoded at line 343-345.
- These are fallback shells only; the default parsing tries to extract a simple command first.

**Which parser handles command strings:** /Volumes/d/code/vite-task/crates/vite_shell/src/lib.rs (using `brush_parser`) parses command strings at plan time. The parser extracts the program name and arguments without executing shell metacharacters (lines 61-93 show unquoting logic that rejects parameter expansion `$`, command substitution `$()`, arithmetic `$(())`, etc.).

**Shell features supported:**
- **Simple commands:** `program arg1 arg2` ✓ (parsed and executed directly)
- **Environment variable prefixes:** `FOO=bar program args` ✓ (extracted and passed as env vars, lines 108-126)
- **Quoted arguments:** Single/double quotes with escape sequences ✓ (unquote() handles this, lines 61-93)
- **&&-chaining:** `cmd1 && cmd2 && cmd3` ✓ (split and items executed sequentially, lines 153-160)
- **$() command substitution:** ✗ (fails parse, returns None, falls back to shell)
- **$VAR env expansion:** ✗ (fails parse, returns None, falls back to shell)
- **Glob patterns:** ✗ (not parsed; would be expanded by the spawned process)
- **Pipes `|` and redirects `>`:** ✗ (not supported; would require shell)

**Parsing behavior:** /Volumes/d/code/vite-task/crates/vite_task_plan/src/plan.rs:135-405 shows the logic: if `try_parse_as_and_list()` returns `Some(parsed_subcommands)`, each subcommand is executed directly (line 137-405). Otherwise (line 339-404), the entire command string is passed to the shell. This is deterministic based on the command string syntax.

---

### 3. stdin/TTY Handling

**Do tasks get a PTY?** No. Tasks are spawned with inherited or piped stdio, but never with a PTY allocation. The codebase does not use pseudo-terminal libraries for task execution.

**Evidence:** /Volumes/d/code/vite-task/crates/vite_task/src/session/execute/spawn.rs:18-27 defines `SpawnStdio::Inherited` and `SpawnStdio::Piped`, with no PTY option. Piped I/O is drained via /Volumes/d/code/vite-task/crates/vite_task/src/session/execute/pipe.rs (referenced at line 30), which handles line buffering for output collection.

**Can interactive tasks work?** Partially:
- Tasks in `interleaved` mode with caching disabled inherit stdin, so prompts/input CAN work (see /Volumes/d/code/vite-task/docs/stdio.md:73-74 and execution/mod.rs:495-498).
- Tasks with caching enabled always get stdin redirected to `/dev/null` (line 104 in spawn.rs), breaking interactive input.
- Tasks in `labeled` or `grouped` modes always get `/dev/null` stdin (stdio.md:75-76).
- Without a PTY, full interactive features (readline history, signal handling via raw mode) will not work even if stdin is inherited.

**Stdio multiplexing with concurrency:**

Handled by the `--log` flag (docs/stdio.md:1-105):
- **`interleaved` (default):** Output streams directly to terminal as produced. Concurrent task output intermixes (lines 21-34 show example).
- **`labeled`:** Each line prefixed with `[packageName#taskName]`. Output still streams (lines 36-49).
- **`grouped`:** Output buffered per task and printed as a block after completion (lines 51-67).

**Piping mechanism:** When caching is enabled OR `--log` is not `interleaved`, stdout/stderr are piped. The `pipe_stdio()` function (execute/pipe.rs referenced at execute/mod.rs:557) drains both pipes concurrently and writes to the configured writers. For `grouped` mode, the writers buffer in memory; for `labeled`, they prefix and stream; for `interleaved` uncached, output is inherited directly.

---

### 4. Failure Semantics

**Fast-fail is the default behavior:**
- /Volumes/d/code/vite-task/src/session/execute/mod.rs:258-260: When any task exits with non-zero status, `execute_leaf` calls `fast_fail_token.cancel()`.
- Cancellation prevents semaphore acquisition (line 173-174) and drains in-flight futures (lines 141-144).
- NO flag exists to disable fast-fail (e.g., `--no-fast-fail`).

**Exit codes:** 
- /Volumes/d/code/vite-task/src/session/execute/mod.rs:252-254: `SpawnOutcome::Spawned(status)` returns the child's exit status. If any task fails, the graph execution reports failure to the reporter (line 788 in Session::execute_graph).
- /Volumes/d/code/vite-task/src/session/reporter/mod.rs (referenced at lines 39-42) likely aggregates exit statuses.

**Retries:** None. No retry mechanism exists in the execution model.

**Behavior when dependsOn fails:**
- The failing dependency cancels the fast_fail_token.
- Dependent tasks that have not yet been scheduled will fail the semaphore acquire and be skipped (execute/mod.rs:173-174).
- Dependent tasks already in flight are NOT killed retroactively; they complete normally. Only new task scheduling is prevented.
- Once a task fails, no results are cached for in-flight tasks (line 613-616 in execute/mod.rs checks `cancelled` before cache update).

---

### 5. Cancellation

**Ctrl-C handling:**
- /Volumes/d/code/vite-task/docs/cancellation.md:1-23 documents the semantics.
- The OS delivers SIGINT/CTRL_C_EVENT to the entire foreground process group (line 8-9).
- Vite Task does NOT intercept the signal; instead, running processes handle it directly.
- Vite Task's `interrupt_token` (execute/mod.rs:90-91) is cancelled by the CLI layer (not shown here but represents user action).
- When `interrupt_token.is_cancelled()`, no new tasks are scheduled (line 98), and results are not cached (line 613).

**Signal forwarding:**
- Child processes receive SIGINT directly from the OS. On Windows, CTRL_C_EVENT is sent to the job object (execute/spawn.rs:114-116 assigns child to a job).
- Vite Task does not explicitly forward signals; the OS does this.

**Grace periods:** None. When fast-fail or Ctrl-C occurs, children are killed immediately:
- On Unix: `SIGKILL` (via fspy's wait_handle monitoring the cancellation_token, spawn.rs:108).
- On Windows: `TerminateJobObject` (win_job.rs referenced at line 115).

**Orphaned child processes:** 
- On Unix with fspy disabled: If the parent crashes before `child.wait.await` (spawn.rs:172), children may become orphaned (no explicit cleanup).
- On Windows: Job Object cleanup ensures descendants are killed (line 124: `drop(job)` triggers KILL_ON_CLOSE).
- With fspy enabled: fspy's wait_handle watches the cancellation token and kills the direct child before dropping (line 123-126).

---

### 6. Scheduling

**DAG Scheduling model:**
- /Volumes/d/code/vite-task/src/session/execute/mod.rs:113-158: Uses a dependency-count approach.
- Nodes with zero dependencies are scheduled first (lines 131-135).
- As tasks complete, their dependents' counts decrement. When count reaches zero, they are scheduled (lines 150-156).
- A per-graph `Semaphore` (line 118-119) limits concurrent execution to the concurrency limit (default 4, docs/concurrency.md:3-14).

**Cache-hit behavior:**
- Cache-hit tasks execute `execute_spawn` which returns `SpawnOutcome::CacheHit` immediately (execute/mod.rs:252).
- Cache hits DO count against the concurrency limit (they acquire a semaphore permit at line 173).
- This means cache replay is serialized, not parallelized.

**Task start order determinism:**
- Initial scheduling iterates over `dep_count` entries (line 131: `&dep_count` is an FxHashMap, which is unordered).
- Subsequent scheduling follows completion order (line 150-156: uses `neighbors_directed` in graph order, which is petgraph's stable iteration).
- Result: **not deterministic** — tasks with the same dependency level may start in any order on the first level.

---

### 7. Cross-package Script Ordering with -r

**Do package.json scripts get implicit topological same-name edges?** No.

**Evidence:**
- /Volumes/d/code/vite-task/crates/vite_task_graph/src/lib.rs:378-409 shows the full task graph loading logic.
- Lines 388-404 add ONLY explicit `dependsOn` edges from task configuration.
- Package.json scripts are loaded identically to task-config tasks (lines 349-375), but no special same-name dependency edges are created for either source.
- /Volumes/d/code/vite-task/crates/vite_task_graph/src/query/mod.rs:217-253 (`add_dependencies`) only follows edges that match `filter_edge`, which is always `TaskDependencyType::is_explicit()` (line 141).
- Lines 406-408 confirm: "Topological dependency edges are no longer pre-computed here. Ordering is now handled at query time via the package subgraph..."

**How ordering happens instead:**
- /Volumes/d/code/vite-task_graph/src/query/mod.rs:151-215 (`map_subgraph_to_tasks`) uses package graph edges to order tasks.
- The package graph has edges A→B if package A depends on package B (workspace dependency graph).
- When querying a task across all packages, the package subgraph is resolved first (stage 1, line 129), then tasks are mapped from packages (stage 2, lines 133-137).
- Result: Tasks inherit the package dependency order, but there are NO implicit same-name edges.

**Consequence:** When running `vp run -r build` in a monorepo, if packageA and packageB have no direct or transitive package dependency, their `build` tasks will run in parallel (subject to the concurrency limit). There is no automatic "build all in topological order" unless explicitly specified via `dependsOn` in the task config or the package dependency graph.

**Is edge creation in query/mod.rs source-agnostic?** Yes. Lines 171-178 build a `pkg_to_task` map that looks up tasks by `TaskId { package_index, task_name }`. This works identically for tasks from `tasks` config and package.json scripts because both are indexed in `node_indices_by_task_id` (see lib.rs:374, 344 where both sources insert into this map). The task source (TaskConfig vs PackageJsonScript) does not affect querying.

---

## Summary Table

| Aspect | Behavior | File/Line |
|--------|----------|-----------|
| Persistent tasks | Not supported; `--parallel` workaround only | execute/mod.rs:113-158; docs/concurrency.md:30-44 |
| In-process builtins | Only `echo` | vite_task_plan/in_process.rs:55-84 |
| Shell fallback | `/bin/sh -c` (Unix) or `cmd.exe /d /s /c` (Windows) | plan.rs:341-354 |
| PTY allocation | None | spawn.rs:18-27 |
| Interactive stdin | Yes, only in `interleaved` mode with cache disabled | execute/mod.rs:495-498; stdio.md:73-74 |
| Fast-fail | Always enabled; no disable flag | execute/mod.rs:258-260 |
| Signal forwarding | OS-level; direct child receives SIGINT | cancellation.md:8-9 |
| Kill grace period | None (immediate SIGKILL/TerminateJobObject) | spawn.rs:108, 124 |
| Concurrency limiting | Semaphore-based; cache hits count against limit | execute/mod.rs:118-119, 173 |
| Task start order | Unordered for initially-ready tasks | execute/mod.rs:131-135 |
| Same-name implicit edges | None; only explicit `dependsOn` | lib.rs:406-408; query/mod.rs:217-253 |


## Gotchas
- No persistent task support: dev servers must use --parallel flag, which breaks normal dependency ordering and can starve other tasks if the concurrency limit is exhausted
- Fast-fail is mandatory with no disable flag: a single task failure kills all siblings and prevents scheduling dependents, unlike turborepo's --no-fail-fast
- Ctrl-C does not grant grace periods: child processes are killed immediately (SIGKILL) without allowing cleanup (SIGTERM first); running tasks are not cached even if they exit 0
- Cache-hit tasks count against concurrency limit: a burst of cache hits can block new tasks from starting, serializing otherwise-parallel work
- No implicit same-name task edges across packages: vp run -r build does not create a topological order unless the package graph has dependency edges or tasks have explicit dependsOn
- Shell invocation is a fallback only: commands must be simple (no $(), pipes, redirects) to avoid shell, but commands with any unsupported syntax silently fall back to /bin/sh -c without warning
- Interactive stdin only works uncached: tasks with cache:true always get /dev/null stdin, breaking prompts even if --log=interleaved is set
- Task start order is non-deterministic: nodes with equal dependencies may start in any order due to FxHashMap iteration
- No signal handling customization: both SIGINT and task failure use the same fast-fail token and immediate kill; no way to intercept or log signal-related cancellations separately