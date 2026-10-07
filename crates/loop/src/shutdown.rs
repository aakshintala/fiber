//! The loop's part of a shutdown (`docs/invocation.md`, "Shutdown"): once
//! the turn loop has stopped, every job is stopped and each job's end is
//! written before `fiber_exited`. No model reads the session again, so
//! nothing that would only be shown to one is written.

use contract::events::Event;
use contract::inbox::Delivery;
use contract::{ErrorCode, JobId};

use crate::inbox::{CLOSING, STALE_REPLY, STALE_STEER, accept, reject};
use crate::jobs::{Queued, claimed};
use crate::{Error, Loop};

impl Loop {
    /// `result` once the jobs have settled, when a shutdown started; else
    /// `result` unchanged. The first error wins.
    pub(crate) fn settled(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        if !self.shutting_down() {
            return result;
        }
        let settled = self.settle();
        result.and(settled)
    }

    /// Stops every running job and writes each job end the loop holds or
    /// the inbox delivers, as `job_completed` with no turn, until no job
    /// runs. Steers, handoffs, ending notices and monitor batches are
    /// dropped unwritten. The stop is sent again on every pass, so a call
    /// that moved to the background after the first is stopped too. The
    /// wait has no deadline: the shutdown's bound limits it.
    fn settle(&mut self) -> Result<(), Error> {
        for piece in std::mem::take(&mut self.queued) {
            if let Queued::Job(completed) = piece {
                self.log
                    .append(&Event::JobCompleted(completed), None, None)?;
            }
        }
        for delivery in std::mem::take(&mut self.deferred) {
            self.settle_one(delivery)?;
        }
        loop {
            let running = self.running();
            if running.is_empty() {
                // A job's end is sent before `running` stops listing it, so
                // what is waiting now holds the last ends.
                for delivery in self.inbox.try_iter().collect::<Vec<_>>() {
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
    fn stop_jobs(&self, running: &[JobId]) {
        if let Some(jobs) = &self.ending.jobs {
            for id in running {
                jobs.stop(id);
            }
        }
    }

    /// One delivery taken while the jobs settle.
    fn settle_one(&mut self, delivery: Delivery) -> Result<(), Error> {
        match delivery {
            Delivery::Job(notice) => {
                if let Some(completed) = claimed(notice) {
                    self.log
                        .append(&Event::JobCompleted(completed), None, None)?;
                }
            }
            // Written like any line still written; unlike a monitor batch,
            // it is not dropped.
            Delivery::ExtensionExec(exec) => {
                self.log.append(&Event::ExtensionExec(exec), None, None)?;
            }
            Delivery::ExtensionLog(entry) => {
                self.log
                    .append(&Event::ExtensionLog(entry.clone()), None, None)?;
                self.diag.extension_log(&entry.extension, &entry.message);
            }
            Delivery::Prompt(_, ack) | Delivery::Steer(_, ack) | Delivery::Handoff(_, _, ack) => {
                reject(ack, ErrorCode::Closing, CLOSING);
            }
            Delivery::SteerDrop(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_STEER),
            Delivery::Reply(_, ack) => reject(ack, ErrorCode::StaleRequest, STALE_REPLY),
            Delivery::Close(ack) => accept(ack),
            Delivery::JobLine(_) | Delivery::Cancelled => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
