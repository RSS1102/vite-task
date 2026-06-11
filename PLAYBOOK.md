# Vite Task Migration Playbook (turborepo/nx → `vp run`)

Status: v2 (2026-06-11). Based on vp 0.1.24 + vite-task source reading (see `reports/source-notes/*.md` for cited deep-dives) + scratch experiments in `scratch/`.

## What Vite Task is (mental model)

- `vp run <task>` runs **package.json scripts** and **tasks defined in `vite.config.ts`** (`run.tasks`). One task name per invocation, mapped across selected packages.
- There is **no turbo.json/nx.json equivalent**: no central pipeline file. Tasks belong to the package whose `vite.config.ts` defines them. Root config tasks = root package only.
- Cross-package ordering is **implicit**: when multiple packages are selected (`-r`, `-t`, `--filter`), same-named tasks run in package dependency order (edges from `workspace:` protocol deps only). Packages lacking the task are bridged through.
- `dependsOn` is for **other tasks**, package-local by default (`['build']`) or explicit cross-package (`['@scope/pkg#build']`). **No `^build` syntax** — that semantic comes from selection flags instead.
- Caching: automatic input tracking via fspy (syscall-level file access tracing). Tasks cached by default; scripts cached only with `--cache` flag or root `run.cache: { scripts: true }`. Cache = replay of terminal output + restore of declared `output` globs. Cache dir: `node_modules/.vite/task-cache`.
- Tasks run with a **strict env allowlist** (HOME, PATH, CI, NEXT_*, VERCEL_*, etc.). Other vars need `env` (fingerprinted) or `untrackedEnv` (not fingerprinted) per task.

## Verified facts that gate migration design

| Fact | Consequence |
|---|---|
| `vp` reads `vite.config.{ts,js,...}` plain object export; no `vite-plus` install needed; `vite-task.json` is ignored by vp | Migrations need no dependency changes; write `vite.config.ts` files |
| Root tasks don't propagate to packages | Per-package `vite.config.ts` for task-level control; or rely on scripts + root `run.cache.scripts: true` |
| Root `run.cache: { scripts: true }` caches every package's scripts workspace-wide | **Minimal-diff migration**: keep all package.json scripts, add one root config file |
| Task names may not collide with script names | When promoting a script to a task, delete/rename the script |
| Only `workspace:` protocol deps create graph edges | Repos with `"*"`/exact-version internal deps lose topological ordering — either rewrite deps to `workspace:*` (note in report) or accept unordered |
| `^build` → load error | Turbo `"dependsOn": ["^build"]` translates to: run with `-r`/`-t` (implicit topo) + per-package `dependsOn: ['build']` for own-package prerequisites |
| Dependency must EXIT successfully before dependent starts | turbo `persistent: true` deps (dev-server sidecars) cannot be modeled — report as missing feature; keep dev tasks as plain parallel runs (`vp run -r --parallel dev`) |
| Shell builtins (`echo`-only commands) run in-process → uncacheable | Don't fret over "cache disabled" on trivial echo scripts |
| Task writing a file it read (e.g. `.tsbuildinfo`, in-place caches) → cache not updated | Add `input: [{auto:true}, '!**/*.tsbuildinfo']`-style exclusions |
| Outputs are only archived if `output` globs declared; default = terminal replay only | For build tasks, declare `output: ['dist/**']` to get artifact restore |

## Migration recipe (per repo)

1. **Inventory**: read `turbo.json`/`nx.json` (+ per-package configs), root+package `package.json` scripts, workspace layout, packageManager, internal dep style.
2. **Choose migration depth per task**:
   - Scripts-only (cheapest): keep scripts; root `vite.config.ts` with `run: { cache: { scripts: true } }`. Auto-tracking handles inputs. Good for `lint`, `typecheck`, `test`.
   - Task promotion: for tasks needing `dependsOn`, `env`, `output` restore, or input pruning, move the script command into the package's `vite.config.ts` `run.tasks.<name>.command` and delete the script. Mind the name-collision rule.
3. **Translate turbo.json fields**:
   - `pipeline.X.dependsOn: ["^X"]` → nothing (implicit with `-r`); document run command as `vp run -r X`.
   - `dependsOn: ["Y"]` (same pkg) → `dependsOn: ['Y']` on task X.
   - `dependsOn: ["pkg#Y"]` → `dependsOn: ['pkg#Y']`.
   - `outputs: [...]` → `output: [...]` (per-package config; note `.next/**` etc. — exclude `!.next/cache/**`).
   - `inputs: [...]` → usually omit (auto-tracking) or `input:` for pruning; `$TURBO_DEFAULT$` ≈ `{auto: true}`.
   - `env`/`globalEnv` → per-task `env`; `passThroughEnv` → `untrackedEnv`.
   - `globalDependencies` → `input: [{pattern: '...', base: 'workspace'}]` additions on affected tasks (or omit; auto-tracking usually sees them).
   - `persistent: true` → `cache: false`; cannot be a dependency (missing feature).
   - `cache: false` → `cache: false`.
   - Root-level "global" scripts (`turbo run build` in root package.json) → `vp run -r build` (recursion to self is auto-pruned).
4. **Translate nx**: targets defined by `nx.json` targetDefaults + project.json/package.json. Only migrate repos where targets are plain commands (`nx:run-commands` or package scripts). Nx executor targets (`@nx/webpack:webpack` etc.) can't run outside nx → report blocked. `dependsOn: [{"projects": "dependencies", "target": "build"}]` ≡ `^build` → same translation. namedInputs → input globs or auto.
5. **Run + verify** (always from repo root unless testing cwd behavior):
   - `vp run -r <task>` (or filtered subset) → all succeed.
   - Re-run → expect cache hits (`N/N cache hit`).
   - Touch a source file in one package → only that package (+ dependents if they read its outputs) re-runs.
   - If `output` configured: delete an output dir, re-run, confirm restore.
   - Capture timings + the `--last-details` summary in the report.
6. **Branch**: commit migration on branch `vite-task-migration` in the clone. Keep diffs minimal; don't reformat. Remove turbo/nx config only if fully replaced (otherwise keep both and note coexistence).
7. **Report**: structured outcome — what migrated cleanly, what needed workarounds, what's impossible (missing features), cache-hit data, surprises.

## Confirmed by source reading (citations in reports/source-notes/)

**Definitively missing vs turborepo/nx** (do not search for these; report them when a repo needs them):
- Watch mode (`turbo watch`), dry-run, task-graph print/visualize, `--affected`/`--since` git-based filtering, remote/shared cache (local SQLite + tar.zst only), `--continue`/no-fail-fast flag (fast-fail is mandatory), `persistent: true` tasks, retries, per-task `--output-logs` modes, turbo-style root pipeline config, nx executors/plugins.
- turbo.json / nx.json are completely ignored by vp — leave them in place; coexistence is fine (run either tool).

**Mechanics to exploit:**
- Per-package configs may be `vite.config.{js,mjs,ts,cjs,mts,cts}` — evaluated by Vite's config loader, npm imports allowed. A failing config import fails the whole task-graph load: when adding `run` to an existing vite.config.ts, make sure its imports resolve from that package.
- `dependsOn` missing target = HARD ERROR for the whole run. Only add `dependsOn: ['build']` in packages that actually have a `build` task/script. No globs in dependsOn.
- Cross-package ordering of the *selected* task comes from package-graph edges (workspace: protocol deps incl. devDependencies/peerDependencies). Bridging skips packages lacking the task.
- Additional CLI args go only to explicitly requested tasks, never to dependsOn-pulled tasks; args become part of the cache key; flags must come BEFORE the task name.
- Default passthrough env is generous: HOME/USER/PATH/CI/TMP..., `GITHUB_*`, `RUNNER_*`, `VERCEL*`, `NEXT_*`, `VSCODE_*`, `PLAYWRIGHT_*`, `DOCKER_*`, `VP_*`, `*_TOKEN`, NODE_OPTIONS, PNPM_HOME, COREPACK_*. Sensitive-looking fingerprinted values (`*_KEY`, `*_TOKEN`, `AWS_*`) are SHA256-hashed in the cache, not stored raw.
- env/untrackedEnv support `*`, `?`, `[..]`, `{a,b}` wildcards.
- Compound splitting handles only top-level `&&` lists; `;`, `||`, pipes, `$()`, `$VAR` make the whole string run under `/bin/sh -c` as ONE cached unit (still fine, just not split).
- Output archives are tar.zst; **symlinks are skipped** in archiving; extraction overwrites existing files.
- Read-write overlap (fspy-inferred only) silently skips cache update → perpetual misses; fix with `input: [{auto:true}, '!<path>']` exclusions (`.tsbuildinfo`, `.eslintcache`, `.next/cache`, vite/webpack caches, coverage dirs).
- Interactive prompts only work for uncached tasks in default `--log=interleaved` mode (stdin is /dev/null otherwise; no PTY ever).
- Cache location override: `VITE_CACHE_PATH`. Cache hits consume concurrency slots.
- Workspace discovery: pnpm-workspace.yaml (negated globs OK) or package.json `workspaces` (array/object). Root package is always in the graph and included by `-r`. Yarn PnP NOT supported. Lockfiles never consulted. Nested workspaces not supported.
- `vp run` with no task name in non-TTY prints the task list (exit 0).
- Exit codes: single failure → that code (clamped 1-255), including when the failure is a dependsOn dependency of the requested task (verified); multiple failures/infra errors → 1. CAUTION when checking exit codes in fish: `vp run x | tail` makes `$status` report tail's status — use `bash -c '...; echo $?'`.

## Env gotchas (strict allowlist)

Build tools needing env vars fail or behave differently silently. Common needs:
- Next.js: `NEXT_*` auto-passed; but `NODE_ENV`, custom `*_URL`s need declaring.
- CI detection (`CI`) auto-passed.
- `FORCE_COLOR=1` injected by default unless listed.
When a build fails under vp but works with pnpm run, suspect env first.

## Pilot learnings (create-t3-turbo, verified end-to-end)

- **Read-write overlap exclusion catalog** (add as `input` negations when the "not cached because it modified its input" message appears; `--last-details` names the exact path):
  - `!.cache/**` (tsc incremental tsbuildinfo), `!**/*.tsbuildinfo`
  - `!next-env.d.ts`, `!.next`, `!.next/**` (next build)
  - `!.output/**`, `!node_modules/.nitro/**`, `!.tanstack/**` (nitro/tanstack-start)
  - `!node_modules/.vite-temp/**` — vite bundles vite.config.ts to a temp file on EVERY vite build; affects all vite-based packages
  - eslint `--cache` files, vitest/jest caches, coverage dirs
- **Directory-listing invalidation**: a tool that lists its package dir makes the dir listing an input; deleting an output dir (`.next`) then re-running = miss instead of restore. Fix: `{ pattern: '!<workspace-rel-pkg-path>', base: 'workspace' }` in `input`. (Feature idea: auto-exclude declared `output` globs from inputs.)
- **Exit 137 in summaries = fast-fail SIGKILL** of siblings, not a real failure; find the exit-1 task.
- **Tool-specific turbo coupling**: repos may use `eslint-plugin-turbo` (no-undeclared-env-vars reads turbo.json) — remove it when removing turbo.json. Grep for `turbo` in lint configs, CI, docs.
- **node_modules files are inputs** — after any install, expect legitimate one-time misses.
- **dotenv pattern**: `dotenv -e ../../.env -- <cmd>` works; .env file reads are tracked as inputs. Root-devDep bins resolved fine from package tasks in practice.
- **Selective invalidation is content-based and often better than turbo**: a comment-only change in db/src rebuilt db + apps that bundle its TS source, while api (which reads only db's emitted .d.ts) stayed cached.
- **Format new files**: run the repo's prettier on generated vite.config.ts files so `format` checks stay green.
- **Tree model verified**: root `ci: vp run -r lint && vp run -r typecheck && vp run -r build` expands in-process to one graph per phase (31 tasks → 100% cache hits on second run). Self-recursion (`build: vp run -r build` selected by `-r build`) is pruned automatically.
- Verification recipe that worked: `-r build` ×2 → expect N/N hits; edit one source file → expect selective misses; delete an output dir → expect restore (hit); `-r typecheck` ×2; `-r lint` ×2; compound ci ×2.

## trpc-migration learnings (verified on the committed trpc branch)

- **Single cache slot per task**: cache lookup goes through the latest fingerprint only — edit A→B→back-to-A is a miss, and alternating forwarded args (`lint` vs `lint --fix`) evict each other's entry. Don't promise turbo-style content-addressed history in MIGRATION.md.
- **tsdown/tsup read back their own dist on warm rebuilds** (sourcemaps, bin files) → `!dist/**` input negation; bonus: with dist excluded from inputs, `rm -rf dist` + rerun = restore-from-archive hit (no dir-listing invalidation).
- **Watch scripts selected across packages with workspace edges DEADLOCK** under implicit topo ordering (watcher never exits; dependents wait forever) → always run watch/dev fleets with `--parallel`.
- **SIGTERM to the vp parent orphans child processes** (a `tsdown --watch` kept running after kill) — warn users; ctrl-c (SIGINT to the group) is the supported path.
- Scripts that **regenerate files they read** (`scripts/entrypoints.ts` rewriting package.json/turbo.json in tsdown onSuccess) need `!package.json` etc. input negations; `eslint --cache` needs `!.eslintcache`.
- Before rewriting non-workspace internal deps, DEMONSTRATE the edge loss (`turbo build --filter=X --dry` task count vs `vp run -t X#build` task count) — that contrast is the evidence the report needs.
- Keeping turbo for unmigrated subtrees (www/examples) works fine — coexistence verified; per-package turbo.json files may be load-bearing for build scripts that read them (trpc's entrypoints.ts) — grep before deleting.

## Useful commands

- `vp run` (list tasks), `vp run -r X`, `vp run -t pkg#X`, `vp run --filter './apps/web' X`
- `vp run --cache X` (force-enable), `--no-cache`, `--ignore-depends-on`, `--parallel`, `--concurrency-limit N`, `-v`, `--last-details`
- `vp cache clean`
- `VP_RUN_CONCURRENCY_LIMIT`
