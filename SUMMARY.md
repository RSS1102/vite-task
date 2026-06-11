# Vite Task migration project — summary

Goal: migrate popular OSS nx/turborepo monorepos to Vite Task (`vp run`, Vite+ 0.1.24), verify by really running them, report missing features, and propose documentation improvements.

## What was produced

- **45 candidate repos** researched and verified (tool, package manager, internal-dep style, pipeline complexity, feasibility): `reports/candidates.json`.
- **9 monorepos migrated, verified, and committed** on a `vite-task-migration` branch in each clone, each with a detailed `MIGRATION.md`:

| repo | tool | scope | headline verification |
|---|---|---|---|
| t3-oss/create-t3-turbo | turbo | full repo (3 apps, 10 pkgs) | `vp run ci` = 31 tasks, 100% warm hits; .next/dist archive restore |
| trpc/trpc | turbo | packages/* | 7/7 + 4/4 hits; proved `link-workspace-packages` edge loss (turbo 4 tasks vs vp 1), fixed via workspace:* rewrite |
| vercel/turborepo kitchen-sink | turbo | full example (Next/Remix/Vite/Express/bunchee) | `vp run ci` = 25/25 hits across all frameworks |
| zenstackhq/zenstack | turbo | 18 packages incl. langium codegen | 43/43 build units; codegen chain modeled 3 ways, found silent stale-restore bug in `&&` model |
| TanStack/router | nx | 42 packages | 399-unit `vp:test:ci` 100%; README edit = 399/399 hits (fspy beats namedInputs); found corrupt-cache false-green bug |
| TanStack/query | nx | 26 packages | **420-task CI pipeline (8 TS-version sweep incl.) 420/420 warm, 538s saved**; `tsc --build`→`tsc -p` pattern discovered |
| shadcn-ui/ui | turbo | CLI + v4 registry codegen | 1520-test suite cached; registry:build 13.8s→0.85s; bun pipeline ran under tsx |
| vercel/ai | turbo | 63 packages | turbo fully removed; 64/64 build, 105/105 test:ci; 60-entry env list → 18 wildcards; shared config-factory pattern |
| resend/react-email | turbo | packages/* + root biome lint | build/test/typecheck 100% warm; turbo + remote-cache tokens removed from CI |

- **`reports/MISSING-FEATURES.md`** — 4 bugs (worst: corrupt cache.db → 0 tasks run, exit 0 = CI false-green) + 20 gaps ranked by impact, each tied to concrete repo evidence.
- **`reports/DOCS-IMPROVEMENTS.md`** — 4 new doc pages outlined (turbo migration, nx migration, tree-model composition, caching troubleshooting) + 11 fixes to existing pages + DX quick wins.
- **`reports/PLAYBOOK.md`** — the battle-tested migration playbook (mental model, translation tables, exclusion catalog, verification recipe).
- **`reports/source-notes/*.md`** — cited deep-dives into CLI surface, caching internals, config schema, execution semantics, workspace handling, and the tree model.

## Verdict: can Vite Task replace turborepo/nx today?

**For local development on pnpm `workspace:`-style monorepos: yes, often with a better experience.** Automatic input tracking eliminated whole config categories (`inputs`, `globalDependencies`, `namedInputs` translated to *nothing* in most repos), and content-based invalidation is measurably finer than turbo/nx hashing — e.g. a dependency rebuild whose emitted `.d.ts` is byte-identical does **not** invalidate dependents (proven on trpc, query, shadcn, zenstack). The tree model (compound scripts + in-process `vp run` expansion) cleanly replaced `turbo run`/`nx run-many` orchestration scripts, up to a 420-task CI pipeline replaying from cache in one command.

**For CI replacement: not yet.** The blockers, in order: remote/shared cache (every repo's CI depends on it), the corrupt-cache false-green bug, affected/changed-since selection, keep-going mode, and persistent-task semantics. Repos whose internal deps aren't `workspace:` protocol (exact pins, ranges, npm/yarn star versions) silently lose all ordering — the single most dangerous semantic trap, hit in 3 of 9 repos.

**The recurring migration tax** is read-write-overlap whack-a-mole (tools re-reading their own outputs); a default exclusion list plus auto-excluding declared outputs would remove ~80% of it. The recurring **payoff** is deleting config: turbo.json/nx.json knowledge mostly dissolves into "run `vp run -r <task>` and declare outputs".

## Not covered / future work

- Unmigrated coverage dimensions: npm-workspaces star-dep repos (documenso), yarn-berry repos (mitosis, twenty), executor-heavy nx repos (storybook, lerna — likely unmigratable without command rewrites), giant repos (cal.com, n8n).
- e2e/DB-backed test tiers were scoped out everywhere (no services in the verification environment).
- CI workflow rewrites were attempted only on vercel/ai and react-email; others note the requirement in MIGRATION.md.
