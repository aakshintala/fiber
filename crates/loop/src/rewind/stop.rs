//! Stopping every running job for a rewind (`docs/events.md`, "Rewind"): a
//! rewind never adopts jobs, so a job running when the rewind is accepted
//! is stopped, its end written before `rewound`, and the new session starts
//! with no jobs.

use std::collections::HashMap;
use std::path::Path;

use contract::events::{Event, JobCompleted, Outcome};
use contract::inbox::Delivery;
use contract::{ErrorCode, JobId};

use crate::{Error, Loop};

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;

/// One job the note lists as stopped (`docs/events.md`, "Rewind"): its
/// id, and its `job_started` description when the old log holds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoppedJob {
    pub(crate) job_id: JobId,
    pub(crate) description: Option<String>,
}

/// Whether `kind` is a job-record line: the four kinds the stop wait writes
/// between `write_last_settled` and `rewound`, so nothing else in the run
/// is the stop's.
///
/// debt: raw kind strings duplicating the `Event` variants, ceiling six
/// kinds and two passes; decode each line once and match on the variants
/// when this reader grows a third pass or a seventh kind.
fn is_job_record(kind: &str) -> bool {
    matches!(
        kind,
        "job_started" | "delegate_started" | "delegate_finished" | "job_completed"
    )
}

/// Whether `kind` is a line a hook wrote beside a stop: extension state
/// carried with a changed end. The scan reads past these without
/// crossing the settlement boundary below. A hook's `notice` is
/// ephemeral and never reaches the log, so it cannot interleave here.
fn is_hook_line(kind: &str) -> bool {
    matches!(kind, "extension_state_set" | "extension_state_unset")
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
/// is `rewound`, in log order, each joined to its `job_started` across
/// the whole log for its description. A log not ending in `rewound`
/// yields nothing, as does a run with no listed end.
///
/// Sound because `stop_jobs_for_rewind` is the only writer between
/// `write_last_settled` and `rewound`, and it writes only job-record
/// lines there.
pub(crate) fn stopped_jobs(old: &Path) -> Result<Vec<StoppedJob>, Error> {
    let lines = log::read(old)?;
    let Some(last) = lines.last() else {
        return Ok(Vec::new());
    };
    if last.kind != "rewound" {
        return Ok(Vec::new());
    }
    // A start is written when its job starts, long before the trailing
    // run, so the join is by id across the whole log. A job with no
    // `job_started` in the log is listed by id alone.
    let mut descriptions = HashMap::new();
    for line in &lines {
        if let Some(Event::JobStarted(started)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
        {
            descriptions.insert(started.job_id.clone(), started.description.clone());
        }
    }
    let mut run = Vec::new();
    for line in lines.iter().rev().skip(1) {
        if is_hook_line(&line.kind) {
            continue;
        }
        if !is_job_record(&line.kind) {
            break;
        }
        run.push(line);
    }
    run.reverse();
    let mut stopped = Vec::new();
    for line in &run {
        if let Some(Event::JobCompleted(completed)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
            && is_stopped(&completed)
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
    /// on every pass, and reads with no extra deadline of its own: the
    /// bound on a job's stop is the normal job stop's
    /// (`docs/events.md`, "Rewind"). Every delivery except a job's
    /// end goes through `refuse_after_rewound`, which answers an
    /// acknowledgement `closing` and drops the rest. Only a closed inbox
    /// ends the wait early: `Disconnected` returns at once instead of
    /// spinning, and the final inbox drain always runs, even when no job
    /// is running.
    /// Stops every running job and writes each end that arrives, before the
    /// rewind writes `rewound`. With no jobs wired it does nothing. When
    /// `running` reads empty it still runs one `try_iter` drain of
    /// already-waiting deliveries, as `settle` does at its empty branch,
    /// then returns. Otherwise it sends `stop` for every running job, again
    /// on every pass, and reads with no extra deadline of its own: the
    /// bound on a job's stop is the normal job stop's
    /// (`docs/events.md`, "Rewind"). Every delivery except a job's
    /// end goes through `refuse_after_rewound`, which answers an
    /// acknowledgement `closing` and drops the rest. Only a closed inbox
    /// ends the wait early: `Disconnected` returns at once instead of
    /// spinning, and the final inbox drain always runs, even when no job
    /// is running.
    pub(crate) fn stop_jobs_for_rewind(&mut self) -> Result<(), Error> {
        self.stop_jobs_for_transition(false)
    }

    /// Stops every running job and writes each end that arrives, before the
    /// move writes `moved`: the same wait `stop_jobs_for_rewind` runs,
    /// with the same disconnect behavior, except a competing `rewind` or
    /// `change_workspace` is refused `busy` until `moved` is durable
    /// (`docs/events.md`, "Change workspace").
    pub(crate) fn stop_jobs_for_move(&mut self) -> Result<(), Error> {
        self.stop_jobs_for_transition(true)
    }

    /// The shared stop wait for a rewind's or a move's last line: sends
    /// `stop` for every running job, again on every pass, and reads with
    /// no extra deadline of its own. When `busy_competitors` a competing
    /// `rewind` or `change_workspace` is refused `busy`; otherwise every
    /// delivery except a job's end goes through `refuse_after_rewound`.
    fn stop_jobs_for_transition(&mut self, busy_competitors: bool) -> Result<(), Error> {
        if !self.has_jobs() {
            return Ok(());
        }
        loop {
            let running = self.running();
            if running.is_empty() {
                self.drain_waiting(busy_competitors)?;
                return Ok(());
            }
            self.stop_jobs(&running);
            match self.recv_bounded(None) {
                Ok(delivery) => self.stop_one(delivery, busy_competitors)?,
                // With no deadline the wait never times out: only a
                // closed inbox ends it.
                Err(_) => return Ok(()),
            }
        }
    }

    /// One drain of the deliveries already waiting in the inbox: each is
    /// stopped, a job's end written, a competing transition refused
    /// `busy` when `busy_competitors`, and anything else refused as after
    /// `rewound`.
    fn drain_waiting(&mut self, busy_competitors: bool) -> Result<(), Error> {
        let deliveries = self.inbox.try_iter().collect::<Vec<_>>();
        for delivery in deliveries {
            self.stop_one(delivery, busy_competitors)?;
        }
        Ok(())
    }

    /// One delivery during the stop wait: a job's end is redacted and
    /// hooked like any end, then written under its own shape; a competing
    /// `rewind` or `change_workspace` is refused `busy` when
    /// `busy_competitors`, since its last line is not durable yet; and
    /// everything else is refused as after `rewound`.
    fn stop_one(&mut self, delivery: Delivery, busy_competitors: bool) -> Result<(), Error> {
        match delivery {
            Delivery::Job(notice) => {
                self.redact_output(&notice.completed, None)?;
                if let Some((ahead, completed, delegate)) = crate::jobs::claimed(notice) {
                    let (ahead, completed, delegate) =
                        self.deliver_end(None, ahead, completed, delegate)?;
                    self.write_job_end(ahead, completed, delegate)?;
                }
                Ok(())
            }
            Delivery::Rewind(.., ack) | Delivery::ChangeWorkspace(.., ack) if busy_competitors => {
                crate::inbox::reject(ack, ErrorCode::Busy, super::workspace::MOVE_RUNNING);
                Ok(())
            }
            other @ (Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Message(..)
            | Delivery::Model(..)
            | Delivery::Reload(..)
            | Delivery::McpReconnect(..)
            | Delivery::Credential(..)
            | Delivery::Name(..)
            | Delivery::Rewind(..)
            | Delivery::ChangeWorkspace(..)
            | Delivery::Handoff(..)
            | Delivery::Reply(..)
            | Delivery::Close(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::InnerCall(_)
            | Delivery::ExtensionLog(_)
            | Delivery::ProposeNotes(..)
            | Delivery::ExtensionJob(..)
            | Delivery::ExtensionUsage(_)
            | Delivery::DelegateUsage { .. }
            | Delivery::Shell(..)
            | Delivery::Cancelled) => {
                crate::rewind::command::refuse_after_rewound(other);
                Ok(())
            }
        }
    }
}
