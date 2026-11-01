//! Resuming a finished delegate as a new run of the same job: reading the
//! run's boundary, launching the child, and the receipt that drives it.
//! The steering attempt's state and lifecycle (the commit gate, backoff,
//! closing wait and answer) stays in the parent module.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use contract::diag::{Op, Timing};

use super::{Attempt, DelegateMessage, nothing_delivered};
use crate::delegate::run::{Launched, Runner, Start};
use crate::error::JobsError;
use crate::registry::runs::{Resume, Resumed};
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};
use contract::{ActionId, JobId, SessionId};
use support::group::Listing;

/// Reads the `seq` of a delegate log's last complete line, or the reason
/// it could not be read, which names the log's path. A reason string, as
/// `Resolve`'s lookup answers today, because `jobs` cannot name
/// `log::Error` (`docs/architecture.md`, "The call rules"). `Ok(None)`
/// only for a regular, empty log.
pub type LastSeq = Arc<dyn Fn(&Path, &SessionId) -> Result<Option<u64>, String> + Send + Sync>;

/// Reads the delegate's durable worktree record in its workspace's project.
/// `None` means the session recorded no worktree; a read error fails closed.
pub type WorktreeRecord = Arc<
    dyn Fn(&Path, &SessionId) -> Result<Option<contract::shapes::Worktree>, String> + Send + Sync,
>;

/// Why starting the resumed run failed: nothing was started, nothing
/// queued.
pub(crate) enum StartFailed {
    /// The log's worktree record or last line could not be read; the reason names its path.
    Boundary(String),
    /// Neither the workspace nor a recorded repository is available.
    Workspace(String),
    /// The child could not be spawned.
    Spawn(io::Error),
}

/// What a resumed run's `start` returns: the runner's handles and the log
/// the receipt names.
struct Started {
    runner: Runner,
    child: Child,
    listing: Listing,
    log: String,
    /// The start's stamp, carried out of the gated closure: the closure
    /// only reads the clock, and the guard is created and dropped once
    /// the run is committed and its thread runs.
    start: Option<(Timing, Instant)>,
}

impl DelegateMessage {
    /// Resumes the ended delegate, or fails what the registry answers. A
    /// cancel before the resume starts nothing. The caller holds the
    /// steering-gate decision: it only calls here when no send is
    /// committed, so this never reads the gate.
    pub(crate) fn resume_now(
        &self,
        id: &str,
        message: &str,
        action: &Option<ActionId>,
        cancel: &dyn Cancel,
    ) -> Attempt {
        // A cancel after this check can still start the resume: the run
        // exists, and its start must be logged.
        if cancel.is_cancelled() {
            return Attempt::Done(nothing_delivered(id));
        }
        match self.resume(id, message, action.clone()) {
            Ok(resumed) => Attempt::Done(resumed_receipt(id, resumed)),
            Err(Resume::Full) => Attempt::Done(JobsError::DelegatesFull.output(true)),
            Err(Resume::Unknown) => Attempt::Done(
                JobsError::UnknownJob {
                    id: JobId(id.to_owned()),
                }
                .output(true),
            ),
            Err(Resume::NotDelegate) => Attempt::Done(
                JobsError::NotDelegate {
                    id: JobId(id.to_owned()),
                }
                .output(true),
            ),
            Err(Resume::Live(session, run)) => Attempt::Found(session, run),
            Err(Resume::Exhausted) => Attempt::Done(
                JobsError::ResumeExhausted {
                    id: JobId(id.to_owned()),
                }
                .output(true),
            ),
            Err(Resume::Start(StartFailed::Boundary(reason))) => Attempt::Done(
                JobsError::ResumeBoundary {
                    id: JobId(id.to_owned()),
                    reason,
                }
                .output(true),
            ),
            Err(Resume::Start(StartFailed::Workspace(message))) => Attempt::Done(
                JobsError::DelegateWorktree {
                    failure: contract::shapes::Failure {
                        code: contract::ErrorCode::IoFailed,
                        message,
                        retry_after_ms: None,
                        provider: None,
                    },
                }
                .output(true),
            ),
            Err(Resume::Start(StartFailed::Spawn(source))) => {
                Attempt::Done(JobsError::ResumeSpawn { source }.output(true))
            }
        }
    }

    /// Starts run `job.run + 1` of the ended delegate `id`: reads the
    /// run's boundary, then launches the new run. A failed read fails
    /// closed and starts nothing.
    fn resume(
        &self,
        id: &str,
        message: &str,
        action: Option<ActionId>,
    ) -> Result<Resumed<Started>, Resume<StartFailed>> {
        let parent = self.parent.clone();
        let message = message.to_owned();
        let id = id.to_owned();
        let launch = Arc::clone(&self.launch);
        let watch = Arc::clone(&self.watch);
        let last_seq = Arc::clone(&self.last_seq);
        let clock = Arc::clone(&self.clock);
        let bound = self.bound;
        let cap = self.cap;
        // Cloned before the resume gate is taken: the start closure only
        // reads the clock, never creates or drops a guard under the gate.
        let timing = self.registry.timing().cloned();
        self.registry.resume(&id, action, |resumable| {
            // Inside the resume gate, after the "ended" check: no other
            // run can start between the read and the spawn.
            let workspace = Path::new(&resumable.delegate.workspace);
            let recorded =
                (self.worktree_record)(workspace, &resumable.delegate.delegate_session_id)
                    .map_err(StartFailed::Boundary)?;
            let repository = recorded
                .as_ref()
                .and_then(|record| record.repository.as_deref())
                .map(Path::new);
            let launch_workspace = if workspace.is_dir() {
                workspace.to_path_buf()
            } else if let Some(repository) = repository.filter(|repository| repository.is_dir()) {
                repository.to_path_buf()
            } else {
                let reason = match repository {
                    Some(repository) => format!(
                        "recorded repository `{}` is unavailable",
                        repository.display()
                    ),
                    None => "there is no recorded repository".to_owned(),
                };
                return Err(StartFailed::Workspace(format!(
                    "Cannot resume delegate in missing workspace `{}`; {reason}.",
                    workspace.display()
                )));
            };
            let after = match last_seq(workspace, &resumable.delegate.delegate_session_id) {
                Ok(after) => after,
                Err(reason) => return Err(StartFailed::Boundary(reason)),
            };
            let sandbox = (self.sandbox)().map_err(|key| {
                StartFailed::Spawn(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cannot inherit {key}: path is not UTF-8"),
                ))
            })?;
            let launched = Launched {
                depth: self.depth.saturating_add(1),
                session_id: resumable.delegate.delegate_session_id.clone(),
                job_id: JobId(id.clone()),
                parent: parent.clone(),
                prompt: message.clone(),
                workspace: launch_workspace,
                // A resume continues the child's own session: its log
                // already holds every inherited root, so no argv carries
                // them again.
                added: Vec::new(),
                sandbox,
                worktree: None,
                start: Start::Resume,
            };
            let log = resumable.output_path.clone();
            // The stamp covers building the runner and spawning the
            // child. No clock is read without an installed timing.
            let start = timing.as_ref().map(|timing| (timing.clone(), clock.now()));
            let (runner, stop) = Runner::new(
                JobId(id.clone()),
                launched.session_id.clone(),
                PathBuf::from(&resumable.output_path),
                Arc::clone(&clock),
                bound,
                cap,
                Arc::clone(&watch),
                after,
                None,
                Some(self.registry.usage_forward()),
            );
            let runner = runner.with_worktree(recorded.as_ref().map(|record| {
                super::super::run::WorktreeRun {
                    end: super::super::EndWorktree {
                        path: PathBuf::from(&record.path),
                        branch: record.branch.clone(),
                        base: None,
                        remove: false,
                    },
                    guard: None,
                    finish: Arc::clone(&self.finish),
                }
            }));
            let (child, listing) = match runner.spawn(&launch, &launched) {
                Ok(started) => started,
                Err(source) => return Err(StartFailed::Spawn(source)),
            };
            Ok((
                stop,
                Started {
                    runner,
                    child,
                    listing,
                    log,
                    start,
                },
            ))
        })
    }
}

/// The receipt of a committed resume, driving the new run on its own
/// thread. The start is always logged: the `Settle` writes it in
/// `finish`, or the run's notice claim writes it when the output drops.
fn resumed_receipt(id: &str, resumed: Resumed<Started>) -> Output {
    let Resumed {
        settle,
        unreported,
        value:
            Started {
                runner,
                child,
                listing,
                log,
                start,
            },
    } = resumed;
    thread::spawn(move || runner.drive(child, Some(listing), unreported));
    // The run is committed and its own drive thread has been spawned:
    // the start ends here, after the resume gate is released.
    if let Some((timing, started)) = start {
        drop(timing(Op::JobsStart, None, Some(started)));
    }
    Output {
        content: vec![ContentPart::Text {
            text: format!("Resumed delegate {id}. Its log is {log}."),
        }],
        settle: Some(settle),
        ..Output::default()
    }
}
