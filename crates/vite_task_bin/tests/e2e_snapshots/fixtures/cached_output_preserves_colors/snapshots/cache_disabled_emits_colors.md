# cache_disabled_emits_colors

Baseline: with `cache: false`, stdio is inherited so vitest sees the PTY as
a TTY and emits its full color output (cyan `RUN` banner, green checkmarks,
bold green `passed`). This is the "good" output the cache-hit replay is
expected to match.

## `NO_COLOR=<UNSET> TERM=xterm-256color vt run test-uncached`

```
$ vitest run ⊘ cache disabled

 RUN  v<vitest_version> <workspace>

 ✓ example.test.ts (2 tests) <duration>
   ✓ addition <duration>
   ✓ string concat <duration>

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  <start_at>
   Duration  <duration> (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ vitest run\e[m \e[30m\xe2\x8a\x98\e[m \e[90mcache disabled\e[39;46;1m RUN \e[m \e[36mv<vitest_version> \e[90m<workspace>\e[m \e[32m\xe2\x9c\x93\e[m example.test.ts \e[2m(2 tests)\e[32;22m <duration>
\e[m   \e[32m\xe2\x9c\x93\e[m addition\e[32m <duration>
\e[m   \e[32m\xe2\x9c\x93\e[m string concat\e[32m <duration>\e[39m Test Files \e[m \e[32;1m1 passed\e[90;22m (1)
\e[39;2m      Tests \e[m \e[32;1m2 passed\e[90;22m (2)
\e[39;2m   Start at \e[m <start_at>
\e[2m   Duration \e[m <duration>\e[2m (transform <duration>, setup <duration>, collect <duration>, tests <duration>, environment <duration>, prepare <duration>)\e[m
```
