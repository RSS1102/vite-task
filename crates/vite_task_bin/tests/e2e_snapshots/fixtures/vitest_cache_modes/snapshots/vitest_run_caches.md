# vitest_run_caches

`vitest run` creates a Vite server for transforms, but it does not listen on a port and Vitest sets `server.watch = null` in run mode. Vitest's own result cache is disabled so this fixture isolates Vite's task-cache signal; the task should stay cacheable.

## `vt run --cache vitest-run`

first run: Vitest uses Vite as a non-watching transform server

```
$ vitest run --no-cache --reporter=./quiet-reporter.mjs
```

## `vt run --cache vitest-run`

cache hit: no port listen or filesystem watcher opted out

```
$ vitest run --no-cache --reporter=./quiet-reporter.mjs ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vt run --last-details`

summary shows the cached replay

```

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    Vite+ Task Runner • Execution Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Statistics:   1 tasks • 1 cache hits • 0 cache misses
Performance:  100% cache hit rate

Task Details:
────────────────────────────────────────────────
  [1] vitest-cache-modes-fixture#vitest-run: $ vitest run --no-cache --reporter=./quiet-reporter.mjs ✓
      → Cache hit - output replayed -
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```
