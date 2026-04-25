# ansi_colors_preserved_across_cache_hit_labeled

Same scenario as above, but with `--log=labeled` so output flows through
`LabeledWriter` (which prefixes each line with the task label). The label
prefix must not strip or corrupt SGR escape sequences inside the line.

## `vt run --log=labeled test`

cache miss — labeled lines from live stdout

```
[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt
[@test/cached-output-preserves-colors#test]  DEV   v3.2.4  /tmp/vitest-fixture
[@test/cached-output-preserves-colors#test] 
[@test/cached-output-preserves-colors#test]  ✓ example.test.ts (2 tests)
[@test/cached-output-preserves-colors#test] 
[@test/cached-output-preserves-colors#test]  Test Files  1 passed (1)
[@test/cached-output-preserves-colors#test]       Tests  2 passed (2)
[@test/cached-output-preserves-colors#test]    Start at  10:24:18
[@test/cached-output-preserves-colors#test]    Duration  <duration>
```

**Raw output (ANSI escapes visible):**

```
\e[m[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt
[@test/cached-output-preserves-colors#test] \e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture
\e[m[@test/cached-output-preserves-colors#test]
[@test/cached-output-preserves-colors#test]  \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)
\e[m[@test/cached-output-preserves-colors#test]
[@test/cached-output-preserves-colors#test]  \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m[@test/cached-output-preserves-colors#test]       \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m[@test/cached-output-preserves-colors#test]    \e[2mStart at\e[m  10:24:18
[@test/cached-output-preserves-colors#test]    \e[2mDuration\e[m  <duration>
```

## `vt run --log=labeled test`

cache hit — labeled lines from cached stdout

```
[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt ◉ cache hit, replaying
[@test/cached-output-preserves-colors#test]  DEV   v3.2.4  /tmp/vitest-fixture
[@test/cached-output-preserves-colors#test] 
[@test/cached-output-preserves-colors#test]  ✓ example.test.ts (2 tests)
[@test/cached-output-preserves-colors#test] 
[@test/cached-output-preserves-colors#test]  Test Files  1 passed (1)
[@test/cached-output-preserves-colors#test]       Tests  2 passed (2)
[@test/cached-output-preserves-colors#test]    Start at  10:24:18
[@test/cached-output-preserves-colors#test]    Duration  <duration>

---
vt run: cache hit.
```

**Raw output (ANSI escapes visible):**

```
\e[m[@test/cached-output-preserves-colors#test] $ vtt print-file vitest_output.txt \xe2\x97\x89 cache hit, replaying
[@test/cached-output-preserves-colors#test] \e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture
\e[m[@test/cached-output-preserves-colors#test]
[@test/cached-output-preserves-colors#test]  \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)
\e[m[@test/cached-output-preserves-colors#test]
[@test/cached-output-preserves-colors#test]  \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m[@test/cached-output-preserves-colors#test]       \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m[@test/cached-output-preserves-colors#test]    \e[2mStart at\e[m  10:24:18
[@test/cached-output-preserves-colors#test]    \e[2mDuration\e[m  <duration>---
vt run: cache hit.
```
