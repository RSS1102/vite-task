# Decision log

Decisions taken while implementing the two auto-tracking rule sets
(`target/fspy-overlap-experiments/read-write-overlaps.md` and
`output-cleanup.md`) and wiring them through vite-plus into emdash.

Newest entries at the bottom. Each entry records the drift, the choice, and why.

## D1 — Tracer scope: all signals, all three platforms

Confirmed with the requester before starting. The full rule set needs fspy to
gain open success/errno, `O_CREAT`/`O_TRUNC`/`O_EXCL`, confirmed writes, rename
source and destination, unlink/rmdir, and mkdir with errno — on the Unix
preload, the Linux seccomp fallback, and the Windows detours. No platform is
stubbed. `AGENTS.md` forbids skipping tests on either platform.

## D2 — Rules implemented in place, no crate extraction

Confirmed before starting. The `vite_task_fs_cache` extraction stays a separate
follow-up so it keeps its no-behavior-change property. This PR changes behavior
and would obscure that proof.

## D3 — Gitignore via the `ignore` crate

Confirmed before starting. Hand-rolling gitignore precedence, negation and
nesting is easy to get subtly wrong, and shelling out to `git check-ignore` puts
a process spawn on a hot path and fails when git is absent.

## D4 — vite-plus preview PR lives on the upstream repo

Confirmed before starting. `publish-preview.yml` gates on
`github.repository == 'voidzero-dev/vite-plus'` plus a `preview-build` label, so
a fork cannot produce a pkg.pr.new build. Branch is pushed to the upstream repo;
the PR stays a draft and is not merged.

## D5 — Approximate "confirmed write" from open flags, do not intercept `write`

**Drift.** `read-write-overlaps.md` defines a write as a confirmed
`write`/`pwrite`/`writev`/`ftruncate` on a write-opened descriptor. Implementing
that needs a per-process fd table plus interception of `close`, `dup`, `dup2` and
`dup3`, because a descriptor number can otherwise be reacquired by an owner the
tracer never saw. I hit exactly that bug in the experiment probe, where
`vite.config.mjs` looked written because fd 18 was reused.

**Decision.** Record open success and the `O_CREAT`, `O_TRUNC`, `O_EXCL` flags
instead, and treat a mutation as `O_TRUNC`, or `O_CREAT | O_EXCL`, or a rename
destination. A bare write-access open is recorded as capability only and is not
a mutation.

**Why this is sufficient.** Checked against every case in the experiment matrix:

- Prettier, ESLint, Stylelint, Vite, Astro all rewrite through
  `O_CREAT | O_TRUNC`, so they are still detected.
- Atomic writers create temporaries with `O_CREAT | O_EXCL` and publish by
  rename, so both halves are still detected.
- Biome's warm run opens a clean source `O_RDWR` with no truncation and does not
  write. It is now correctly *not* a mutation, which is the false positive the
  rule exists to remove.
- Lock files across cargo, rustc and Parcel are opened `O_RDWR | O_CREAT`
  without truncation and only flocked, so they stop being collected.

**What it gives up.** Mutation through a writable mmap is invisible. The only
instance in the matrix is Parcel's `lock.mdb`, a lock file that should not be
archived anyway. Recorded as a known gap rather than fixed.

## D6 — Skip `getdents` success; use mkdir instead

**Drift.** `output-cleanup.md` lists "errno on getdents or scandir" as the way to
tell that a directory listing failed, which matters for the
`unconditional-clean` case where a tool lists a directory that does not exist.

**Decision.** Do not add it. Fix 3, "a directory the task created is not an
input", covers the same case using successful `mkdir`, which is far cheaper to
intercept and which the experiment already showed absorbs 264 of 307 derived
directory listings including all 256 of Go's cache shards.
