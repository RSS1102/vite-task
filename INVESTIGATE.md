# Things to investigate

Found while verifying automatic input/output tracking (#571) against
[emdash](https://github.com/emdash-cms/emdash): 20 packages, 25 build tasks,
mostly plain `tsdown`.

Neither item is a regression from #571 — both reproduce as described with that
branch's rules in place, and #2b is a case the rules were meant to cover but
attribute by the wrong path.

Both trace to the same underlying fact. pnpm links every workspace package into
`node_modules/<name>`, and `packages/core` is the package named `emdash`:

```
node_modules/emdash -> ../packages/core
```

So `packages/core/dist` and `node_modules/emdash/dist` are the same directory
under two names. Tracking attributes accesses by path, so a write recorded
against one name is not recognised as touching the other.

**Environment.** `vp 0.0.0-commit.88ab4592d55342c11a070c47e8fc7205f675fda3`
(preview of voidzero-dev/vite-plus#2260, carrying vite-task #571), emdash branch
`agent/vite-plus-package-build-cache`. Local: macOS 26.5.1, M4 Pro, 12 cores.
CI: `ubuntu-latest`, 4 vCPU.

---

## 1. `--parallel` appears to fingerprint partially written dependency output

**Severity:** correctness-adjacent, not just a hit-rate loss. A stored cache entry
describes a dependency directory that was mid-write, so the entry is keyed on
torn state. It does not currently produce a wrong tree, because the next run
detects the difference and reruns — but it is a cache entry that never should
have been stored.

### Observation

With the default concurrency limit (4), the warm run is a full hit:

```
vp run: 25/25 cache hit (100%)
```

With `--parallel` (unlimited concurrency), the warm run reproducibly loses 7
tasks. Three consecutive cold+warm pairs, each after `rm -rf
node_modules/.vite/task-cache`:

| run | warm result     |
| --- | --------------- |
| 1   | 18/25 cache hit |
| 2   | 18/25 cache hit |
| 3   | 18/25 cache hit |

An earlier pass gave 19/25, so the count is not perfectly fixed.

### Reproduction

```bash
cd emdash
rm -rf node_modules/.vite/task-cache
pnpm exec vp run --cache --parallel --filter '{./packages/**}' build   # cold
pnpm exec vp run --cache --parallel --filter '{./packages/**}' build   # warm
```

### Evidence

Every miss names a file _appearing_ in `node_modules/emdash/dist`, i.e. in
`packages/core`'s output:

```
~/packages/cloudflare$ tsdown                    ○ cache miss: 'index.d.mts' added in 'node_modules/emdash/dist/seed'
~/packages/workerd$ tsdown                       ○ cache miss: 'index.mjs' added in 'node_modules/emdash/dist'
~/packages/plugins/sandboxed-test$ node ...build ○ cache miss: 'plugin-types.mjs' added in 'node_modules/emdash/dist'
~/packages/plugins/webhook-notifier$ node ...build ○ cache miss: 'plugin-types.mjs' added in 'node_modules/emdash/dist'
~/packages/plugins/audit-log$ node ...build      ○ cache miss: 'plugin-types.d.mts' added in 'node_modules/emdash/dist'
~/packages/plugins/atproto$ node ...build        ○ cache miss: 'plugin-types.mjs' added in 'node_modules/emdash/dist'
~/packages/plugins/marketplace-test$ node ...build ○ cache miss: 'plugin-types.d.mts' added in 'node_modules/emdash/dist'
```

The readers are all consumers of `emdash`. The writer is `packages/core#build`.
Which file is named varies between runs — `index.d.mts`, `plugin-types.mjs`,
`middleware.d.mts`, `request-context.d.mts` — which is what a race looks like.

### Hypothesis (not yet confirmed)

`added in` means the directory had **more** entries at fingerprint time than when
the entry was stored. So the corruption is on the **cold** run: consumers
fingerprinted `node_modules/emdash/dist` while `packages/core#build` was still
writing into it, storing an entry that describes an incomplete directory. The
warm run then sees the finished directory and correctly reports a difference.

That implies the consumers were running concurrently with `packages/core#build`,
which raises the open question below.

### Open questions

- Does `--parallel` still respect dependency order? Concurrency is described as
  per graph level (`vite_task_plan/src/plan.rs:742`), which should keep
  `packages/core#build` in an earlier level than its consumers. If ordering holds,
  consumers should never observe a partial `dist`, and the hypothesis above is
  wrong — so what else explains `added in`?
- A single writer is confirmed, so this is not two tasks writing the same
  directory. Every file named in the misses is `packages/core`'s own output:
  `plugin-types.mjs`, `plugin-types.d.mts` and `index.mjs` all live in
  `packages/core/dist/`, produced by core's own `tsdown` entry points. The
  separate `@emdash-cms/plugin-types` package builds into its own `dist` and does
  not write here. That leaves timing, not ownership, as the thing to explain.
- Should a directory fingerprint taken while another task in the same run is
  writing to that directory be storable at all, or should the run decline to cache
  the reader?

### Suggested next step

Re-run with `VITE_TASK_DEBUG_TRACKING=1` on the cold `--parallel` run and compare
the recorded write timestamps for `packages/core#build` against the reads from
consumers, to establish whether the two actually overlap in time. That settles
ordering versus second-writer before any fix is designed.

---

## 2. Three tasks never hit on emdash, even with the default limit

Steady state on CI with the concurrency limit at its default, and with no change
touching any package:

```
vp run: 22/25 cache hit (88%), 115.61s saved
```

The same three tasks miss every run:

```
~/packages/registry-lexicons$ pnpm run build:lexicons ○ cache miss: 'node_modules/.pnpm-workspace-state-v1.json' modified
~/packages/registry-lexicons$ pnpm run build:types    ○ cache miss: 'node_modules/.pnpm-workspace-state-v1.json' modified
~/packages/core$ tsdown                               ○ cache miss: 'index.d.mts' removed from 'node_modules/emdash/dist'
```

### 2a. `registry-lexicons` reads pnpm's workspace state file

Both tasks run through `pnpm run ...`, so the `pnpm` process itself reads
`node_modules/.pnpm-workspace-state-v1.json`. pnpm rewrites that file on install,
so its content cannot survive a fresh CI checkout, and the fingerprint can never
settle across machines.

This is the case upstream documents under
[When To Add Manual Config](https://viteplus.dev/guide/automatic-data-tracking#when-to-add-manual-config),
and the immediate workaround is a manual `input` exclusion in emdash.

**Worth deciding upstream:** whether this belongs in the same always-ignored set
as `node_modules` and `.git`. It is a package manager's own bookkeeping, it is
never an authored source, and any task invoked via `pnpm run` will read it — so
every pnpm workspace that wraps a task in `pnpm run` hits this. Note the file
already lives under `node_modules`, so the existing rule does not catch it: that
rule only resolves read-then-write overlaps, and this is a pure read, which is
legitimately an input.

### 2b. `packages/core` reads its own output through the `node_modules` alias

`packages/core` is the package named `emdash`, so pnpm self-links it and
`tsdown` resolves the package's own entry points through
`node_modules/emdash/dist/...`. The task therefore reads its own output.

#571 added a rule for exactly this shape — a gitignored path underneath a
directory the task wrote into is the task's own derived state, not an input — but
it attributes by path. The write is recorded at `packages/core/dist/index.d.mts`
and the read at `node_modules/emdash/dist/index.d.mts`. The read path is not
lexically under `packages/core/`, so the rule does not fire.

On a fresh checkout the directory does not exist yet when the task starts, which
is why the miss reads `removed from` rather than `modified`.

**Fix direction to evaluate:** resolve symlinks when attributing an access, so
both names collapse to one identity before classification. Open questions:

- Cost. This would mean a `realpath`-style resolution per tracked path, on a hot
  path that currently does string work only. Resolving just the
  `node_modules/<name>` prefix may be enough and much cheaper.
- Where to resolve. Doing it in the tracer changes what every backend reports and
  loses the name the task actually used, which is the name that appears in cache
  miss messages. Doing it in classification keeps reporting intact.
- Whether the workspace already knows the answer. `vite_workspace` maps package
  names to directories, so `node_modules/emdash` -> `packages/core` may be
  derivable without touching the filesystem at all. This looks like the cheapest
  option and should be checked first.
- Correctness limit: this fixes the alias case, not hard links, and not two
  distinct symlinks into the same tree.

### Note on measurement

These three misses cost real time but are not why the cache is useful here: the
same run still saved 115.61s. Fixing 2a and 2b would take emdash from 22/25 to
25/25 on CI, matching what a warm local run already achieves.
