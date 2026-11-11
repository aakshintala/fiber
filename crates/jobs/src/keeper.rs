//! Keeper lifecycle decisions without IO (`docs/tools.md`, "The keeper").

use std::collections::BTreeMap;

use contract::{JobId, keeper::Exit};

/// An IO action chosen by the keeper's lifecycle state.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Stop and reap a tool call or a shutdown job.
    Stop {
        /// The hosted call.
        id: String,
    },
    /// Append an adopted job's ending while holding the log.
    WriteJobCompleted {
        /// The hosted call.
        id: String,
        /// The durable job.
        job_id: JobId,
        /// The reaped ending.
        exit: Exit,
        /// A live-session exit may already have been written before the crash.
        unless_logged: bool,
    },
    /// Append a held job's bound ending while holding the log.
    WriteKeeperTimeout {
        /// The durable job.
        job_id: JobId,
        /// The reaped ending, carried through so observed process details survive.
        exit: Exit,
    },
    /// Acquire the log before any completion is written.
    HoldLog,
    /// No watched jobs remain; release the log and exit.
    Exit,
}

#[derive(Default)]
enum Phase {
    #[default]
    Live,
    Holding,
    /// A resuming session took the log and holds these jobs' endings.
    HandedOver,
    Shutdown,
    Gone,
}

struct Process {
    job: Option<JobId>,
    exit: Option<Exit>,
    expired: bool,
}

/// Decides which processes survive lifeline EOF and which endings need writing.
#[derive(Default)]
pub struct Supervisor {
    phase: Phase,
    processes: BTreeMap<String, Process>,
}

impl Supervisor {
    /// Creates an empty live-session supervisor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a process, initially owned by its tool call.
    pub fn started(&mut self, id: String) {
        self.processes.insert(
            id,
            Process {
                job: None,
                exit: None,
                expired: false,
            },
        );
    }

    /// Transfers a known call to a durable job.
    pub fn adopt(&mut self, id: &str, job_id: JobId) -> bool {
        let Some(process) = self.processes.get_mut(id) else {
            return false;
        };
        process.job = Some(job_id);
        true
    }

    /// Stops all live processes and forgets unconfirmed completions.
    pub fn shutdown_requested(&mut self) -> Vec<Step> {
        self.phase = Phase::Shutdown;
        let stops = self
            .processes
            .iter()
            .filter(|(_, process)| process.exit.is_none())
            .map(|(id, _)| Step::Stop { id: id.clone() })
            .collect();
        self.processes.clear();
        stops
    }

    /// Chooses crash takeover only when a job or unconfirmed job ending exists.
    pub fn lifeline_eof(&mut self) -> Vec<Step> {
        if matches!(self.phase, Phase::Shutdown | Phase::Gone) {
            self.phase = Phase::Gone;
            return vec![Step::Exit];
        }
        let has_jobs = self.processes.values().any(|process| process.job.is_some());
        let mut steps = Vec::new();
        if has_jobs {
            self.phase = Phase::Holding;
            steps.push(Step::HoldLog);
        } else {
            self.phase = Phase::Gone;
        }
        steps.extend(self.tool_stops());
        let ended: Vec<_> = self
            .processes
            .iter()
            .filter_map(|(id, process)| {
                Some((id.clone(), process.job.clone()?, process.exit.clone()?))
            })
            .collect();
        for (id, job_id, exit) in ended {
            self.processes.remove(&id);
            steps.push(Step::WriteJobCompleted {
                id,
                job_id,
                exit,
                unless_logged: true,
            });
        }
        if !self.processes.values().any(|process| process.job.is_some()) {
            self.phase = Phase::Gone;
            steps.push(Step::Exit);
        }
        steps
    }

    /// Marks held jobs expired and stops every live process, shutdown-style.
    /// Valid only while holding after a crash; any other phase returns empty
    /// and marks nothing. Endings are written later, when each process reaps.
    pub fn bound_elapsed(&mut self) -> Vec<Step> {
        if !matches!(self.phase, Phase::Holding) {
            return Vec::new();
        }
        let mut stops = Vec::new();
        for (id, process) in self.processes.iter_mut() {
            if process.job.is_some() {
                process.expired = true;
            }
            if process.exit.is_none() {
                stops.push(Step::Stop { id: id.clone() });
            }
        }
        stops
    }

    /// Hands the held jobs to a resuming session, returning each call with
    /// its durable job. Valid only while holding and before the bound has
    /// marked a job expired: a job the bound is already stopping stays with
    /// the keeper that is writing its `keeper_timeout`, so any other phase
    /// returns `None` and changes nothing. After this, an ending is
    /// recorded for the session to deliver, and the keeper exits when the
    /// last of them is confirmed.
    pub fn hand_over(&mut self) -> Option<Vec<(String, JobId)>> {
        if !matches!(self.phase, Phase::Holding)
            || self.processes.values().any(|process| process.expired)
        {
            return None;
        }
        self.phase = Phase::HandedOver;
        Some(
            self.processes
                .iter()
                .filter_map(|(id, process)| Some((id.clone(), process.job.clone()?)))
                .collect(),
        )
    }

    /// Abandons takeover when another writer already holds the log.
    pub fn lock_lost(&mut self) -> Vec<Step> {
        let mut steps = self.tool_stops();
        self.processes.clear();
        self.phase = Phase::Gone;
        steps.push(Step::Exit);
        steps
    }

    /// Records a reaped ending; live-session job endings await confirmation.
    /// A live call can reap before its Adopt line arrives, so the exit is
    /// kept even with no job bound yet: the later adopt still binds, and the
    /// crash takeover still writes the one ending.
    pub fn exited(&mut self, id: &str, exit: Exit) -> Vec<Step> {
        let Some(mut process) = self.processes.remove(id) else {
            return Vec::new();
        };
        match self.phase {
            Phase::Live => {
                process.exit = Some(exit);
                self.processes.insert(id.to_owned(), process);
                Vec::new()
            }
            Phase::Holding => {
                let Some(job_id) = process.job.clone() else {
                    return Vec::new();
                };
                let write = if process.expired {
                    Step::WriteKeeperTimeout { job_id, exit }
                } else {
                    Step::WriteJobCompleted {
                        id: id.to_owned(),
                        job_id,
                        exit,
                        unless_logged: false,
                    }
                };
                let mut steps = vec![write];
                if !self.processes.values().any(|process| process.job.is_some()) {
                    self.phase = Phase::Gone;
                    steps.push(Step::Exit);
                }
                steps
            }
            Phase::HandedOver => {
                // The session delivers this ending and writes it; the keeper
                // keeps it until the session confirms the write.
                if process.job.is_some() {
                    process.exit = Some(exit);
                    self.processes.insert(id.to_owned(), process);
                }
                Vec::new()
            }
            Phase::Shutdown | Phase::Gone => Vec::new(),
        }
    }

    /// Forgets an ending already appended by the live session. Once handed
    /// over, the last confirmed ending lets the keeper exit.
    pub fn confirmed(&mut self, id: &str) -> Vec<Step> {
        let ended = self
            .processes
            .get(id)
            .is_some_and(|process| process.exit.is_some());
        if ended {
            self.processes.remove(id);
        }
        if ended
            && matches!(self.phase, Phase::HandedOver)
            && !self.processes.values().any(|process| process.job.is_some())
        {
            self.phase = Phase::Gone;
            return vec![Step::Exit];
        }
        Vec::new()
    }

    fn tool_stops(&self) -> Vec<Step> {
        self.processes
            .iter()
            .filter(|(_, process)| process.job.is_none() && process.exit.is_none())
            .map(|(id, _)| Step::Stop { id: id.clone() })
            .collect()
    }
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
