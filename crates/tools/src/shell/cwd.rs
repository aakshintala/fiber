//! The directories one shell call may run in (`docs/tools.md`, "Shell",
//! "Effects"): `cd` moves the later commands of its call, and a lost path
//! refuses every later element.
//!
//! The states are `(succeeded, directory)` pairs, starting at one `(true,
//! workdir)`. An element joined by `&&` runs on the succeeded states and
//! keeps the failed ones; joined by `||` it runs on every state and keeps
//! the succeeded ones; joined by `;` or first it runs on every state and
//! keeps none. Leaving a `cd` element maps each running state to
//! `(true, joined)` and `(false, directory)`; leaving any other element
//! maps it to `(true, directory)` and `(false, directory)`, so a later
//! command checks its paths against every directory it may run in. Joins
//! normalise `.` and `..` lexically at join time, mirroring bash's logical
//! `cd`: `..` pops one component, and a `..` with an empty stack stays as
//! a literal `..` component, so `cd ..` past the workdir root still
//! resolves through the deny, which canonicalises every declared path.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::syntax::Join;

/// The most distinct directories one element may run in; more means the
/// path is lost and every later element is `executes`.
pub(super) const MAX_DIRECTORIES: usize = 32;

/// The `(succeeded, directory)` states of one call.
pub(super) struct Cwd {
    states: BTreeSet<(bool, String)>,
}

/// Whether `join` runs its element on a state with this success value.
fn runs(join: Join, succeeded: bool) -> bool {
    match join {
        Join::First | Join::Seq => true,
        Join::And => succeeded,
        Join::Or => true,
    }
}

/// Whether `join` carries a state with this success value past its element.
fn keeps(join: Join, succeeded: bool) -> bool {
    match join {
        Join::First | Join::Seq => false,
        Join::And => !succeeded,
        Join::Or => succeeded,
    }
}

impl Cwd {
    pub(super) fn new(workdir: &str) -> Cwd {
        Cwd {
            states: BTreeSet::from([(true, workdir.to_owned())]),
        }
    }

    /// The directories `join` runs its element in, or nothing when more
    /// than [`MAX_DIRECTORIES`] distinct directories would run it and the
    /// path is lost.
    pub(super) fn enter(&self, join: Join) -> Option<Vec<String>> {
        let dirs: BTreeSet<String> = self
            .states
            .iter()
            .filter(|(succeeded, _)| runs(join, *succeeded))
            .map(|(_, dir)| dir.clone())
            .collect();
        if dirs.len() > MAX_DIRECTORIES {
            return None;
        }
        Some(dirs.into_iter().collect())
    }

    /// Fold the element just checked back into the states: `Some` target
    /// for a `cd` element, `None` for any other.
    pub(super) fn leave(&mut self, join: Join, cd_target: Option<&str>) {
        let mut next = BTreeSet::new();
        for (succeeded, dir) in std::mem::take(&mut self.states) {
            if keeps(join, succeeded) {
                next.insert((succeeded, dir.clone()));
            }
            if runs(join, succeeded) {
                match cd_target {
                    Some(target) => {
                        next.insert((true, join_dir(&dir, target)));
                        next.insert((false, dir));
                    }
                    None => {
                        next.insert((true, dir.clone()));
                        next.insert((false, dir));
                    }
                }
            }
        }
        self.states = next;
    }
}

/// Append `target` to `dir` and normalise `.` and `..` lexically: an
/// absolute target replaces the directory, `.` stays, `..` pops one
/// component, and a `..` with an empty stack stays literal.
pub(super) fn join_dir(dir: &str, target: &str) -> String {
    let mut joined = if target.starts_with('/') {
        PathBuf::from("/")
    } else {
        PathBuf::from(dir)
    };
    for component in Path::new(target).components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if !joined.pop() {
                    joined.push("..");
                }
            }
            Component::Normal(piece) => {
                joined.push(piece);
            }
        }
    }
    joined.to_string_lossy().into_owned()
}

#[cfg(test)]
#[path = "cwd_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cwd_props_tests.rs"]
mod prop_tests;
