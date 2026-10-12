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
use contract::{ErrorCode, shapes::Failure};
use log::diag::{Diag, Level, Process, Severity};

use crate::{failure, project};

mod shared;

pub use shared::{Shared, share};

/// A worktree this session process created, until `end`.
pub struct Isolation {
    created: worktree::Created,
    session: SessionId,
    home: PathBuf,
    clock: Arc<dyn Clock>,
    repository: PathBuf,
    /// A share of the worktree's lock file, held until `end`: a resumed
    /// or recreating process runs in a worktree it did not just create,
    /// so it shares the file for its life. `None` for a worktree this
    /// process just created, which no other process can share yet.
    shared: Option<Shared>,
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
    let repository = project(launch);
    let key = log::project_key(&repository);
    let path = home
        .join("projects")
        .join(&key)
        .join("worktrees")
        .join(&session.0);
    let branch = format!("fiber/{}", session.0);
    match worktree::create(launch, &path, &branch, add) {
        Ok(created) => Ok(Isolation::new(
            created, session, home, clock, repository, None,
        )),
        Err(error) => Err(failure(error.code(), sentence(&error, &path))),
    }
}

/// What a recreation found at the recorded path.
pub enum Recreated {
    /// The path was absent: the worktree was made again, at the same
    /// path and branch from the repository's current HEAD. Ends by the
    /// same rules as a created worktree.
    Made(Isolation),
    /// The path is present: another process already runs in the
    /// worktree, and the returned share is held for the process's life.
    Present(Shared),
}

/// What a failed recreation leaves behind.
pub struct RecreateError {
    /// The failure to report.
    pub failure: Failure,
    /// The made worktree, when it was made and only its share failed:
    /// the caller ends it after releasing whatever would keep it.
    pub isolation: Option<Isolation>,
}

impl std::fmt::Debug for RecreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecreateError")
            .field("failure", &self.failure)
            .field("isolation", &self.isolation.is_some())
            .finish()
    }
}

/// Makes the recorded worktree again at the same path and branch from the
/// repository's current HEAD: `repository` is the recorded git common
/// directory, `path` and `branch` the recorded worktree. A present path
/// is shared, never remade: the worktree stays exactly as it is. `add`
/// runs the one hook-running command, `git worktree add`: like
/// `Command::output`, or a cancellable runner. A failure carries Fiber's
/// own sentence, never git's output.
#[allow(
    clippy::too_many_arguments,
    reason = "one recreation needs its repository, path, branch, home, session, clock and runners"
)]
#[allow(
    clippy::result_large_err,
    reason = "the caller matches the isolation out of the error by value"
)]
pub fn recreate(
    repository: &Path,
    path: &Path,
    branch: &str,
    home: &Path,
    session: &SessionId,
    clock: Arc<dyn Clock>,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<Recreated, RecreateError> {
    recreate_with(
        repository,
        path,
        branch,
        home,
        session,
        clock,
        add,
        &|| {},
        &shared::Creating::downgrade,
    )
}

/// [`recreate`] with its two seams: `before_wait` runs before every
/// blocking lock wait, and `downgrade` converts the creation's exclusive
/// hold into the share the returned isolation keeps. Production passes a
/// no-op and the real conversion; tests park on both to force the
/// interleavings a second process makes.
#[allow(
    clippy::too_many_arguments,
    reason = "one recreation needs its repository, path, branch, home, session, clock, runners and seams"
)]
#[allow(
    clippy::result_large_err,
    reason = "the caller matches the isolation out of the error by value"
)]
fn recreate_with(
    repository: &Path,
    path: &Path,
    branch: &str,
    home: &Path,
    session: &SessionId,
    clock: Arc<dyn Clock>,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
    before_wait: &dyn Fn(),
    downgrade: &dyn Fn(shared::Creating) -> Result<Shared, Failure>,
) -> Result<Recreated, RecreateError> {
    match shared::claim(path, before_wait) {
        Ok(shared::Claim::Present(shared)) => Ok(Recreated::Present(shared)),
        Ok(shared::Claim::Create(creating)) => {
            let created = match worktree::recreate(repository, path, branch, add) {
                Ok(created) => created,
                Err(error) => {
                    return Err(RecreateError {
                        failure: recreate_failure(&error, repository, path),
                        isolation: None,
                    });
                }
            };
            match downgrade(creating) {
                Ok(shared) => Ok(Recreated::Made(Isolation::new(
                    created,
                    session,
                    home,
                    clock,
                    repository.to_owned(),
                    Some(shared),
                ))),
                Err(failure) => Err(RecreateError {
                    failure,
                    isolation: Some(Isolation::new(
                        created,
                        session,
                        home,
                        clock,
                        repository.to_owned(),
                        None,
                    )),
                }),
            }
        }
        Err(failure) => Err(RecreateError {
            failure,
            isolation: None,
        }),
    }
}

/// One fixed sentence for a recreation's `worktree::Error`: a missing
/// repository names both directories, and every other error reads as the
/// matching creation failure does, never with git's output.
fn recreate_failure(error: &worktree::Error, repository: &Path, path: &Path) -> Failure {
    match error {
        worktree::Error::NotARepository { .. } => failure(
            ErrorCode::IoFailed,
            format!(
                "The repository {} is gone, so the worktree {} cannot be made again.",
                repository.display(),
                path.display()
            ),
        ),
        worktree::Error::Exists { .. }
        | worktree::Error::GitMissing
        | worktree::Error::Git { .. }
        | worktree::Error::Io { .. } => failure(error.code(), sentence(error, path)),
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
        worktree::Error::Io { path, source } => format!("{}: {}.", path.display(), source.kind()),
    }
}

impl Isolation {
    /// One constructor for both entry points: `isolate` shares nothing
    /// yet, and `recreate` keeps the share it arrived on.
    fn new(
        created: worktree::Created,
        session: &SessionId,
        home: &Path,
        clock: Arc<dyn Clock>,
        repository: PathBuf,
        shared: Option<Shared>,
    ) -> Self {
        Self {
            created,
            session: session.clone(),
            home: home.to_owned(),
            clock,
            repository,
            shared,
            #[cfg(test)]
            pause: None,
        }
    }

    /// The worktree's canonical path.
    pub fn path(&self) -> &Path {
        &self.created.path
    }

    /// The `session_started` worktree: the canonical path, the branch, and
    /// the repository's git common directory.
    pub fn worktree(&self) -> Worktree {
        Worktree {
            path: self.created.path.to_string_lossy().into_owned(),
            branch: self.created.branch.clone(),
            repository: Some(self.repository.to_string_lossy().into_owned()),
        }
    }

    /// Runs between the first scan's hold and the revalidating scan, so a
    /// test can force the interleaving a session starting in between.
    /// Test-only.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_pause(mut self, pause: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.pause = Some(pause);
        self
    }

    /// Removes the worktree when it holds nothing uncommitted and no
    /// commit beyond its base, while holding every session working in it
    /// still. Runs after `close()`, at most once: it takes `self`.
    /// A kept worktree writes no line; an uncertain one keeps the worktree
    /// and writes one diagnostic `warn` line (see `failed`).
    pub fn end(mut self) {
        // Our own share goes first: a removal that waited behind it
        // would keep the worktree, and no other process shares a
        // worktree back to life once this returns.
        drop(self.shared.take());
        // No process may be working in the worktree while it is
        // inspected and removed: the exclusive take waits for no one,
        // and a worktree another process shares is kept with no line.
        // The hold lasts until `end` returns, around the scan and the
        // removal below.
        let _removal = match shared::exclusive(&self.created.path) {
            Ok(shared::Removal::Exclusive(file)) => Some(file),
            Ok(shared::Removal::InUse) => return,
            Err(error) => return self.failed(&error),
        };
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
            Ok(worktree::Removed::Whole) => {
                // The lock file stays while its worktree stays: unlink it
                // only after a whole removal, still holding the
                // exclusive take, so a waiter that locked the old inode
                // reopens the new file. An unlink failure keeps nothing
                // to lose, only the one warn line.
                if let Some(lock) = shared::lock_path(&self.created.path)
                    && let Err(source) = std::fs::remove_file(&lock)
                {
                    self.failed(&worktree::Error::Io { path: lock, source });
                }
            }
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
