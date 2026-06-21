# vitest_watch_disables_cache

`vitest watch` starts Vite's filesystem watcher. Vitest's own result cache is disabled so this fixture isolates Vite's task-cache signal. The custom reporter exits after the first completed test run so the e2e can assert that the completed watch-mode task still opted out of caching.

## `VITEST_EXIT_AFTER_RUN=1 vt run --cache vitest-watch`

first run: watch mode observes the filesystem and opts out

```
$ vitest watch --no-cache --reporter=./quiet-reporter.mjs
```

## `VITEST_EXIT_AFTER_RUN=1 vt run --cache vitest-watch`

re-executes because the watch run was not cached

```
$ vitest watch --no-cache --reporter=./quiet-reporter.mjs
```

## `vt run --last-details`

summary names the watcher opt-out

```

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    Vite+ Task Runner • Execution Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Statistics:   1 tasks • 0 cache hits • 1 cache misses
Performance:  0% cache hit rate

Task Details:
────────────────────────────────────────────────
  [1] vitest-cache-modes-fixture#vitest-watch: $ vitest watch --no-cache --reporter=./quiet-reporter.mjs ✓
      → Not cached: the task opted out of caching
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```
