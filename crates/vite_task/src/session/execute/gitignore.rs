//! Version control's opinion about a path, used to break read-first overlaps.
//!
//! When a task reads a path and then rewrites it, access mechanics cannot say
//! whether that path is a source the task fixed up or state the task manages: a
//! formatter rewriting a source and a linter rewriting its own cache look
//! identical. Version control can. A tracked file the task rewrote is a modified
//! input; an ignored one is derived state.
//!
//! This is a proxy, not a definition. Gitignore answers "is this committed",
//! which is not quite "is this derived" — Astro's `.astro/settings.json` is
//! user-authored preferences inside an ignored directory. Explicit input and
//! output globs override the answer, so the proxy only has to be right for paths
//! nobody declared.
//!
//! When there is no repository, every read-first overlap resolves to *input*.
//! That is the safe direction: the task modified something it declared as a
//! dependency, so the run is not cached. Availability degrades, correctness does
//! not.

#![cfg(fspy)]

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use vite_path::{AbsolutePath, RelativePathBuf};

/// Gitignore matching rooted at the workspace.
pub struct WorkspaceGitignore {
    matcher: Option<Gitignore>,
}

impl WorkspaceGitignore {
    /// Build a matcher for the workspace root.
    ///
    /// Collects `.gitignore` at the root plus `.git/info/exclude`. Nested
    /// `.gitignore` files inside subdirectories are picked up by the builder as
    /// it walks the ones it is given, and a missing file is simply no rules.
    ///
    /// A malformed pattern is not fatal. Failing the whole task because one line
    /// of a `.gitignore` is unparseable would be worse than treating that line as
    /// absent, and the fallback direction is safe: fewer ignore rules means more
    /// paths look tracked, which means fewer runs are cached.
    pub fn open(workspace_root: &AbsolutePath) -> Self {
        let mut builder = GitignoreBuilder::new(workspace_root.as_path());
        // Errors here describe individual bad patterns; the partial matcher
        // built from the good ones is still worth having.
        drop(builder.add(workspace_root.join(".gitignore").as_path()));
        drop(builder.add(workspace_root.join(".git/info/exclude").as_path()));
        Self { matcher: builder.build().ok() }
    }

    /// Whether version control ignores this path.
    ///
    /// `false` when there is no matcher at all, which is the input-leaning
    /// answer.
    pub fn is_ignored(&self, path: &RelativePathBuf) -> bool {
        let Some(matcher) = &self.matcher else {
            return false;
        };
        // A directory and a file with the same name match different patterns
        // (`dist/` only matches the directory), so the caller's path is checked
        // both ways rather than guessing.
        matcher.matched_path_or_any_parents(path.as_path(), false).is_ignore()
            || matcher.matched_path_or_any_parents(path.as_path(), true).is_ignore()
    }
}
