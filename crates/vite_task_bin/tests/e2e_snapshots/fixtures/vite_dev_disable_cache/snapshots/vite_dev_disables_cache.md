# vite_dev_disables_cache

`vt run --cache dev` brings up a Vite dev server programmatically in middleware mode and closes it immediately. Middleware mode skips the port listen, but the default Vite dev watcher calls `disableCache()` via `@voidzero-dev/vite-task-client`, so this run is never stored — the next invocation re-executes (cache miss / NotFound).

## `vt run --cache dev`

first run — Vite dev watcher calls disableCache

```
$ node dev.mjs
```

## `vt run --cache dev`

cache miss (NotFound) because the first run was not stored

```
$ node dev.mjs
```
