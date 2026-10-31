//! An owned sandbox home: the root owns the tree and the Fiber home
//! derives from it, so dropping the value removes every directory the
//! test built (`docs/testing/running.md`, "Running tests").

#![allow(dead_code, reason = "each including test target uses its own subset")]

use std::path::{Path, PathBuf};

/// A sandbox home owned by the test that built it: `fiber_home` is
/// `root.join("fiber-home")`, and dropping the value removes the root.
pub(crate) struct SandboxHome {
    root: fakes::TempDir,
    fiber_home: PathBuf,
}

impl SandboxHome {
    /// Creates `<parent>/<prefix>-<suffix>/fiber-home`, owning the root.
    pub(crate) fn new(parent: &Path, prefix: &str) -> Self {
        let root = fakes::TempDir::under(parent, prefix);
        let fiber_home = root.path().join("fiber-home");
        Self { root, fiber_home }
    }

    /// The owned root the home derives from.
    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }

    /// The Fiber home confined calls use.
    pub(crate) fn fiber_home(&self) -> &Path {
        &self.fiber_home
    }

    /// Releases the owned root.
    pub(crate) fn into_root(self) -> fakes::TempDir {
        self.root
    }
}
