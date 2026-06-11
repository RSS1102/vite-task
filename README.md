# Vite Task migration reports

What happens when you take **nine popular OSS monorepos built on Turborepo or Nx and migrate them to [Vite Task](https://github.com/voidzero-dev/vite-task)** (`vp run`, Vite+ 0.1.24)? This branch is the answer: every migration was actually executed and verified on a real machine (macOS arm64) — cold runs, warm-cache re-runs, selective-invalidation probes, and output-restore-after-delete — not just config translation.

Migrated and verified: create-t3-turbo, trpc, Turborepo's own kitchen-sink example, ZenStack, TanStack Router, TanStack Query, shadcn/ui, Vercel AI SDK, react-email. Largest single result: TanStack Query's entire CI pipeline — 420 task units including an 8-TypeScript-version typecheck sweep — replaying as **420/420 cache hits from one `vp run test:ci` command**.

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
