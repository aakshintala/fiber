//! The loop's part of a shutdown (`docs/invocation.md`, "Shutdown"): once
//! the turn loop has stopped, every job is stopped and each job's end is
//! written before `fiber_exited`. No model reads the session again, so
//! nothing that would only be shown to one is written.

use std::sync::Arc;

use contract::events::{DelegateFinished, Event, JobCompleted};
use contract::inbox::Delivery;
use contract::jobs::Taken;
use contract::{ErrorCode, JobId};

use crate::inbox::{CLOSING, STALE_REPLY, STALE_STEER, reject};
use crate::jobs::{Queued, claimed};
use crate::{Error, Loop};

/// Forgets the normal end's extension-owned ids: what [`Loop::released`]
/// installs and [`Loop::settled`] calls at the top of its normal-end branch.
pub(crate) type ReleaseDelegates = Arc<dyn Fn(&[JobId]) + Send + Sync>;

impl Loop {
    /// Forgets the ids the normal-end branch releases through `release`:
    /// without it a normal end releases nothing and every queued end
    /// still runs `end_extension_job`.
    pub fn released(mut self, release: ReleaseDelegates) -> Self {
        self.release = Some(release);
        self
    }

    /// `result` once the jobs have settled. A normal end stops only
    /// extension-owned jobs; a shutdown stops every job. The first error wins.
    pub(crate) fn settled(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        if self.shutting_down() {
            self.close_extension_starts();
            let settled = self.settle();
            return result.and(settled);
        }
        if self.rewound {
            return result;
        }
        self.close_extension_starts();
        let mut running = self.running();
        if running.is_empty() {
            return result;
        }
        // The release comes first: a queued end for a released id is
        // dropped below, so it cannot run `end_extension_job` and queue
        // a `run` for a binding that is already gone. Without an installed
        // callback nothing is released and nothing is suppressed.
        let released = match &self.release {
            Some(release) => {
                // The extension-owned ids in the branch's snapshot, in the
                // same order: the inverse of `model_running`, since ended
                // ids already left `running`.
                let released: Vec<JobId> = running
                    .iter()
                    .filter(|id| self.handoff.carry.owned(&id.0))
                    .cloned()
                    .collect();
                release(&released);
                released
            }
            None => Vec::new(),
        };
        let settled = (|| {
            // Only the first drain suppresses: it holds the ends queued
            // or deferred at the release. Later drains see only ends that
            // arrived after it, which still run.
            let mut suppress = true;
            loop {
                let deliveries = std::mem::take(&mut self.deferred)
                    .into_iter()
                    .chain(self.inbox.try_iter());
                for delivery in deliveries {
                    if let Delivery::Job(notice) = &delivery {
                        running.retain(|id| id != &notice.completed.job_id);
                        // A released id's queued or deferred end is
                        // dropped: the stop below still ends the job and
                        // its real end is still written, but with no binding
                        // left its completion queues no `run`.
                        if suppress && released.contains(&notice.completed.job_id) {
                            continue;
                        }
                    }
                    self.settle_normal_one(delivery)?;
                }
                suppress = false;
                running.retain(|id| self.handoff.carry.owned(&id.0));
                if running.is_empty() {
                    return Ok(());
                }
                self.stop_jobs(&running);
                match self.inbox.recv() {
                    Ok(delivery) => self.settle_normal_one(delivery)?,
                    Err(_) => return Ok(()),
                }
                running = self.running();
            }
        })();
        result.and(settled)
    }

    /// A normal end still runs delivery hooks on any job end it takes.
    fn settle_normal_one(&mut self, delivery: Delivery) -> Result<(), Error> {
        match delivery {
            Delivery::Job(notice) => self.end_extension_job(notice),
            delivery @ (Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Message(..)
            | Delivery::Model(..)
            | Delivery::Credential(..)
            | Delivery::Reload(_)
            | Delivery::McpReconnect(..)
            | Delivery::Name(..)
            | Delivery::Rewind(..)
            | Delivery::ChangeWorkspace(..)
            | Delivery::Handoff(..)
            | Delivery::Close(_)
            | Delivery::Cancelled
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::ExtensionJob(..)
            | Delivery::ProposeNotes(..)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::ExtensionLog(_)
            | Delivery::ExtensionUsage(_)
            | Delivery::DelegateUsage { .. }
            | Delivery::Shell(..)
            | Delivery::Reply(..)
            | Delivery::Prompt(..)
            | Delivery::InnerCall(_)) => self.settle_one(delivery),
        }
    }

    /// Stops every running job and writes each job end the loop holds or
    /// the inbox delivers, as `job_completed` with no turn, until no job
    /// runs. Steers, handoffs, ending notices and monitor batches are
    /// dropped unwritten. The stop is sent again on every pass, so a call
    /// that moved to the background after the first is stopped too. The
    /// wait has no deadline: the shutdown's bound limits it.
    fn settle(&mut self) -> Result<(), Error> {
        // After `rewound` the queued ends are dropped unwritten, with the
        // rest refused in `settle_one`.
        let queued = std::mem::take(&mut self.queued);
        if !self.rewound {
            for piece in queued {
                match piece {
                    Queued::Job(ahead, completed, delegate) => {
                        self.write_job_end(ahead, completed, delegate)?;
                    }
                    // Claimed here, at the settle that writes it.
                    Queued::Notice(notice) => {
                        if let Some((ahead, completed, delegate)) = claimed(notice) {
                            self.write_job_end(ahead, completed, delegate)?;
                        }
                    }
                    Queued::Steer(..)
                    | Queued::Line(_)
                    | Queued::Handoff(..)
                    | Queued::Pending(..) => {}
                }
            }
        }
        for delivery in std::mem::take(&mut self.deferred) {
            self.settle_one(delivery)?;
        }
        loop {
            let running = self.running();
            if running.is_empty() {
                if let Some(hooks) = &self.hooks {
                    hooks.wait_for_runs();
                }
                // A job's end is sent before `running` stops listing it;
                // extension runs queue their lines before the barrier returns.
                for delivery in self.inbox.try_iter() {
                    self.settle_one(delivery)?;
                }
                return Ok(());
            }
            self.stop_jobs(&running);
            match self.inbox.recv() {
                Ok(delivery) => self.settle_one(delivery)?,
                Err(_) => return Ok(()),
            }
        }
    }

    /// Sends each of `running` its stop. A job already sent one ignores it.
    pub(crate) fn stop_jobs(&self, running: &[JobId]) {
        if let Some(jobs) = &self.ending.jobs {
            for id in running {
                jobs.stop(id);
            }
        }
    }

    /// Refuses every later extension-owned delegate start with `closing`,
    /// before the end path's first `running()` read. A second call changes
    /// nothing; with no jobs wired it does nothing.
    pub(crate) fn close_extension_starts(&self) {
        if let Some(jobs) = &self.ending.jobs {
            jobs.close_extension_starts();
        }
    }

    /// A job's end: the queued records taken ahead first, then its
    /// delegate's final message, bounded as it is written, then
    /// `job_completed`, all through `Transcript::write` with no turn and
    /// no action. No model reads the session after the settle, so the
    /// render has no reader; the lines in the log are unchanged.
    pub(crate) fn write_job_end(
        &mut self,
        ahead: Vec<Taken>,
        completed: JobCompleted,
        delegate: Option<DelegateFinished>,
    ) -> Result<(), Error> {
        for taken in ahead {
            self.write_taken(taken, None, None)?;
        }
        if let Some(finished) = delegate {
            let event = super::calls::job_event(
                &self.log,
                contract::jobs::JobRecord::DelegateFinished(finished),
            );
            self.write(&event, None, None)?;
        }
        self.write(&Event::JobCompleted(completed), None, None)?;
        Ok(())
    }

    /// One delivery taken while the jobs settle.
    fn settle_one(&mut self, delivery: Delivery) -> Result<(), Error> {
        // After `rewound` or `moved` nothing is written: every
        // acknowledgement still waiting is refused `closing`, the rest is
        // dropped (`docs/events.md`, "Rewind", "Change workspace").
        if self.rewound {
            crate::rewind::command::refuse_after_rewound(delivery);
            return Ok(());
        }
        match delivery {
            Delivery::Job(notice) if self.handoff.carry.owned(&notice.completed.job_id.0) => {
                self.end_extension_job(notice)?;
            }
            Delivery::Job(notice) => {
                if let Some((ahead, completed, delegate)) = claimed(notice) {
                    self.write_job_end(ahead, completed, delegate)?;
                }
            }
            // Written like any line still written; unlike a monitor batch,
            // it is not dropped.
            Delivery::ExtensionExec(exec) => self.record_extension_exec(exec)?,
            Delivery::ExtensionJob(started, delegate) => {
                self.record_extension_job(started, delegate)?
            }
            Delivery::ExtensionUsage(line) => self.record_extension_usage(line)?,
            Delivery::DelegateUsage { session, line } => self.copy_usage(&session, line)?,
            Delivery::Interaction(requested) => self.record_interaction(requested)?,
            Delivery::Resolved(resolved, ack) => self.record_resolved(resolved, ack)?,
            Delivery::ExtensionLog(entry) => self.record_extension_log(entry)?,
            // Written and accepted at the settle, as at any drain: it
            // starts no turn.
            Delivery::Shell(line, ack) => self.take_shell(line, ack)?,
            Delivery::Prompt(_, ack)
            | Delivery::ProposeNotes(_, ack)
            | Delivery::Steer(_, ack)
            | Delivery::Handoff(_, _, ack)
            | Delivery::Rewind(_, _, ack)
            | Delivery::ChangeWorkspace(_, _, ack)
            | Delivery::Model(_, ack)
            | Delivery::Reload(ack)
            | Delivery::McpReconnect(_, ack)
            | Delivery::Name(_, ack)
            | Delivery::Credential(_, ack) => {
                reject(ack, ErrorCode::Closing, CLOSING);
            }
            Delivery::SteerDrop(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_STEER),
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => self.take_close(ack),
            Delivery::JobLine(_)
            | Delivery::Message(..)
            | Delivery::InnerCall(_)
            | Delivery::Cancelled => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
