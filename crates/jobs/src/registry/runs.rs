//! One run at a time: a delegate's runs share its registry entry, and each
//! run's news settles in event order (`docs/delegates.md`, "Identity and
//! resume"). A run's number is never reused; a resume commits the next one
//! under a registry-wide gate, so two messages to one finished delegate
//! start one run.

use std::sync::{Arc, Weak};
use std::time::Instant;

use contract::events::Outcome;
use contract::events::{DelegateFinished, DelegateStarted, JobCompleted};
use contract::inbox::Claim;
use contract::jobs::{JobRecord, Stop, Taken};
use contract::shapes::Failure;
use contract::tool::{Cancel, Settle};
use contract::{ActionId, JobId, SessionId};

use super::park::Parked;
use super::{Registry, final_text, lock};
use crate::delegate::outcome::empty;
use crate::error::JobsError;

/// What `id` names in this session's registry.
#[derive(Debug)]
pub(crate) enum Target {
    /// No job has this id.
    Unknown,
    /// A job that is not a delegate.
    NotDelegate,
    /// A running delegate: its session and its run's number.
    Live(SessionId, u64),
    /// An ended delegate, ready to resume.
    Ended,
}

/// What a `wait` or a `stop` that reached a run returns to the model: its
/// final text at once, and its `Settle` for the loop to write.
pub(crate) struct Answer {
    /// The lines the model sees.
    pub(crate) text: String,
    /// Takes run k's news through its end mark, the first time any taker
    /// takes it; `None` while the run still runs.
    pub(crate) settle: Option<Settle>,
}

/// Where a queued record sits: its run, and whether it ends that run. A
/// run's start is appended at its resume's commit, its news at its end,
/// and run j+1 commits only after run j ended, so marks never decrease
/// along the queue, and `(j-1, end) < (j, start) < (j, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Mark {
    run: u64,
    end: bool,
}

impl Mark {
    /// A run's start records sort here.
    pub(crate) fn start(run: u64) -> Self {
        Self { run, end: false }
    }

    /// A run's news sorts here.
    pub(crate) fn end(run: u64) -> Self {
        Self { run, end: true }
    }
}

/// One unwritten record, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pending {
    at: Mark,
    record: JobRecord,
    action: Option<ActionId>,
}

/// A superseded run's end, kept for a later `wait` or `stop` that asked
/// about it: one small entry per resume, never removed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunEnd {
    /// The superseded run's number.
    pub(crate) run: u64,
    /// How it ended.
    pub(crate) completed: JobCompleted,
    /// Its delegate's finish, when it had one.
    pub(crate) finished: Option<DelegateFinished>,
}

/// What starting run `next` needs: the first run's record, copied.
pub(crate) struct Resumable {
    /// The first run's `DelegateStarted`, copied to the new run's.
    pub(crate) delegate: DelegateStarted,
    /// The job's description, kept for the new run's `Started`.
    pub(crate) description: String,
    /// The job's output path, kept for the new run's `Started`.
    pub(crate) output_path: String,
}

/// A committed resume: the new run's end reporter, its value, and the
/// `Settle` that writes everything the loop has not yet written through
/// the new run's start.
pub(crate) struct Resumed<T> {
    /// Takes through the new run's start mark: the ended run's unwritten
    /// news, then the new run's start.
    pub(crate) settle: Settle,
    /// Reports the new run's end, once.
    pub(crate) unreported: super::Unreported,
    /// What `start` returned.
    pub(crate) value: T,
}

pub(crate) const DELEGATE_CAP: usize = 10;

fn running_delegates(inner: &super::Inner) -> usize {
    inner
        .jobs
        .iter()
        .filter(|job| job.delegate.is_some() && matches!(job.phase, super::Phase::Running))
        .count()
}

#[derive(Debug)]
pub(crate) enum Admit<E> {
    Closed,
    Full,
    Start(E),
}

/// Why `resume` started nothing.
pub(crate) enum Resume<E> {
    /// This session already runs the maximum delegates.
    Full,
    /// No job has this id.
    Unknown,
    /// A job that is not a delegate.
    NotDelegate,
    /// The job is running: its session and its run's number.
    Live(SessionId, u64),
    /// The run count is exhausted: no start ran, nothing changed.
    Exhausted,
    /// `start` failed: nothing changed, nothing queued.
    Start(E),
}

/// How `await_end` ended.
pub(crate) enum Waited {
    /// Run `run` no longer runs.
    Ended,
    /// `until` passed first.
    Timeout,
    /// `cancel` fired first.
    Cancelled,
}

impl Registry {
    pub(crate) fn admit<T, E>(
        &self,
        extension: bool,
        start: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, Admit<E>> {
        let _gate = lock(&self.start_gate);
        // The closed-starts check runs before the delegate cap: a start
        // after a sweep begins gets `closing` even with 10 extension
        // delegates still listed, never `DelegatesFull`.
        if extension && lock(&self.inner).extension_starts_closed {
            return Err(Admit::Closed);
        }
        if running_delegates(&lock(&self.inner)) >= DELEGATE_CAP {
            return Err(Admit::Full);
        }
        start().map_err(Admit::Start)
    }

    /// What `id` names: a running delegate pins its run's number, so a
    /// commit for that run refuses once the registry shows another.
    pub(crate) fn target(&self, id: &str) -> Target {
        let inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter().find(|job| job.started.job_id.0 == id) else {
            return Target::Unknown;
        };
        if job.started.extension.is_some() {
            return Target::Unknown;
        }
        let Some(delegate) = &job.delegate else {
            return Target::NotDelegate;
        };
        match &job.phase {
            super::Phase::Running => Target::Live(delegate.delegate_session_id.clone(), job.run),
            super::Phase::Ended(_) => Target::Ended,
        }
    }

    /// True only while `run` is the entry's run and running.
    pub(crate) fn running(&self, id: &str, run: u64) -> bool {
        let inner = lock(&self.inner);
        is_running(&inner, id, run)
    }

    /// The entry's current run, when `id` names a job.
    pub(crate) fn run_of(&self, id: &str) -> Option<u64> {
        lock(&self.inner)
            .jobs
            .iter()
            .find(|job| job.started.job_id.0 == id)
            .map(|job| job.run)
    }

    /// Starts run `job.run + 1` of the ended delegate `id`, reusing its
    /// entry. The gate is held from the "ended" check through the commit,
    /// so two resumes of one job start one run; `start` runs with the
    /// inner lock released. On `Ok` the ended run's end moves to `ends`,
    /// the new run's start is appended after its news, and the new run is
    /// recorded running. On `Err` nothing changes and nothing is queued.
    pub(crate) fn resume<T, E>(
        self: &Arc<Self>,
        id: &str,
        action: Option<ActionId>,
        start: impl FnOnce(&Resumable) -> Result<(Stop, T), E>,
    ) -> Result<Resumed<T>, Resume<E>> {
        let _gate = lock(&self.start_gate);
        let (job_id, next, resumable) = {
            let inner = lock(&self.inner);
            let Some(job) = inner.jobs.iter().find(|job| job.started.job_id.0 == id) else {
                return Err(Resume::Unknown);
            };
            if job.started.extension.is_some() {
                return Err(Resume::Unknown);
            }
            let Some(delegate) = &job.delegate else {
                return Err(Resume::NotDelegate);
            };
            match &job.phase {
                super::Phase::Running => {
                    return Err(Resume::Live(delegate.delegate_session_id.clone(), job.run));
                }
                super::Phase::Ended(_) => {}
            }
            if running_delegates(&inner) >= DELEGATE_CAP {
                return Err(Resume::Full);
            }
            let Some(next) = job.run.checked_add(1) else {
                return Err(Resume::Exhausted);
            };
            (
                job.started.job_id.clone(),
                next,
                Resumable {
                    delegate: delegate.clone(),
                    description: job.started.description.clone(),
                    output_path: job.path.to_string_lossy().into_owned(),
                },
            )
        };
        let run = next - 1;
        let (stop, value) = match start(&resumable) {
            Ok(started) => started,
            Err(error) => return Err(Resume::Start(error)),
        };
        // The new run is recorded with what the resume was started with.
        let description = resumable.description.clone();
        let latched = {
            let mut inner = lock(&self.inner);
            let stop_all = inner.stop_all_delegates;
            let Some(job) = inner
                .jobs
                .iter_mut()
                .find(|job| job.started.job_id == job_id)
            else {
                return Err(Resume::Unknown);
            };
            // The gate held from the check, and only a resume commits a
            // new run while only an end settles a running one, so the
            // entry this check saw ended is still this run, ended. Both
            // checks fail closed: anything unexpected answers live and
            // changes nothing.
            if job.run != run {
                let session = job
                    .delegate
                    .as_ref()
                    .map(|delegate| delegate.delegate_session_id.clone())
                    .unwrap_or_else(|| SessionId("s_gone".into()));
                return Err(Resume::Live(session, job.run));
            }
            let completed = match &job.phase {
                super::Phase::Ended(completed) => completed.clone(),
                super::Phase::Running => {
                    let session = job
                        .delegate
                        .as_ref()
                        .map(|delegate| delegate.delegate_session_id.clone())
                        .unwrap_or_else(|| SessionId("s_gone".into()));
                    return Err(Resume::Live(session, job.run));
                }
            };
            job.phase = super::Phase::Running;
            job.ends.push(RunEnd {
                run,
                completed: (*completed).clone(),
                finished: job.finished.clone(),
            });
            let started = contract::events::JobStarted {
                job_id: job_id.clone(),
                tool: Some("delegate_message".into()),
                extension: None,
                description,
                output_path: job.path.to_string_lossy().into_owned(),
            };
            let delegate_started = resumable.delegate.clone();
            job.queue.push_back(Pending {
                at: Mark::start(next),
                record: JobRecord::Started(started),
                action: action.clone(),
            });
            job.queue.push_back(Pending {
                at: Mark::start(next),
                record: JobRecord::DelegateStarted(delegate_started),
                action,
            });
            job.run = next;
            job.stop = Arc::from(stop.0);
            job.stop_sent = false;
            job.finished = None;
            // As in `open_started`: a latched stop-all stops the new
            // run at once, with the lock released.
            super::Inner::latched_stop(stop_all, job)
        };
        self.park.bump();
        if let Some(stop) = latched {
            stop();
        }
        Ok(Resumed {
            settle: self.settle_through(&job_id.0, Mark::start(next)),
            unreported: super::Unreported {
                job_id,
                run: next,
                registry: Some(Arc::clone(self)),
                delegate: true,
            },
            value,
        })
    }

    /// Removes and returns the longest prefix of `id`'s queue at or below
    /// `mark`, in queue order.
    pub(crate) fn take_through(&self, id: &str, mark: Mark) -> Vec<Taken> {
        let mut inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter_mut().find(|job| job.started.job_id.0 == id) else {
            return Vec::new();
        };
        let split = job
            .queue
            .iter()
            .position(|pending| pending.at > mark)
            .unwrap_or(job.queue.len());
        job.queue
            .drain(..split)
            .map(|pending| Taken {
                record: pending.record,
                action: pending.action,
            })
            .collect()
    }

    /// Takes through run `run`'s end mark for the notice of that run:
    /// `Some` of what it took ahead of the notice's own `DelegateFinished`
    /// and `Completed`, `None` taking nothing once that `Completed`
    /// already left the queue.
    pub(crate) fn claim_through(&self, id: &str, run: u64) -> Option<Vec<Taken>> {
        let mut inner = lock(&self.inner);
        let job = inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id.0 == id)?;
        let mark = Mark::end(run);
        let split = job
            .queue
            .iter()
            .position(|pending| pending.at > mark)
            .unwrap_or(job.queue.len());
        // The claim holds only for its own run's `Completed`: an entry
        // with an earlier mark is another run's news, already taken or
        // never this run's.
        let holds = match job.queue.get(split.wrapping_sub(1)) {
            Some(pending) => {
                pending.at == mark && matches!(pending.record, JobRecord::Completed(_))
            }
            None => false,
        };
        if !holds {
            return None;
        }
        let mut taken: Vec<Pending> = job.queue.drain(..split).collect();
        taken.pop();
        if matches!(
            taken.last().map(|pending| &pending.record),
            Some(JobRecord::DelegateFinished(_))
        ) {
            taken.pop();
        }
        Some(
            taken
                .into_iter()
                .map(|pending| Taken {
                    record: pending.record,
                    action: pending.action,
                })
                .collect(),
        )
    }

    /// A `Settle` taking `id`'s queue through `mark` over a weak registry:
    /// nothing was started when the registry is gone.
    pub(crate) fn settle_through(&self, id: &str, mark: Mark) -> Settle {
        let registry = Weak::clone(&self.me);
        let id = id.to_owned();
        Settle(Arc::new(move || {
            registry
                .upgrade()
                .map(|registry| registry.take_through(&id, mark))
                .unwrap_or_default()
        }))
    }

    /// Run `run`'s final text and a `Settle` naming it: from its phase
    /// while current and ended, from `ends` once a resume superseded it.
    /// `None` while it runs or when no run `run` ever ran.
    pub(crate) fn answer_for(&self, id: &str, run: u64) -> Option<Answer> {
        let inner = lock(&self.inner);
        let job = inner.jobs.iter().find(|job| job.started.job_id.0 == id)?;
        let (completed, finished) = if job.run == run {
            match &job.phase {
                super::Phase::Running => return None,
                super::Phase::Ended(completed) => (&**completed, job.finished.as_ref()),
            }
        } else {
            let ended = job.ends.iter().find(|ended| ended.run == run)?;
            (&ended.completed, ended.finished.as_ref())
        };
        let finished = if job.started.extension.is_some() {
            None
        } else {
            finished
        };
        let text = final_text(&job.path, completed, finished);
        Some(Answer {
            text,
            settle: Some(self.settle_through(id, Mark::end(run))),
        })
    }

    /// Parks until `running(id, run)` is false, `until` passes, or
    /// `cancel` fires.
    pub(crate) fn await_end(
        self: &Arc<Self>,
        id: &str,
        run: u64,
        until: Instant,
        cancel: &dyn Cancel,
    ) -> Waited {
        match self.park_until(id, run, Some(until), cancel) {
            Parked::Ended => Waited::Ended,
            Parked::Timeout => Waited::Timeout,
            Parked::Cancelled => Waited::Cancelled,
        }
    }

    /// Records run `run`'s end: its news joins the queue, and its notice
    /// claims through its end mark. Acts only on the entry whose run
    /// matches and is running: a late report of a superseded run records
    /// nothing on the new one.
    pub(crate) fn finish_run(
        &self,
        run: u64,
        completed: JobCompleted,
        delegate: Option<DelegateFinished>,
    ) {
        let mut guard = lock(&self.inner);
        let inner = &mut *guard;
        if let Some(job) = inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id.0 == completed.job_id.0)
            && job.run == run
            && matches!(job.phase, super::Phase::Running)
        {
            job.phase = super::Phase::Ended(Box::new(completed.clone()));
            // The terminal closes with the job.
            job.input = None;
            // A delegate's finish is kept past the end, for a later
            // `wait` and its text.
            job.finished = delegate.clone();
            if let Some(finished) = delegate.clone() {
                job.queue.push_back(Pending {
                    at: Mark::end(run),
                    record: JobRecord::DelegateFinished(finished),
                    action: None,
                });
            }
            job.queue.push_back(Pending {
                at: Mark::end(run),
                record: JobRecord::Completed(completed.clone()),
                action: None,
            });
            if let Some(inbox) = &inner.inbox {
                let registry = Weak::clone(&self.me);
                let id = completed.job_id.0.clone();
                let claim = Claim(Box::new(move || {
                    registry
                        .upgrade()
                        .and_then(|registry| registry.claim_through(&id, run))
                }));
                // A loop that is gone takes no news; the end stays
                // recorded for `list`.
                let _sent =
                    inbox.send(contract::inbox::Delivery::Job(contract::inbox::JobNotice {
                        completed,
                        claim,
                        delegate,
                    }));
            }
        }
        self.park.bump();
    }

    /// Reports an outcome that is not a job's end: a monitor's failure to
    /// record, or a shell job's drop, as today. A delegate's drop carries
    /// an empty finish.
    pub(crate) fn fail_job(&self, job_id: JobId, run: u64, delegate: bool) {
        let finished = delegate.then(|| empty(&job_id));
        self.finish_run(
            run,
            JobCompleted {
                job_id,
                status: Outcome::Failed,
                error: Some(Failure::from(&JobsError::JobNoResult)),
                process: None,
                output_tail: None,

                notice: None,
                changed_by: None,
            },
            finished,
        );
    }
}

/// True only while `run` is the entry's run and running.
pub(super) fn is_running(inner: &super::Inner, id: &str, run: u64) -> bool {
    inner.jobs.iter().any(|job| {
        job.started.job_id.0 == id && job.run == run && matches!(job.phase, super::Phase::Running)
    })
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
