//! A session's own worktree (`docs/invocation.md`, "Isolation"): creating it
//! before `session_started`, and removing it at the end when it holds
//! nothing to lose.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Clock;
use contract::diag::{Op, Timing};
use contract::shapes::Worktree;
use log::diag::{Diag, Level, Process, Severity};

use crate::{SessionError, project};

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
    /// How long the one `worktree::create` or `worktree::recreate` call
    /// took, stamped with the isolation's own clock before and after it.
    /// The writer opens later, so the opener derives the start from this.
    made_in: Duration,
    /// The session's timing once its writer opens: set at most once by
    /// the opener, read by `end` for the removal line.
    timing: Arc<OnceLock<Timing>>,
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
) -> Result<Isolation, SessionError> {
    let repository = project(launch);
    let key = log::project_key(&repository);
    let path = home
        .join("projects")
        .join(&key)
        .join("worktrees")
        .join(&session.0);
    let branch = format!("fiber/{}", session.0);
    // The level is unknown while the worktree is made, so both reads
    // are unconditional: at `info` they are the only extra cost and no
    // line is ever written from them.
    let start = clock.now();
    match worktree::create(launch, &path, &branch, add) {
        Ok(created) => {
            let made_in = elapsed(&clock, start);
            Ok(Isolation::new(
                created, session, home, clock, repository, None, made_in,
            ))
        }
        Err(error) => Err(SessionError::WorktreeFailed {
            message: sentence(&error, &path),
            source: error,
        }),
    }
}

/// A delegate creation failure, kept typed for the caller's argument check.
#[derive(Debug, thiserror::Error)]
pub enum DelegateCreateError {
    /// The requested workspace is not in a repository.
    #[error("{path} is not in a git repository, so it cannot have a worktree.")]
    NotARepository {
        /// The requested workspace.
        path: PathBuf,
    },
    /// A filesystem or git operation failed; the message excludes git output.
    #[error("{message}")]
    Failed {
        /// A safe sentence describing the failure.
        message: String,
    },
}

/// Creates a delegate's worktree while sharing its lock before git adds it.
/// The caller keeps the returned share until the child acknowledges adoption.
pub fn create_delegate_worktree(
    workspace: &Path,
    home: &Path,
    session: &SessionId,
    add: &dyn Fn(&mut Command) -> io::Result<Output>,
) -> Result<(worktree::Created, Shared), DelegateCreateError> {
    let project = config::ProjectKey::new(log::project_key(&project(workspace))).map_err(|_| {
        DelegateCreateError::Failed {
            message: "the repository project key is not valid for Fiber home".to_owned(),
        }
    })?;
    let path = config::project_worktrees_dir(home, &project).join(&session.0);
    let branch = format!("fiber/{}", session.0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DelegateCreateError::Failed {
            message: format!("{}: {}.", parent.display(), source.kind()),
        })?;
    }
    let hold = share(&path).map_err(|error| DelegateCreateError::Failed {
        message: error.to_string(),
    })?;
    let created =
        worktree::create(workspace, &path, &branch, add).map_err(|error| match error {
            worktree::Error::NotARepository { path } => {
                DelegateCreateError::NotARepository { path }
            }
            worktree::Error::Exists { .. }
            | worktree::Error::GitMissing
            | worktree::Error::Git { .. }
            | worktree::Error::Io { .. } => DelegateCreateError::Failed {
                message: sentence(&error, &path),
            },
        })?;
    Ok((created, hold))
}

/// Shares a parent's pre-created delegate worktree before session startup.
pub fn adopt_delegate_worktree(
    path: &Path,
    session: &SessionId,
) -> Result<Option<(Worktree, Shared)>, SessionError> {
    let hold = share(path)?;
    match worktree::inspect(path) {
        Ok(worktree::Inspection::Worktree(found))
            if found.branch == format!("fiber/{}", session.0) =>
        {
            Ok(Some((
                Worktree {
                    path: path.to_string_lossy().into_owned(),
                    branch: found.branch,
                    repository: Some(found.common_dir.to_string_lossy().into_owned()),
                },
                hold,
            )))
        }
        Ok(
            worktree::Inspection::Worktree(_)
            | worktree::Inspection::NotAWorktree
            | worktree::Inspection::Detached,
        ) => Ok(None),
        Err(error) => Err(SessionError::WorktreeAccess {
            message: sentence(&error, path),
            source: error,
        }),
    }
}

/// An exclusive worktree hold for prune, released when dropped.
pub struct RemovalHold {
    _file: std::fs::File,
}

/// Tries to hold a worktree exclusively without waiting. `None` means in use.
pub fn try_hold_removal(path: &Path) -> Result<Option<RemovalHold>, SessionError> {
    match shared::exclusive(path) {
        Ok(shared::Removal::Exclusive(file)) => Ok(Some(RemovalHold { _file: file })),
        Ok(shared::Removal::InUse) => Ok(None),
        Err(error) => Err(SessionError::WorktreeAccess {
            message: sentence(&error, path),
            source: error,
        }),
    }
}

/// Reports a delegate worktree and ends a fresh run by the session's removal
/// rules. Resumed runs report only; their session process owns removal.
#[allow(
    clippy::too_many_arguments,
    reason = "a run end carries the recorded worktree and its owner"
)]
pub fn end_delegate_worktree(
    home: &Path,
    session: &SessionId,
    clock: Arc<dyn Clock>,
    path: &Path,
    branch: &str,
    base: Option<&str>,
    remove: bool,
) -> contract::events::FinishedWorktree {
    let inspected = worktree::inspect(path);
    let dirty = match &inspected {
        Ok(worktree::Inspection::Worktree(found)) => found.uncommitted,
        Ok(worktree::Inspection::NotAWorktree) if !path.exists() => false,
        _ => true,
    };
    if remove
        && let Some(base) = base
        && let Ok(worktree::Inspection::Worktree(found)) = inspected
    {
        Isolation::new(
            worktree::Created {
                path: path.to_owned(),
                branch: branch.to_owned(),
                base: base.to_owned(),
            },
            session,
            home,
            clock,
            found.common_dir,
            None,
            Duration::ZERO,
        )
        .end();
    }
    contract::events::FinishedWorktree {
        worktree: Worktree {
            path: path.to_string_lossy().into_owned(),
            branch: branch.to_owned(),
            repository: None,
        },
        dirty,
    }
}

/// What a recreation found at the recorded path.
#[allow(
    clippy::large_enum_variant,
    reason = "a made worktree owns its isolation; boxing would change the public shape"
)]
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
    pub failure: SessionError,
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
    downgrade: &dyn Fn(shared::Creating) -> Result<Shared, SessionError>,
) -> Result<Recreated, RecreateError> {
    match shared::claim(path, before_wait) {
        Ok(shared::Claim::Present(shared)) => Ok(Recreated::Present(shared)),
        Ok(shared::Claim::Create(creating)) => {
            // Only the one `worktree::recreate` call is stamped: the
            // lock claim above and the downgrade below stay outside.
            let start = clock.now();
            let created = match worktree::recreate(repository, path, branch, add) {
                Ok(created) => created,
                Err(error) => {
                    return Err(RecreateError {
                        failure: recreate_failure(error, repository, path),
                        isolation: None,
                    });
                }
            };
            let made_in = elapsed(&clock, start);
            match downgrade(creating) {
                Ok(shared) => Ok(Recreated::Made(Isolation::new(
                    created,
                    session,
                    home,
                    clock,
                    repository.to_owned(),
                    Some(shared),
                    made_in,
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
                        made_in,
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
fn recreate_failure(error: worktree::Error, repository: &Path, path: &Path) -> SessionError {
    match error {
        worktree::Error::NotARepository { .. } => SessionError::RepositoryGone {
            repository: repository.to_owned(),
            path: path.to_owned(),
        },
        worktree::Error::Exists { .. }
        | worktree::Error::GitMissing
        | worktree::Error::Git { .. }
        | worktree::Error::Io { .. } => SessionError::WorktreeFailed {
            message: sentence(&error, path),
            source: error,
        },
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

/// Opens the worktree's create line once the diagnostic writer exists.
pub type WorktreeOpener = Arc<dyn Fn(&Timing) + Send + Sync>;

/// How long the one stamped `worktree::create` or `worktree::recreate`
/// call took, from `start` to now: a clock that ran backwards reads as
/// zero.
fn elapsed(clock: &Arc<dyn Clock>, start: Instant) -> Duration {
    clock
        .now()
        .checked_duration_since(start)
        .unwrap_or(Duration::ZERO)
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
        made_in: Duration,
    ) -> Self {
        Self {
            created,
            session: session.clone(),
            home: home.to_owned(),
            clock,
            repository,
            shared,
            made_in,
            timing: Arc::new(OnceLock::new()),
            #[cfg(test)]
            pause: None,
        }
    }

    /// Opens the slot for the create line: the only way a caller emits
    /// it. The call that sets the slot emits one `worktree.create` line
    /// from the stamped duration, starting at `now` minus `made_in`; a
    /// clock that ran backwards starts at `now`. Later calls emit
    /// nothing. A worktree adopted as present has no `Isolation` and no
    /// opener, so it writes none.
    pub fn opener(&self) -> WorktreeOpener {
        let slot = Arc::clone(&self.timing);
        let clock = Arc::clone(&self.clock);
        let made_in = self.made_in;
        Arc::new(move |timing: &Timing| {
            if slot.set(timing.clone()).is_ok() {
                let now = clock.now();
                let start = now.checked_sub(made_in).unwrap_or(now);
                drop(timing(Op::WorktreeCreate, None, Some(start)));
            }
        })
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
    // A let chain does not compile with `||`, so the `&&` mutant is unviable.
    #[cfg_attr(false, mutants::skip)]
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
        // A process whose log ends `rewound` or `moved` never removes its
        // worktree: the session that continues it runs there after a
        // rewind, and the old tree stays for a later prune after a move
        // (`docs/invocation.md`, "Isolation"). The creator's lock above
        // holds its log still, so its last line is read under it, before
        // any git command runs.
        if first
            .iter()
            .find(|user| user.id == self.session)
            .and_then(|creator| log::last_line(&creator.dir))
            .is_some_and(|line| line.kind == "rewound" || line.kind == "moved")
        {
            return;
        }
        // A session that continues in this worktree keeps it after the
        // new session exits: its `session_started` records the same
        // worktree path (`docs/invocation.md`, "Isolation"). The
        // session's own row always matches, so it is excluded. While
        // the new session lives, the exclusive take above already keeps
        // the worktree; this covers the new session exiting first.
        if first
            .iter()
            .filter(|user| user.id != self.session)
            .filter_map(|user| user.worktree.as_deref())
            .any(|recorded| same_worktree(recorded, &self.created.path))
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
        // The removal line covers the one real removal only: the guard
        // starts immediately before it and drops right after it returns,
        // before the lock-file unlink and while every hold is still
        // held. With no opened timing nothing is read or called.
        let removal = self
            .timing
            .get()
            .and_then(|timing| timing(Op::WorktreeRemove, None, None));
        let removed = worktree::remove(&self.created.path, &inspected, false);
        drop(removal);
        match removed {
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

/// Whether the worktree path a session's `session_started` records names
/// the created worktree: the exact spelling first, else both sides
/// canonicalized, so an alias spelling of the same directory keeps. Any
/// canonicalization failure is not a match.
fn same_worktree(recorded: &str, created: &Path) -> bool {
    recorded == created.to_string_lossy()
        || match (
            std::fs::canonicalize(recorded),
            std::fs::canonicalize(created),
        ) {
            (Ok(recorded), Ok(created)) => recorded == created,
            _ => false,
        }
}

#[cfg(test)]
#[path = "isolation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "isolation/timing_tests.rs"]
mod timing_tests;
