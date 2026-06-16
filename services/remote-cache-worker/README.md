# Vite+ remote cache — Cloudflare Worker

A Cloudflare Worker that backs Vite+ remote task caching. It fronts two
storage primitives behind one bearer-authenticated HTTP API:

- **R2** (`CACHE_R2`) stores output archives (`*.tar.zst`).
- **Workers KV** (`CACHE_KV`) stores the fingerprint maps (cache entries and
  execution-key → cache-key mappings).

The `vp run` client (see `crates/vite_task/src/session/cache/remote.rs`) talks
to it when `VITE_REMOTE_CACHE_URL` is set. The local cache stays authoritative;
the remote tier just lets independent machines share results.

## HTTP API

All blobs are raw request/response bodies (no framing). `:key` values are
opaque, schema-versioned hashes produced by the client.

| Method | Path                    | Behavior                     |
| ------ | ----------------------- | ---------------------------- |
| `GET`  | `/`                     | `200` health check (no auth) |
| `GET`  | `/v1/entries/:key`      | `200` + blob, or `404`       |
| `PUT`  | `/v1/entries/:key`      | `204`                        |
| `GET`  | `/v1/fingerprints/:key` | `200` + blob, or `404`       |
| `PUT`  | `/v1/fingerprints/:key` | `204`                        |
| `GET`  | `/v1/artifacts/:id`     | `200` + bytes, or `404`      |
| `PUT`  | `/v1/artifacts/:id`     | `204`                        |

When `AUTH_TOKEN` is configured, every `/v1/*` request must carry
`Authorization: Bearer <AUTH_TOKEN>` or it is rejected with `401`. The `/`
health check is always unauthenticated.

## Local development

```bash
npm install
cp .dev.vars.example .dev.vars   # sets AUTH_TOKEN=local-dev-token
npm run dev                      # serves http://localhost:8787, simulating KV + R2 locally
```

Point the client at it:

```bash
export VITE_REMOTE_CACHE_URL=http://localhost:8787
export VITE_REMOTE_CACHE_TOKEN=local-dev-token
vp run build
```

`wrangler dev` simulates KV and R2 on disk under `.wrangler/`, so no Cloudflare
account is needed to exercise the full client ↔ worker round-trip.

## Deploying

```bash
wrangler r2 bucket create vite-remote-cache
wrangler kv namespace create CACHE_KV     # copy the printed id into wrangler.toml
wrangler secret put AUTH_TOKEN            # set a strong shared token
wrangler deploy
```

Then configure the client with the deployed URL and the same token:

```bash
export VITE_REMOTE_CACHE_URL=https://vite-remote-cache.<your-subdomain>.workers.dev
export VITE_REMOTE_CACHE_TOKEN=<the AUTH_TOKEN you set>
```

> **Note:** never deploy without setting `AUTH_TOKEN` — an unset token allows
> unauthenticated access (intended only for local `wrangler dev`).

### D1 as an alternative to KV

KV is used here because the fingerprint stores are pure key→blob lookups and KV
values can be up to 25 MiB. D1 (SQLite) is a viable alternative if you want
strong read-after-write consistency or to mirror the local cache's two-table
schema; swap the `CACHE_KV` binding for a `[[d1_databases]]` binding and replace
the `get`/`put` calls in `handleKv` with prepared statements.
