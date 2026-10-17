//! Stopping every running job for a rewind (`docs/events.md`, "Rewind"): a
//! rewind never adopts jobs, so a job running when the rewind is accepted
//! is stopped, its end written before `rewound`, and the new session starts
//! with no jobs.

use std::sync::mpsc::RecvTimeoutError;

use contract::inbox::Delivery;

use crate::{Error, Loop};

/// How long a rewind waits for running jobs to end: the shutdown's
/// per-process bound.
pub(crate) const STOP_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

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
                let deliveries = self.inbox.try_iter().collect::<Vec<_>>();
                for delivery in deliveries {
                    self.stop_one(delivery)?;
                }
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
                    let deliveries = self.inbox.try_iter().collect::<Vec<_>>();
                    for delivery in deliveries {
                        self.stop_one(delivery)?;
                    }
                    return Ok(());
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
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
