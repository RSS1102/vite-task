# Runner-Aware Tools in Vite+

`vp build` now has a better source of cache metadata: the tool itself.

Most task runners make the user describe caching behavior:

```json
{
  "tasks": {
    "build": {
      "command": "vp build",
      "input": ["src/**", "index.html", "vite.config.ts"],
      "output": ["dist/**"],
      "env": ["VITE_*", "NODE_ENV"]
    }
  }
}
```

That works, but it puts the burden on the wrong party.

There are three parties:

```mermaid
flowchart LR
  User["User\nruns vp build\nowns overrides"]
  Runner["Runner\nexecutes, fingerprints,\nrestores cache"]
  Tool["Tool\nknows build semantics"]

  User --> Runner
  Runner --> Tool
  Tool -- reports cache facts --> Runner
  User -. manual input/output/env config .-> Runner
```

The user knows the project.

The runner knows how to cache.

The tool knows what actually matters.

## fspy As The Baseline

Vite+ already has automatic inference through fspy.

When a cached task runs, fspy observes file-system access at the syscall level. Reads become inferred inputs. Writes become inferred outputs.

```mermaid
flowchart TD
  A["vp build"] --> B["Runner starts task with fspy"]
  B --> C["Tool reads src/main.ts"]
  B --> D["Tool writes dist/assets/main.js"]
  C --> E["Input fingerprint"]
  D --> F["Output archive"]
  E --> G["Next run: validate inputs"]
  F --> H["Next run: restore outputs"]
```

This removes a lot of user config.

But fspy sees access, not intent.

If Vite reads `dist/` before deleting it, fspy sees an input. If Vite writes `node_modules/.vite/`, fspy sees an output. Those facts are technically true, but not the caching semantics the build needs.

## Runner Awareness

Runner awareness lets the tool report intent back to the runner.

Vite imports a tiny client from `@voidzero-dev/vite-task-client`. Outside `vp build`, the calls do nothing. Inside `vp build`, the runner injects an IPC client and records the reports.

```mermaid
sequenceDiagram
  participant U as User
  participant R as Runner
  participant T as Tool

  U->>R: vp build
  R->>T: start build with fspy + IPC
  T->>R: getEnvs("VITE_*")
  T->>R: ignoreInput(outDir)
  T->>R: ignoreInput(cacheDir)
  T->>R: ignoreOutput(cacheDir)
  R->>R: merge fspy access + tool reports
  R->>U: cache hit or miss
```

This shifts the default responsibility from user config to tool-owned semantics.

The user still has final control through manual `input`, `output`, and `env` config. Tool reports refine automatic inference; they do not erase explicit user choices.

## When Vite Reports Envs

Env vars affect the generated bundle.

Example:

```ts
console.log(import.meta.env.VITE_API_URL)
```

Run one:

```sh
VITE_API_URL=https://staging.example vp build
```

Run two:

```sh
VITE_API_URL=https://prod.example vp build
```

The output should change, so the cache must miss.

Vite already owns the `envPrefix` rule. By default, that means `VITE_*`. So Vite reports matching envs to the runner with `getEnvs("VITE_*")`.

```mermaid
flowchart TD
  A["Vite config: envPrefix = VITE_"]
  B["Vite calls getEnvs('VITE_*')"]
  C["Runner snapshots matching env names + values"]
  D["VITE_API_URL changes"]
  E["Cache miss"]

  A --> B --> C --> D --> E
```

This avoids asking the user to duplicate Vite config in task config:

```json
{
  "env": ["VITE_*"]
}
```

Vite also reports envs like `NODE_ENV` when build resolution depends on them and they are not already present in `process.env`.

Example:

```sh
NODE_ENV=production vp build
NODE_ENV=development vp build
```

Those two builds can resolve different behavior, so the env belongs in the fingerprint.

## When Vite Reports `ignoreInput`

`ignoreInput(path)` means: “if fspy saw reads under this path, do not treat them as cache inputs.”

It is used when a read is an implementation detail, not a build dependency.

Example: before writing `dist/`, Vite may empty it.

```mermaid
flowchart TD
  A["Vite prepares outDir"]
  B["emptyDir(dist) reads existing entries"]
  C["Build writes dist/assets/main.js"]
  D["Without ignoreInput: dist is both input and output"]
  E["With ignoreInput(dist): cleanup read is ignored"]
  F["dist remains an output"]

  A --> B --> D
  A --> E --> F
  C --> F
```

The read of `dist/` should not invalidate the next build.

The output files under `dist/` still matter. They are archived and restored on a cache hit.

## When Vite Reports `ignoreOutput`

`ignoreOutput(path)` means: “if fspy saw writes under this path, do not archive them as cache outputs.”

It is used for tool-owned scratch state.

Example: Vite’s dependency optimizer uses a cache directory such as `node_modules/.vite/`.

```mermaid
flowchart LR
  A["Vite dep optimizer"]
  B["reads node_modules/.vite"]
  C["writes node_modules/.vite"]
  D["Vite reports ignoreInput(cacheDir)"]
  E["Vite reports ignoreOutput(cacheDir)"]
  F["Runner excludes cacheDir from build cache metadata"]

  A --> B --> D --> F
  A --> C --> E --> F
```

That directory is not the application build output.

It is Vite’s own optimizer state. Vite already knows how to validate it through its own metadata, so the task runner should not treat it as an input or restored artifact.

## Temporary Files

Some Vite internals create temporary files.

For example, config loading can bundle a config file into a temporary module, write it, import it, then move on.

That file is both read and written during the build, but it is not a project input or build output.

```ts
ignoreInput(tempConfigFile)
ignoreOutput(tempConfigFile)
```

The runner should ignore that file in both directions.

## Manual Config Still Wins

Runner awareness is not a lock-in to tool decisions.

If a user manually declares an input, that input stays part of the cache key.

```json
{
  "tasks": {
    "build": {
      "command": "vp build",
      "input": ["special-generated-file.json"],
      "output": ["dist/**"],
      "env": ["CUSTOM_BUILD_FLAG"]
    }
  }
}
```

Tool reports apply to automatic fspy inference.

Manual config is still the escape hatch when a project has behavior the tool cannot know.

## The Principle

Vite+ caching is moving toward this division of responsibility:

```mermaid
flowchart TD
  Tool["Tool owns semantics\nenvPrefix, cacheDir, outDir, temp files"]
  Runner["Runner owns mechanics\nfingerprint, cache lookup, restore"]
  User["User owns policy\nmanual overrides when needed"]

  Tool --> Runner
  User --> Runner
```

Most task runners ask the user to describe tool internals.

Vite+ uses fspy to infer what happened, then runner-aware tools explain what those observations mean.

The Vite integration is the first step: Vite reports envs, internal cache paths, output cleanup reads, and temporary files from the code paths that own them.

Sources reviewed: [Vite PR #22453](https://github.com/vitejs/vite/pull/22453), [linked proposal](https://github.com/voidzero-dev/vite-task/blob/runner-aware-tools/docs/runner-task-ipc/vite-proposal.md).
