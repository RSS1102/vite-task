# Remote Caching

`vp run` keeps a local task cache under `node_modules/.vite/task-cache`. Remote
caching adds an optional **second tier** shared across machines: a task that
misses locally can still hit a result computed earlier by a teammate or a CI
runner, and every fresh result is pushed up so the next machine skips the work.

The local cache always stays authoritative — remote caching never changes
whether a result is _correct_, only whether it can be _reused_ across machines.

## Enabling

Set two environment variables:

```sh
export VITE_REMOTE_CACHE_URL=https://your-cache.example.workers.dev
export VITE_REMOTE_CACHE_TOKEN=<shared bearer token>   # optional
vp run build
```

- `VITE_REMOTE_CACHE_URL` — base URL of the remote cache endpoint. Setting it
  enables the remote tier; leaving it unset disables remote caching entirely.
- `VITE_REMOTE_CACHE_TOKEN` — bearer token sent with every request as
  `Authorization: Bearer <token>`. Optional (a local development endpoint may
  not require auth), but any real deployment should require one.

### Configuring via a `.env` file

Both variables can also be set in a `.env` file in the workspace root, which
keeps the token out of your shell profile and CI environment:

```sh
# .env (workspace root)
VITE_REMOTE_CACHE_URL=https://your-cache.example.workers.dev
VITE_REMOTE_CACHE_TOKEN=<shared bearer token>
```

The process environment wins when a variable is set in both places. The `.env`
is only read for these two variables; it does not otherwise affect task
execution. Add `.env` to `.gitignore` so the token is never committed.

## How it works

Each task lookup is **local-first**:

1. Check the local cache. On a hit, run nothing — replay the captured output and
   restore output files. (No network involved.)
2. On a local miss, ask the remote tier. A remote hit is **backfilled** into the
   local cache — its metadata row and its output archive are written locally —
   so the replay path and every subsequent lookup are identical to a local hit.
3. On a remote miss too, run the task.

After a task runs successfully, its result is pushed to the remote tier
(the output archive first, then the fingerprint metadata).

Remote I/O is **best-effort**: a remote cache outage, timeout, or auth failure
logs a warning and degrades to local behavior — it never fails a build. Remote
interaction is invisible in normal output; a remote-backed hit looks exactly
like a local cache hit.

### What is stored where

- **Output archives** (the `*.tar.zst` of a task's `output` files) are stored as
  opaque blobs, addressed by a content id.
- **Fingerprint metadata** (the cache entry describing inputs, tracked env vars,
  captured logs, and the archive reference; plus the execution-key → cache-key
  mapping used to report _what changed_ on a miss) is stored as small key→blob
  records.

Remote keys are hashed and prefixed with the cache schema version, so clients
pinning different Vite+ versions never read each other's incompatible entries —
the same isolation the local cache gets from its `vN/` directory.

## Hosting on Cloudflare

A reference Cloudflare Worker lives in
[`services/remote-cache-worker`](../services/remote-cache-worker). It stores
output archives in **R2** and fingerprint metadata in **Workers KV**, behind the
bearer-authenticated HTTP API the client expects. See its `README.md` for local
development (`wrangler dev`) and deployment. The client speaks a small, generic
HTTP protocol, so any endpoint implementing it works — Cloudflare is just the
reference host.

## Security

The token is a shared secret: anyone with it can read and write cached task
results (including captured build logs and output files). Treat it like a CI
secret, scope the endpoint to trusted users, and always require a token on a
deployed endpoint.
