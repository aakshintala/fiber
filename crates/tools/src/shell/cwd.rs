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

use super::syntax::Join;

/// The most distinct directories one element may run in; more means the
/// path is lost and every later element is `executes`.
pub(super) const MAX_DIRECTORIES: usize = 32;

/// The `(succeeded, directory)` states of one call.
pub(super) struct Cwd {
    states: BTreeSet<(bool, String)>,
    running: Vec<(bool, String)>,
    kept: BTreeSet<(bool, String)>,
}

impl Cwd {
    pub(super) fn new(workdir: &str) -> Cwd {
        Cwd {
            states: BTreeSet::from([(true, workdir.to_owned())]),
            running: Vec::new(),
            kept: BTreeSet::new(),
        }
    }

    /// The directories `join` runs its element in, sorted and deduplicated,
    /// or nothing when more than [`MAX_DIRECTORIES`] distinct directories
    /// would run it and the path is lost.
    pub(super) fn enter(&mut self, join: Join) -> Option<Vec<String>> {
        let mut running = BTreeSet::new();
        let mut kept = BTreeSet::new();
        for state in &self.states {
            let succeeded = state.0;
            let runs = match join {
                Join::First | Join::Seq => true,
                Join::And => succeeded,
                Join::Or => true,
            };
            if runs {
                running.insert(state.clone());
            }
            let keeps = match join {
                Join::First | Join::Seq => false,
                Join::And => !succeeded,
                Join::Or => succeeded,
            };
            if keeps {
                kept.insert(state.clone());
            }
        }
        let dirs: BTreeSet<String> = running.iter().map(|(_, dir)| dir.clone()).collect();
        if dirs.len() > MAX_DIRECTORIES {
            return None;
        }
        let dirs: Vec<String> = dirs.into_iter().collect();
        self.running = running.into_iter().collect();
        self.kept = kept;
        Some(dirs)
    }

    /// Fold the element just checked back into the states: `Some` target
    /// for a `cd` element, `None` for any other.
    pub(super) fn leave(&mut self, cd_target: Option<&str>) {
        let mut next = std::mem::take(&mut self.kept);
        for (_, dir) in std::mem::take(&mut self.running) {
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
        self.states = next;
    }
}

/// Append `target` to `dir` and normalise `.` and `..` lexically: an
/// absolute target replaces the directory, `.` stays, `..` pops one
/// component, and a `..` with an empty stack stays literal.
fn join_dir(dir: &str, target: &str) -> String {
    let mut absolute = dir.starts_with('/');
    let mut parts: Vec<&str> = dir.split('/').filter(|piece| !piece.is_empty()).collect();
    if target.starts_with('/') {
        absolute = true;
        parts.clear();
    }
    for piece in target.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    parts.push("..");
                }
            }
            _ => parts.push(piece),
        }
    }
    let mut joined = parts.join("/");
    if absolute {
        joined.insert(0, '/');
    }
    joined
}

#[cfg(test)]
#[path = "cwd_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cwd_props_tests.rs"]
mod prop_tests;
