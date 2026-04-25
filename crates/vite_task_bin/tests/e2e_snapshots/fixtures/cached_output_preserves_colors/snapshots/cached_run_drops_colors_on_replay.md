# cached_run_drops_colors_on_replay

Run the cached `vitest run` task twice. The first run is a cache miss with
piped stdio — vitest no longer sees a TTY, and (under the harness's default
`TERM=dumb`) the runner doesn't auto-add `FORCE_COLOR` either, so vitest
writes plain bytes. The second run replays whatever the first one cached, so
it's plain too. Compare the snapshot's "Raw output (ANSI escapes visible)"
blocks against `cache_disabled_emits_colors.md`: every SGR code that appears
in the uncached baseline is missing here, demonstrating issue #358.

## `NO_COLOR=<UNSET> vt run test`

cache miss — vitest runs with piped stdio, emits no colors

```
$ vitest run

 RUN  v<vitest_version> <workspace>

 ✓ example.test.ts (2 tests) <duration>

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  <start_at>
   Duration  <duration> (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ vitest run\e[m RUN  v<vitest_version> <workspace> \xe2\x9c\x93 example.test.ts (2 tests) <duration> Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  <start_at>
   Duration  <duration> (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)
```

## `NO_COLOR=<UNSET> vt run test`

cache hit — replays the plain bytes captured above

```
$ vitest run ◉ cache hit, replaying

 RUN  v<vitest_version> <workspace>

 ✓ example.test.ts (2 tests) <duration>

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  <start_at>
   Duration  <duration> (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)


---
vt run: cache hit.
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ vitest run\e[m \e[32m\xe2\x97\x89\e[m \e[90mcache hit, replaying\e[m RUN  v<vitest_version> <workspace> \xe2\x9c\x93 example.test.ts (2 tests) <duration> Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  <start_at>
   Duration  <duration> (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)\e[90m---
\e[34;1mvt run:\e[m cache hit, \e[32;1m<duration>\e[m saved.
```
