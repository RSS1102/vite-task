# ansi_colors_preserved_across_cache_hit_grouped

Same scenario as above, but with `--log=grouped` so output flows through
`GroupedWriter` (which buffers all bytes for a task and replays them as one
block). The grouped block on the cache hit must contain the same SGR escape
sequences as the original run.

## `vt run --log=grouped test`

cache miss — grouped block built from live stdout

```
[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt
── [@test/cached-output-preserves-colors#test] ──
 DEV   v3.2.4  /tmp/vitest-fixture

 ✓ example.test.ts (2 tests)

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  10:24:18
   Duration  <duration>
```

**Raw output (ANSI escapes visible):**

```
\e[m[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt
\xe2\x94\x80\xe2\x94\x80 [@test/cached-output-preserves-colors#test] \xe2\x94\x80\xe2\x94\x80
\e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture\e[m \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)\e[m \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m      \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m   \e[2mStart at\e[m  10:24:18
   \e[2mDuration\e[m  <duration>
```

## `vt run --log=grouped test`

cache hit — grouped block built from cached stdout

```
[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt ◉ cache hit, replaying
── [@test/cached-output-preserves-colors#test] ──
 DEV   v3.2.4  /tmp/vitest-fixture

 ✓ example.test.ts (2 tests)

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  10:24:18
   Duration  <duration>

---
vt run: cache hit.
```

**Raw output (ANSI escapes visible):**

```
\e[m[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt \xe2\x97\x89 cache hit, replaying
\xe2\x94\x80\xe2\x94\x80 [@test/cached-output-preserves-colors#test] \xe2\x94\x80\xe2\x94\x80
\e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture\e[m \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)\e[m \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m      \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m   \e[2mStart at\e[m  10:24:18
   \e[2mDuration\e[m  <duration>---
vt run: cache hit.
```
