//! A parent's message to its delegate: steering a running one at its next
//! step boundary, or resuming a finished one as a new run of the same job
//! (`docs/delegates.md`, "Talking to a delegate"). The receipt says which.

use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
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
use support::clock::Parker;
use support::group::Listing;

use super::run::{Launch, Launched, Runner, Watch, mint_command_id};
use crate::registry::runs::{Resume, Resumed, Target, Waited, failed};
use crate::registry::Registry;

/// Steers the delegate's session: the jobs-local mirror of `doors::steer`,
/// as `Watch` mirrors the watch. One connection per message, one command
/// id per call. `commit` runs once per connection, after the subscribe is
/// accepted and before any byte of the `steer` line is written; false
/// returns [`Steered::Withheld`] with nothing written. `on_queued` runs
/// once the message is queued, proved by the probe's answer.
pub type Steer = Arc<
    dyn Fn(
            &SessionId,
            &str,
            &str,
            &mut dyn FnMut() -> bool,
            &mut dyn FnMut(),
        ) -> Steered
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

/// Reads the `seq` of a delegate log's last complete line, or the reason
/// it could not be read, which names the log's path. A reason string, as
/// `Resolve`'s lookup answers today, because `jobs` cannot name
/// `log::Error` (`docs/architecture.md`, "The call rules"). `Ok(None)`
/// only for a regular, empty log.
pub type LastSeq = Arc<dyn Fn(&SessionId) -> Result<Option<u64>, String> + Send + Sync>;

/// What the call's gate holds: whether the call was cancelled, and the run
/// a committed attempt went to. The lock is released before any write, so
/// a blocked write never holds it; the lock order is gate, then the
/// registry's inner lock, and nothing takes them the other way.
pub(crate) struct SteerGate {
    cancelled: bool,
    sent_to: Option<u64>,
}

/// Why starting the resumed run failed: nothing was started, nothing
/// queued.
pub(crate) enum StartFailed {
    /// The log's last line could not be read; the reason names its path.
    Boundary(String),
    /// The child could not be spawned.
    Spawn(io::Error),
}

/// Steers a running delegate, or resumes a finished one. The fields are
/// wired by `main`; `bound` is both the stop bound and the closing wait.
pub struct DelegateMessage {
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

/// What a resumed run's `start` returns: the runner's handles and the log
/// the receipt names.
struct Started {
    runner: Runner,
    child: Child,
    listing: Listing,
    log: String,
}

/// What one steer attempt sends the call's thread.
enum AttemptMsg {
    /// The message is queued; the call emits its one delta.
    Queued,
    /// The attempt's answer.
    Done(Steered),
}

/// What an attempt came to on the call's thread.
enum Attempt {
    /// The call is answered.
    Done(Output),
    /// Steer again, waiting the backoff first only after `Gone`; at once
    /// after `Withheld`.
    Again(Duration),
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
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
        super::check_pair(arguments, "id", "message").map_err(EffectsError::Arguments)?;
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
        let (id, message) = match super::check_pair(arguments, "id", "message") {
            Ok(pair) => pair,
            Err(message) => return fail(ErrorCode::InvalidArguments, message),
        };
        // Before any lookup: nothing is steered, resumed or written.
        if message.trim().is_empty() {
            return fail(ErrorCode::InvalidArguments, "`message` must not be empty.".into());
        }
        // One command id per call, on every connection the call opens.
        let command = mint_command_id();
        let gate = Arc::new(Mutex::new(SteerGate {
            cancelled: false,
            sent_to: None,
        }));
        let park = Arc::new(Parker::new());
        cancel.subscribe(Arc::downgrade(&park) as Weak<dyn Wake>);
        loop {
            match self.registry.target(&id) {
                Target::Unknown => {
                    let message = super::super::tool::unknown_message(&id);
                    return fail(ErrorCode::InvalidArguments, message);
                }
                Target::NotDelegate => {
                    return fail(
                        ErrorCode::InvalidArguments,
                        format!("Job `{id}` is not a delegate."),
                    );
                }
                Target::Live(session, run) => {
                    return self.steer_loop(&session, run, &command, &id, &message, &action, &gate, &park, cancel, emit);
                }
                Target::Ended => {
                    return self.resume_now(&id, &message, &action, cancel, &gate, &command, &park, emit);
                }
            }
        }
    }

    /// Steers the live run until the call is answered: one command id,
    /// one gate, resends to the same run only.
    #[allow(
        clippy::too_many_arguments,
        reason = "steering carries the call's whole context"
    )]
    fn steer_loop(
        &self,
        session: &SessionId,
        run: u64,
        command: &str,
        id: &str,
        message: &str,
        action: &Option<ActionId>,
        gate: &Arc<Mutex<SteerGate>>,
        park: &Arc<Parker>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Output {
        let mut backoff = super::run::START_BACKOFF;
        let mut emitted = false;
        loop {
            if let Some(sent) = lock(gate).sent_to
                && sent != run
            {
                return unanswered(id);
            }
            match self.attempt(
                session, command, id, message, run, backoff, action, gate, park,
                cancel, emit, &mut emitted,
            ) {
                Attempt::Done(output) => return output,
                Attempt::Again(wait) => {
                    backoff = wait;
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
            if let Some(steered) = self.pump(&rx, &id, emit, emitted) {
                return self.answer(
                    steered, &id, &message, run, backoff, &action, &gate, cancel,
                    &command, &park, emit,
                );
            }
            if cancel.is_cancelled() {
                return Attempt::Done(self.cancelled(&id, &gate));
            }
            let seen = park.generation();
            if let Some(steered) = self.pump(&rx, &id, emit, emitted) {
                return self.answer(
                    steered, &id, &message, run, backoff, &action, &gate, cancel,
                    &command, &park, emit,
                );
            }
            if cancel.is_cancelled() {
                return Attempt::Done(self.cancelled(&id, &gate));
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
        command: &str,
        park: &Arc<Parker>,
        emit: &dyn Emit,
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
            Steered::Rejected(failure) => match failure.code {
                ErrorCode::Closing => self.closing(
                    id, message, run, action, gate, cancel, command, park, emit,
                ),
                ErrorCode::DuplicateCommand => Attempt::Done(lost(id)),
                _ => Attempt::Done(self.refused(id, failure)),
            },
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
        command: &str,
        park: &Arc<Parker>,
        emit: &dyn Emit,
    ) -> Attempt {
        lock(gate).sent_to = None;
        let until = self
            .clock
            .now()
            .checked_add(self.bound)
            .unwrap_or_else(|| self.clock.now());
        match self.registry.await_end(id, run, until, cancel) {
            Waited::Ended => Attempt::Done(self.resume_now(
                id, message, action, cancel, gate, command, park, emit,
            )),
            Waited::Timeout => Attempt::Done(fail(
                ErrorCode::Closing,
                format!(
                    "Delegate {id} is finishing its run and did not take the message; send it again once it has finished."
                ),
            )),
            Waited::Cancelled => Attempt::Done(self.cancelled(id, gate)),
        }
    }

    /// Resumes the ended delegate, or fails what the registry answers. A
    /// committed send elsewhere fails `indeterminate`; a cancel before the
    /// resume starts nothing.
    #[allow(
        clippy::too_many_arguments,
        reason = "a resume carries the call's whole context"
    )]
    fn resume_now(
        &self,
        id: &str,
        message: &str,
        action: &Option<ActionId>,
        cancel: &dyn Cancel,
        gate: &Arc<Mutex<SteerGate>>,
        command: &str,
        park: &Arc<Parker>,
        emit: &dyn Emit,
    ) -> Output {
        if lock(gate).sent_to.is_some() {
            return unanswered(id);
        }
        // A cancel after this check can still start the resume: the run
        // exists, and its start must be logged.
        if cancel.is_cancelled() {
            return nothing_delivered(id);
        }
        match self.resume(id, message, action.clone()) {
            Ok(resumed) => resumed_receipt(id, resumed),
            Err(Resume::Unknown) => {
                let message = super::super::tool::unknown_message(id);
                fail(ErrorCode::InvalidArguments, message)
            }
            Err(Resume::NotDelegate) => fail(
                ErrorCode::InvalidArguments,
                format!("Job `{id}` is not a delegate."),
            ),
            Err(Resume::Live(session, run)) => self.steer_loop(
                &session, run, command, id, message, action, gate, park, cancel, emit,
            ),
            Err(Resume::Exhausted) => fail(
                ErrorCode::ToolError,
                format!("Delegate {id} cannot run again: its run count is exhausted."),
            ),
            Err(Resume::Start(StartFailed::Boundary(reason))) => fail(
                ErrorCode::ToolError,
                format!(
                    "Resuming delegate {id} failed: its log could not be read: {reason}. Nothing was started."
                ),
            ),
            Err(Resume::Start(StartFailed::Spawn(source))) => fail(
                ErrorCode::ToolError,
                format!("Resuming the delegate failed: {source}."),
            ),
        }
    }

    /// What a rejection carries: its own code when a tool call may carry
    /// it, else `tool_error` naming the delegate's code. The caller
    /// answers `closing` and `duplicate_command` itself.
    fn refused(&self, id: &str, failure: Failure) -> Output {
        let text = format!("Delegate {id} refused the message: {}", failure.message);
        match failure.code {
            ErrorCode::MessageRefused | ErrorCode::HookFailed => fail(failure.code, text),
            ErrorCode::Closing | ErrorCode::DuplicateCommand => {
                unreachable!("the caller answers these")
            }
            ErrorCode::AmbiguousMatch
            | ErrorCode::AuthenticationFailed
            | ErrorCode::Blocked
            | ErrorCode::BlockedHost
            | ErrorCode::BudgetExceeded
            | ErrorCode::Busy
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
            | ErrorCode::HookUnapproved
            | ErrorCode::HttpError
            | ErrorCode::Indeterminate
            | ErrorCode::InstructionsLarge
            | ErrorCode::InvalidArguments
            | ErrorCode::InvalidRequest
            | ErrorCode::IoFailed
            | ErrorCode::LogCorrupt
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
            | ErrorCode::NamePinned
            | ErrorCode::NoMatch
            | ErrorCode::NoModel
            | ErrorCode::NonzeroExit
            | ErrorCode::NotFound
            | ErrorCode::NotStepBoundary
            | ErrorCode::NotSubscribed
            | ErrorCode::Orphaned
            | ErrorCode::OutputCap
            | ErrorCode::OutputTruncated
            | ErrorCode::PairingFailed
            | ErrorCode::PathChanged
            | ErrorCode::ProtocolUnsupported
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
            | ErrorCode::Other(_) => fail(
                ErrorCode::ToolError,
                format!(
                    "Delegate {id} refused the message ({}): {}",
                    code_name(&failure.code),
                    failure.message
                ),
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

    /// Starts run `job.run + 1` of the ended delegate `id`: reads the
    /// run's boundary, then launches the new run. A failed read fails
    /// closed and starts nothing.
    fn resume(
        &self,
        id: &str,
        message: &str,
        action: Option<ActionId>,
    ) -> Result<Resumed<Started>, Resume<StartFailed>> {
        let parent = self.parent.clone();
        let message = message.to_owned();
        let id = id.to_owned();
        let launch = Arc::clone(&self.launch);
        let watch = Arc::clone(&self.watch);
        let last_seq = Arc::clone(&self.last_seq);
        let clock = Arc::clone(&self.clock);
        let bound = self.bound;
        let cap = self.cap;
        self.registry.resume(&id, action, |resumable| {
            // Inside the resume gate, after the "ended" check: no other
            // run can start between the read and the spawn.
            let after = match last_seq(&resumable.delegate.delegate_session_id) {
                Ok(after) => after,
                Err(reason) => return Err(StartFailed::Boundary(reason)),
            };
            let launched = Launched {
                session_id: resumable.delegate.delegate_session_id.clone(),
                job_id: JobId(id.clone()),
                parent: parent.clone(),
                model: resumable.delegate.model.clone(),
                prompt: message.clone(),
                workspace: PathBuf::from(&resumable.delegate.workspace),
                resume: true,
            };
            let log = resumable.output_path.clone();
            let (runner, stop) = Runner::new(
                JobId(id.clone()),
                launched.session_id.clone(),
                PathBuf::from(&resumable.output_path),
                Arc::clone(&clock),
                bound,
                cap,
                Arc::clone(&watch),
                after,
            );
            let (child, listing) = match runner.spawn(&launch, &launched) {
                Ok(started) => started,
                Err(source) => return Err(StartFailed::Spawn(source)),
            };
            Ok((
                stop,
                Started {
                    runner,
                    child,
                    listing,
                    log,
                },
            ))
        })
    }
}

/// A failed tool call: the model reads `content`, the caller matches on
/// the error's message.
fn fail(code: ErrorCode, message: String) -> Output {
    failed(code, format!("{message}\n"), message)
}

/// The call's answer when it failed nothing: plain text.
fn text_only(text: String) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        ..Output::default()
    }
}

/// A copy reached the delegate's run, but its answer was lost.
fn lost(id: &str) -> Output {
    fail(
        ErrorCode::Indeterminate,
        format!(
            "Delegate {id} received the message, but its answer was lost; it joins the delegate's turn unless the delegate refused it."
        ),
    )
}

/// A copy may have joined a run that has since ended.
fn unanswered(id: &str) -> Output {
    fail(
        ErrorCode::Indeterminate,
        format!(
            "Delegate {id} ended before answering; the message may have joined its last turn. Send it again to resume the delegate."
        ),
    )
}

/// Nothing reached any run: the call was cancelled first.
fn nothing_delivered(id: &str) -> Output {
    text_only(format!(
        "The call was cancelled before the message reached delegate {id}; nothing was delivered."
    ))
}

/// The receipt of a committed resume, driving the new run on its own
/// thread. The start is always logged: the `Settle` writes it in
/// `finish`, or the run's notice claim writes it when the output drops.
fn resumed_receipt(id: &str, resumed: Resumed<Started>) -> Output {
    let Resumed {
        settle,
        unreported,
        value: Started {
            runner,
            child,
            listing,
            log,
        },
    } = resumed;
    thread::spawn(move || runner.drive(child, Some(listing), unreported));
    Output {
        content: vec![ContentPart::Text {
            text: format!("Resumed delegate {id}. Its log is {log}."),
        }],
        settle: Some(settle),
        ..Output::default()
    }
}

/// The delegate's code, named for the message.
fn code_name(code: &ErrorCode) -> String {
    match code {
        ErrorCode::Other(name) => name.clone(),
        code => serde_json::to_value(code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "error".into()),
    }
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod steering_tests;

#[cfg(test)]
#[path = "message_resume_tests.rs"]
mod resume_tests;
