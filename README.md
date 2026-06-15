# Vite Task migration reports

What happens when you take **nine popular OSS monorepos built on Turborepo or Nx and migrate them to [Vite Task](https://github.com/voidzero-dev/vite-task)** (`vp run`, Vite+ 0.1.24)? This branch is the answer: every migration was actually executed and verified on a real machine (macOS arm64) — cold runs, warm-cache re-runs, selective-invalidation probes, and output-restore-after-delete — not just config translation.

Migrated and verified: create-t3-turbo, trpc, Turborepo's own kitchen-sink example, ZenStack, TanStack Router, TanStack Query, shadcn/ui, Vercel AI SDK, react-email. Largest single result: TanStack Query's entire CI pipeline — 420 task units including an 8-TypeScript-version typecheck sweep — replaying as **420/420 cache hits from one `vp run test:ci` command**.

## Outcome ratios

Denominator: the full [candidates.json](./candidates.json) research pool (**45 rows**; a few repos appear twice because they were revisited with updated metadata), not just the nine repos that were migrated.

Accounting rule: **verified migration** means a repo from the pool was actually migrated and proved with real `vp run` executions; **clean success** means Vite Task cleanly replaced the intended turbo/nx scope for that migration; **partial success** means a meaningful, verified scope migrated, but material subtrees or task families still stayed on turbo/nx or plain scripts because of missing Vite Task capabilities or environment constraints.

| measure | current result on Vite+ 0.1.24 | if [#467](https://github.com/voidzero-dev/vite-task/pull/467) lands |
|---|---:|---:|
| researched candidate pool | 45/45 (100.0%) | 45/45 (100.0%) |
| verified migrations | 9/45 (20.0%) | 9/45 (20.0%) |
| clean successes | 5/45 (11.1%) | 5/45 (11.1%) |
| scoped partial successes | 4/45 (8.9%) | 4/45 (8.9%) |
| researched but not migrated | 36/45 (80.0%) | 36/45 (80.0%) |
| `^task` workaround burden in candidates that use that pattern | 43/45 (95.6%) have `^build`/`^compile`/`^topo`-style package-dependency task semantics | 0/43 need that workaround after adopting object-form `dependsOn`, e.g. `{ "task": "build", "from": "dependencies" }` |

The five clean successes are create-t3-turbo, the Turborepo kitchen-sink example, TanStack Query, Vercel AI SDK, and react-email. The four partial successes are trpc (packages/* only), ZenStack (packages/* only), TanStack Router (packages/* only), and shadcn/ui (CLI + v4 registry scope). #467 would remove the biggest config-translation workaround across the pool, but it does **not** by itself change the success split above: the remaining partials are blocked by non-`workspace:` graph edges, retained e2e/service subtrees, persistent/watch semantics, affected/remote-cache gaps, or CI integration work.

## Missing-feature impact

Sorted by candidate-pool reach: how many of the 45 candidate rows would materially improve if the feature existed. These are not additive; one repo can need several fixes, and some counts are conservative when the candidate notes do not spell out CI details.

| rank | missing feature | candidate-pool reach | improvement |
|---:|---|---:|---|
| 1 | Remote/shared cache | 45/45 for CI parity; explicitly hit in every verified migration | Turns local-only success into a plausible CI replacement instead of relying on ad hoc `actions/cache` for `node_modules/.vite/task-cache`. |
| 2 | Native package-dependency task selection (`^task` parity, #467) | 43/45 (95.6%) | Removes phase-pipeline/manual `pkg#task` workarounds for the dominant turbo/nx pattern: "run this task in dependency packages first." |
| 3 | Auto-exclude declared outputs and common tool caches from inputs | 26/45 (57.8%) explicitly list output-heavy pipelines; all 9 verified migrations hit it | Converts the biggest migration tax into a default: fewer read-write-overlap misses, fewer directory-listing misses, more reliable archive restore. |
| 4 | Watch + persistent/service task semantics | 21/45 (46.7%) | Lets dev-server fleets, `turbo watch`/`nx watch`, and "test depends on a running service" workflows migrate instead of staying as scripts or old-tool islands. |
| 5 | Workspace graph edges beyond `workspace:` protocol | 17/45 (37.8%) | Unblocks exact-version, star-version, caret-linked, pnpm `linkWorkspacePackages`, npm/yarn workspace, and version-match dependency styles without rewriting package manifests. |
| 6 | Nx executor/inferred-target/project-graph support or import tooling | 6/45 (13.3%) | Makes classic Nx repos with executors, inference plugins, `project.json`, or plugin-computed targets migratable without hand-reconstructing every command. |
| 7 | Affected/changed-since selection | 4/45 (8.9%) explicitly mention it | Replaces `nx affected` and turbo git-range filters with native scheduling instead of full-graph runs that lean on cache hits. |

## Start here

| read | if you want |
|---|---|
| [SUMMARY.md](./SUMMARY.md) | the 5-minute version: what was migrated, verification numbers per repo, and the verdict on Vite Task as a turbo/nx replacement |
| [MISSING-FEATURES.md](./MISSING-FEATURES.md) | the prioritized gap list: **4 bugs** (worst: a corrupt cache.db makes `vp run` execute zero tasks and exit 0 — a CI false-green) **+ 20 missing features**, each tied to the concrete repo that needed it |
| [DOCS-IMPROVEMENTS.md](./DOCS-IMPROVEMENTS.md) | actionable documentation work: 4 new guide pages (Turborepo migration, Nx migration, task composition, caching troubleshooting) + fixes to existing pages + small DX changes with outsized docs value |

## Supporting material

- **[PLAYBOOK.md](./PLAYBOOK.md)** — the migration playbook that was refined across all nine repos: Vite Task's mental model, turbo.json/nx.json translation tables, the read-write-overlap exclusion catalog, env semantics, and the verification recipe. If you're migrating a repo yourself, this is the document to follow.
- **[candidates.json](./candidates.json)** — 45 OSS repos researched as migration candidates, with tool, package manager, internal-dependency style (the `workspace:` protocol question matters!), pipeline complexity, and feasibility ratings.
- **[migrations/](./migrations)** — structured per-repo reports (scope, verification numbers, missing features, bugs, surprises, input exclusions needed) for the repos whose agents returned them.
- **[source-notes/](./source-notes)** — deep-dives into the vite-task source with file:line citations, written before/during the migrations: [CLI surface](./source-notes/cli.md), [caching internals & fspy](./source-notes/caching.md), [config loading & schema](./source-notes/config.md), [execution semantics](./source-notes/execution.md), [workspace discovery & filters](./source-notes/workspace.md), and the [tree model](./source-notes/tree-model.md) (`&&` splitting + in-process `vp run` expansion — Vite Task's most under-documented differentiator).

## Where the migrated code lives

Each migration is a commit on a `vite-task-migration` branch in a local clone (not pushed to the upstream projects), accompanied by a `MIGRATION.md` documenting the translation map, the verified numbers, and repo-specific gotchas. The structured reports under [migrations/](./migrations) and the per-repo sections in [SUMMARY.md](./SUMMARY.md) capture the substance.

## Method, in one paragraph

Vite Task's capabilities were first mapped from source (the public docs are ~3 pages) and validated with scratch experiments; that produced the playbook. Candidates were researched and verified against their actual default-branch files. Migrations then ran as parallel agent waves, each following the playbook and required to *prove* its result: run the pipeline twice (second run must hit cache), edit one source file (only the dependency cone may rebuild), delete declared outputs (must restore from archive), and record the numbers. Findings flowed back into the playbook between waves — which is why later migrations (query's 420-task pipeline, zenstack's three-way codegen-chain experiment) probe deeper than earlier ones. One early "bug" (exit 0 on failed dependency) was retracted after adversarial re-testing exposed it as a shell-pipeline artifact; the bugs that remain in [MISSING-FEATURES.md](./MISSING-FEATURES.md) survived that same scrutiny.
