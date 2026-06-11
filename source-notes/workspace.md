## Vite Task Workspace Discovery and Package Manager Support

### 1. Workspace Root Location Strategy

**File detection priority (lib.rs:140-175; package_manager.rs:135-193):**
1. **pnpm-workspace.yaml** - highest priority, detected first
2. **package.json with `workspaces` field** - npm/yarn/bun workspace format
3. If neither found at current directory, walk up parent directories
4. When filesystem root reached without finding workspace markers, fall back to single package mode

**turbo.json handling:** NOT read at all. No references to turbo.json in codebase. If both turbo.json and pnpm-workspace.yaml exist, pnpm-workspace.yaml wins via directory walk priority.

**Workspace root always determined from first matching marker** found while walking up directory tree. The function returns a `WorkspaceRoot` struct with the marker file contents and an enum variant identifying which type (PnpmWorkspaceYaml, NpmWorkspaceJson, or NonWorkspacePackage).

### 2. Supported Workspace Formats

**pnpm-workspace.yaml (lib.rs:38-44):**
- Parses `packages:` array of glob patterns
- Format: `packages:\n  - "packages/*"\n  - "apps/*"`
- Supports negated globs with `!` prefix: `!packages/excluded*` (lib.rs:103-109, 530-573)
- Negated patterns use `PathGlobSet` for filtering after inclusion walk (lib.rs:110, 124-125)
- Globs appended with `/package.json` for file discovery (lib.rs:102)

**package.json workspaces (lib.rs:46-81):**
- Array form: `"workspaces": ["packages/*", "apps/*"]`
- Object form (Bun/Yarn classic): `"workspaces": {"packages": ["packages/*", "apps/*"], "catalog": {...}}`
- Both forms supported via `NpmWorkspaces` enum with `into_packages()` method (lib.rs:64-68)
- Negated globs also supported in npm/yarn workspaces (lib.rs:1106-1164 test)
- Catalog field silently ignored (lib.rs:1295-1337 test)

**yarn berry specifics:** No explicit yarn berry detection. Uses standard package.json workspaces format. No special handling for .pnp.cjs or pnpfile.

**bun:** Treated identically to npm/yarn—reads package.json workspaces field. Tests confirm object-form workspaces work (lib.rs:1229-1292).

**lerna.json:** NOT supported. No references in codebase.

**Negated glob semantics:** Last-match-wins algorithm. When both inclusion and exclusion patterns match a package, exclusion wins (lib.rs:576-633 test example: `"packages/**"`, `"!packages/excluded/**"`, `"packages/excluded/a"` → includes a but excludes b).

### 3. Package Manager Detection and node_modules Layout

**Package manager detection:** NOT performed. No explicit PM detection code.

**node_modules requirement:** NOT required. Workspace discovery works on filesystem paths alone—reads only:
- pnpm-workspace.yaml (YAML parse)
- package.json workspaces field (JSON parse)
- Individual package.json files for dependency graphs

**Yarn PnP support:** Untested. Code expects to find package.json files on disk. Yarn PnP would fail—no special handling for .pnp.cjs.

**Lockfile usage:** None. Lockfiles (package-lock.json, pnpm-lock.yaml, yarn.lock) are NOT consulted for workspace detection or dependency resolution.

### 4. Package Graph Edges: Dependency Types

**Edge creation rules (package.rs:46-71, lib.rs:151-211):**
- Only `workspace:` protocol dependencies create edges
- Regular semantic version specs are silently skipped (lib.rs:207)
- External packages cannot create workspace edges

**Workspace protocol variants supported:**
- `workspace:*` → matches any version of the local package
- `workspace:^1.0.0` → parses version but matches any local package (version ignored for graph, lib.rs:56-69)
- `workspace:@scope/pkg-a@^1.0.0` → extracts package name via split on last `@` (lib.rs:62-65)
- Non-`workspace:` prefixed deps are skipped (lib.rs:56)

**Dependency types tracked (package.rs:5-11, lib.rs:147-212):**
- `dependencies` → `DependencyType::Normal`
- `devDependencies` → `DependencyType::Dev`
- `peerDependencies` → `DependencyType::Peer`
- All three iterated via `get_workspace_dependencies()` (package.rs:47-70)

**Cycles allowed:** Not an error condition. Tests confirm circular dependencies are valid (lib.rs:819-873). Both edges a→b and b→a are added (lib.rs:203-204).

**Self-dependencies skipped:** If package lists itself in workspace: dependencies, edge is not added (lib.rs:202-205). Prevents self-loops in task graph.

**Duplicate package names handling:** Currently allowed (lib.rs:695-729). Test `test_get_package_graph_duplicate_names` confirms both duplicates are added. Name-to-path mapping uses `SmallVec1` to store multiple paths (lib.rs:148, 171-178).

### 5. Root Package Inclusion

**Always included:** The workspace root package (empty relative path, RelativePathBuf::default()) is ALWAYS added to the graph (lib.rs:318-340).

**Inclusion logic (lib.rs:299-340):**
1. Walk workspace globs to find member packages.json
2. Track whether any member has empty relative path (`has_root_package` flag, lib.rs:315)
3. If no member glob matched root, create a synthetic node from workspace_root/package.json (lib.rs:319-340)
4. If root package.json missing, use empty `PackageJson::default()` (lib.rs:330-331)

**Included in `-r` (recursive):** Yes. `PackageQuery::all()` calls `full_subgraph()` which includes every node_index in the graph (package_graph.rs:226-234).

**Included in `--filter` matches:** Depends on filter. The root is included if:
- Explicitly selected by name
- Selected by `--workspace-root` / `-w` flag (package_filter.rs:141, package_graph.rs:327-335)
- Selected by directory pattern matching workspace root path
- Included by `{./}` braced directory selector

**Not automatically included by default filter:** The implicit cwd filter (`ContainingPackage`) only includes the package containing cwd (package_graph.rs:311-315). If cwd is workspace root, only root is selected.

### 6. --filter Grammar (All Supported Forms)

**Module:** package_filter.rs, lines 1-1300+

**Selector forms (parse_filter, lines 460-509):**

1. **Exact name:** `foo`, `@scope/pkg` → `PackageSelector::Name(PackageNamePattern::Exact)`
2. **Glob pattern:** `@scope/*`, `*-utils` → `PackageSelector::Name(PackageNamePattern::Glob)` (wax semantics: * and ? only)
3. **Relative path (unbraced):** `.`, `..`, `./foo`, `../foo` → `PackageSelector::Directory(DirectoryPattern::Exact)` with traversal DISABLED (package_filter.rs:559-561)
4. **Braced path:** `{./foo}`, `{packages/*}` → `PackageSelector::Directory(DirectoryPattern::Glob)` with traversal ENABLED (package_filter.rs:541-551)
5. **Directory glob:** `./packages/*`, `./packages/**` → `PackageSelector::Directory(DirectoryPattern::Glob)` (package_filter.rs:577-593)
6. **Name + directory intersection:** `app{./packages/*}`, `pattern{./dir}` → `PackageSelector::NameAndDirectory` (package_filter.rs:548-550, 700-732)
7. **Workspace root:** `-w` / `--workspace-root` flag → `PackageSelector::WorkspaceRoot` (package_filter.rs:259, 327-335)

**Traversal suffixes (package_filter.rs:468-499):**
- `foo...` → `TraversalDirection::Dependencies` (transitive dependencies)
- `...foo` → `TraversalDirection::Dependents` (transitive dependents)
- `...foo...` → `TraversalDirection::Both` (dependents then their deps)
- `foo^...` → `Dependencies` with `exclude_self: true` (deps only, not foo)
- `...^foo` → `Dependents` with `exclude_self: true` (dependents only, not foo)
- Unbraced paths discard traversal (package_filter.rs:504-507, 1084-1111)

**Exclusion:** `!foo` prefix (package_filter.rs:465-466)

**Scoped auto-completion:** Exact name `bar` with no match auto-resolves to `@scope/bar` if exactly one scoped variant exists (package_graph.rs:366-380).

**No git ref support:** No `--since`, `--changed-since`, or git-based filtering in parse_filter (package_filter.rs:1-31 module docs make no mention).

**Whitespace splitting:** `--filter "a b"` splits into two tokens internally (package_filter.rs:315-325).

### 7. Multiple Lockfiles, Nested Workspaces, Symlinks, Nameless Packages

**Multiple lockfiles:** Not relevant. Workspace detection is file-system based, ignoring lockfiles entirely.

**Nested workspaces:** NOT explicitly supported by design. Once a workspace root is found (pnpm-workspace.yaml or package.json with workspaces), upward walk stops. A nested workspace (child directory with its own pnpm-workspace.yaml) would be discovered as a package via parent's glob patterns but its own workspace config would be ignored—it's treated as a single package, not a workspace root. No test coverage for nested workspaces.

**Symlinked packages:** Symlinks followed during glob walk (wax::Glob::walk, lib.rs:115-131). No special symlink detection or handling. Lexical path normalization in package_filter.rs (resolve_filter_path, line 603) can produce incorrect results with symlinks (documented comment at line 600-602).

**Packages without name field:** Allowed. Default to empty string (package.rs:17, `#[serde(default)]`). Cannot be referenced by dependencies because empty-string dependencies are skipped (lib.rs:184-187). Node is included in graph but forms no edges.

---

## File Citations (Complete)

- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_manager.rs:135-193` - find_workspace_root implementation
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:36-44` - PnpmWorkspace struct
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:46-81` - NpmWorkspaces enum and NpmWorkspace struct
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:98-135` - WorkspaceMemberGlobs::get_package_json_paths with negation logic
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:151-212` - PackageGraphBuilder::add_package and build with edge creation
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:299-340` - load_package_graph root package handling
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:530-573` - Test: last-match-wins negation
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/lib.rs:819-873` - Test: circular dependencies
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package.rs:5-71` - DependencyType and workspace: protocol parsing
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_filter.rs:1-31` - Module-level filter syntax docs
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_filter.rs:460-509` - parse_filter with traversal logic
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_filter.rs:527-571` - parse_core_selector with braces and name+dir
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_graph.rs:327-335` - WorkspaceRoot selector resolution
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_graph.rs:366-380` - Scoped auto-completion logic
- `/Volumes/d/code/vite-task/crates/vite_workspace/src/package_graph.rs:226-234` - full_subgraph includes all nodes


## Gotchas
- turbo.json is completely ignored—only pnpm-workspace.yaml and package.json workspaces are detected. In a repo with both, pnpm-workspace.yaml wins via directory walk priority.
- yarn PnP (PnP without node_modules) is not supported—code expects package.json files to exist on disk.
- Lockfiles (pnpm-lock.yaml, yarn.lock, package-lock.json) are never consulted for workspace or dependency resolution.
- Nested workspaces (child directory with its own pnpm-workspace.yaml) are NOT recognized—the child workspace config is ignored, and the child is treated as a single package.
- Symlink normalization is lexical, not filesystem-aware, so paths like /a/symlink/../b resolve to /a/b rather than following symlink targets (intentional pnpm compatibility).
- Unbraced relative paths (./foo, .., ../bar) discard traversal suffixes—{./foo}... works but ./foo... silently drops the ... suffix for pnpm compatibility.
- Duplicate package names are allowed in the same workspace (both are added to graph). Resolution with 'unique: true' (pkg#task specifier) errors if multiple packages share a name.
- Packages without a name field default to empty string and cannot be referenced by any workspace: dependencies.
- The root package is ALWAYS added to the graph even if workspace globs don't include it. If root package.json is missing, an empty default is used.
- External (non-workspace:) dependencies are silently ignored for graph edges—only workspace: protocol creates edges.
- Cycles in the package graph are not errors—both directions of a circular dependency add edges.
- Self-referential dependencies (package depends on itself) are skipped to prevent self-loops in the task graph.
- No git-based filtering (--since, --changed-since) is supported by the filter parser.
- Version constraints in workspace: specs (workspace:^1.0.0) are parsed but ignored—any version of the local package matches.
- Bun workspaces are treated identically to npm/yarn—object-form workspaces with catalog fields work but the catalog is ignored for task dependencies.