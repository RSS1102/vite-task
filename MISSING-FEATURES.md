# Vite Task: missing features & bugs for nx/turborepo replacement

Evidence base: 9 real OSS monorepos migrated to `vp run` (vp 0.1.24, macOS arm64) and verified with real runs — create-t3-turbo, trpc, turborepo's kitchen-sink example, zenstack, TanStack/router, TanStack/query, shadcn-ui, vercel/ai, react-email — plus source-level analysis (reports/source-notes/). Per-repo details: each repo's MIGRATION.md on its `vite-task-migration` branch and reports/migrations/*.json.

## Bugs (fix before anything else)

B1. **Corrupted cache.db → silent false green.** A corrupt 99MB cache.db (left by an interrupted session) made every `vp run` print "Cache lookup failed: Encoded sequence length exceeded preallocation limit…", run **zero tasks, and exit 0** (`last-summary.json: {"tasks":[],"exit_code":0}`; verified under bash). In CI the pipeline passes while running nothing. Seen independently on TanStack/router ("/tmp/router-corrupt-cache.db" preserved) and vercel/ai ("Invalid tag encoding" variant). Cache lookup failure must be a hard error (or at minimum run everything uncached), never 0-tasks-exit-0.

B2. **Silent stale-output restore across `&&` items.** When an earlier item of a compound command rewrites files that a later item's output archive contains, and those files are input-excluded, the later item cache-hits and restores **stale** files over the earlier item's fresh output. Demonstrated on zenstack's langium chain: after a grammar regex edit, the cached patch item restored an old `grammar.ts` over the fresh codegen (confirmed against a `--no-cache` control). vp could detect intra-task write/restore conflicts.

B3. **`cache: false` + `env`/`untrackedEnv`/`input`/`output` fails the whole workspace load** with the cryptic serde error "data did not match any variant of untagged enum UserTaskDefinition", naming only the package dir. Needs a targeted diagnostic ("env requires caching enabled" or, better, make env passthrough orthogonal to caching — see F7).

B4. **Process-tree cleanup:** SIGTERM to the vp parent orphans children (tsdown --watch on trpc); SIGINT cleaned direct children but orphaned a nested `run-p` subtree (zenstack). Only terminal Ctrl-C to the process group fully works.

## High impact (blockers for typical CI replacement)

F1. **Remote / shared cache.** Absent (local SQLite + tar.zst only). Every migrated repo's CI depends on it: TanStack/router uses Nx Cloud *distributed agents*; react-email/shadcn-ui CI carried TURBO_TOKEN/TEAM; ai/zenstack matrix builds rebuild per job. Workaround everywhere: persist `node_modules/.vite/task-cache` with actions/cache. Note: vp re-validates input hashes at hit time, which makes shared caches *safer* than turbo's — good foundation to build on.

F2. **Affected/changed-since selection** (`nx affected`, `turbo --filter=[ref]`). Absent (no git integration in the filter parser). TanStack router & query root scripts are literally `nx affected …`; replacement is full-graph runs that lean on cache hits — workable locally, but CI scheduling/log noise remains. fspy-grade input knowledge could make a native `--affected` more precise than git-diff-based tools.

F3. **Watch mode** (`turbo watch`, `nx watch --all`). Absent. Hit in t3 (`turbo watch dev --continue`), router/query (`nx watch -- pnpm build:all`), shadcn (tsup --watch + next dev fleet). Dev-server fleets work via `vp run -r --parallel dev` (self-watching servers), but task-graph-aware rebuild-on-change is unreplicated, and **watchers deadlock under topological ordering** if `--parallel` is forgotten (selected watchers never "complete").

F4. **Persistent/service task semantics** (`persistent: true`, turbo `with`). A never-exiting dependency blocks dependents forever (verified empirically). Real case: shadcn's integration tests need the v4 dev server up → kept the repo's start-server-and-test wrapper.

F5. **Workspace graph edges beyond the `workspace:` protocol.** Only `workspace:` ranges create edges (TODO at vite_workspace/src/package.rs:56). Proven loss of ordering on: trpc (exact pins + `link-workspace-packages=true`: turbo planned 4 tasks, vp planned 1), TanStack/router (`>=1.171.7` range → edge lost, fixed with explicit `dependsOn: ['@tanstack/router-core#build']`), shadcn (4.11.0 version-match linking). Migrations rewrote deps to `workspace:*` — fine for branches, but upstream support for pnpm link-workspace-packages/overrides (or at least a **warning when a multi-package selection yields zero edges**) is needed. Also affects npm/yarn star/exact-version repos (documenso, sentry, storybook, drizzle patterns — unmigrated).

## Medium impact (workarounds exist, but every repo hit them)

F6. **Auto-exclusion of declared `output` globs (and tool caches) from inputs.** The #1 practical time sink: tools re-read/rewrite their own products, producing perpetual "not cached because it modified its input" or spurious dir-listing misses. Catalog accumulated across repos: `node_modules/.vite-temp/**` (every vite build), `*.tsbuildinfo` (tsc incremental; vitest typecheck writes one inside the pnpm store), `next-env.d.ts`/`.next/**`, `.output/**` + nitro state, tsdown/tsup re-reading `dist/**`, `pnpm pack`+prepack rewriting `dist/package.json`/`README.md`, prettier `--cache` → `node_modules/.cache` poisoning *other* tasks via dir listings, eslint `.eslintcache`, vitest `node_modules/.vite/vitest/**`, test suites writing temp files into src fixtures (react-email), scripts regenerating package.json/turbo.json (trpc). shadcn's registry task needed ~12 negations that exactly mirror its own output globs. Defaults/presets + auto-excluding outputs would eliminate most of this.

F7. **Env passthrough is coupled to caching.** Strict allowlist applies only to cached tasks; `cache: false` tasks inherit the FULL environment (and `env`/`untrackedEnv` on them is a load error, B3). So turbo's `globalPassThroughEnv` + `cache:false` maps to "no env hygiene at all" (zenstack's DB-matrix tests). Decouple env declaration from cache config.

F8. **Keep-going mode** (`turbo --continue`, nx `--nx-bail=false`). Fast-fail is mandatory; one failure SIGKILLs siblings (exit 137 collateral confused every migration at least once). zenstack's 3 pre-existing test failures cancelled the rest of the fleet; shadcn's 3-phase check can't surface lint+typecheck failures together.

F9. **Single cache slot per task (no content-addressed history).** Edit→revert = miss; alternating forwarded args (`lint`/`lint --fix`, shadcn's `registry:build --examples` vs full) evict each other; branch-switching workflows rebuild everything. Turbo/nx hit on any previously-seen hash. (vercel/ai observed multiple *env-fingerprint* entries coexisting, so the store may already support multi-entry — extend to args/inputs.)

F10. **Per-item config for `&&` compound items.** Input/output config is shared across a task's split items; zenstack's codegen chain needed different exclusions per item, forcing an sh-c fusion workaround (and naive shared exclusions cause B2). Tree-model pipelines would compose better with per-item overrides.

F11. **`tsc --build` (composite project references) is incompatible with per-package caching.** Downstream `--build` rewrites *upstream* packages' dist when mtimes/tsbuildinfo disagree (guaranteed after archive restore) → cross-package read-write overlap, plateaus at ~94% hits forever. Hit on TanStack/query AND vercel/ai. Migration fix: per-package `tsc -p` + topo ordering (query went 94%→100%). Worth a loud docs warning and/or detection.

F12. **Config-load fragility at scale:** vp evaluates **every** vite.config in the workspace (even outside `--filter`) as real code. Configs that import workspace dists create bootstrap circularity: after `rm -rf` one dist, *every* vp invocation in TanStack/router failed until rebuilt with nx. Lazy/filtered config loading or import-error tolerance for unselected packages would fix it.

## Lower impact / by-design differences (document rather than build)

F13. **Central pipeline config:** per-package vite.config.ts is the model; uniform repos write near-identical files ×N (t3: 11, query: 26, ai solved it with a shared `vite.tasks.mjs` factory imported by each config — works because configs are real JS; document this pattern). Root-level task defaults would cut diffs dramatically.
F14. **`^build` shorthand:** errors as "Task '^build' not found" — detect `^` and explain the translation (largest migration cohort will hit it in minute one). Also: there is no exact encoding of "dependents' tasks wait for dependencies' *builds* only" — own-package `dependsOn: ['build']` + implicit topo is close but over-serializes same-named tasks; explicit `pkg#task` lists are exact but unmaintained.
F15. **Per-script cache opt-out:** `run.cache.scripts: true` is all-or-nothing; non-idempotent scripts (`clean`, `db:push`) make the cheap scripts-only migration unsafe.
F16. **Dry-run / graph visualization:** absent (`--last-details` is post-hoc only).
F17. **Per-task log modes** (`outputLogs: new-only`): `--log` is global.
F18. **nx executor targets** can't run outside nx (router's Playwright-sharding plugin; lerna/storybook patterns) — command-level rewrite territory, be explicit in docs.
F19. **Duplicate package names** error on `pkg#task` ("Package name is ambiguous") — zenstack legitimately has two `zenstack-v3` packages; pnpm/turbo tolerate it. Workarounds: cwd-based runs or directory filters.
F20. **FORCE_COLOR=1 injection** into cacheable tasks changes CLI output and broke shadcn's snapshot assertions; override requires knowing the env-declaration trick.
