# ansi_colors_preserved_across_cache_hit

Running the same cached task twice should yield byte-identical user-visible
output on the cache hit. The "Raw output (ANSI escapes visible)" block lets
the snapshot show whether SGR sequences survived the cache write/read cycle —
issue #358 reports they are dropped on cache hit.

## `vt run test`

cache miss — original colored output is captured

```
$ vtt print-file vitest_output.txt
 DEV   v3.2.4  /tmp/vitest-fixture

 ✓ example.test.ts (2 tests)

 Test Files  1 passed (1)
      Tests  2 passed (2)
   Start at  10:24:18
   Duration  <duration>
```

**Raw output (ANSI escapes visible):**

```
\e[m$ vtt print-file vitest_output.txt
\e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture\e[m \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)\e[m \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m      \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m   \e[2mStart at\e[m  10:24:18
   \e[2mDuration\e[m  <duration>
```

## `vt run test`

cache hit — colored output should be replayed verbatim

```
$ vtt print-file vitest_output.txt ◉ cache hit, replaying
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
\e[m$ vtt print-file vitest_output.txt \xe2\x97\x89 cache hit, replaying
\e[36;1m DEV \e[m  \e[34mv3.2.4\e[m  \e[90m/tmp/vitest-fixture\e[m \e[32m\xe2\x9c\x93\e[m \e[2mexample.test.ts\e[m \e[90m(2 tests)\e[m \e[2mTest Files\e[m  \e[32;1m1 passed\e[m \e[90m(1)
\e[m      \e[2mTests\e[m  \e[32;1m2 passed\e[m \e[90m(2)
\e[m   \e[2mStart at\e[m  10:24:18
   \e[2mDuration\e[m  <duration>---
vt run: cache hit.
```
