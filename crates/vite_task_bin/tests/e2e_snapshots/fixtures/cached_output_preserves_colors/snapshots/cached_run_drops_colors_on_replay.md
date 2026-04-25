# cached_run_drops_colors_on_replay

Run the cached task twice. The first run is a cache miss with piped stdio —
the child no longer sees a TTY, and (for the reasons described at the top of
this file) the runner doesn't forward `FORCE_COLOR` either, so the child
writes plain text. The second run replays whatever was cached, so it's also
plain. Compare the snapshot's "Raw output (ANSI escapes visible)" blocks
against `cache_disabled_emits_colors.md`: every SGR code that appears in the
uncached baseline is missing here, demonstrating issue #358.

## `NO_COLOR=<UNSET> vt run test`

cache miss — child runs with piped stdio, emits no colors

```
$ node ./emit-colors.mjs
✓ example.test.ts
✓ another.test.ts

Test Files  2 passed (2)
     Tests  4 passed (4)
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ node ./emit-colors.mjs
\e[m\xe2\x9c\x93 example.test.ts
\xe2\x9c\x93 another.test.tsTest Files  2 passed (2)
     Tests  4 passed (4)
```

## `NO_COLOR=<UNSET> vt run test`

cache hit — replays the plain bytes captured above

```
$ node ./emit-colors.mjs ◉ cache hit, replaying
✓ example.test.ts
✓ another.test.ts

Test Files  2 passed (2)
     Tests  4 passed (4)

---
vt run: cache hit.
```

**Raw output (ANSI escapes visible):**

```
\e[m\e[34m$ node ./emit-colors.mjs\e[m \e[32m\xe2\x97\x89\e[m \e[90mcache hit, replaying
\e[m\xe2\x9c\x93 example.test.ts
\xe2\x9c\x93 another.test.tsTest Files  2 passed (2)
     Tests  4 passed (4)\e[90m---
\e[34;1mvt run:\e[m cache hit, \e[32;1m<duration>\e[m saved.
```
