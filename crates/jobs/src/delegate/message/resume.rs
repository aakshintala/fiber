//! Resuming a finished delegate as a new run of the same job: reading the
//! run's boundary, launching the child, and the receipt that drives it.
//! The steering attempt's state and lifecycle (the commit gate, backoff,
//! closing wait and answer) stays in the parent module.

use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;
use std::thread;

use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};
use contract::{ActionId, ErrorCode, JobId, SessionId};

use super::{Attempt, DelegateMessage, fail, nothing_delivered};
use crate::delegate::run::{Launched, Runner};
use crate::registry::runs::{Resume, Resumed};
use crate::tool::unknown_message;
use support::group::Listing;

/// Reads the `seq` of a delegate log's last complete line, or the reason
/// it could not be read, which names the log's path. A reason string, as
/// `Resolve`'s lookup answers today, because `jobs` cannot name
/// `log::Error` (`docs/architecture.md`, "The call rules"). `Ok(None)`
/// only for a regular, empty log.
pub type LastSeq = Arc<dyn Fn(&SessionId) -> Result<Option<u64>, String> + Send + Sync>;

/// Why starting the resumed run failed: nothing was started, nothing
/// queued.
pub(crate) enum StartFailed {
    /// The log's last line could not be read; the reason names its path.
    Boundary(String),
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
            Err(Resume::Unknown) => {
                let message = unknown_message(id);
                Attempt::Done(fail(ErrorCode::InvalidArguments, message))
            }
            Err(Resume::NotDelegate) => Attempt::Done(fail(
                ErrorCode::InvalidArguments,
                format!("Job `{id}` is not a delegate."),
            )),
            Err(Resume::Live(session, run)) => Attempt::Found(session, run),
            Err(Resume::Exhausted) => Attempt::Done(fail(
                ErrorCode::ToolError,
                format!("Delegate {id} cannot run again: its run count is exhausted."),
            )),
            Err(Resume::Start(StartFailed::Boundary(reason))) => Attempt::Done(fail(
                ErrorCode::ToolError,
                format!(
                    "Resuming delegate {id} failed: its log could not be read: {reason}. Nothing was started."
                ),
            )),
            Err(Resume::Start(StartFailed::Spawn(source))) => Attempt::Done(fail(
                ErrorCode::ToolError,
                format!("Resuming the delegate failed: {source}."),
            )),
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
        self.registry.resume(&id, action, |resumable| {
            // Inside the resume gate, after the "ended" check: no other
            // run can start between the read and the spawn.
            let after = match last_seq(&resumable.delegate.delegate_session_id) {
                Ok(after) => after,
                Err(reason) => return Err(StartFailed::Boundary(reason)),
            };
            let launched = Launched {
                session_id: resumable.delegate.delegate_session_id.clone(),
                job_id: JobId(id.clone()),
                parent: parent.clone(),
                model: resumable.delegate.model.clone(),
                prompt: message.clone(),
                workspace: PathBuf::from(&resumable.delegate.workspace),
                resume: true,
            };
            let log = resumable.output_path.clone();
            let (runner, stop) = Runner::new(
                JobId(id.clone()),
                launched.session_id.clone(),
                PathBuf::from(&resumable.output_path),
                Arc::clone(&clock),
                bound,
                cap,
                Arc::clone(&watch),
                after,
            );
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
            },
    } = resumed;
    thread::spawn(move || runner.drive(child, Some(listing), unreported));
    Output {
        content: vec![ContentPart::Text {
            text: format!("Resumed delegate {id}. Its log is {log}."),
        }],
        settle: Some(settle),
        ..Output::default()
    }
}
