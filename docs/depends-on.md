# RFC: Enhanced `dependsOn` Syntax

## Background

Today, `dependsOn` entries can only refer to a single task by name (`"build"`) or by package-qualified name (`"pkg#build"`). A common pattern in monorepo task runners is "run `build` in all transitive dependencies first" — tools like Nx (`^build`) and Turborepo (`^build`) support this, but each introduces its own symbol with its own meaning.

The CLI already supports package selection through flags like `--recursive`, `--transitive`, and `--filter`. Rather than invent yet another DSL with new symbols, we reuse the exact same mental model and syntax from `vp run`.

### Design principle

**No new mental models.** If you know how to write `vp run`, you know how to write a `dependsOn` entry. The flag names, filter syntax, and task specifier format are identical.

## Current Syntax

```jsonc
{
  "tasks": {
    "test": {
      "dependsOn": [
        "build", // same-package task
        "utils#build", // task in a specific package
      ],
    },
  },
}
```

These simple forms remain valid and unchanged under both proposed styles.

## Proposed Syntax

Two equivalent styles are proposed.

### Style 1: CLI string syntax

Each `dependsOn` element is a string (existing syntax) or a string array written exactly as you would type CLI arguments to `vp run`:

```jsonc
{
  "tasks": {
    "test": {
      "dependsOn": [
        // Existing syntax — still works
        "build",
        "utils#build",

        // Run `build` across all workspace packages
        ["--recursive", "build"],

        // Run `build` in current package and its transitive dependencies
        ["--transitive", "build"],

        // Run `build` in packages matching a filter
        ["--filter", "@myorg/core", "build"],
        ["--filter", "@myorg/core...", "build"], // @myorg/core and its deps
      ],
    },
  },
}
```

Each element in the array is one CLI token, exactly as you would pass to `vp run`.

**Supported flags:**

| Flag                 | Short          | Meaning                                                            |
| -------------------- | -------------- | ------------------------------------------------------------------ |
| `--recursive`        | `-r`           | All workspace packages                                             |
| `--transitive`       | `-t`           | Current package + its transitive dependencies                      |
| `--filter <pattern>` | `-F <pattern>` | Packages matching a [filter expression](https://pnpm.io/filtering) |
| `--workspace-root`   | `-w`           | The workspace root package                                         |

Everything after the flags is the task specifier (e.g. `build`, `pkg#task`).

### Style 2: Object syntax

Each `dependsOn` element can be an object whose keys mirror the CLI flag names:

```jsonc
{
  "tasks": {
    "test": {
      "dependsOn": [
        // Existing syntax — still works as plain strings
        "build",
        "utils#build",

        // Run `build` across all workspace packages
        { "recursive": "build" },

        // Run `build` in current package and its transitive dependencies
        { "transitive": "build" },

        // Run `build` in packages matching a filter
        { "filter": "@myorg/core", "task": "build" },
        { "filter": "@myorg/core...", "task": "build" },

        // Multiple filters
        { "filter": ["@myorg/core", "@myorg/utils"], "task": "build" },

        // Workspace root
        { "workspaceRoot": "build" },
      ],
    },
  },
}
```

**Object forms:**

| Form                                               | Meaning                                                          |
| -------------------------------------------------- | ---------------------------------------------------------------- |
| `{ "recursive": "<task>" }`                        | Run `<task>` across all workspace packages.                      |
| `{ "transitive": "<task>" }`                       | Run `<task>` in current package and its transitive dependencies. |
| `{ "filter": "<pattern>", "task": "<task>" }`      | Run `<task>` in packages matching a filter expression.           |
| `{ "filter": ["<p1>", "<p2>"], "task": "<task>" }` | Run `<task>` in packages matching multiple filters.              |
| `{ "workspaceRoot": "<task>" }`                    | Run `<task>` in the workspace root package.                      |

The same validation rules from the CLI apply:

- `recursive`, `transitive`, `filter`, and `workspaceRoot` are mutually exclusive.
- When using `filter`, the task name goes in a separate `task` field (since `filter` takes a pattern as its value).

## Context: "Current Package"

When `--transitive` or a filter with traversal suffixes (e.g. `@myorg/core...`) resolves packages, "current package" means the package that owns the task containing this `dependsOn` entry — the same package that would be inferred from an unqualified `"build"` dependency today.

## Comparison

|                    | Style 1 (CLI string)                                           | Style 2 (Object)                                                   |
| ------------------ | -------------------------------------------------------------- | ------------------------------------------------------------------ |
| Learning curve     | None if you already know `vp run` — identical syntax           | Minimal — same flag names, written as JSON keys                    |
| IDE autocompletion | Yes — TypeScript tuple types can constrain each array position | Yes — TypeScript object types can validate keys and suggest fields |
| Config consistency | Unusual — CLI syntax embedded in config arrays                 | Consistent — matches the object style used elsewhere in the config |
