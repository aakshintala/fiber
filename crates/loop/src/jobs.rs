//! A finished background job wakes the loop (`docs/tools.md`, "Background
//! jobs"): its notice starts a turn while the loop is idle, joins the
//! running turn at the next step boundary, and is written as a
//! `job_completed` with no action. A session about to end with jobs running
//! wakes the model once with the ending notice, then waits for every job.
//! A resume marks each job a crash left running `orphaned`
//! (`docs/events.md`, "Resume").

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use contract::events::{
    Event, InputItem, JobCompleted, JobLine, JobsPendingNotified, Outcome, PendingReason,
};
use contract::inbox::{JobNotice, Message};
use contract::jobs::Jobs;
use contract::provider::Input;
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode, JobId, TurnId};

use crate::prompt::{body, fill};
use crate::{Error, Loop};

/// What an orphaned job's `job_completed` says.
const ORPHANED: &str = "The process that ran this job died; it may still be running.";

/// Something taken from the inbox and not yet written, in arrival order:
/// a steering message, or a job's end whose claim held.
#[derive(Debug)]
pub(crate) enum Queued {
    /// Written as `steering_applied`, or as a `message` item when it starts
    /// a turn. Fiber's own ending notice only ever starts a turn.
    Steer(Message),
    /// Written as `job_completed` with no action; named in a `jobs` item
    /// when it starts a turn.
    Job(JobCompleted),
    /// A monitor's batch: written as `job_line`; named in a `jobs` item when
    /// it starts a turn. No claim applies to it.
    Line(JobLine),
    /// A person's `handoff`: named in a `handoff` item when it starts a
    /// turn, and run at the next step boundary (`docs/handoff.md`, "A
    /// person"); no line is written for it.
    Handoff(CommandId, Option<String>),
    /// The jobs the ending notice or the jobs check named, and which of the
    /// two it was: written as `jobs_pending_notified`, the first line of its
    /// turn's first step.
    Pending(Vec<JobId>, PendingReason),
}

/// The session's jobs as the loop ends (`docs/tools.md`, "Background
/// jobs"; `docs/invocation.md`, "Lifecycle").
#[derive(Default)]
pub(crate) struct Ending {
    /// The session's jobs. `None`: no job ever runs, and the loop ends as
    /// it would without jobs.
    jobs: Option<Arc<dyn Jobs>>,
    /// The ending notice was given. It is given once per process.
    notified: bool,
    /// A turn started after `close` was taken: it, and every later turn,
    /// takes no steer.
    pub(crate) after_close: bool,
    /// An idle wait saw a job running and has not since seen none.
    ran: bool,
    /// The idle deadline counted from when a wait last saw the jobs end.
    quiet: Option<Instant>,
    /// When the last prompt or steer was taken, or the first idle wait
    /// began: the unattended delay counts from here.
    attended: Option<Instant>,
    /// The jobs check was given since the last prompt or steer, so it is
    /// not armed (`docs/invocation.md`, "Lifecycle").
    checked: bool,
}

/// The notice's completion, when its claim holds: no `jobs wait` or `stop`
/// already returned the job's final state to the model.
pub(crate) fn claimed(notice: JobNotice) -> Option<JobCompleted> {
    (notice.claim.0)().then_some(notice.completed)
}

impl Loop {
    /// The session's jobs: their ends wake the model, and the session does
    /// not end while one runs (`docs/tools.md`, "Background jobs"). Without
    /// them no job ever runs.
    pub fn jobs(mut self, jobs: Arc<dyn Jobs>) -> Self {
        self.ending.jobs = Some(jobs);
        self
    }

    /// Whether the loop was given the session's jobs.
    pub(crate) fn has_jobs(&self) -> bool {
        self.ending.jobs.is_some()
    }

    /// The jobs still running, in start order.
    pub(crate) fn running(&self) -> Vec<JobId> {
        self.ending
            .jobs
            .as_ref()
            .map(|jobs| jobs.running())
            .unwrap_or_default()
    }

    /// The ending notice for `running`, once per process: true when this
    /// call gave it. Its text is a `message` from Fiber that starts the
    /// next turn, and `jobs_pending_notified` is queued for that turn's
    /// first step (`docs/events.md`, `jobs_pending_notified`).
    pub(crate) fn notify_pending(&mut self, running: Vec<JobId>, input: &mut Vec<Queued>) -> bool {
        if std::mem::replace(&mut self.ending.notified, true) {
            return false;
        }
        self.notice(running, PendingReason::Ending, input);
        true
    }

    /// A prompt or a steer was taken: the session is attended from now,
    /// and the jobs check is armed again.
    pub(crate) fn attended(&mut self) {
        self.ending.attended = Some(self.log.clock().now());
        self.ending.checked = false;
    }

    /// When an idle wait starts the unattended clock: from now, unless a
    /// prompt or an earlier wait already started it.
    pub(crate) fn start_unattended(&mut self) {
        if self.ending.attended.is_none() {
            self.ending.attended = Some(self.log.clock().now());
        }
    }

    /// When the jobs check comes due: the idle delay after the last prompt,
    /// while it is armed and jobs run. `None` with no idle delay, no jobs
    /// running, or the check already given since the last prompt.
    pub(crate) fn check_deadline(&self) -> Option<Instant> {
        let after = self.idle_exit?;
        if self.ending.checked || self.running().is_empty() {
            return None;
        }
        self.ending.attended?.checked_add(after)
    }

    /// The jobs check, once until the next prompt: a `message` from Fiber
    /// listing the running jobs starts the next turn, and
    /// `jobs_pending_notified` with `reason` `unattended` is queued for its
    /// first step. The session does not end.
    pub(crate) fn check_jobs(&mut self, input: &mut Vec<Queued>) {
        self.ending.checked = true;
        let running = self.running();
        self.notice(running, PendingReason::Unattended, input);
    }

    /// One notice about `running`: its text, as a `message` from Fiber, is
    /// the next turn's input, and `jobs_pending_notified` is queued for that
    /// turn's first step (`docs/events.md`, `jobs_pending_notified`).
    fn notice(&mut self, running: Vec<JobId>, reason: PendingReason, input: &mut Vec<Queued>) {
        input.push(Queued::Steer(Message {
            content: vec![ContentPart::Text {
                text: pending_text(&running, reason),
            }],
            sender: Sender {
                origin: Origin::Fiber,
                command_id: None,
            },
        }));
        self.queued.push_back(Queued::Pending(running, reason));
    }

    /// The deadline an idle wait keeps, from the `deadline` it began with:
    /// none while a job runs, since idle means no jobs running, and never
    /// before the idle delay has passed since a wait saw the last job end
    /// (`docs/invocation.md`, "Lifecycle"). `running` is read here, before
    /// the caller's last look at the inbox: a job's end is sent there
    /// before `running` stops listing it.
    pub(crate) fn idle_until(&mut self, deadline: Option<Instant>) -> Option<Instant> {
        let deadline = deadline?;
        if !self.running().is_empty() {
            self.ending.ran = true;
            return None;
        }
        if std::mem::take(&mut self.ending.ran) {
            self.ending.quiet = self.idle_deadline();
        }
        Some(
            self.ending
                .quiet
                .map_or(deadline, |quiet| quiet.max(deadline)),
        )
    }

    /// A notice taken while a turn runs or an approval waits: queued for
    /// the next step boundary, after anything already queued.
    pub(crate) fn admit_job(&mut self, notice: JobNotice) {
        if let Some(completed) = claimed(notice) {
            self.queued.push_back(Queued::Job(completed));
        }
    }

    /// `turn_started`'s input, from what the idle wait collected, in
    /// arrival order: each message as a `message` item, each run of
    /// consecutive job notices and monitor batches as one `jobs` item that
    /// names each job once. They are queued, so their `job_completed` and
    /// `job_line` lines are written at the first step boundary
    /// (`docs/events.md`, `turn_started`).
    pub(crate) fn turn_input(&mut self, pieces: Vec<Queued>) -> Vec<InputItem> {
        let mut input: Vec<InputItem> = Vec::new();
        for piece in pieces {
            match piece {
                Queued::Steer(message) => input.push(InputItem::Message {
                    content: message.content,
                    sender: message.sender,
                    changed_by: None,
                }),
                Queued::Handoff(command_id, instructions) => {
                    input.push(InputItem::Handoff {
                        command_id: command_id.clone(),
                    });
                    self.queued
                        .push_back(Queued::Handoff(command_id, instructions));
                }
                Queued::Job(completed) => {
                    name_job(&mut input, &completed.job_id);
                    self.queued.push_back(Queued::Job(completed));
                }
                Queued::Line(line) => {
                    name_job(&mut input, &line.job_id);
                    self.queued.push_back(Queued::Line(line));
                }
                // A `jobs_pending_notified` a cancelled turn kept: its
                // message is already logged, so it is no input item, and is
                // written at the first step boundary.
                Queued::Pending(ids, reason) => self.queued.push_back(Queued::Pending(ids, reason)),
            }
        }
        input
    }

    /// Writes each `jobs_pending_notified` at the front of the queue: the
    /// ending notice or the jobs check queues it before anything else, and
    /// it is the first line after its turn's first `step_started`
    /// (`docs/events.md`, `jobs_pending_notified`).
    pub(crate) fn write_pending(&mut self, turn: &TurnId) -> Result<(), Error> {
        while let Some(Queued::Pending(..)) = self.queued.front() {
            if let Some(Queued::Pending(job_ids, reason)) = self.queued.pop_front() {
                let event = Event::JobsPendingNotified(JobsPendingNotified { job_ids, reason });
                self.append(&event, turn, None)?;
            }
        }
        Ok(())
    }

    /// Writes everything queued, in arrival order: a steer as
    /// `steering_applied`, a job's end as `job_completed` with no action.
    /// Returns whether a steer was written.
    pub(crate) fn write_queued(&mut self, turn: &TurnId) -> Result<bool, Error> {
        let mut steered = false;
        while let Some(piece) = self.queued.pop_front() {
            let event = match piece {
                Queued::Steer(message) => {
                    steered = true;
                    Event::SteeringApplied(contract::events::SteeringApplied {
                        content: message.content,
                        sender: message.sender,
                        changed_by: None,
                    })
                }
                Queued::Job(completed) => Event::JobCompleted(completed),
                Queued::Line(line) => Event::JobLine(line),
                // Held for the step's handoff check, which runs it.
                Queued::Handoff(_, instructions) => {
                    self.handoff.held.push(instructions);
                    continue;
                }
                Queued::Pending(job_ids, reason) => {
                    Event::JobsPendingNotified(JobsPendingNotified { job_ids, reason })
                }
            };
            self.append(&event, turn, None)?;
        }
        Ok(steered)
    }

    /// Writes one `job_completed` per job `lines` started and never ended,
    /// in start order, unless a `rewound` handed the job on: the process
    /// that ran it died with the one that wrote the log. Nothing touches a
    /// process (`docs/events.md`, "Resume"). A suspended turn's batch is
    /// still open, and a message there would separate its calls from their
    /// results, so those notices join the conversation once
    /// [`Loop::finish_suspended`] has written the batch's results, where
    /// `rebuild` renders them too.
    pub(crate) fn mark_orphans(&mut self, lines: &[Envelope]) -> Result<(), Error> {
        for completed in orphans(lines)? {
            if self.suspended.is_some() {
                let event = Event::JobCompleted(completed.clone());
                self.log.append(&event, None, None)?;
                self.handoff.carry.fold_jobs(&event);
                self.held.push(Input::User {
                    text: notice_text(&completed),
                });
                continue;
            }
            crate::util::write(
                &self.log,
                &mut self.conversation,
                &mut self.reviewed,
                &self.model.reference,
                &Event::JobCompleted(completed),
                None,
                None,
                &mut self.changes.had,
                &mut self.handoff.carry,
            )?;
        }
        Ok(())
    }
}

/// Names `id` in the `jobs` item `input` ends with, once, or starts one.
fn name_job(input: &mut Vec<InputItem>, id: &JobId) {
    if let Some(InputItem::Jobs { job_ids }) = input.last_mut() {
        if !job_ids.contains(id) {
            job_ids.push(id.clone());
        }
    } else {
        input.push(InputItem::Jobs {
            job_ids: vec![id.clone()],
        });
    }
}

/// The jobs `lines` started with no `job_completed` and no `rewound` naming
/// them, in start order, each as its `orphaned` completion.
fn orphans(lines: &[Envelope]) -> Result<Vec<JobCompleted>, Error> {
    let mut started: Vec<JobId> = Vec::new();
    let mut settled: HashSet<JobId> = HashSet::new();
    for line in lines.iter().filter(|line| line.is_durable()) {
        let event = Event::from_envelope(line).map_err(Error::Unreadable)?;
        if let Some(Event::JobStarted(job)) = event {
            started.push(job.job_id);
        } else if let Some(Event::JobCompleted(job)) = event {
            settled.insert(job.job_id);
        } else if let Some(Event::Rewound(rewound)) = event {
            settled.extend(rewound.jobs);
        }
    }
    Ok(started
        .into_iter()
        .filter(|id| settled.insert(id.clone()))
        .map(|job_id| JobCompleted {
            job_id,
            status: Outcome::Failed,
            error: Some(Failure {
                code: ErrorCode::Orphaned,
                message: ORPHANED.to_owned(),
                retry_after: None,
                provider: None,
            }),
            process: None,
            output_tail: None,
        })
        .collect())
}

/// What a notice about running jobs tells the model, naming `job_ids`: the
/// ending notice, or the jobs check. It is logged as its turn's `message`
/// item, which a resume renders as it does any message
/// (`docs/prompt-cache.md`).
pub(crate) fn pending_text(job_ids: &[JobId], reason: PendingReason) -> String {
    let ids: Vec<&str> = job_ids.iter().map(|id| id.0.as_str()).collect();
    fill(
        body(
            crate::conversation::MESSAGES_MD,
            match reason {
                PendingReason::Ending => "jobs-pending",
                PendingReason::Unattended => "jobs-check",
            },
        )
        .trim_end(),
        &[("job_ids", ids.join(", ").as_str())],
    )
}

/// What a `job_line` tells the model: the monitor's batch, and the count
/// the rate limit suppressed when there is one. Read from the line alone,
/// so a resume renders the same bytes.
pub(crate) fn line_text(line: &JobLine) -> String {
    let mut text = fill(
        body(crate::conversation::MESSAGES_MD, "job-line").trim_end(),
        &[
            ("job_id", line.job_id.0.as_str()),
            ("lines", line.lines.as_str()),
        ],
    );
    if let Some(suppressed) = line.suppressed {
        text.push('\n');
        text.push_str(&fill(
            body(crate::conversation::MESSAGES_MD, "job-line-suppressed").trim_end(),
            &[("suppressed", suppressed.to_string().as_str())],
        ));
    }
    text
}

/// What a `job_completed` with no action tells the model: the job, how it
/// ended, then any exit code, signal, error and last output. Read from the
/// line alone, so a resume renders the same bytes.
pub(crate) fn notice_text(completed: &JobCompleted) -> String {
    let status = match completed.status {
        Outcome::Completed => "completed",
        Outcome::Failed => "failed",
        Outcome::Cancelled => "cancelled",
    };
    let mut text = fill(
        body(crate::conversation::MESSAGES_MD, "job-completed").trim_end(),
        &[("job_id", completed.job_id.0.as_str()), ("status", status)],
    );
    let process = completed.process.as_ref();
    if let Some(code) = process.and_then(|process| process.exit_code) {
        text.push_str(&format!("\nExit code {code}."));
    }
    if let Some(signal) = process.and_then(|process| process.signal.as_deref()) {
        text.push_str(&format!("\nKilled by {signal}."));
    }
    // A shell's error repeats its exit code or signal line; it is said
    // once.
    if let Some(error) = &completed.error
        && !text.lines().any(|line| line == error.message.trim_end())
    {
        text.push('\n');
        text.push_str(error.message.trim_end());
    }
    if let Some(tail) = &completed.output_tail {
        text.push_str("\nLast output:\n");
        text.push_str(tail.trim_end());
    }
    text
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
