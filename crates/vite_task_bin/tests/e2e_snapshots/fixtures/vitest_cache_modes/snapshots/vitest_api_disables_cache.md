# vitest_api_disables_cache

`vitest --api` binds a Vite/Vitest API server port. Binding the port should call `disableCache()` immediately before listening, so completed API runs are not stored.

## `vt run --cache vitest-api`

first run: --api listens on a port and opts out

```
$ vitest --api=51289 --run --no-cache --reporter=./quiet-reporter.mjs
```

## `vt run --cache vitest-api`

re-executes because the API run was not cached

```
$ vitest --api=51289 --run --no-cache --reporter=./quiet-reporter.mjs
```

## `vt run --last-details`

summary names the port-listen opt-out

```

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    Vite+ Task Runner • Execution Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Statistics:   1 tasks • 0 cache hits • 1 cache misses
Performance:  0% cache hit rate

Task Details:
────────────────────────────────────────────────
  [1] vitest-cache-modes-fixture#vitest-api: $ vitest --api=51289 --run --no-cache --reporter=./quiet-reporter.mjs ✓
      → Not cached: the task opted out of caching
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```
