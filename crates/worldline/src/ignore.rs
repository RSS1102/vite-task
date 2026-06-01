//! Path-ignore rules for the snapshotter.
//!
//! By default worldline only snapshots files under the working directory, and
//! skips a built-in set of directory names (`.git`, `node_modules`, `target`,
//! …). Users add `--ignore <glob>` patterns (matched against the path relative
//! to the working directory) and can turn the built-ins off.

use std::path::Component;

use vite_path::{AbsolutePath, AbsolutePathBuf};
use vite_str::Str;
use wax::Program as _;

/// Directory names ignored by default, anywhere under the working directory.
const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    ".cache",
    ".turbo",
    ".next",
    ".DS_Store",
];

/// A resolved set of ignore rules.
pub struct IgnoreSet {
    cwd: AbsolutePathBuf,
    use_defaults: bool,
    globs: Vec<wax::Glob<'static>>,
}

impl IgnoreSet {
    /// Build an ignore set rooted at `cwd`.
    ///
    /// # Errors
    ///
    /// Returns an error if any user glob pattern fails to parse.
    pub fn new(cwd: AbsolutePathBuf, use_defaults: bool, patterns: &[Str]) -> anyhow::Result<Self> {
        let globs = patterns
            .iter()
            .map(|pattern| Ok(wax::Glob::new(pattern.as_str())?.into_owned()))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { cwd, use_defaults, globs })
    }

    /// Whether `path` should be excluded from snapshotting.
    ///
    /// Paths outside the working directory are always ignored.
    #[must_use]
    pub fn is_ignored(&self, path: &AbsolutePath) -> bool {
        let Ok(Some(relative)) = path.strip_prefix(self.cwd.as_absolute_path()) else {
            return true;
        };

        if self.use_defaults {
            for component in relative.as_path().components() {
                if let Component::Normal(name) = component
                    && let Some(name) = name.to_str()
                    && DEFAULT_IGNORED_DIRS.contains(&name)
                {
                    return true;
                }
            }
        }

        self.globs.iter().any(|glob| glob.is_match(relative.as_path()))
    }
}
