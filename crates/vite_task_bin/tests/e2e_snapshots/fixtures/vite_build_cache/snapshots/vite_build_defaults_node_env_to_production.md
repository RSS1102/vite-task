# vite_build_defaults_node_env_to_production

When the runner has no `NODE_ENV`, Vite's `getEnv('NODE_ENV')` call must return JavaScript `undefined`. This lets Vite apply its normal production-build default instead of assigning the string `"null"` to `process.env.NODE_ENV`.

## `vt run --cache build-production-node-env`

Vite config hook observes NODE_ENV=production

```
$ ASSERT_NODE_ENV=production vite build
```
