# remote_cache_restores_across_machines

End-to-end remote caching against a real Cloudflare Worker running under local
`wrangler dev` (R2 + KV emulated). A `remote-cache-start` task launches the
worker and writes its (dynamic) URL and bearer token to files; `vt run build`
reads them via VITE_REMOTE_CACHE_URL_FILE / VITE_REMOTE_CACHE_TOKEN_FILE, so the
port and secret never appear in this snapshot.

This validates the cache UPDATE path end to end:
  1. the first build misses both caches, executes, and updates the cache;
  2. an immediate re-build is a LOCAL cache hit — proving the run updated the
     local cache;
  3. after wiping the local cache (simulating a fresh machine) the build still
     hits — proving the update was also pushed to the remote tier — and restores
     the output file from the remote archive.

Requires Node + wrangler (from packages/tools), so this case is `ignore`d and
runs only with `cargo test -- --include-ignored`.

## `vt run remote-cache-start`

launch the local wrangler dev worker; writes url/token files

```
$ node scripts/remote-cache-dev.mjs start --state-dir .remote-cache --url-file .remote-cache/url --token-file .remote-cache/token ⊘ cache disabled
```

## `VITE_REMOTE_CACHE_URL_FILE=.remote-cache/url VITE_REMOTE_CACHE_TOKEN_FILE=.remote-cache/token vt run build`

first build — local + remote miss, executes and updates both caches

```
$ vtt write-file dist/output.txt built
```

## `VITE_REMOTE_CACHE_URL_FILE=.remote-cache/url VITE_REMOTE_CACHE_TOKEN_FILE=.remote-cache/token vt run build`

re-build, nothing changed — LOCAL cache hit proves the first run updated the cache

```
$ vtt write-file dist/output.txt built ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt print-file dist/output.txt`

output is on disk after the run

```
built
```

## `vtt rm -rf dist`

delete the output so a restore from the remote archive is observable

```
```

## `vtt rm -rf node_modules/.vite/task-cache`

wipe the LOCAL cache to simulate a fresh machine

```
```

## `VITE_REMOTE_CACHE_URL_FILE=.remote-cache/url VITE_REMOTE_CACHE_TOKEN_FILE=.remote-cache/token vt run build`

empty local cache — this hit can only come from the remote tier the first run updated

```
$ vtt write-file dist/output.txt built ◉ cache hit, replaying

---
vt run: cache hit.
```

## `vtt print-file dist/output.txt`

output restored from the remote archive

```
built
```

## `vt run remote-cache-stop`

shut the worker down

```
$ node scripts/remote-cache-dev.mjs stop --state-dir .remote-cache ⊘ cache disabled
```
