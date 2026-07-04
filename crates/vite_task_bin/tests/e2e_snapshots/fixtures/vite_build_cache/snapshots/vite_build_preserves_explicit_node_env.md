# vite_build_preserves_explicit_node_env

When the runner has an explicit `NODE_ENV`, Vite's `getEnv('NODE_ENV')` call must return that value instead of applying the production-build default.

## `NODE_ENV=test vt run --cache build-test-node-env`

Vite config hook observes NODE_ENV=test

```
$ ASSERT_NODE_ENV=test vite build
```
