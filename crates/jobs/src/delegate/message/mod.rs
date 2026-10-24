//! A parent's message to its delegate: steering a running one at its next
//! step boundary, or resuming a finished one as a new run of the same job
//! (`docs/delegates.md`, "Talking to a delegate"). The receipt says which.

use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Event, Progress};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure};
use contract::tool::{Ask, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, ErrorCode, JobId, SessionId};
use serde_json::{Map, Value, json};
use support::{clock::Parker, lock};

use super::run::{Launch, Watch, mint_command_id};
use crate::error::JobsError;
use crate::registry::Registry;
use crate::registry::runs::{Target, Waited};

pub(crate) mod resume;

pub use resume::LastSeq;

/// Steers the delegate's session: the jobs-local mirror of `doors::steer`,
/// as `Watch` mirrors the watch. One connection per message, one command
/// id per call. `commit` runs once per connection, after the subscribe is
/// accepted and before any byte of the `steer` line is written; false
/// returns [`Steered::Withheld`] with nothing written. `on_queued` runs
/// once the message is queued, proved by the probe's answer.
pub type Steer = Arc<
    dyn Fn(&SessionId, &str, &str, &mut dyn FnMut() -> bool, &mut dyn FnMut()) -> Steered
        + Send
        + Sync,
>;

/// What one steer attempt came to: the jobs-local mirror of
/// `doors::Steered`.
#[derive(Debug)]
pub enum Steered {
    /// The delegate accepted the steering message.
    Accepted,
    /// The delegate refused it, with its code and message.
    Rejected(Failure),
    /// The socket refused or closed before answering.
    Gone,
    /// `commit` refused, so no byte was written.
    Withheld,
}

/// What the call's gate holds: whether the call was cancelled, and the run
/// a committed attempt went to. The lock is released before any write, so
/// a blocked write never holds it; the lock order is gate, then the
/// registry's inner lock, and nothing takes them the other way.
pub(crate) struct SteerGate {
    cancelled: bool,
    sent_to: Option<u64>,
}

/// Steers a running delegate, or resumes a finished one. The fields are
/// wired by `main`; `bound` is both the stop bound and the closing wait.
pub struct DelegateMessage {
    /// This session's depth in the delegate tree.
    pub depth: u32,
    /// The session's jobs.
    pub registry: Arc<Registry>,
    /// The parent session.
    pub parent: SessionId,
    /// The session clock: backoff, the closing wait and every park.
    pub clock: Arc<dyn Clock>,
    /// How long a stop waits, and a closing steer waits for the run.
    pub bound: Duration,
    /// The output cap, in bytes of the delegate's `events.jsonl`.
    pub cap: u64,
    /// Builds the child's command.
    pub launch: Launch,
    /// Watches the child's socket.
    pub watch: Watch,
    /// Steers the delegate's session.
    pub steer: Steer,
    /// Reads the delegate log's boundary.
    pub last_seq: LastSeq,
}

/// What one steer attempt sends the call's thread.
enum AttemptMsg {
    /// The message is queued; the call emits its one delta.
    Queued,
    /// The attempt's answer.
    Done(Steered),
}

/// What an attempt came to on the call's thread.
#[allow(
    clippy::large_enum_variant,
    reason = "one answer per call; boxing it would allocate on every steer attempt"
)]
pub(crate) enum Attempt {
    /// The call is answered.
    Done(Output),
    /// Ask `target` again: after the backoff past `Gone`, at once past
    /// `Withheld`.
    Again(Duration),
    /// Steer the live run another message started: the commit still
    /// refuses a run that ended since, so a stale pair is safe.
    Found(SessionId, u64),
}

impl Tool for DelegateMessage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_message".to_owned(),
            description: "Steers a running delegate with `message`, which \
                 joins its turn at its next step boundary, or resumes a \
                 finished one with `message` as a new run of the same job. \
                 Says which in its receipt; acts only on this session's own \
                 delegates."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The delegate's job id, from its receipt."
                    },
                    "message": {
                        "type": "string",
                        "description": "What to tell it."
                    }
                },
                "required": ["id", "message"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        super::check_pair(arguments, "id", "message")
            .map_err(|error| EffectsError::Arguments(error.to_string()))?;
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            // Always judged by the parent's reviewer: no fast path,
            // session grant or standing allow skips it
            // (`docs/delegates.md`, "Permissions").
            always_reviewed: true,
            hosts: None,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        self.body(arguments, None, cancel, emit)
    }

    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        // Only the call's own action: the turn comes from the loop, which
        // writes the queued start under it. Nothing is ever asked.
        self.body(arguments, Some(ask.action()), cancel, emit)
    }
}

impl DelegateMessage {
    /// Runs the call with `action`, the caller's own when it had an `Ask`.
    fn body(
        &self,
        arguments: &Map<String, Value>,
        action: Option<ActionId>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Output {
        if let Some(error) = super::depth_refusal(self.depth, "delegate_message") {
            return error.output(false);
        }
        let (id, message) = match super::check_pair(arguments, "id", "message") {
            Ok(pair) => pair,
            Err(error) => return error.output(true),
        };
        // Before any lookup: nothing is steered, resumed or written.
        if message.trim().is_empty() {
            return JobsError::MessageEmpty.output(true);
        }
        // One command id per call, on every connection the call opens.
        let command = mint_command_id();
        let gate = Arc::new(Mutex::new(SteerGate {
            cancelled: false,
            sent_to: None,
        }));
        let park = Arc::new(Parker::new());
        cancel.subscribe(Arc::downgrade(&park) as Weak<dyn Wake>);
        // Timeouts are read off the session clock: a clock move wakes
        // the park, and the wait re-reads its deadline.
        self.clock
            .subscribe(Arc::downgrade(&park) as Weak<dyn Wake>);
        let mut backoff = super::run::START_BACKOFF;
        let mut emitted = false;
        // The live run to steer: from `target`, or from a resume that
        // found one. A stale pair is safe: the commit refuses a run that
        // ended since.
        let mut pending: Option<(SessionId, u64)> = None;
        loop {
            let (session, run) = match pending.take() {
                Some(pair) => pair,
                None => match self.registry.target(&id) {
                    Target::Unknown => {
                        return JobsError::UnknownJob {
                            id: JobId(id.clone()),
                        }
                        .output(true);
                    }
                    Target::NotDelegate => {
                        return JobsError::NotDelegate {
                            id: JobId(id.clone()),
                        }
                        .output(true);
                    }
                    Target::Live(session, run) => (session, run),
                    Target::Ended => {
                        // The gate decision lives here: a committed send
                        // elsewhere fails without starting a resume.
                        if lock(&gate).sent_to.is_some() {
                            return unanswered(&id);
                        }
                        match self.resume_now(&id, &message, &action, cancel) {
                            Attempt::Done(output) => return output,
                            Attempt::Found(session, run) => (session, run),
                            // A resume answers or finds; never waits.
                            Attempt::Again(_) => continue,
                        }
                    }
                },
            };
            if let Some(sent) = lock(&gate).sent_to
                && sent != run
            {
                return unanswered(&id);
            }
            match self.attempt(
                &session,
                &command,
                &id,
                &message,
                run,
                backoff,
                &action,
                &gate,
                &park,
                cancel,
                emit,
                &mut emitted,
            ) {
                Attempt::Done(output) => return output,
                Attempt::Again(wait) => {
                    backoff = wait;
                }
                Attempt::Found(session, run) => {
                    pending = Some((session, run));
                }
            }
        }
    }

    /// while this thread parks on its answer, the queued mark and the
    /// cancel. The thread is never joined: it holds nothing this thread
    /// or the registry needs.
    #[allow(
        clippy::too_many_arguments,
        reason = "one attempt carries the call's whole steer context"
    )]
    fn attempt(
        &self,
        session: &SessionId,
        command: &str,
        id: &str,
        message: &str,
        run: u64,
        backoff: Duration,
        action: &Option<ActionId>,
        gate: &Arc<Mutex<SteerGate>>,
        park: &Arc<Parker>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        emitted: &mut bool,
    ) -> Attempt {
        let (tx, rx) = mpsc::channel();
        {
            let steer = Arc::clone(&self.steer);
            let registry = Arc::clone(&self.registry);
            let session = session.clone();
            let command = command.to_owned();
            let message = message.to_owned();
            let id = id.to_owned();
            let gate = Arc::clone(gate);
            let park = Arc::clone(park);
            thread::spawn(move || {
                // The commit for the run this attempt is made for: false
                // unless the registry still shows that run running and the
                // call uncancelled. Already `Some(run)` on a resend.
                let mut commit = {
                    let gate = Arc::clone(&gate);
                    move || {
                        let mut gate = lock(&gate);
                        if gate.cancelled || !registry.running(&id, run) {
                            return false;
                        }
                        gate.sent_to = Some(run);
                        true
                    }
                };
                let mut queued = || {
                    let _sent = tx.send(AttemptMsg::Queued);
                    park.bump();
                };
                let steered = steer(&session, &command, &message, &mut commit, &mut queued);
                let _sent = tx.send(AttemptMsg::Done(steered));
                park.bump();
            });
        }
        loop {
            if let Some(steered) = self.pump(&rx, id, emit, emitted) {
                return self.answer(steered, id, message, run, backoff, action, gate, cancel);
            }
            if cancel.is_cancelled() {
                return Attempt::Done(self.cancelled(id, gate));
            }
            let seen = park.generation();
            if let Some(steered) = self.pump(&rx, id, emit, emitted) {
                return self.answer(steered, id, message, run, backoff, action, gate, cancel);
            }
            if cancel.is_cancelled() {
                return Attempt::Done(self.cancelled(id, gate));
            }
            park.park(self.clock.as_ref(), None, seen);
        }
    }

    /// Drains the attempt's marks, emitting the one delta on the first
    /// queued mark, and returns the attempt's answer when it arrived. The
    /// delta goes out on the call's own thread while it is parked, never
    /// on the attempt's, so nothing emits after the call returns.
    fn pump(
        &self,
        rx: &mpsc::Receiver<AttemptMsg>,
        id: &str,
        emit: &dyn Emit,
        emitted: &mut bool,
    ) -> Option<Steered> {
        for msg in rx.try_iter() {
            match msg {
                AttemptMsg::Queued => {
                    if !*emitted {
                        *emitted = true;
                        emit.emit(&Event::ToolCallDelta(Progress {
                            text: Some(format!(
                                "Queued for delegate {id}; it joins at the delegate's next step boundary."
                            )),
                            details: None,
                        }));
                    }
                }
                AttemptMsg::Done(steered) => return Some(steered),
            }
        }
        None
    }

    /// What one attempt's answer does: a receipt, a failure, a closing
    /// wait, or another attempt.
    #[allow(
        clippy::too_many_arguments,
        reason = "an answer carries the attempt's whole context"
    )]
    fn answer(
        &self,
        steered: Steered,
        id: &str,
        message: &str,
        run: u64,
        backoff: Duration,
        action: &Option<ActionId>,
        gate: &Arc<Mutex<SteerGate>>,
        cancel: &dyn Cancel,
    ) -> Attempt {
        match steered {
            Steered::Accepted => Attempt::Done(Output {
                content: vec![ContentPart::Text {
                    text: format!(
                        "Steered delegate {id}: the message joins its turn at its next step boundary."
                    ),
                }],
                ..Output::default()
            }),
            Steered::Rejected(failure) => {
                self.rejected(id, message, run, action, gate, cancel, failure)
            }
            // Nothing is known to have reached any run: after the backoff
            // the call asks `target` again, or resumes.
            Steered::Gone => self.gone(id, run, backoff, gate, cancel),
            // No byte was written. A cancel prevents every further
            // attempt; a stale run asks `target` again at once.
            Steered::Withheld => {
                if lock(gate).cancelled || cancel.is_cancelled() {
                    Attempt::Done(self.cancelled(id, gate))
                } else {
                    Attempt::Again(backoff)
                }
            }
        }
    }

    /// A `Gone` attempt: waits past the backoff for run `run`, doubling
    /// it to the cap for the next attempt. The wait ends early when the
    /// run does; a cancel answers as cancelled.
    fn gone(
        &self,
        id: &str,
        run: u64,
        backoff: Duration,
        gate: &Arc<Mutex<SteerGate>>,
        cancel: &dyn Cancel,
    ) -> Attempt {
        let until = self
            .clock
            .now()
            .checked_add(backoff)
            .unwrap_or_else(|| self.clock.now());
        let next = backoff
            .checked_mul(2)
            .map(|doubled| doubled.min(super::run::MAX_BACKOFF))
            .unwrap_or(super::run::MAX_BACKOFF);
        match self.registry.await_end(id, run, until, cancel) {
            Waited::Ended | Waited::Timeout => Attempt::Again(next),
            Waited::Cancelled => Attempt::Done(self.cancelled(id, gate)),
        }
    }

    /// A `closing` rejection: no earlier copy reached that run, so the
    /// send mark is cleared. When the run ends the call resumes it; still
    /// running at the bound it fails `closing`.
    #[allow(
        clippy::too_many_arguments,
        reason = "a closing wait carries the resume's context"
    )]
    fn closing(
        &self,
        id: &str,
        message: &str,
        run: u64,
        action: &Option<ActionId>,
        gate: &Arc<Mutex<SteerGate>>,
        cancel: &dyn Cancel,
    ) -> Attempt {
        lock(gate).sent_to = None;
        let until = self
            .clock
            .now()
            .checked_add(self.bound)
            .unwrap_or_else(|| self.clock.now());
        match self.registry.await_end(id, run, until, cancel) {
            // The gate was just cleared above, so the resume starts with
            // no committed send; the gate decision stays here.
            Waited::Ended => self.resume_now(id, message, action, cancel),
            Waited::Timeout => Attempt::Done(
                JobsError::SteerClosing {
                    id: JobId(id.to_owned()),
                }
                .output(true),
            ),
            Waited::Cancelled => Attempt::Done(self.cancelled(id, gate)),
        }
    }

    /// What a rejection carries: `closing` waits for the run and resumes
    /// it, `duplicate_command` fails `indeterminate`, a code a tool call
    /// may carry is kept, and every other code fails `tool_error` naming
    /// the delegate's code. Every variant is named: a variant added to
    /// `ErrorCode` later fails to compile until someone places it.
    #[allow(
        clippy::too_many_arguments,
        reason = "a rejection carries the attempt's whole context"
    )]
    fn rejected(
        &self,
        id: &str,
        message: &str,
        run: u64,
        action: &Option<ActionId>,
        gate: &Arc<Mutex<SteerGate>>,
        cancel: &dyn Cancel,
        failure: Failure,
    ) -> Attempt {
        match failure.code {
            ErrorCode::Closing => self.closing(id, message, run, action, gate, cancel),
            ErrorCode::DuplicateCommand => Attempt::Done(lost(id)),
            ErrorCode::MessageRefused | ErrorCode::HookFailed => Attempt::Done(
                JobsError::SteerRefused {
                    id: JobId(id.to_owned()),
                    failure,
                }
                .output(true),
            ),
            ErrorCode::AmbiguousMatch
            | ErrorCode::AuthenticationFailed
            | ErrorCode::Blocked
            | ErrorCode::BlockedHost
            | ErrorCode::BudgetExceeded
            | ErrorCode::Busy
            | ErrorCode::CacheRebuild
            | ErrorCode::Cancelled
            | ErrorCode::CommandConflict
            | ErrorCode::ConfigInvalid
            | ErrorCode::ConfigKeyIgnored
            | ErrorCode::ConnectionFailed
            | ErrorCode::ContextOverflow
            | ErrorCode::CredentialFailed
            | ErrorCode::CredentialMissing
            | ErrorCode::DelegateSession
            | ErrorCode::DepthExceeded
            | ErrorCode::ExtensionFailed
            | ErrorCode::ExtensionIncompatible
            | ErrorCode::ExtensionMissing
            | ErrorCode::ExtensionNotFound
            | ErrorCode::ExtensionNotInstalled
            | ErrorCode::ExtensionRequiredFailed
            | ErrorCode::ExtensionShadowed
            | ErrorCode::ExtensionUnapproved
            | ErrorCode::ExtensionUnavailable
            | ErrorCode::FetchFailed
            | ErrorCode::Flooded
            | ErrorCode::HistoryLoading
            | ErrorCode::HookUnapproved
            | ErrorCode::HttpError
            | ErrorCode::HubVersion
            | ErrorCode::Indeterminate
            | ErrorCode::InstructionsLarge
            | ErrorCode::InvalidArguments
            | ErrorCode::InvalidRequest
            | ErrorCode::IoFailed
            | ErrorCode::LogCorrupt
            | ErrorCode::LoginUrl
            | ErrorCode::Malformed
            | ErrorCode::McpCancelRequested
            | ErrorCode::McpPromptFailed
            | ErrorCode::McpRequiredServerFailed
            | ErrorCode::McpServerUnapproved
            | ErrorCode::McpServerUnavailable
            | ErrorCode::McpToolRemoved
            | ErrorCode::ModelAmbiguous
            | ErrorCode::ModelInvalid
            | ErrorCode::ModelNotFound
            | ErrorCode::ModelUnconfigured
            | ErrorCode::ModelsLoading
            | ErrorCode::NamePinned
            | ErrorCode::NoMatch
            | ErrorCode::NoModel
            | ErrorCode::NonzeroExit
            | ErrorCode::NotFound
            | ErrorCode::NotStepBoundary
            | ErrorCode::NotSubscribed
            | ErrorCode::NotesUnchanged
            | ErrorCode::Orphaned
            | ErrorCode::OutputCap
            | ErrorCode::OutputTruncated
            | ErrorCode::PairingFailed
            | ErrorCode::PathChanged
            | ErrorCode::PausedTooLong
            | ErrorCode::ProviderUnavailable
            | ErrorCode::QuotaExceeded
            | ErrorCode::RateLimited
            | ErrorCode::Refused
            | ErrorCode::RepositoryCodeSkipped
            | ErrorCode::ReviewerSelectionFailed
            | ErrorCode::SessionHasDependents
            | ErrorCode::SessionHeld
            | ErrorCode::SessionNotFound
            | ErrorCode::Signal
            | ErrorCode::SkillInvalid
            | ErrorCode::SkillShadowed
            | ErrorCode::SkillsLarge
            | ErrorCode::StaleFile
            | ErrorCode::StaleRequest
            | ErrorCode::StateTooLarge
            | ErrorCode::StreamClosed
            | ErrorCode::StreamIncomplete
            | ErrorCode::SummaryFailed
            | ErrorCode::SwitchPending
            | ErrorCode::Timeout
            | ErrorCode::TooLarge
            | ErrorCode::ToolDefinitionsLarge
            | ErrorCode::ToolError
            | ErrorCode::Unauthenticated
            | ErrorCode::UnknownCommand
            | ErrorCode::UnknownStopReason
            | ErrorCode::UnknownTool
            | ErrorCode::Unreachable
            | ErrorCode::UnreadableReply
            | ErrorCode::UnsupportedFile
            | ErrorCode::Usage
            | ErrorCode::VersionConflict
            | ErrorCode::WebSearchUnavailable
            | ErrorCode::Other(_) => Attempt::Done(
                JobsError::SteerRefusedOther {
                    id: JobId(id.to_owned()),
                    delegate_code: code_name(&failure.code),
                    message: failure.message.clone(),
                }
                .output(true),
            ),
        }
    }

    /// Answers a cancelled call by what the gate shows: nothing was
    /// delivered before any commit, and a committed send may still join
    /// its turn. Either way no error.
    fn cancelled(&self, id: &str, gate: &Mutex<SteerGate>) -> Output {
        let mut gate = lock(gate);
        gate.cancelled = true;
        text_only(match gate.sent_to {
            None => format!(
                "The call was cancelled before the message reached delegate {id}; nothing was delivered."
            ),
            Some(_) => format!(
                "The call was cancelled after sending to delegate {id} was committed; the message may still join its turn."
            ),
        })
    }
}

/// A copy reached the delegate's run, but its answer was lost.
fn lost(id: &str) -> Output {
    JobsError::AnswerLost {
        id: JobId(id.to_owned()),
    }
    .output(true)
}

/// The call's answer when it failed nothing: plain text.
fn text_only(text: String) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        ..Output::default()
    }
}

/// A copy may have joined a run that has since ended.
fn unanswered(id: &str) -> Output {
    JobsError::Unanswered {
        id: JobId(id.to_owned()),
    }
    .output(true)
}

/// Nothing reached any run: the call was cancelled first.
fn nothing_delivered(id: &str) -> Output {
    text_only(format!(
        "The call was cancelled before the message reached delegate {id}; nothing was delivered."
    ))
}

/// The delegate's code, named for the message.
fn code_name(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "error".into())
}

#[cfg(test)]
#[path = "../message_tests.rs"]
mod steering_tests;

#[cfg(test)]
#[path = "../message_resume_tests.rs"]
mod resume_tests;
