# cached_run_preserves_colors_on_replay

Run the cached `vitest run` task twice. The first run is a cache miss with
piped stdio — vitest no longer sees a TTY, so colors only survive if the
runner forwards `FORCE_COLOR` to the child. The second run replays the
captured bytes; the colors recorded above must reappear. Compare the
snapshot's "Raw output (ANSI escapes visible)" blocks against
`cache_disabled_emits_colors.md`: every SGR code in the uncached baseline
should also appear here. The repro of issue #358 is the inverse of this
assertion — a regression that drops colors would show as missing SGR
sequences.

## `NO_COLOR=<UNSET> vt run test`

cache miss — vitest runs with piped stdio, FORCE_COLOR forwarded so colors are captured

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
\e[m\e[34m$ vitest run\e[39;46;1m RUN \e[m \e[36mv<vitest_version> \e[90m<workspace>\e[m \e[32m\xe2\x9c\x93\e[m example.test.ts \e[2m(2 tests)\e[32;22m <duration>\e[39m Test Files \e[m \e[32;1m1 passed\e[90;22m (1)
\e[39;2m      Tests \e[m \e[32;1m2 passed\e[90;22m (2)
\e[39;2m   Start at \e[m <start_at>
\e[2m   Duration \e[m <duration>\e[2m (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)\e[m
```

## `NO_COLOR=<UNSET> vt run test`

cache hit — replays the colored bytes captured above

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
\e[m\e[34m$ vitest run\e[m \e[32m\xe2\x97\x89\e[m \e[90mcache hit, replaying\e[39;46;1m RUN \e[m \e[36mv<vitest_version> \e[90m<workspace>\e[m \e[32m\xe2\x9c\x93\e[m example.test.ts \e[2m(2 tests)\e[32;22m <duration>\e[39m Test Files \e[m \e[32;1m1 passed\e[90;22m (1)
\e[39;2m      Tests \e[m \e[32;1m2 passed\e[90;22m (2)
\e[39;2m   Start at \e[m <start_at>
\e[2m   Duration \e[m <duration>\e[2m (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)\e[90;22m---
\e[34;1mvt run:\e[m cache hit, \e[32;1m<duration>\e[m saved.
```
