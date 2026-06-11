
# Vite Task Config Loading and Schema Mapping

## 1. Config File Recognition and TypeScript Evaluation

### Recognized Config File Names
Per `/Volumes/d/code/vite-plus/packages/cli/src/resolve-vite-config.ts` lines 9-16, vite-plus recognizes these config files in order:
- `vite.config.js`
- `vite.config.mjs`
- `vite.config.ts` (evaluated via Vite's resolveConfig)
- `vite.config.cjs`
- `vite.config.mts`
- `vite.config.cts`

The first matching file in this order is loaded and used.

### TypeScript Evaluation
- **Runtime**: Vite's own `resolveConfig` function (`@voidzero-dev/vite-plus-core/vite` exports) is called from JS to load vite.config files
- **Process**: The JS side (vite-plus CLI) calls `resolveUniversalViteConfig` which invokes Vite's config loader, returning the parsed `vite.config.ts` as a JSON string containing `{ run, ...otherConfig }` (lines 112-130 of resolve-vite-config.ts)
- **Import Support**: Full npm package imports are supported in vite.config.ts since it's processed through Vite's standard loader (can use `defineConfig` from 'vite' and any npm package)
- **Error Handling**: If vite.config.ts throws or imports unavailable packages, the error propagates through `resolveUniversalViteConfig` to the Rust core, which surfaces it to the user. The JS loader does not have fault tolerance for bad configs.

## 2. Complete UserRunConfig/Task Schema

All fields are in `/Volumes/d/code/vite-task/crates/vite_task_graph/src/config/user.rs`. **PUBLIC DOCS COVERAGE**: The docs mention command, dependsOn, cache, env, untrackedEnv, input, output, cwd plus run.cache and run.enablePrePostScripts. The schema has NO additional fields beyond what docs list.

### UserRunConfig (root-level)
```
{
  cache?: boolean | { scripts?: boolean, tasks?: boolean },
  tasks?: Record<string, TaskDefinition>,
  enablePrePostScripts?: boolean
}
```

Fields:
- **cache** (lines 301-305): Root-level global cache config. Can be `true` (enables both scripts and tasks), `false` (disables both), or detailed `{ scripts: bool, tasks: bool }`. Defaults to `{ scripts: false, tasks: true }` when omitted.
- **tasks** (line 308): Map of task name -> TaskDefinition.
- **enablePrePostScripts** (line 318): Automatically run `preX`/`postX` package.json scripts as lifecycle hooks when script `X` is executed. Defaults to `true` when omitted (line 289).

### UserTaskDefinition (tasks map values)
Two forms (serde untagged, lines 221-231):
1. **CommandShorthand**: A single string or array of strings `["cmd1", "cmd2"]` — expanded to `{ command: <value>, ...defaults }` with `UserTaskOptions::default()`
2. **Object**: Full `UserTaskConfig` with explicit options

### UserTaskConfig (full form)
```
{
  command: string | string[],
  cwd?: string,
  dependsOn?: string[],
  cache?: false | { env?: string[], untrackedEnv?: string[], input?: [...], output?: [...] }
}
```

Fields (lines 161-219):
- **command** (line 214): Required. Single string or array of command strings, run in order.
- **cwd** (lines 163-164): Relative to package root (not workspace root). Resolved to absolute at plan time. Empty/omitted = package directory.
- **dependsOn** (line 167): Task specifiers for dependencies. Format: `"taskName"` or `"packageName#taskName"`.
- **cache** (line 171): Flattened union—either `cache: false` to disable, or cache enabled with optional env/input/output config.

### EnabledCacheConfig (when cache is enabled)
```
{
  env?: string[],
  untrackedEnv?: string[],
  input?: UserInputsConfig,
  output?: UserOutputEntry[]
}
```

Fields (lines 126-154):
- **env** (line 128): Environment variable names to fingerprint (affect cache key). Type: `string[]`.
- **untrackedEnv** (line 131): Environment variable names passed through WITHOUT affecting cache key. Type: `string[]`. Supports wildcard patterns (see section 5).
- **input** (lines 133-143): Default when omitted is `[{auto: true}]`. Can be:
  - Empty array `[]` to disable automatic file tracking
  - Array of strings (glob patterns, resolved relative to package dir)
  - Array of objects `{ pattern: string, base: "workspace" | "package" }` for explicit base
  - Array including `{ auto: true }` to enable automatic tracking alongside explicit patterns
  - Negative patterns prefixed with `!` to exclude files
- **output** (lines 145-153): Files to archive on successful run and restore on cache hit. Default when omitted is no output archiving. Same format as input but no `auto` option.

### UserInputsConfig / UserInputEntry
Union type (lines 54-61):
- `string`: Glob pattern (positive or negative with `!` prefix), resolved relative to package dir
- `{ pattern: string, base: "workspace" | "package" }`: Glob with explicit base
- `{ auto: true }`: Enable automatic file tracking

### UserOutputEntry
Union type (lines 77-81):
- `string`: Glob pattern (positive or negative with `!` prefix)
- `{ pattern: string, base: "workspace" | "package" }`: Glob with explicit base

## 3. Root-Only vs Per-Package Options

**Root-only (error if set in non-root package)**:
- **cache**: Global cache configuration (lines 122-129 in lib.rs)
- **enablePrePostScripts**: Lifecycle hook enablement (lines 125-128 in lib.rs)

**Per-package**:
- **tasks**: Task definitions (can appear in any package's vite.config.ts)
- **task options** (cwd, dependsOn, cache config): Per-task in any package

**Validation**: TaskGraph::load (lib.rs) validates root-only fields during first pass, returning `CacheInNonRootPackage` or `PrePostScriptsInNonRootPackage` errors if violated.

## 4. dependsOn Specifier Grammar

From `/Volumes/d/code/vite-task/crates/vite_task_graph/src/specifier.rs`:

**Grammar** (lines 27-33):
- Format: `"taskName"` or `"packageName#taskName"`
- Parsing uses `rsplit_once('#')` — splits on the LAST `#` found
- `"packageName#taskName"` → specifies task in specific package
- `"taskName"` (no `#`) → task in same package as dependent task
- `"#taskName"` with empty packageName (edge case) → valid but unusual, means task in package named ""

**Glob support**: NO glob patterns in dependsOn. Only literal task and package names.

**Self-reference**: Not explicitly handled as error in specifier grammar, but would fail at lookup time if a task depends on itself by name.

**Missing target behavior**: Error. `DependencySpecifierLookupError` returned if package not found (`PackageNameNotFound`) or task not found in package (`TaskNameNotFound`). No silent skipping (lines 114-119 in lib.rs).

## 5. env/untrackedEnv Wildcard Grammar

From `/Volumes/d/code/vite-task/crates/vite_glob/src/env.rs`:

**Matching engine**: `globset` with `literal_separator(false)` so `*`, `?`, `[...]`, `{a,b}` behave as plain-string wildcards (not path globs).

**Wildcards supported**:
- `*`: Matches zero or more characters (e.g., `VITE_*` matches `VITE_FOO`, `VITE_`)
- `?`: Matches exactly one character
- `[...]`: Character class (e.g., `APP[12]_*`)
- `{a,b}`: Brace alternation (e.g., `{VITE,NEXT}_*`)

**Case sensitivity**: Case-sensitive on Unix, case-insensitive on Windows (env semantics, lines 28-30).

**Negation in sets**: Patterns prefixed with `!` in untrackedEnv are excludes (lines 84-86). A name matches if it matches an include pattern and NO exclude pattern.

### DEFAULT_UNTRACKED_ENV (built-in passthrough allowlist)
From `/Volumes/d/code/vite-task/crates/vite_task_graph/src/config/mod.rs` lines 398-481, the complete default list: HOME, USER, TZ, LANG, SHELL, PWD, PATH, XDG_RUNTIME_DIR, XAUTHORITY, DBUS_SESSION_BUS_ADDRESS, CI, NODE_OPTIONS, COREPACK_*, NPM_CONFIG_STORE_DIR, PNPM_HOME, LD_LIBRARY_PATH, LD_PRELOAD, DYLD_FALLBACK_LIBRARY_PATH, DYLD_INSERT_LIBRARIES, LIBPATH, DISPLAY, TMP, TEMP, VERCEL, VERCEL_*, NEXT_*, USE_OUTPUT_FOR_EDGE_FUNCTIONS, NOW_BUILDER, VC_MICROFRONTENDS_CONFIG_FILE_NAME, GITHUB_*, RUNNER_*, APPDATA, LOCALAPPDATA, PROGRAMDATA, SYSTEMROOT, SYSTEMDRIVE, USERPROFILE, HOMEDRIVE, HOMEPATH, WINDIR, PATHEXT, ProgramFiles, ProgramFiles[(]x86[)], ELECTRON_RUN_AS_NODE, JB_INTERPRETER, _JETBRAINS_TEST_RUNNER_RUN_SCOPE_TYPE, JB_IDE_*, VSCODE_*, DOCKER_*, BUILDKIT_*, COMPOSE_*, PLAYWRIGHT_*, VP_*, *_TOKEN.

## 6. input/output Glob Grammar Details

From `/Volumes/d/code/vite-task/crates/vite_task_graph/src/config/mod.rs` lines 132-252:

### Glob Resolution
- **Base directory**: Package directory by default; workspace root if `base: "workspace"` specified
- **Resolution process** (lines 281-323):
  1. Partition glob into invariant prefix and variant part using `wax::Glob::partition()`
  2. Join invariant prefix with base directory, clean the path
  3. Strip workspace root prefix → make workspace-relative
  4. Re-escape the stripped prefix with `wax::escape()`, rejoin with variant
  5. Result: workspace-root-relative glob patterns

### Pattern Format
- **Positive globs**: e.g., `"src/**/*.ts"`, `"package.json"`, `"dist/"`
- **Negative globs**: Prefixed with `!`, e.g., `"!node_modules/**"`
- **Trailing `/`**: Shorthand for `/**` (all files under directory), e.g., `"dist/"` → `"dist/**"`

### Input-Specific Defaults
- **Omitted input**: Defaults to `[{auto: true}]` — automatic file tracking enabled
- **Empty array `[]`**: Disables file tracking entirely (inference disabled, no input)
- **With explicit patterns**: Auto inference disabled unless `{auto: true}` is explicitly included in the array

### Output-Specific Defaults
- **Omitted output**: No output archiving (empty config, `includes_auto: false`)
- **Empty array `[]`**: No output archiving (same as omitted)

### Negation Ordering
No requirement for order—negative patterns are applied AFTER positive patterns to produce final set. Sets are sorted deterministically in BTreeSet for stable cache keys.

### Dotfiles and Directories
- **Dotfiles**: Included if matched by glob. `.*` matches dotfiles; `src/**` matches dotfiles under src.
- **Directories vs files**: Globs are file-based. A pattern like `dist/` or `dist/**` matches files inside dist, not the directory itself.

### Error Cases
- Glob pattern resolves outside workspace root: `GlobOutsideWorkspace` error
- Invalid glob syntax: `InvalidGlob` error

## 7. package.json Scripts Interaction

From `/Volumes/d/code/vite-task/crates/vite_shell/src/lib.rs` and `/Volumes/d/code/vite-task/crates/vite_task_graph/src/lib.rs`:

### pre/post Hooks Expansion
- **Enabled by default**: `enablePrePostScripts` defaults to `true`
- **Expansion**: When script `X` is executed, automatically look for and execute `preX` and `postX` in the same package if they exist
- **Dependency edges**: Pre/post hooks are added to the task graph as explicit dependency edges

### Compound Command Splitting Rules
- **Supported operator**: Only `&&` (logical AND). Stops parsing on first non-AND operator.
- **Parsing method**: Uses `brush_parser` to parse as POSIX shell and extract AND-OR list
- **Restriction**: Only accepts `SeparatorOperator::Sequence` at top level (`;` between compound list items), with AND operators between commands
- **No support**: Pipes `|`, semicolon `;` at command level, `||` (OR), subshells, `cd foo && bar` inside script strings
- **Behavior if unsupported**: Script parsing returns `None`, script is executed as-is in shell (not split)

Example valid: `echo one && echo two && echo three` → 3 separate commands
Example invalid: `echo one; echo two` → parsed as single shell command, not split

### Nested vp run Inlining
From `/Volumes/d/code/vite-task/crates/vite_task_bin/src/lib.rs` lines 74-107:

When `vt run` or `vp run` is called inside a task script:
- **Interception**: CommandHandler parses the invocation using clap's `Command` parser
- **Supported flags**: `--ignore-depends-on`, `-v/--verbose`, `--cache`, `--no-cache`, `--log <MODE>`, `--concurrency-limit <N>`, `--parallel`, `--filter` (package filtering)
- **Cache inheritance**: When nested `vp run` has no explicit cache flag, it inherits parent's resolved cache config
- **Output display**: Nested runs inside scripts produce no interactive UI; errors are surfaced as task failures

## Implementation Path: JS to Rust
1. User runs `vp run` at command line
2. `/Volumes/d/code/vite-plus/packages/cli/src/bin.ts` entry point calls `run(options)` NAPI binding
3. NAPI binding invokes `resolveUniversalViteConfig` JS callback which calls Vite's `resolveConfig` to load vite.config.ts, extracts `config.run` JSON, returns stringified config to Rust
4. Rust core receives config via NAPI, deserializes to `UserRunConfig`
5. TaskGraph::load is called with config, tasks built into execution plan
6. Plans executed, with nested `vp run` commands re-invoking the same cycle


## Gotchas
- cwd is relative to PACKAGE root, not workspace root — common mistake when moving tasks between packages
- Default input tracking is [{ auto: true }] but specifying ANY explicit patterns disables auto UNLESS you also explicitly include { auto: true }
- Only && is supported for compound scripts — ; || pipes and subshells fail silently (script executed as-is in shell)
- Only workspace root's cache and enablePrePostScripts are honored; non-root configs error, unlike turborepo/nx which silently ignore package-level cache
- dependsOn errors are hard failures — missing targets do NOT skip silently, causing entire run to fail
- Trailing / in globs is shorthand for /**, e.g. dist/ means dist/** not dist itself
- DEFAULT_UNTRACKED_ENV includes wildcard patterns (COREPACK_*, VSCODE_*, etc.) — these are matched with globset, not simple string equality
- env and untrackedEnv both support wildcards, but they interact: untracked patterns prevent fingerprinting even if same env is in both lists
- vp run inside scripts must have flags BEFORE the task name — flags after task name are forwarded to the task and not parsed as vp options
- Base defaults to 'package' for globs, not 'workspace' — relative paths like ../shared are resolved from package dir first
- Negative glob patterns require ! prefix; -dist/** is not a valid negative pattern format
- Input defaults to auto inference, but output defaults to NO archiving — asymmetric defaults
- pre/post scripts are enabled by default globally (enablePrePostScripts true by default) — set false to disable this behavior
- vite.config.ts can import npm packages (evaluated by Vite runtime), so bad imports fail at load time, not at schema validation
- Cache config in package.json scripts task appears inside vite.config, not package.json — no per-script granularity