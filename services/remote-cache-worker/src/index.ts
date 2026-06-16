/**
 * Vite+ remote task cache — Cloudflare Worker.
 *
 * Fronts two storage primitives behind one bearer-authenticated HTTP API that
 * the `vp run` client speaks:
 *
 *   - Workers KV holds the fingerprint maps (cache entries + execution-key
 *     mappings). Pure key -> blob lookups; values are small and well under KV's
 *     25 MiB limit.
 *   - R2 holds the output archives (`*.tar.zst`), which can be large.
 *
 * Protocol (all blobs are raw bodies, no framing):
 *
 *   GET  /v1/entries/:key        -> 200 + blob | 404
 *   PUT  /v1/entries/:key        -> 204
 *   GET  /v1/fingerprints/:key   -> 200 + blob | 404
 *   PUT  /v1/fingerprints/:key   -> 204
 *   GET  /v1/artifacts/:id       -> 200 + bytes | 404
 *   PUT  /v1/artifacts/:id       -> 204
 *
 * The `:key` values are opaque, schema-versioned hashes produced by the client
 * (see `remote_kv_key` in the Rust crate); the worker treats them as opaque.
 */

export interface Env {
  /** KV namespace holding the fingerprint maps. */
  CACHE_KV: KVNamespace;
  /** R2 bucket holding the output archives. */
  CACHE_R2: R2Bucket;
  /**
   * Shared bearer token. When set, every request must carry
   * `Authorization: Bearer <token>`. When unset (e.g. local `wrangler dev`
   * without `.dev.vars`), requests are allowed unauthenticated — never deploy
   * to production without setting it via `wrangler secret put AUTH_TOKEN`.
   */
  AUTH_TOKEN?: string;
}

/** The two KV-backed namespaces, mirroring the client's `RemoteNamespace`. */
const KV_NAMESPACES = new Set(['entries', 'fingerprints']);

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    const parts = pathname.split('/').filter((p) => p.length > 0);

    // Health check / root — unauthenticated, so liveness/readiness probes
    // don't need the token.
    if (parts.length === 0) {
      return new Response('vite+ remote cache\n', { status: 200 });
    }

    // Every cache route requires auth (when a token is configured).
    if (!isAuthorized(request, env)) {
      return new Response('unauthorized', { status: 401 });
    }

    // All cache routes live under /v1/<store>/<key>.
    if (parts.length !== 3 || parts[0] !== 'v1') {
      return new Response('not found', { status: 404 });
    }
    const [, store, key] = parts;
    if (key.length === 0) {
      return new Response('missing key', { status: 400 });
    }

    if (store === 'artifacts') {
      return handleArtifact(request, env, key);
    }
    if (KV_NAMESPACES.has(store)) {
      return handleKv(request, env, store, key);
    }
    return new Response('not found', { status: 404 });
  },
} satisfies ExportedHandler<Env>;

/**
 * Validate the bearer token. Returns true when no token is configured (local
 * development) or when the request presents the matching token.
 */
function isAuthorized(request: Request, env: Env): boolean {
  const expected = env.AUTH_TOKEN;
  if (!expected) {
    return true;
  }
  const header = request.headers.get('Authorization');
  return header === `Bearer ${expected}`;
}

/** GET/PUT a fingerprint blob in KV, namespaced by store to avoid collisions. */
async function handleKv(request: Request, env: Env, store: string, key: string): Promise<Response> {
  const kvKey = `${store}:${key}`;
  switch (request.method) {
    case 'GET': {
      const value = await env.CACHE_KV.get(kvKey, 'arrayBuffer');
      if (value === null) {
        return new Response('not found', { status: 404 });
      }
      return new Response(value, {
        status: 200,
        headers: { 'Content-Type': 'application/octet-stream' },
      });
    }
    case 'PUT': {
      const body = await request.arrayBuffer();
      await env.CACHE_KV.put(kvKey, body);
      return new Response(null, { status: 204 });
    }
    default:
      return new Response('method not allowed', { status: 405 });
  }
}

/** GET/PUT an output archive in R2. */
async function handleArtifact(request: Request, env: Env, id: string): Promise<Response> {
  switch (request.method) {
    case 'GET': {
      const object = await env.CACHE_R2.get(id);
      if (object === null) {
        return new Response('not found', { status: 404 });
      }
      return new Response(object.body, {
        status: 200,
        headers: { 'Content-Type': 'application/octet-stream' },
      });
    }
    case 'PUT': {
      // Stream the body straight into R2 so large archives never buffer fully.
      await env.CACHE_R2.put(id, request.body, {
        httpMetadata: { contentType: 'application/octet-stream' },
      });
      return new Response(null, { status: 204 });
    }
    default:
      return new Response('method not allowed', { status: 405 });
  }
}
