# Vite Task's tree-like task model (`&&` sub-tasks + in-process `vp run` expansion)

Vite Task has two composition models, and good migrations use both:

1. **Graph model** — per-package `run.tasks` with `dependsOn`, plus implicit topological edges between same-named tasks of selected packages (from `workspace:` package deps).
2. **Tree model** — within ONE task/script, the command string is parsed and expanded into a sequential list of items, where each item can itself be a whole nested task graph.

## Mechanics (source-verified)

- A task's command is split on top-level `&&` into `TaskExecution.items: Vec<ExecutionItem>` (vite_task_plan/src/lib.rs:78). Items run **sequentially**; a failing item stops the chain.
- Each item is either:
  - `Leaf(Spawn)` — a process spawn (`tsc`, `dotenv -e .env -- next build`, …),
  - `Leaf(InProcess)` — built-ins (`echo` only today; uncacheable),
  - `Expanded(ExecutionGraph)` — when the item is a `vp run …` / `vpr …` / `vt run …` command, the planner does NOT spawn a child vp; it parses the args (clap), resolves the task query, and **inlines the resulting execution graph** (vite_task_plan/src/plan.rs:232+). This recurses, so trees nest arbitrarily.
- Parsing is conservative: only AND-lists of simple commands split. `;`, `||`, pipes, `$()`, `$VAR` make the whole string a single `/bin/sh -c` leaf (still cacheable as one unit). Env prefixes (`FOO=1 cmd`) are extracted and applied; quoted args are unquoted properly.
- **`cd` is a builtin item**: `cd packages/a && vp run build` adjusts the cwd for subsequent items (plan.rs:154-170) — and the changed cwd changes which package a nested `vp run` query resolves to (plan.rs:241 comment).
- **Each leaf item is cached independently** with key `ExecutionCacheKey::UserTask { task_name, command_item_index, and_item_index, extra_args, package_path }` + the spawn fingerprint. So `"check": "vp lint && vp build"` has two cache entries; if only lint-relevant files changed, build still hits. Cache entries are content-based and therefore **shared across tasks** that run an identical command with identical inputs.
- Extra CLI args are appended only to the LAST item of the last command.
- **Self-recursion pruning**: a root script `"build": "vp run -r build"` is itself selected by `-r build`; the nested query is compared to the parent query and skipped if equal (plan.rs:236-244). Only the exact-same-query case is pruned — `cd`-changed or differently-filtered nested queries expand normally.
- Nested `vp run` honors flags inside scripts: `--filter`, `--parallel`, `--cache/--no-cache`, `--concurrency-limit`, `--log`, `--ignore-depends-on` (vite_task_bin/src/lib.rs:74-107). Flags must precede the task name.
- `vp <tool>` invocations (e.g. `vp lint`) inside scripts are synthesized as cached `vtt` tool invocations rather than spawning a nested vp CLI.
- Pre/post hooks (`preX`/`postX` scripts) are expanded one level deep as additional items around the task's own items.

## What this means for migrations

- **Phase pipelines** (turbo: `test.dependsOn: ["^build"]`, nx: `targetDefaults`): besides per-package `dependsOn`, you can write a root script
  `"ci": "vp run -r lint && vp run -r typecheck && vp run -r build"` —
  three sequential phases, each an inlined graph, every task cached individually. Verified on create-t3-turbo: `vp run ci` = 31 tasks, 100% cache hits on re-run.
- **Codegen→compile chains** within one package: `"build": "node codegen.mjs && tsc"` gives two cache entries; editing only templates re-runs codegen and (if output changed) tsc; editing only src re-runs just tsc. This replaces turbo's two tasks + dependsOn for intra-package sequencing.
- **Root orchestration scripts survive as-is** thanks to self-pruning — `"build": "turbo run build"` → `"build": "vp run -r build"` is a drop-in.
- Sequential semantics are a **barrier**: `vp run -r build && vp run -r test` finishes ALL builds before any test (turbo interleaves per-package). Usually fine; for max interleaving use per-package `dependsOn` instead.

## Doc gap

Public docs cover "Compound Commands" and "Nested vp run" briefly, but not: cd-as-builtin, per-item cache keys/sharing, which flags work inside scripts, self-pruning rule precision, extra-args-to-last-item, or the tree-vs-graph design guidance above. This deserves a dedicated "Composing tasks" guide page.
