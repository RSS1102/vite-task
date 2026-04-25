# cache_disabled_emits_colors

Baseline: with `cache: false`, stdio is inherited so the child sees the PTY
as a TTY and unconditionally emits colors. This is the "good" output the
cache-hit replay should match.

## `NO_COLOR=<UNSET> vt run test-uncached`

```
$ node ./emit-colors.mjs ⊘ cache disabled
✓ example.test.ts
✓ another.test.ts

Test Files  2 passed (2)
     Tests  4 passed (4)
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ node ./emit-colors.mjs\e[m \e[30m\xe2\x8a\x98\e[m \e[90mcache disabled
\e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts
\e[32;22m\xe2\x9c\x93\e[m \e[2manother.test.ts\e[mTest Files  \e[32;1m2 passed\e[m (2)
     Tests  \e[32;1m4 passed\e[m (4)
```
