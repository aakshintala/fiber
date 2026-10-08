//! A session's own worktree (`docs/invocation.md`, "Isolation"): creating it
//! before `session_started`, and removing it at the end when it holds
//! nothing to lose.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;
use contract::shapes::Worktree;
use log::diag::{Diag, Level, Process, Severity};

use crate::{failure, project};

/// A worktree this session process created, until `end`.
pub struct Isolation {
    created: worktree::Created,
    session: SessionId,
    home: PathBuf,
    clock: Arc<dyn Clock>,
    /// Runs between the first scan's hold and the revalidating scan, so a
    /// test can force the interleaving a session starting in between.
    /// Test-only: always `None` outside tests.
    #[cfg(test)]
    pause: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Makes the session's worktree: a new branch from the launch repository's
/// HEAD, checked out under `<home>/projects/<key>/worktrees/<session>`.
/// `add` runs the one hook-running command, `git worktree add`: like
/// `Command::output`, or a cancellable runner. A failure carries Fiber's
/// own sentence, never git's output.
pub fn isolate(
    launch: &Path,
    home: &Path,
    session: &SessionId,
    clock: Arc<dyn Clock>,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Isolation, contract::shapes::Failure> {
    let key = log::project_key(&project(launch));
    let path = home
        .join("projects")
        .join(&key)
        .join("worktrees")
        .join(&session.0);
    let branch = format!("fiber/{}", session.0);
    match worktree::create(launch, &path, &branch, add) {
        Ok(created) => Ok(Isolation {
            created,
            session: session.clone(),
            home: home.to_owned(),
            clock,
            #[cfg(test)]
            pause: None,
        }),
        Err(error) => Err(failure(error.code(), sentence(&error, &path))),
    }
}

/// One fixed sentence for a `worktree::Error`, built from the path and the
/// git subcommand name only: never git's stderr, which can carry
/// configuration or credential text. `NotARepository`, `Exists` and
/// `GitMissing` already carry their sentences, so they are used as they
/// are; the rest name the worktree the failure is about.
fn sentence(error: &worktree::Error, path: &Path) -> String {
    match error {
        worktree::Error::NotARepository { .. }
        | worktree::Error::Exists { .. }
        | worktree::Error::GitMissing => error.to_string(),
        worktree::Error::Git { command, .. } => {
            format!("git {command} failed for the worktree {}.", path.display())
        }
        worktree::Error::Io { source, .. } => {
            format!("{}: {}.", path.display(), source.kind())
        }
    }
}

impl Isolation {
    /// The worktree's canonical path.
    pub fn path(&self) -> &Path {
        &self.created.path
    }

    /// The `session_started` worktree: the canonical path and the branch.
    pub fn worktree(&self) -> Worktree {
        Worktree {
            path: self.created.path.to_string_lossy().into_owned(),
            branch: self.created.branch.clone(),
        }
    }

    /// Runs between the first scan's hold and the revalidating scan, so a
    /// test can force the interleaving a session starting in between.
    /// Test-only.
    #[cfg(test)]
    #[doc(hidden)]
    #[must_use]
    pub fn with_pause(mut self, pause: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.pause = Some(pause);
        self
    }

    /// Removes the worktree when it holds nothing uncommitted and no
    /// commit beyond its base, while holding every session working in it
    /// still. Runs after `close()`, at most once: it takes `self`.
    /// A kept worktree writes no line; an uncertain one keeps the worktree
    /// and writes one diagnostic `warn` line (see `failed`).
    pub fn end(self) {
        let mut held = Vec::new();
        let mut known = Vec::new();
        let first = users(&self.home, &self.created.path);
        for user in &first {
            match log::try_hold(&user.dir) {
                Ok(log::Hold::Held(lock)) => {
                    held.push(lock);
                    known.push(user.id.clone());
                }
                Ok(log::Hold::Busy) | Err(_) => return,
            }
        }
        // A process whose log ends `rewound` never removes its worktree:
        // the session that continues it runs there
        // (`docs/invocation.md`, "Isolation"). The creator's lock above
        // holds its log still, so its last line is read under it, before
        // any git command runs.
        if first
            .iter()
            .find(|user| user.id == self.session)
            .and_then(|creator| log::last_line(&creator.dir))
            .is_some_and(|line| line.kind == "rewound")
        {
            return;
        }
        #[cfg(test)]
        if let Some(pause) = &self.pause {
            pause();
        }
        let inspected = match worktree::inspect(&self.created.path) {
            Ok(worktree::Inspection::Worktree(found)) if found.branch == self.created.branch => {
                found
            }
            // A branch the person switched to, or a detached HEAD, was
            // the person's choice: kept, with no line.
            Ok(worktree::Inspection::Worktree(_)) | Ok(worktree::Inspection::Detached) => {
                return;
            }
            Ok(worktree::Inspection::NotAWorktree) => {
                return self.failed(&worktree::Error::Io {
                    path: self.created.path.clone(),
                    source: io::Error::from(io::ErrorKind::NotFound),
                });
            }
            Err(error) => return self.failed(&error),
        };
        if inspected.uncommitted {
            return;
        }
        match worktree::commits_beyond(&self.created.path, &self.created.branch, &self.created.base)
        {
            Ok(0) => {}
            Ok(_) => return,
            Err(error) => return self.failed(&error),
        }
        for user in users(&self.home, &self.created.path) {
            if known.contains(&user.id) {
                continue;
            }
            match log::try_hold(&user.dir) {
                Ok(log::Hold::Held(lock)) => {
                    held.push(lock);
                }
                Ok(log::Hold::Busy) | Err(_) => return,
            }
        }
        match worktree::remove(&self.created.path, &inspected, false) {
            Ok(worktree::Removed::Whole) => {}
            Ok(worktree::Removed::BranchKept(error)) => self.failed(&error),
            Err(error) => self.failed(&error),
        }
    }

    /// Keeps the worktree and writes one diagnostic `warn` line: code
    /// `io_failed`, process `session`, attached to the session id, in
    /// `logs/session-<id>.log`. The exit code and stdout do not change.
    fn failed(&self, error: &worktree::Error) {
        let diag = Diag::new(
            &self.home,
            Process::Session,
            Level::Info,
            Arc::clone(&self.clock),
        );
        diag.attach(&self.session);
        diag.line(
            Severity::Warn,
            None,
            "io_failed",
            &sentence(error, &self.created.path),
        );
    }
}

/// Every started session working in the canonical worktree: its workspace
/// is the worktree or lies under it, whatever project recorded it. The
/// creator's own session is listed too, when its directory still exists.
fn users(home: &Path, canonical: &Path) -> Vec<log::Started> {
    log::started_sessions(home)
        .into_iter()
        .filter(|started| {
            started
                .workspace
                .as_ref()
                .is_some_and(|work| Path::new(work).starts_with(canonical))
        })
        .collect()
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;
