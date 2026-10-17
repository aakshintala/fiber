//! Stopping every running job for a rewind (`docs/events.md`, "Rewind"): a
//! rewind never adopts jobs, so a job running when the rewind is accepted
//! is stopped, its end written before `rewound`, and the new session starts
//! with no jobs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;

use contract::events::{Event, JobCompleted, Outcome};
use contract::inbox::Delivery;
use contract::{ErrorCode, JobId};

use crate::{Error, Loop};

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;

/// How long a rewind waits for running jobs to end: 5 s on the loop's
/// injected clock, the shutdown's per-process bound (`docs/invocation.md`,
/// "Shutdown"). A stopped delegate's parent waits up to the bound for it
/// to exit, so a shorter wait would cut it off while it writes its own
/// `fiber_exited`.
pub(crate) const STOP_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// One job the note lists as stopped (`docs/events.md`, "Rewind"): its
/// id, and its `job_started` description when the old log holds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoppedJob {
    pub(crate) job_id: JobId,
    pub(crate) description: Option<String>,
}

/// Whether `kind` is a job-record line: the four kinds the stop wait and
/// the Grant 2 batch write between `write_last_settled` and `rewound`, so
/// a line of any other kind ends the run.
fn is_job_record(kind: &str) -> bool {
    matches!(
        kind,
        "job_started" | "delegate_started" | "delegate_finished" | "job_completed"
    )
}

/// Whether `completed` lists its job as stopped: `cancelled`, or `failed`
/// with `error.code` `indeterminate`. A natural end is another status, and
/// an `orphaned` completion another code, so neither lists it.
fn is_stopped(completed: &JobCompleted) -> bool {
    matches!(completed.status, Outcome::Cancelled)
        || matches!(completed.status, Outcome::Failed)
            && completed
                .error
                .as_ref()
                .is_some_and(|error| error.code == ErrorCode::Indeterminate)
}

/// The jobs a rewind stopped, read from the old session's own log: the
/// run of job-record lines directly before the last line, when that line
/// is `rewound`, in log order. A log not ending in `rewound` yields
/// nothing, as does a run with no listed end. An end the `rewind` found
/// already admitted carries the marker turn
/// ([`Loop::take_rewind`]'s Grant 2 batch) and is skipped: the person
/// stopped that job, not the rewind. In-turn ends never reach the run:
/// every turn closes with `turn_completed` before an idle `rewind`, and
/// that kind breaks the run.
///
/// Sound because only two writers run between `write_last_settled` and
/// `rewound`: the Grant 2 batch, whose ends carry the marker turn, and
/// `stop_jobs_for_rewind`, whose ends carry none, and both write only
/// job-record lines there.
pub(crate) fn stopped_jobs(old: &Path) -> Result<Vec<StoppedJob>, Error> {
    let lines = log::read(old)?;
    let Some(last) = lines.last() else {
        return Ok(Vec::new());
    };
    if last.kind != "rewound" {
        return Ok(Vec::new());
    }
    let mut run = Vec::new();
    for line in lines.iter().rev().skip(1) {
        if !is_job_record(&line.kind) {
            break;
        }
        run.push(line);
    }
    run.reverse();
    let mut descriptions = HashMap::new();
    for line in &run {
        if let Some(Event::JobStarted(started)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
        {
            descriptions.insert(started.job_id.clone(), started.description.clone());
        }
    }
    let mut stopped = Vec::new();
    for line in &run {
        if let Some(Event::JobCompleted(completed)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
            && is_stopped(&completed)
            // Admitted news carries the marker turn: the person stopped
            // that job, not the rewind. The stop wait's own ends carry
            // no turn.
            && line.turn_id.is_none()
        {
            stopped.push(StoppedJob {
                job_id: completed.job_id.clone(),
                description: descriptions.get(&completed.job_id).cloned(),
            });
        }
    }
    Ok(stopped)
}

impl Loop {
    /// Stops every running job and writes each end that arrives, before the
    /// rewind writes `rewound`. With no jobs wired it does nothing. When
    /// `running` reads empty it still runs one `try_iter` drain of
    /// already-waiting deliveries, as `settle` does at its empty branch,
    /// then returns. Otherwise it sends `stop` for every running job, again
    /// on every pass, and reads with `recv_bounded` against one deadline
    /// computed only once a job is running. Every delivery except a job's
    /// end goes through `refuse_after_rewound`, which answers an
    /// acknowledgement `closing` and drops the rest. `Timeout` and
    /// `Disconnected` both end the wait: a closed inbox with a job still
    /// running returns at once instead of spinning, and the final inbox
    /// drain always runs, even when no job is running.
    pub(crate) fn stop_jobs_for_rewind(&mut self) -> Result<(), Error> {
        if !self.has_jobs() {
            return Ok(());
        }
        let mut deadline = None;
        loop {
            let running = self.running();
            if running.is_empty() {
                self.drain_waiting()?;
                return Ok(());
            }
            self.stop_jobs(&running);
            let until = match deadline {
                Some(until) => until,
                None => {
                    let computed = self.log.clock().now() + STOP_BOUND;
                    deadline = Some(computed);
                    computed
                }
            };
            match self.recv_bounded(Some(until)) {
                Ok(delivery) => self.stop_one(delivery)?,
                Err(RecvTimeoutError::Timeout) => {
                    self.drain_waiting()?;
                    return Ok(());
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    /// One drain of the deliveries already waiting in the inbox: each is
    /// stopped, a job's end written and anything else refused as after
    /// `rewound`.
    fn drain_waiting(&mut self) -> Result<(), Error> {
        let deliveries = self.inbox.try_iter().collect::<Vec<_>>();
        for delivery in deliveries {
            self.stop_one(delivery)?;
        }
        Ok(())
    }

    /// One delivery during the stop wait: a job's end is written under its
    /// own shape, and everything else is refused as after `rewound`.
    fn stop_one(&mut self, delivery: Delivery) -> Result<(), Error> {
        match delivery {
            Delivery::Job(notice) => {
                if let Some((ahead, completed, delegate)) = crate::jobs::claimed(notice) {
                    self.write_job_end(ahead, completed, delegate)?;
                }
                Ok(())
            }
            other @ (Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Message(_)
            | Delivery::Model(..)
            | Delivery::Credential(..)
            | Delivery::Name(..)
            | Delivery::Rewind(..)
            | Delivery::Handoff(..)
            | Delivery::Reply(..)
            | Delivery::Close(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::InnerCall(_)
            | Delivery::ExtensionLog(_)
            | Delivery::Shell(..)
            | Delivery::Cancelled) => {
                crate::rewind::command::refuse_after_rewound(other);
                Ok(())
            }
        }
    }
}
