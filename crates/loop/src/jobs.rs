//! A finished background job wakes the loop (`docs/tools.md`, "Background
//! jobs"): its notice starts a turn while the loop is idle, joins the
//! running turn at the next step boundary, and is written as a
//! `job_completed` with no action. A resume marks each job a crash left
//! running `orphaned` (`docs/events.md`, "Resume").

use std::collections::HashSet;

use contract::events::{Event, InputItem, JobCompleted, Outcome};
use contract::inbox::{JobNotice, Message};
use contract::provider::Input;
use contract::shapes::Failure;
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
    /// a turn.
    Steer(Message),
    /// Written as `job_completed` with no action; named in a `jobs` item
    /// when it starts a turn.
    Job(JobCompleted),
    /// A person's `handoff`: named in a `handoff` item when it starts a
    /// turn, and run at the next step boundary (`docs/handoff.md`, "A
    /// person"); no line is written for it.
    Handoff(CommandId, Option<String>),
}

/// The notice's completion, when its claim holds: no `jobs wait` or `stop`
/// already returned the job's final state to the model.
pub(crate) fn claimed(notice: JobNotice) -> Option<JobCompleted> {
    (notice.claim.0)().then_some(notice.completed)
}

impl Loop {
    /// A notice taken while a turn runs or an approval waits: queued for
    /// the next step boundary, after anything already queued.
    pub(crate) fn admit_job(&mut self, notice: JobNotice) {
        if let Some(completed) = claimed(notice) {
            self.queued.push_back(Queued::Job(completed));
        }
    }

    /// `turn_started`'s input, from what the idle wait collected, in
    /// arrival order: each message as a `message` item, each run of
    /// consecutive job notices as one `jobs` item. The notices are queued,
    /// so their `job_completed` lines are written at the first step
    /// boundary (`docs/events.md`, `turn_started`).
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
                    let id = completed.job_id.clone();
                    self.queued.push_back(Queued::Job(completed));
                    if let Some(InputItem::Jobs { job_ids }) = input.last_mut() {
                        job_ids.push(id);
                    } else {
                        input.push(InputItem::Jobs { job_ids: vec![id] });
                    }
                }
            }
        }
        input
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
                // Held for the step's handoff check, which runs it.
                Queued::Handoff(_, instructions) => {
                    self.handoff.held.push(instructions);
                    continue;
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
