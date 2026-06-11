# Vite Task documentation improvements

Evidence base: 9 migrated OSS monorepos (see SUMMARY.md). Current public docs are ~3 pages (guide/run, guide/cache, config/run); the vite-task repo's internal docs (stdio/concurrency/cancellation/inputs) contain user-relevant material that isn't public. Items are ordered by how much migration pain they would have prevented.

## New pages to write

1. **"Migrating from Turborepo" guide.** Validated translation table:
   - `pipeline.X.dependsOn: ["^X"]` → nothing: `vp run -r X` orders same-named tasks via the workspace graph. **Call out loudly: edges exist only for `workspace:` protocol deps** — exact pins + pnpm link-workspace-packages (trpc), version ranges (router), version-match linking (shadcn) all silently lose ordering; rewrite to `workspace:*` or add explicit `dependsOn: ['pkg#task']`.
   - `X.dependsOn: ["^build"]` for other tasks → own-package `dependsOn: ['build']` + implicit topo (close, slightly over-serialized), or explicit pkg#task lists (exact), or a root tree-script `vp run -r build && vp run -r X` (phase barrier).
   - `outputs` → `output` globs (archived/restored tar.zst; symlinks skipped). Logs always replay; files restore only when declared.
   - `inputs`/`$TURBO_DEFAULT$`/`globalDependencies` → usually nothing (auto-tracking); negations for self-rewritten files. Nuance: turbo defaults to *git-tracked* files; fspy tracks *actually-read* files including gitignored ones, and **directory listings** — so file *additions* can invalidate where glob-based tools wouldn't, and unread files (e.g. README.md, proven on router: 399/399 hits after a README edit) never do.
   - `globalEnv`/`passThroughEnv` → per-task `env`/`untrackedEnv` (wildcards: vercel/ai's 60-entry list became 18 wildcard patterns like `*_API_KEY`).
   - `persistent`, `--continue`, `turbo watch`, remote cache → not available; set expectations + workarounds (`--parallel` for watcher fleets — REQUIRED, topo ordering deadlocks on watchers).
   - Root scripts `"build": "turbo run build"` → `"build": "vp run -r build"` (self-pruning).
   - Remove `eslint-plugin-turbo` with turbo.json (its rule reads turbo.json); grep CI for TURBO_TOKEN/TEAM.
   - Per-package turbo.json overrides (`web#build`) → that package's own vite.config.ts (the native model).
2. **"Migrating from Nx" guide.** targetDefaults/namedInputs translation (namedInputs → usually nothing); nx-as-orchestrator repos (TanStack family) migrate cleanly, executor-based repos don't; `nx run-many -t a,b,c` → tree-model root script `vp run … a && vp run … b`; `nx affected` → full-graph + cache (be honest about the gap); replace `tsc --build` composite orchestration with per-package `tsc -p` + topo ordering (**`tsc --build` rewrites upstream dists and never caches** — proven on query (94%→100% after the switch) and ai).
3. **"Composing tasks" page (the tree model).** `&&` splitting into separately-cached items; nested `vp run` in-process expansion (flags work inside scripts, before the task name); `cd` as a builtin item; per-item cache keys + content-based cache sharing across tasks; self-recursion pruning; extra args go to the LAST item only; sequential-barrier semantics vs per-package dependsOn; **when NOT to use `&&`**: read-write codegen chains belong in one task or separate tasks + dependsOn — shared input config across items can silently restore stale archives (zenstack).
4. **"Caching troubleshooting" page** keyed to actual messages:
   - "not cached because it modified its input" → exclusion catalog: `node_modules/.vite-temp/**` (every vite build), `**/*.tsbuildinfo` (incl. vitest's copy in the pnpm store), `next-env.d.ts`/`.next/**`, `.output/**`+nitro, tsdown/tsup `!dist/**`, `pnpm pack`/prepack → `dist/package.json`+`README.md` or `--pack-destination`, test temp files in src fixtures.
   - "'X' added/removed in 'dir'" (directory-listing inputs) → sibling-tool caches poisoning OTHER tasks (prettier `--cache` → `node_modules/.cache`, eslint `.eslintcache`); workspace-root dir listing affects packages whose tools walk up (tsup; a root file add invalidates package builds); the `{ pattern: '!<pkg-dir>', base: 'workspace' }` recipe to make deleted outputs restore instead of rebuild.
   - "⊘ cache disabled" → in-process builtins (echo), `--no-cache`, scripts without `--cache`.
   - "Cache lookup failed …" → **currently runs 0 tasks and exits 0 (CI false-green!); `vp cache clean` is the fix**; CI should fail on `0 tasks`.
   - exit 137 = fast-fail SIGKILL collateral; find the real failure.
   - Iterating on configs: changed input/env config = new cache key (expect one full miss); stale *successful* entries replay until inputs change — use `--no-cache` while debugging tool-config changes outside tracked inputs.
   - "Settling runs": after installs or test runs that touch node_modules, expect one round of legitimate misses.

## Fixes/additions to existing pages

5. **Env semantics need a full page**: the real ~50-pattern default passthrough list (docs say "a small set"); **the allowlist applies ONLY to cached tasks — `cache:false` tasks inherit the full environment** (and env/untrackedEnv on them is currently a load error); FORCE_COLOR=1 injection (broke CLI snapshot tests on shadcn) + override pattern; `NODE_ENV` in `env` is treated specially (observed unfingerprinted on ai); sensitive values (`*_KEY` etc.) are sha256-hashed in the cache; untracked env *names* are fingerprinted, values aren't.
6. **Config loading**: a plain-object `export default { run: {...} }` works with no vite/vite-plus dependency (makes vp viable in non-Vite repos — nowhere stated); adding `run` to an existing typed `defineConfig` fails TS excess-property checks → document the spread recipe or ship types; **all workspace configs are evaluated as real code on every run** — configs importing workspace dists brick the repo after a clean (router); share task factories via a plain JS module import (ai's `vite.tasks.mjs` pattern) to avoid N copies of identical config.
7. **`vite-task.json` is not a vp format** (dev-binary playground only) — the vite-task repo's own playground misleads first-time readers.
8. **dependsOn**: hard-errors on missing targets (turbo silently skips) — only declare where the target exists; no globs; `^` prefix should get a targeted error message explaining the translation.
9. **`run.cache.scripts: true` caveat**: caches ALL scripts including non-idempotent ones; no per-script opt-out — recommend task promotion instead.
10. **Make internal docs public**: `--log interleaved|labeled|grouped`, stdin rules (cached tasks get /dev/null; interactive = uncached + interleaved only), Ctrl-C/fast-fail semantics, concurrency (default 4, `VP_RUN_CONCURRENCY_LIMIT`, cache hits consume slots).
11. **Exit codes**: single failure → that task's code (incl. via dependsOn); multiple/infra → 1; cancelled siblings don't count. (Shell gotcha: piping vp changes `$status` in fish — verify with `bash -c`.)
12. **Filters**: unbraced `./dir` silently drops `...` traversal (use `{./dir}...`); no git refs; `--fail-if-no-match` exists; `'./packages/**'` works for nested layouts.
13. **`VITE_CACHE_PATH`** (cache relocation for CI mounts) — undocumented; pair with a CI recipe (persist `node_modules/.vite/task-cache`, treat "Cache lookup failed"/0-task runs as failures).
14. **Forwarded args**: only to explicitly-requested tasks; part of the cache key; single slot means alternating arg sets evict each other (shadcn's registry modes) — document `--` is unnecessary and flags must precede the task name.
15. **Ecosystem integrations**: knip can't see binaries/deps referenced only in `run.tasks` commands (needs ignoreBinaries/ignoreDependencies — query needed 7 entries); sherif flags empty dep-objects left by script removal; `vp --version` reports v0.0.0 (breaks version-gated tooling).

## DX improvements (small code changes with outsized docs value)

- Fail loudly (or run uncached) on cache-db corruption instead of 0-tasks-exit-0 (bug B1 — top priority).
- Warn when a multi-package selection produces zero dependency edges.
- Detect `^` in dependsOn → targeted migration hint.
- Default input exclusions: `node_modules/.vite-temp/**`, `**/*.tsbuildinfo`, `node_modules/.cache/**`, `.eslintcache`; auto-exclude declared `output` globs (incl. their dir-listing entries).
- A `vp run --why pkg#task` / dry-run graph print would have replaced many `--last-details` archaeology sessions.
