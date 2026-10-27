//! Part 7 operation timers (`docs/state.md`, "Diagnostic logs"): one
//! `op_timing` line per timed scope through the one installed timer.
//!
//! Time comes from the injected clock; a test advances the fake clock only
//! after a signal that the code under test is waiting on it, and every
//! receive or join carries the must-succeed bound.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::diag::Observe;
use contract::emit::Emit;
use contract::events::{CacheLifetime, Control, ToolCallRequested, TurnOutcome};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{
    CallError, Delta, Input, ModelCall, ModelRequest, Provider, Reply, ReplyAction,
};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, ErrorCode, SessionId, TurnId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value, json};
use support::lock;

use crate::{BlockLimits, Loop, Model, Reviewer};

// One-shot test controls, reached only from in-fence sites. Each arm is
// consumed by its next hook call; production holds no observer.
static PREAMBLE_EXIT_ADVANCE_MS: Mutex<Option<u64>> = Mutex::new(None);
static FAIL_PREAMBLE_WRITE: Mutex<Option<bool>> = Mutex::new(None);
static REQUEST_SEND_ADVANCE_MS: Mutex<Option<u64>> = Mutex::new(None);
static REQUEST_NOTE_ADVANCE_MS: Mutex<Option<u64>> = Mutex::new(None);
static CANCEL_AFTER_SUBSCRIBE: Mutex<bool> = Mutex::new(false);
static FAIL_STREAM_WRITE: Mutex<Option<bool>> = Mutex::new(None);
static FAIL_HANDOFF_STARTED: Mutex<Option<bool>> = Mutex::new(None);
static FAIL_HANDOFF_COMPLETED: Mutex<Option<bool>> = Mutex::new(None);
static FAIL_RESTARTED: Mutex<Option<bool>> = Mutex::new(None);

/// Advances the preamble test clock by `ms` in the exit hook, once.
pub(crate) fn arm_preamble_exit_advance(ms: u64) {
    *lock(&PREAMBLE_EXIT_ADVANCE_MS) = Some(ms);
}

/// Fails the next `PreambleBuilt` append once with `Error::Log`.
pub(crate) fn arm_fail_preamble_write() {
    *lock(&FAIL_PREAMBLE_WRITE) = Some(true);
}

/// Advances the request test clock by `ms` in the send exit hook, once.
pub(crate) fn arm_request_send_advance(ms: u64) {
    *lock(&REQUEST_SEND_ADVANCE_MS) = Some(ms);
}

/// Advances the request test clock by `ms` in the note exit hook, once.
pub(crate) fn arm_request_note_advance(ms: u64) {
    *lock(&REQUEST_NOTE_ADVANCE_MS) = Some(ms);
}

/// Cancels the turn once at the next cancellable subscription boundary.
pub(crate) fn arm_cancel_after_subscribe() {
    *lock(&CANCEL_AFTER_SUBSCRIBE) = true;
}

/// Fails the next stream sink write once with `Error::Log`.
pub(crate) fn arm_fail_stream_write() {
    *lock(&FAIL_STREAM_WRITE) = Some(true);
}

/// Fails the next handoff-started record once with `Error::Log`.
pub(crate) fn arm_fail_handoff_started() {
    *lock(&FAIL_HANDOFF_STARTED) = Some(true);
}

/// Fails the next handoff-completed record once with `Error::Log`.
pub(crate) fn arm_fail_handoff_completed() {
    *lock(&FAIL_HANDOFF_COMPLETED) = Some(true);
}

/// Fails the next handoff restart once with `Error::Log`.
pub(crate) fn arm_fail_restarted() {
    *lock(&FAIL_RESTARTED) = Some(true);
}

/// Runs after the preamble guard dropped and before the opening is
/// ensured. Advances the armed clock, proving later work stays outside.
pub(crate) fn preamble_exit(clock: &dyn Clock) {
    if let Some(ms) = lock(&PREAMBLE_EXIT_ADVANCE_MS).take() {
        clock.sleep(Duration::from_millis(ms));
    }
}

/// Whether the next `PreambleBuilt` append fails. Consumed once.
pub(crate) fn take_fail_preamble_write() -> bool {
    lock(&FAIL_PREAMBLE_WRITE).take().is_some()
}

/// Runs at the send request scope's exit, inside the scope. Advances the
/// armed clock, proving the scope encloses construction.
pub(crate) fn request_send_exit(clock: &dyn Clock) {
    if let Some(ms) = lock(&REQUEST_SEND_ADVANCE_MS).take() {
        clock.sleep(Duration::from_millis(ms));
    }
}

/// Runs at the note request scope's exit, inside the scope.
pub(crate) fn request_note_exit(clock: &dyn Clock) {
    if let Some(ms) = lock(&REQUEST_NOTE_ADVANCE_MS).take() {
        clock.sleep(Duration::from_millis(ms));
    }
}

/// Runs after the cancellable subscription, before the cancel check.
/// Returns true once when the test armed a synchronous cancel there.
pub(crate) fn take_cancel_after_subscribe() -> bool {
    std::mem::replace(&mut *lock(&CANCEL_AFTER_SUBSCRIBE), false)
}

/// Whether the next stream sink write fails. Consumed once.
pub(crate) fn take_fail_stream_write() -> bool {
    lock(&FAIL_STREAM_WRITE).take().is_some()
}

/// Whether the next handoff-started record fails. Consumed once.
pub(crate) fn take_fail_handoff_started() -> bool {
    lock(&FAIL_HANDOFF_STARTED).take().is_some()
}

/// Whether the next handoff-completed record fails. Consumed once.
pub(crate) fn take_fail_handoff_completed() -> bool {
    lock(&FAIL_HANDOFF_COMPLETED).take().is_some()
}

/// Whether the next handoff restart fails. Consumed once.
pub(crate) fn take_fail_restarted() -> bool {
    lock(&FAIL_RESTARTED).take().is_some()
}

/// The `Error::Log` a faulted write returns: the file system refused.
pub(crate) fn log_io_error() -> Error {
    Error::Log(log::Error::Io {
        path: std::path::PathBuf::from("logs/session-s_test.log"),
        source: std::io::Error::other("fiber-test write fault"),
    })
}

use crate::Error;

// Standing rules that allow nothing and remember nothing.
#[derive(Default)]
struct EmptyRules;

impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A tool whose calls only read, and return `text`: no approval, no review.
struct ReadsTool {
    name: &'static str,
    text: &'static str,
    handoff: Option<&'static str>,
}

impl ReadsTool {
    fn reads(name: &'static str, text: &'static str) -> Self {
        Self {
            name,
            text,
            handoff: None,
        }
    }

    fn noting(name: &'static str, text: &'static str, note: &'static str) -> Self {
        Self {
            name,
            text,
            handoff: Some(note),
        }
    }
}

impl Tool for ReadsTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.name.into(),
            description: "Reads.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: self.text.into(),
            }],
            control: self.handoff.map(|note| Control {
                handoff: Some(note.into()),
                questions: None,
                skill: None,
                name: None,
            }),
            ..Output::default()
        }
    }
}

/// One scripted answer of the timing provider: fragments, then how the
/// call ends, and the fake work inside the call, in milliseconds.
enum Answer {
    /// Emits `deltas`, advances the clock by `advance_ms`, ends with `end`.
    Run {
        deltas: Vec<Delta>,
        end: Result<Reply, CallError>,
        advance_ms: u64,
    },
    /// Parks on the clock until cancelled, then advances by
    /// `cancel_advance_ms` and ends cancelled. The test cancels only after
    /// [`FakeClock::await_parked`] shows the hold waiting on the clock.
    Hold { cancel_advance_ms: u64 },
}

/// A provider on the model seam whose calls advance the fake clock
/// themselves: synchronous fake work, released by no controller.
struct TimingProvider {
    clock: Arc<FakeClock>,
    script: Mutex<VecDeque<Answer>>,
    requests: Mutex<Vec<ModelRequest>>,
    entered: mpsc::Sender<()>,
    cancel: Option<Arc<crate::TurnCancel>>,
    probe_state: bool,
}

impl TimingProvider {
    fn call_state(&self) -> TimingCall {
        TimingCall {
            clock: Arc::clone(&self.clock),
            script: self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Answer::Run {
                    deltas: Vec::new(),
                    end: Err(CallError::Failed {
                        failure: Failure {
                            code: ErrorCode::Other("script_exhausted".into()),
                            message: "The timing provider has no reply left.".into(),
                            retry_after_ms: None,
                            provider: None,
                        },
                        should_retry: None,
                        usage: Box::new(fakes::unnamed_usage()),
                    }),
                    advance_ms: 0,
                }),
            cancelled: AtomicBool::new(false),
            waker: Mutex::new(None),
            entered: self.entered.clone(),
            cancel: self.cancel.clone(),
            probe_state: self.probe_state,
        }
    }
}

impl Provider for TimingProvider {
    fn wire_tools(&self, tools: &[contract::provider::ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.requests.lock().unwrap().push(request.clone());
        Box::new(self.call_state())
    }
}

/// A provider whose `wire_tools` advances the clock: the build work the
/// preamble scope encloses.
struct AdvanceWireProvider {
    clock: Arc<FakeClock>,
    advance_ms: u64,
    inner: Arc<TimingProvider>,
}

impl Provider for AdvanceWireProvider {
    fn wire_tools(&self, tools: &[contract::provider::ToolDefinition]) -> Vec<Map<String, Value>> {
        self.clock.sleep(Duration::from_millis(self.advance_ms));
        self.inner.wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.inner.call(request)
    }
}

struct TimingCall {
    clock: Arc<FakeClock>,
    script: Answer,
    cancelled: AtomicBool,
    waker: Mutex<Option<Arc<support::clock::Parker>>>,
    entered: mpsc::Sender<()>,
    cancel: Option<Arc<crate::TurnCancel>>,
    probe_state: bool,
}

impl TimingCall {
    #[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
    fn cancelled_end(end: Result<Reply, CallError>) -> Result<Reply, CallError> {
        match end {
            Err(CallError::Cancelled { .. }) => end,
            _ => Err(CallError::Cancelled {
                usage: Box::new(fakes::unnamed_usage()),
            }),
        }
    }
}

impl ModelCall for TimingCall {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let _sent = self.entered.send(());
        match &self.script {
            Answer::Run {
                deltas,
                end,
                advance_ms,
            } => {
                for delta in deltas {
                    if self.cancelled.load(Ordering::SeqCst) {
                        return Self::cancelled_end(clone_end(end));
                    }
                    sink(delta.clone());
                }
                if self.cancelled.load(Ordering::SeqCst) {
                    return Self::cancelled_end(clone_end(end));
                }
                // The guard is alive across `run`: reading the signal here
                // deadlocks a regression that holds its lock over the call,
                // failing the test instead of hanging it.
                if self.probe_state
                    && let Some(cancel) = &self.cancel
                {
                    let _ = cancel.state();
                }
                self.clock.sleep(Duration::from_millis(*advance_ms));
                clone_end(end)
            }
            Answer::Hold { cancel_advance_ms } => {
                // Parks far out: the test's cancel is the only release, and
                // the park itself is the signal the test awaits.
                let until = self.clock.now() + Duration::from_secs(3600);
                let parker = Arc::new(support::clock::Parker::new());
                let seen = parker.generation();
                *self.waker.lock().unwrap() = Some(Arc::clone(&parker));
                let keeper: Arc<dyn Wake> = parker.clone();
                self.clock.subscribe(Arc::downgrade(&keeper));
                if self.probe_state
                    && let Some(cancel) = &self.cancel
                {
                    let _ = cancel.state();
                }
                loop {
                    if self.cancelled.load(Ordering::SeqCst) {
                        self.clock.sleep(Duration::from_millis(*cancel_advance_ms));
                        return Err(CallError::Cancelled {
                            usage: Box::new(fakes::unnamed_usage()),
                        });
                    }
                    if self.clock.now() >= until {
                        return Err(CallError::Cancelled {
                            usage: Box::new(fakes::unnamed_usage()),
                        });
                    }
                    parker.park(self.clock.as_ref(), Some(until), seen);
                }
            }
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(parker) = self.waker.lock().unwrap().as_ref() {
            parker.bump();
        }
    }
}

#[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
fn clone_end(end: &Result<Reply, CallError>) -> Result<Reply, CallError> {
    match end {
        Ok(reply) => Ok(reply.clone()),
        Err(CallError::Failed {
            failure,
            should_retry,
            usage,
        }) => Err(CallError::Failed {
            failure: failure.clone(),
            should_retry: *should_retry,
            usage: usage.clone(),
        }),
        Err(CallError::Cancelled { usage }) => Err(CallError::Cancelled {
            usage: usage.clone(),
        }),
    }
}

#[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
fn text_reply(text: &str) -> (Vec<Delta>, Result<Reply, CallError>) {
    let middle = text.char_indices().nth(text.chars().count() / 2);
    let (head, tail) = text.split_at(middle.map_or(text.len(), |(i, _)| i));
    let deltas = [head, tail]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(|part| {
            Delta::Text(contract::events::TextDelta {
                text: (*part).into(),
            })
        })
        .collect();
    (deltas, Ok(fakes::reply(text)))
}

#[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
fn tool_call_reply(name: &str, arguments: Value) -> (Vec<Delta>, Result<Reply, CallError>) {
    let mut end = fakes::reply("");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments: arguments.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    let deltas = vec![Delta::ToolCallArguments(
        contract::events::ToolCallArgumentsDelta {
            index: 0,
            name: Some(name.into()),
            text: arguments.to_string(),
        },
    )];
    (deltas, Ok(end))
}

#[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
fn failed_end(code: ErrorCode) -> Result<Reply, CallError> {
    Err(CallError::Failed {
        failure: Failure {
            code,
            message: "The call failed.".into(),
            retry_after_ms: None,
            provider: None,
        },
        should_retry: None,
        usage: Box::new(fakes::unnamed_usage()),
    })
}

fn run_answer(text: &str, advance_ms: u64) -> Answer {
    let (deltas, end) = text_reply(text);
    Answer::Run {
        deltas,
        end,
        advance_ms,
    }
}

fn run_tool_answer(name: &str, arguments: Value, advance_ms: u64) -> Answer {
    let (deltas, end) = tool_call_reply(name, arguments);
    Answer::Run {
        deltas,
        end,
        advance_ms,
    }
}

fn run_failed_answer(code: ErrorCode, advance_ms: u64) -> Answer {
    Answer::Run {
        deltas: Vec::new(),
        end: failed_end(code),
        advance_ms,
    }
}

struct World {
    home: fakes::TempDir,
    log: Arc<Log>,
    clock: Arc<FakeClock>,
    origin: Instant,
    provider: Arc<TimingProvider>,
    attempts: Arc<Mutex<Vec<u32>>>,
    /// The reviewer's provider, when one judges calls.
    reviewer: Option<Arc<TimingProvider>>,
    /// `turn.request` lines the provider factory saw completed when each
    /// attempt was built: the request scope closes before construction.
    factory_saw: Arc<Mutex<Vec<(usize, usize)>>>,
    inbox_tx: mpsc::Sender<Delivery>,
    cancel: Arc<crate::TurnCancel>,
    /// The loop, parked here between turns. [`turn`] takes it onto its
    /// detached worker for the turn and parks it back with the outcome, so
    /// the deadline can fail the test without joining a hung turn.
    looped: Option<Loop>,
}

impl World {
    /// Borrows the loop parked in the world.
    fn loop_mut(&mut self) -> &mut Loop {
        self.looped
            .as_mut()
            .expect("the loop is parked in the world")
    }
}

struct SwitchRig {
    script_b: Vec<Answer>,
}

struct Build {
    script: Vec<Answer>,
    tools: Vec<Arc<dyn Tool>>,
    retry: Option<crate::Retry>,
    answerable: bool,
    debug: bool,
    load: Option<crate::LoadApproved>,
    load_advance_ms: Option<u64>,
    wire_advance_ms: Option<u64>,
    observe: bool,
    /// Cancels the turn at the factory entry of the given 1-based call:
    /// a deterministic cancel between call construction and the
    /// cancellable subscription.
    cancel_at_attempt: Option<usize>,
    /// Advances the clock on every completed handoff's restart.
    forget_advance_ms: Option<u64>,
    handoff: Option<crate::HandoffSettings>,
    budget: Option<f64>,
    warm: Option<u32>,
    hooks: Option<Arc<dyn contract::hook::Hooks>>,
    probe_state: bool,
    switch: Option<SwitchRig>,
    reviewer_script: Option<Vec<Answer>>,
}

fn plain_build(script: Vec<Answer>) -> Build {
    Build {
        script,
        tools: Vec::new(),
        retry: None,
        answerable: true,
        debug: true,
        load: None,
        load_advance_ms: None,
        wire_advance_ms: None,
        observe: false,
        cancel_at_attempt: None,
        forget_advance_ms: None,
        handoff: None,
        budget: None,
        warm: None,
        hooks: None,
        probe_state: false,
        switch: None,
        reviewer_script: None,
    }
}

fn no_wait() -> crate::Retry {
    crate::Retry {
        attempts: 3,
        initial: Duration::ZERO,
        max: Duration::ZERO,
    }
}

fn model() -> Model {
    Model {
        reference: "fake/model".into(),
        cost: None,
        subscription: false,
    }
}

fn switch_rig(script_b: Vec<Answer>) -> SwitchRig {
    SwitchRig { script_b }
}

fn switcher_for(provider_b: Arc<TimingProvider>) -> crate::Prepare {
    Arc::new(
        move |_: &contract::commands::ModelArgs,
              _: Option<&str>,
              _: Option<contract::ThinkingLevel>| {
            Ok(crate::Prepared {
                provider: Arc::clone(&provider_b) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/model-2".into(),
                    cost: None,
                    subscription: false,
                },
                thinking: None,
                chosen: None,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: crate::HandoffSettings::default(),
                reviewer: Err(Failure {
                    code: ErrorCode::NoModel,
                    message: crate::NO_MODEL_MESSAGE.into(),
                    provider: None,
                    retry_after_ms: None,
                }),
                observe: None,
                reviewer_observe: None,
                web_search: crate::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
            })
        },
    )
}

fn switch_model(world: &mut World) {
    world
        .loop_mut()
        .take_switch(
            crate::switch::Asked::Model(contract::commands::ModelArgs {
                model: "fake/model-2".into(),
                thinking: None,
            }),
            Ack(Box::new(|_| {})),
            true,
        )
        .unwrap();
}

/// How many `op` lines `path` holds now: what the provider factory sees
/// of completed scopes when it builds an attempt.
fn count_op(path: &std::path::Path, op: &str) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["code"] == "op_timing" && line["data"]["op"] == op)
        .count()
}

fn build(mut opts: Build) -> World {
    let home = fakes::TempDir::new("fiber-timing");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = FakeClock::new();
    let origin = clock.origin();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn Clock>;
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let (entered_tx, _) = mpsc::channel();
    let cancel = Arc::new(crate::TurnCancel::default());
    let mut world_attempts = Arc::new(Mutex::new(Vec::new()));
    let factory_saw = Arc::new(Mutex::new(Vec::new()));
    let mut world_reviewer: Option<Arc<TimingProvider>> = None;
    let provider = Arc::new(TimingProvider {
        clock: Arc::clone(&clock),
        script: Mutex::new(opts.script.drain(..).collect()),
        requests: Mutex::new(Vec::new()),
        entered: entered_tx,
        cancel: Some(Arc::clone(&cancel)),
        probe_state: opts.probe_state,
    });
    let provider_for_loop: Arc<dyn Provider> = if let Some(ms) = opts.wire_advance_ms {
        Arc::new(AdvanceWireProvider {
            clock: Arc::clone(&clock),
            advance_ms: ms,
            inner: Arc::clone(&provider),
        })
    } else {
        Arc::clone(&provider) as Arc<dyn Provider>
    };
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let rules: Arc<dyn Rules> = Arc::new(EmptyRules);
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: provider_for_loop,
            model: model(),
            prompt: crate::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&dyn_clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: opts
                .tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules,
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&cancel))
    .answerable(opts.answerable);
    if opts.debug {
        let diag = log::diag::Diag::new(
            home.path(),
            log::diag::Process::Session,
            log::diag::Level::Debug,
            Arc::clone(&dyn_clock) as Arc<dyn Clock>,
        );
        diag.attach(&SessionId("s_test".into()));
        looped = looped.diagnostics(Arc::new(diag));
    }
    if let Some(retry) = opts.retry {
        looped = looped.retry(retry);
    }
    if let Some(load) = opts.load.take() {
        looped = looped.approved_code(load);
    } else if let Some(ms) = opts.load_advance_ms {
        let clock = Arc::clone(&clock);
        looped = looped.approved_code(Arc::new(move |_: &crate::offer::OfferOutcome| {
            clock.sleep(Duration::from_millis(ms));
            Ok(crate::Approved::default())
        }));
    }
    if opts.observe {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&attempts);
        let provider_requests = Arc::clone(&provider);
        let saw = Arc::clone(&factory_saw);
        let cancel_at = opts.cancel_at_attempt;
        let cancel_signal = Arc::clone(&cancel);
        let diag_path = home.path().join("logs").join("session-s_test.log");
        let observe: Observe = Arc::new(move |request: &ModelRequest, attempt: NonZeroU32| {
            seen.lock().unwrap().push(attempt.get());
            saw.lock().unwrap().push((
                count_op(&diag_path, "turn.request"),
                count_op(&diag_path, "turn.model_call"),
            ));
            if cancel_at == Some(seen.lock().unwrap().len()) {
                cancel_signal.cancel();
            }
            provider_requests.call(request)
        });
        looped = looped.observed(Some(observe));
        world_attempts = attempts;
    }
    if let Some(settings) = opts.handoff {
        looped = looped.handoff(settings);
    }
    if let Some(usd) = opts.budget {
        looped = looped.budget(Some(usd));
    }
    if let Some(cap) = opts.warm {
        looped = looped.warm(Some(cap));
    }
    if let Some(hooks) = opts.hooks {
        looped = looped.hooks(hooks);
    }
    if let Some(rig) = opts.switch {
        let provider_b = Arc::new(TimingProvider {
            clock: Arc::clone(&clock),
            script: Mutex::new(rig.script_b.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            entered: mpsc::channel().0,
            cancel: Some(Arc::clone(&cancel)),
            probe_state: false,
        });
        looped = looped.switcher(switcher_for(provider_b), crate::Switchable { chosen: None });
    }
    if let Some(script) = opts.reviewer_script {
        let reviewer = Arc::new(TimingProvider {
            clock: Arc::clone(&clock),
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            entered: mpsc::channel().0,
            cancel: None,
            probe_state: false,
        });
        world_reviewer = Some(reviewer.clone());
        looped = looped.reviewer(
            Ok(Reviewer {
                provider: reviewer as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/reviewer".into(),
                    cost: None,
                    subscription: false,
                },
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            BlockLimits::default(),
        );
    }
    if let Some(ms) = opts.forget_advance_ms {
        let clock = Arc::clone(&clock);
        looped = looped.on_handoff(Arc::new(move || {
            clock.sleep(Duration::from_millis(ms));
        }));
    }
    World {
        home,
        log,
        clock,
        origin,
        provider,
        attempts: world_attempts,
        factory_saw,
        reviewer: world_reviewer,
        inbox_tx,
        cancel,
        looped: Some(looped),
    }
}

fn prompt(world: &World, text: &str) {
    world
        .inbox_tx
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: text.into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_go".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
}

fn person_handoff(world: &World, instructions: Option<&str>) {
    world
        .inbox_tx
        .send(Delivery::Handoff(
            CommandId("c_handoff".into()),
            contract::commands::Handoff {
                instructions: instructions.map(str::to_owned),
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
}

fn turn(world: &mut World) -> Option<TurnOutcome> {
    let (done_tx, done_rx) = mpsc::channel();
    let mut looped = world
        .looped
        .take()
        .expect("the loop is parked in the world");
    // Detached and never joined: the deadline below must fail the test even
    // when the turn hangs. A scoped worker would hold the test at scope exit
    // until the hung turn ends, so the expiry panic could never unwind and
    // the test would hang until nextest kills it (docs/testing.md, "Waits
    // and timeouts"). The worker owns the loop; on expiry it is left behind
    // with it, and the park-back below runs only when the turn ends in time.
    std::thread::spawn(move || {
        let outcome = looped.turn();
        let _sent = done_tx.send((outcome, looped));
    });
    let (outcome, looped) =
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv_or_fail(&done_rx, "the turn ends");
    world.looped = Some(looped);
    outcome.unwrap()
}

/// Every `op_timing` record in the session's diagnostic file, in order.
fn op_records(world: &World) -> Vec<OpRecord> {
    let path = world.home.path().join("logs").join("session-s_test.log");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["code"] == "op_timing")
        .map(|line| {
            let data = line["data"].clone();
            let mut keys: Vec<String> = data
                .as_object()
                .map(|object| object.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            OpRecord {
                code: line["code"].as_str().unwrap_or_default().into(),
                level: line["level"].as_str().unwrap_or_default().into(),
                process: line["process"].as_str().unwrap_or_default().into(),
                session: line["session_id"].as_str().unwrap_or_default().into(),
                message: line["message"].as_str().unwrap_or_default().into(),
                op: data["op"].as_str().unwrap_or_default().into(),
                ms: data["ms"].as_u64().unwrap_or(u64::MAX),
                keys,
                raw: line,
            }
        })
        .collect()
}

#[derive(Debug)]
struct OpRecord {
    code: String,
    level: String,
    process: String,
    session: String,
    message: String,
    op: String,
    ms: u64,
    keys: Vec<String>,
    raw: Value,
}

fn ops(world: &World, op: &str) -> Vec<OpRecord> {
    op_records(world)
        .into_iter()
        .filter(|line| line.op == op)
        .collect()
}

/// Only Part 7's four operations: Part 8's tool timers carry `tool` as a
/// third key and are asserted in `tool_timing_tests`.
fn p7_records(world: &World) -> Vec<OpRecord> {
    op_records(world)
        .into_iter()
        .filter(|line| {
            matches!(
                line.op.as_str(),
                "startup.preamble" | "turn.request" | "turn.model_call" | "turn.handoff"
            )
        })
        .collect()
}

/// Every Part 7 `op_timing` line carries the session identity and only
/// `op` and `ms`: never an argument, a path or any content. Callers pass
/// [`p7_records`]; Part 8's tool timers carry `tool` as a third key.
fn assert_wire(records: &[OpRecord]) {
    for record in records {
        assert_eq!(record.code, "op_timing", "{:?}", record.raw);
        assert_eq!(record.level, "debug", "{:?}", record.raw);
        assert_eq!(record.process, "session", "{:?}", record.raw);
        assert_eq!(record.session, "s_test", "{:?}", record.raw);
        assert_eq!(record.message, "An operation ended.", "{:?}", record.raw);
        assert_eq!(
            record.keys,
            vec!["ms".to_owned(), "op".to_owned()],
            "{:?}",
            record.raw
        );
    }
}

fn kinds(world: &World) -> Vec<String> {
    log::read(&world.home.path().join("s_test"))
        .unwrap()
        .iter()
        .map(|line| line.kind.clone())
        .collect()
}

// The preamble boundary: approved loading stays outside, the build's wire
// work stays inside, and the opening's work stays outside.
#[test]
fn the_first_build_times_only_the_build() {
    let mut world = build(Build {
        load_advance_ms: Some(11),
        wire_advance_ms: Some(3),
        ..plain_build(vec![run_answer("Hello.", 0)])
    });
    arm_preamble_exit_advance(17);
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let records = ops(&world, "startup.preamble");
    assert_eq!(records.len(), 1, "{:?}", op_records(&world));
    assert_eq!(
        records[0].ms, 3,
        "11 ms of loading and 17 ms of opening stay out"
    );
    assert_wire(&p7_records(&world));
    assert_eq!(
        kinds(&world),
        vec![
            "session_started".to_owned(),
            "preamble_built".to_owned(),
            "opening_message".to_owned(),
            "turn_started".to_owned(),
            "step_started".to_owned(),
            "assistant_message_started".to_owned(),
            "text_completed".to_owned(),
            "usage_recorded".to_owned(),
            "assistant_message_completed".to_owned(),
            "turn_completed".to_owned(),
        ],
        "the preamble journey writes exactly these durable events",
    );
}

#[test]
fn a_second_turn_reuses_the_preamble_without_timing_it() {
    let mut world = build(plain_build(vec![
        run_answer("One.", 0),
        run_answer("Two.", 0),
    ]));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
    prompt(&world, "again");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "startup.preamble").len(),
        1,
        "unchanged reuse writes no line"
    );
}

#[test]
fn a_resumed_process_builds_once_more() {
    let mut first = build(plain_build(vec![run_answer("One.", 0)]));
    prompt(&first, "hi");
    assert_eq!(turn(&mut first), Some(TurnOutcome::Completed));
    assert_eq!(ops(&first, "startup.preamble").len(), 1);
    // The resume folds the same log in a new process: its first build is
    // that process's first construction, so it writes one line. Dropping
    // the first loop and its log releases the session lock.
    let World {
        home,
        log: _log,
        looped: _looped,
        ..
    } = first;
    drop(_looped);
    drop(_log);
    let clock = FakeClock::new();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn Clock>;
    let log = Arc::new(
        Log::open(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let resumed = crate::resume::resumed(log.dir()).unwrap();
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let diag = log::diag::Diag::new(
        home.path(),
        log::diag::Process::Session,
        log::diag::Level::Debug,
        Arc::clone(&dyn_clock),
    );
    diag.attach(&SessionId("s_test".into()));
    let looped = Loop::resume(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(TimingProvider {
                clock: Arc::clone(&clock),
                script: Mutex::new(VecDeque::from([run_answer("Two.", 0)])),
                requests: Mutex::new(Vec::new()),
                entered: mpsc::channel().0,
                cancel: None,
                probe_state: false,
            }) as Arc<dyn Provider>,
            model: model(),
            prompt: crate::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&dyn_clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.path().join("workspace").display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        resumed,
    )
    .unwrap()
    .diagnostics(Arc::new(diag));
    let mut second = World {
        home,
        log,
        clock: Arc::clone(&clock),
        origin: clock.origin(),
        provider: Arc::new(TimingProvider {
            clock: Arc::clone(&clock),
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            entered: mpsc::channel().0,
            cancel: None,
            probe_state: false,
        }),
        attempts: Arc::new(Mutex::new(Vec::new())),
        factory_saw: Arc::new(Mutex::new(Vec::new())),
        reviewer: None,
        inbox_tx,
        cancel: Arc::new(crate::TurnCancel::default()),
        looped: Some(looped),
    };
    prompt(&second, "again");
    assert_eq!(turn(&mut second), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&second, "startup.preamble").len(),
        2,
        "the resumed process's first build writes one line"
    );
}
#[test]
fn a_rewound_loop_reuses_its_logged_preamble_silently() {
    let mut world = build(plain_build(vec![
        run_answer("One.", 0),
        run_answer("Two.", 0),
    ]));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
    let log = Arc::clone(&world.log);
    let clock = Arc::clone(&world.clock) as Arc<dyn Clock>;
    let rewound = Loop::rewound(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(TimingProvider {
                clock: Arc::clone(&world.clock),
                script: Mutex::new(VecDeque::from([run_answer("Two.", 0)])),
                requests: Mutex::new(Vec::new()),
                entered: mpsc::channel().0,
                cancel: None,
                probe_state: false,
            }) as Arc<dyn Provider>,
            model: model(),
            prompt: crate::PromptInputs::new(
                world.home.path().to_path_buf(),
                "/bin/sh".into(),
                world
                    .home
                    .path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: {
                let (tx, rx) = mpsc::channel();
                world.inbox_tx = tx;
                rx
            },
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: world.home.path().join("workspace").display().to_string(),
                credentials: world.home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        crate::Rewound {
            factory: None,
            from: contract::shapes::Point {
                session_id: SessionId("s_test".into()),
                seq: contract::Seq(1),
            },
            worktree: None,
            how: crate::Continuation::Rewind {
                note: String::new(),
                summary: None,
                name: None,
            },
        },
    )
    .unwrap();
    world.looped = Some(rewound);
    prompt(&world, "again");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "startup.preamble").len(),
        1,
        "a preinstalled preamble builds nothing"
    );
}

#[test]
fn a_switch_before_the_first_build_still_builds_once() {
    let rig = switch_rig(vec![run_answer("Switched.", 0)]);
    let mut world = build(Build {
        switch: Some(rig),
        ..plain_build(vec![])
    });
    switch_model(&mut world);
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
}

#[test]
fn a_switch_after_a_build_rebuilds_silently() {
    let rig = switch_rig(vec![run_answer("Switched.", 0)]);
    let mut world = build(Build {
        switch: Some(rig),
        ..plain_build(vec![run_answer("One.", 0)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
    switch_model(&mut world);
    prompt(&world, "again");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "startup.preamble").len(),
        1,
        "a same-session rebuild writes no line"
    );
}

#[test]
fn a_loop_that_never_turns_writes_no_preamble_timing() {
    let world = build(plain_build(vec![]));
    assert!(ops(&world, "startup.preamble").is_empty());
}

#[test]
fn a_failed_approved_load_writes_no_preamble_timing() {
    let mut world = build(Build {
        load: Some(Arc::new(|_: &crate::offer::OfferOutcome| {
            Err(Failure {
                code: ErrorCode::Other("repository_unreadable".into()),
                message: "The repository code could not be read.".into(),
                retry_after_ms: None,
                provider: None,
            })
        })),
        ..plain_build(vec![])
    });
    let Err(error) = world.loop_mut().ensure_preamble() else {
        panic!("a failed load fails the build");
    };
    assert!(matches!(error, Error::RepositoryCode(_)));
    assert!(ops(&world, "startup.preamble").is_empty());
}

#[test]
fn a_failed_preamble_write_times_once_and_installs_nothing() {
    let mut world = build(plain_build(vec![run_answer("Hello.", 0)]));
    arm_fail_preamble_write();
    let Err(error) = world.loop_mut().ensure_preamble() else {
        panic!("a failed write fails the build");
    };
    assert!(matches!(error, Error::Log(_)));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
    // Nothing installed: the retry builds successfully, and stays silent.
    let opened = world.loop_mut().ensure_preamble().unwrap();
    assert!(opened, "the retry installs the preamble");
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "startup.preamble").len(), 1);
}

/// A clock counting its monotonic reads: at `info` the build contributes
/// none, while ordinary product reads would still count.
struct SpyClock {
    inner: Arc<FakeClock>,
    reads: std::sync::atomic::AtomicU64,
}

impl Clock for SpyClock {
    fn now(&self) -> Instant {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.now()
    }

    fn wall(&self) -> std::time::SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

#[test]
fn at_info_the_build_runs_but_writes_no_timer_line() {
    let mut world = build(Build {
        debug: false,
        ..plain_build(vec![run_answer("Hello.", 0)])
    });
    // At `info` the build still runs, but the timer is one level check
    // and no line is written.
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert!(
        kinds(&world).contains(&"preamble_built".to_owned()),
        "construction still runs at info"
    );
    assert!(
        op_records(&world).is_empty(),
        "no timer line at info: {:?}",
        op_records(&world)
    );
}

#[test]
fn at_info_ensure_preamble_takes_no_monotonic_read() {
    let home = fakes::TempDir::new("fiber-timing-spy");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let spy = Arc::new(SpyClock {
        inner: FakeClock::new(),
        reads: 0.into(),
    });
    let dyn_clock = Arc::clone(&spy) as Arc<dyn Clock>;
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let mut looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::new(TimingProvider {
                clock: FakeClock::new(),
                script: Mutex::new(VecDeque::new()),
                requests: Mutex::new(Vec::new()),
                entered: mpsc::channel().0,
                cancel: None,
                probe_state: false,
            }) as Arc<dyn Provider>,
            model: model(),
            prompt: crate::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&dyn_clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        None,
    )
    .unwrap();
    let _ = inbox_tx;
    spy.reads.store(0, Ordering::SeqCst);
    looped.ensure_preamble().unwrap();
    assert_eq!(
        spy.reads.load(Ordering::SeqCst),
        0,
        "at info the timer takes no monotonic read"
    );
}

// A request scope covers the history assembly plus the request, and closes
// before the provider builds the call: the factory sees it completed.
#[test]
fn an_ordinary_send_times_one_closed_request() {
    let mut world = build(Build {
        observe: true,
        ..plain_build(vec![run_answer("Hello.", 0)])
    });
    arm_request_send_advance(5);
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let records = ops(&world, "turn.request");
    assert_eq!(records.len(), 1, "{:?}", op_records(&world));
    assert_eq!(records[0].ms, 5);
    assert_wire(&p7_records(&world));
    assert_eq!(
        *world.factory_saw.lock().unwrap(),
        vec![(1, 0)],
        "the factory saw the closed request and no open model scope"
    );
    assert_eq!(*world.attempts.lock().unwrap(), vec![1]);
}

#[test]
fn a_note_request_times_its_assembly() {
    let mut world = build(plain_build(vec![
        run_answer("Note.", 0),
        run_answer("Done.", 0),
    ]));
    arm_request_note_advance(4);
    person_handoff(&world, Some("focus"));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let requests = ops(&world, "turn.request");
    assert_eq!(requests.len(), 2, "{:?}", op_records(&world));
    assert_eq!(requests[0].ms, 4, "the note assembly carries its hold");
    assert_eq!(requests[1].ms, 0, "the ordinary send advanced nothing");
    assert_wire(&p7_records(&world));
    assert!(
        kinds(&world).contains(&"handoff_completed".to_owned()),
        "a handoff ran: {:?}",
        kinds(&world)
    );
}

#[test]
fn a_missing_preamble_fails_closed_with_no_request_or_model_timing() {
    let mut world = build(plain_build(vec![]));
    let turn_id = TurnId("t_test".into());
    let step = world.loop_mut().send(&turn_id).unwrap();
    assert!(
        matches!(step, crate::Step::Ended(_)),
        "the turn fails closed without a preamble"
    );
    assert!(ops(&world, "turn.request").is_empty());
    assert!(ops(&world, "turn.model_call").is_empty());
}

#[test]
fn a_missing_preamble_overflow_moves_results_but_times_neither() {
    use contract::events::ToolCallArgumentsDelta;
    let mut world = build(plain_build(vec![]));
    let action = ActionId("a_result".into());
    world.loop_mut().conversation.push(Input::Assistant {
        model: "fake/model".into(),
        text: "Working.".into(),
        provider_item: None,
    });
    world.loop_mut().conversation.push(Input::ToolCall {
        action_id: action.clone(),
        model: "fake/model".into(),
        call: ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
    });
    world.loop_mut().conversation.push(Input::ToolResult {
        action_id: action,
        text: "Sunny.".into(),
        is_error: false,
        images: Vec::new(),
        pdfs: Vec::new(),
    });
    let _ = ToolCallArgumentsDelta {
        index: 0,
        name: None,
        text: String::new(),
    };
    let turn_id = TurnId("t_test".into());
    let step = world.loop_mut().overflowed(&turn_id, None).unwrap();
    assert!(matches!(step, crate::Step::Ended(_)));
    assert!(ops(&world, "turn.request").is_empty());
    assert!(ops(&world, "turn.model_call").is_empty());
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let artifacts = world.log.dir().join("artifacts");
    let moved = std::fs::read_dir(&artifacts)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(moved, 1, "moved results still write their artifact");
}

#[test]
fn a_budget_refusal_times_the_request_but_runs_no_call() {
    let mut world = build(Build {
        budget: Some(0.0),
        ..plain_build(vec![run_answer("Unused.", 0)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Failed));
    assert_eq!(ops(&world, "turn.request").len(), 1);
    assert!(ops(&world, "turn.model_call").is_empty());
    assert!(
        world.provider.requests.lock().unwrap().is_empty(),
        "no call ran"
    );
}

/// Hooks refusing every model call before it is sent.
struct RefusingHooks;

impl contract::hook::Hooks for RefusingHooks {
    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Refused(contract::hook::Refusal {
                extension: "fiber.test/refuse".into(),
                reason: "The test refused the call.".into(),
            }),
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

/// Hooks answering `before_handoff` with one answer and passing
/// everything else.
struct HandoffHooks {
    answer: Mutex<contract::hook::BeforeHandoffAnswer>,
}

impl HandoffHooks {
    fn noting() -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(contract::hook::BeforeHandoffAnswer {
                outcome: contract::hook::BeforeHandoffOutcome::Note {
                    text: "Hook note.".into(),
                    extension: "fiber.test/notes".into(),
                },
                notices: Vec::new(),
            }),
        })
    }

    fn passing() -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(contract::hook::BeforeHandoffAnswer {
                outcome: contract::hook::BeforeHandoffOutcome::NoNote,
                notices: Vec::new(),
            }),
        })
    }
}

impl contract::hook::Hooks for HandoffHooks {
    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        self.answer.lock().unwrap().clone()
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

// A handoff whose note a hook wrote holds no `turn.request` scope: the
// assembly's span is stamped, held across the hook, then discarded.
#[test]
fn a_hook_note_writes_no_request_scope_for_the_handoff() {
    let mut world = build(Build {
        hooks: Some(HandoffHooks::noting()),
        ..plain_build(vec![run_answer("Done.", 0)])
    });
    arm_request_note_advance(4);
    person_handoff(&world, Some("focus"));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    // The answer's own request is the only `turn.request` line: the
    // handoff's assembly, with its 4 ms advance, wrote none.
    let requests = ops(&world, "turn.request");
    assert_eq!(requests.len(), 1, "{:?}", op_records(&world));
    assert_eq!(requests[0].ms, 0);
    assert_wire(&p7_records(&world));
    assert!(
        kinds(&world).contains(&"handoff_completed".to_owned()),
        "a handoff ran: {:?}",
        kinds(&world)
    );
    assert_eq!(world.provider.requests.lock().unwrap().len(), 1);
}

// With no hook note the handoff's assembly writes exactly one
// `turn.request` line, carrying the exit hook's advance.
#[test]
fn a_model_note_writes_one_request_scope_for_the_handoff() {
    let mut world = build(Build {
        hooks: Some(HandoffHooks::passing()),
        ..plain_build(vec![run_answer("Note.", 0), run_answer("Done.", 0)])
    });
    arm_request_note_advance(4);
    person_handoff(&world, Some("focus"));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let requests = ops(&world, "turn.request");
    assert_eq!(requests.len(), 2, "{:?}", op_records(&world));
    assert_eq!(requests[0].ms, 4, "the note assembly carries its hold");
    assert_eq!(requests[1].ms, 0, "the ordinary send advanced nothing");
    assert_wire(&p7_records(&world));
    assert_eq!(world.provider.requests.lock().unwrap().len(), 2);
}

#[test]
fn a_hook_refusal_times_the_request_but_runs_no_call() {
    let mut world = build(Build {
        hooks: Some(Arc::new(RefusingHooks)),
        ..plain_build(vec![run_answer("Unused.", 0)])
    });
    prompt(&world, "hi");
    let outcome = turn(&mut world);
    assert_eq!(outcome, Some(TurnOutcome::Failed), "{:?}", kinds(&world));
    assert_eq!(ops(&world, "turn.request").len(), 1);
    assert!(ops(&world, "turn.model_call").is_empty());
    assert!(
        world.provider.requests.lock().unwrap().is_empty(),
        "no call ran"
    );
}

#[test]
fn a_retry_constructs_one_request_for_two_attempts() {
    let mut world = build(Build {
        observe: true,
        retry: Some(no_wait()),
        ..plain_build(vec![
            run_failed_answer(ErrorCode::RateLimited, 0),
            run_answer("Recovered.", 0),
            run_answer("Second.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.request").len(), 1);
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 2]);
    prompt(&world, "again");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.request").len(), 2);
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 2, 1]);
}

#[test]
fn an_overflow_note_moves_results_and_times_three_requests() {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    let mut world = build(Build {
        tools: vec![weather],
        retry: Some(no_wait()),
        ..plain_build(vec![
            run_tool_answer("get_weather", json!({}), 0),
            run_failed_answer(ErrorCode::ContextOverflow, 0),
            run_answer("The note.", 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(
        ops(&world, "turn.request").len(),
        4,
        "send, failed send, note and resend: {:?}",
        ops(&world, "turn.request")
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let artifacts = world.log.dir().join("artifacts");
    let moved = std::fs::read_dir(&artifacts)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(moved, 1, "the overflow moved its tool results");
}

// An admitted attempt times its run: one scope, closed on return, holding
// the fake hold. A later advance changes nothing recorded.
#[test]
fn an_admitted_attempt_times_its_run() {
    let mut world = build(Build {
        observe: true,
        ..plain_build(vec![run_answer("Hello.", 7)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let models = ops(&world, "turn.model_call");
    assert_eq!(models.len(), 1, "{:?}", op_records(&world));
    assert_eq!(models[0].ms, 7);
    assert_wire(&p7_records(&world));
    assert_eq!(
        *world.factory_saw.lock().unwrap(),
        vec![(1, 0)],
        "no model line before its run returned"
    );
    world.clock.sleep(Duration::from_secs(60));
    assert_eq!(ops(&world, "turn.model_call")[0].ms, 7);
}

#[test]
fn a_failed_attempt_times_its_hold() {
    let mut world = build(Build {
        retry: Some(crate::Retry {
            attempts: 0,
            initial: Duration::ZERO,
            max: Duration::ZERO,
        }),
        ..plain_build(vec![run_failed_answer(ErrorCode::InvalidRequest, 7)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Failed));
    let models = ops(&world, "turn.model_call");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].ms, 7);
    let kinds = kinds(&world);
    assert!(
        kinds.contains(&"assistant_message_completed".to_owned()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"turn_completed".to_owned()), "{kinds:?}");
}

#[test]
fn a_partial_failure_after_deltas_times_its_hold() {
    let (mut deltas, _) = text_reply("Part.");
    let (arg_deltas, _) = tool_call_reply("get_weather", json!({}));
    deltas.extend(arg_deltas);
    let end = Err(CallError::Failed {
        failure: Failure {
            code: ErrorCode::StreamIncomplete,
            message: "The stream broke.".into(),
            retry_after_ms: None,
            provider: None,
        },
        should_retry: Some(false),
        usage: Box::new(fakes::call_usage("gen_2")),
    });
    let mut world = build(plain_build(vec![Answer::Run {
        deltas,
        end,
        advance_ms: 7,
    }]));
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Failed),
        "{:?}",
        kinds(&world)
    );
    let models = ops(&world, "turn.model_call");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].ms, 7);
    let kinds = kinds(&world);
    assert!(
        kinds.contains(&"assistant_message_completed".to_owned()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"usage_recorded".to_owned()), "{kinds:?}");
    // The partial usage names the generation the deltas streamed under:
    // the failure arrived after streamed fragments, not before any.
    let generations: Vec<String> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .iter()
        .filter(|line| line.kind == "usage_recorded")
        .filter_map(|line| line.payload.get("generation_id"))
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    assert_eq!(generations, vec!["gen_2".to_owned()], "{kinds:?}");
}

// A cancel after live admission is an entered attempt: one scope, even
// though the provider fast-cancels.
#[test]
fn a_cancel_mid_call_times_one_scope() {
    fakes::within(
        "a cancel mid call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let mut world = build(Build {
                probe_state: true,
                ..plain_build(vec![Answer::Hold {
                    cancel_advance_ms: 7,
                }])
            });
            prompt(&world, "hi");
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let outcome = world
                        .looped
                        .as_mut()
                        .expect("the loop is parked in the world")
                        .turn();
                    done_tx.send(outcome).unwrap();
                });
                // The hold parks far out: the test cancels only once the park
                // shows the call waiting on the clock.
                assert!(
                    world.clock.await_parked(
                        world.origin + Duration::from_secs(3600),
                        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    ),
                    "the held call parks before the cancel lands"
                );
                world.cancel.cancel();
                let outcome = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_rx)
                    .expect("the cancelled turn ends");
                assert_eq!(outcome.unwrap(), Some(TurnOutcome::Interrupted));
            });
            let models = ops(&world, "turn.model_call");
            assert_eq!(models.len(), 1, "{:?}", op_records(&world));
            assert_eq!(models[0].ms, 7);
        },
    );
}

#[test]
fn a_cancel_before_the_call_is_sent_runs_no_model_scope() {
    // The factory runs before the subscription: cancelling there lands
    // deterministically before the post-subscription check, which ends
    // the admitted path with no timer through the existing run path.
    let mut world = build(Build {
        observe: true,
        cancel_at_attempt: Some(1),
        ..plain_build(vec![run_answer("Unused.", 7)])
    });
    prompt(&world, "hi");
    assert_eq!(
        world.loop_mut().turn().unwrap(),
        Some(TurnOutcome::Interrupted)
    );
    assert!(
        ops(&world, "turn.model_call").is_empty(),
        "a prevented admission never starts timing"
    );
    assert_eq!(ops(&world, "turn.request").len(), 1);
    assert_eq!(
        world.provider.requests.lock().unwrap().len(),
        1,
        "the call still ran through the existing path"
    );
}

#[test]
fn a_cancel_at_subscription_runs_uncancelled_work_with_no_scope() {
    let mut world = build(plain_build(vec![run_answer("Hello.", 7)]));
    arm_cancel_after_subscribe();
    prompt(&world, "hi");
    assert_eq!(
        world.loop_mut().turn().unwrap(),
        Some(TurnOutcome::Interrupted)
    );
    assert!(
        ops(&world, "turn.model_call").is_empty(),
        "a prevented admission never starts timing"
    );
    assert_eq!(
        world.provider.requests.lock().unwrap().len(),
        1,
        "the call still ran through the existing path"
    );
}

#[test]
fn a_cancel_after_completion_writes_no_second_scope() {
    let mut world = build(plain_build(vec![run_answer("Hello.", 7)]));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.model_call").len(), 1);
    world.cancel.cancel();
    world.clock.sleep(Duration::from_secs(60));
    assert_eq!(ops(&world, "turn.model_call").len(), 1);
}

#[test]
fn a_stream_write_error_closes_one_scope_and_returns_the_error() {
    let mut world = build(plain_build(vec![run_answer("Hello.", 7)]));
    arm_fail_stream_write();
    prompt(&world, "hi");
    let Err(error) = world.loop_mut().turn() else {
        panic!("a failed sink write fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    let models = ops(&world, "turn.model_call");
    assert_eq!(models.len(), 1, "exactly one timer closes");
    assert_eq!(models[0].ms, 7);
    assert_eq!(
        world.provider.requests.lock().unwrap().len(),
        1,
        "no request is duplicated"
    );
}

#[test]
fn a_retry_measures_runs_without_the_backoff() {
    fakes::within(
        "a retry without its backoff",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let mut world = build(Build {
                observe: true,
                probe_state: true,
                retry: Some(crate::Retry {
                    attempts: 3,
                    initial: Duration::from_secs(2),
                    max: Duration::from_secs(60),
                }),
                ..plain_build(vec![
                    run_failed_answer(ErrorCode::RateLimited, 7),
                    run_answer("Recovered.", 7),
                ])
            });
            prompt(&world, "hi");
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let outcome = world
                        .looped
                        .as_mut()
                        .expect("the loop is parked in the world")
                        .turn();
                    done_tx.send(outcome).unwrap();
                });
                // The backoff parks two seconds past the failed run's 7 ms: the
                // test advances only once the wait shows on the clock.
                let backoff_until =
                    world.origin + Duration::from_millis(7) + Duration::from_secs(2);
                assert!(
                    world.clock.await_parked(
                        backoff_until,
                        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    ),
                    "the retry parks its backoff before the advance"
                );
                world.clock.advance_when_parked(
                    Some(backoff_until),
                    None,
                    Duration::from_secs(2),
                    &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                );
                let outcome = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_rx)
                    .expect("the retried turn ends");
                assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
            });
            let models = ops(&world, "turn.model_call");
            assert_eq!(models.len(), 2, "{:?}", op_records(&world));
            assert_eq!(models[0].ms, 7);
            assert_eq!(models[1].ms, 7, "the backoff stays outside both scopes");
            assert_eq!(ops(&world, "turn.request").len(), 1);
            assert_eq!(*world.attempts.lock().unwrap(), vec![1, 2]);
        },
    );
}

#[test]
fn a_cancel_during_the_backoff_runs_nothing_more() {
    fakes::within(
        "a cancel during the backoff",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let mut world = build(Build {
                retry: Some(crate::Retry {
                    attempts: 3,
                    initial: Duration::from_secs(2),
                    max: Duration::from_secs(60),
                }),
                ..plain_build(vec![
                    run_failed_answer(ErrorCode::RateLimited, 7),
                    run_answer("Unused.", 7),
                ])
            });
            prompt(&world, "hi");
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let outcome = world
                        .looped
                        .as_mut()
                        .expect("the loop is parked in the world")
                        .turn();
                    done_tx.send(outcome).unwrap();
                });
                let backoff_until =
                    world.origin + Duration::from_millis(7) + Duration::from_secs(2);
                assert!(
                    world.clock.await_parked(
                        backoff_until,
                        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    ),
                    "the retry parks its backoff before the cancel"
                );
                world.cancel.cancel();
                let outcome = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_rx)
                    .expect("the cancelled turn ends");
                assert_eq!(outcome.unwrap(), Some(TurnOutcome::Interrupted));
            });
            assert_eq!(ops(&world, "turn.model_call").len(), 1);
            assert_eq!(
                world.provider.requests.lock().unwrap().len(),
                1,
                "no second run started"
            );
        },
    );
}

#[test]
fn an_exhausted_retry_adds_no_scope() {
    let mut world = build(Build {
        retry: Some(crate::Retry {
            attempts: 0,
            initial: Duration::ZERO,
            max: Duration::ZERO,
        }),
        ..plain_build(vec![
            run_failed_answer(ErrorCode::RateLimited, 7),
            run_answer("Unused.", 7),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Failed));
    assert_eq!(ops(&world, "turn.model_call").len(), 1);
    assert_eq!(world.provider.requests.lock().unwrap().len(), 1);
}

/// A tool whose calls execute, so the reviewer judges them.
struct ExecTool;

impl Tool for ExecTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "shell".into(),
            description: "Runs.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "ran it".into(),
            }],
            ..Output::default()
        }
    }
}

#[test]
fn a_reviewer_judgement_runs_but_adds_no_turn_scope() {
    let mut world = build(Build {
        tools: vec![Arc::new(ExecTool)],
        reviewer_script: Some(vec![run_answer("allow", 0)]),
        ..plain_build(vec![
            run_tool_answer("shell", json!({}), 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "go");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    let reviewer = world.reviewer.clone().expect("a reviewer provider");
    assert_eq!(
        reviewer.requests.lock().unwrap().len(),
        1,
        "the judgement really ran"
    );
    assert_eq!(
        ops(&world, "turn.model_call").len(),
        2,
        "only the turn's own attempts are timed: {:?}",
        op_records(&world)
    );
    assert_eq!(ops(&world, "turn.request").len(), 2);
}

#[test]
fn a_warming_refresh_runs_but_adds_no_turn_scope() {
    let mut world = build(Build {
        observe: true,
        warm: Some(1),
        ..plain_build(vec![run_answer("Warm.", 0), run_answer("Warmer.", 0)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.model_call").len(), 1);
    assert_eq!(ops(&world, "turn.request").len(), 1);
    let start = world.clock.now();
    let stop = world
        .loop_mut()
        .warm_stop(start)
        .expect("the session warms");
    // No code under test waits on the clock here; the refresh compares its due point only (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    world.clock.advance(Duration::from_secs(3575));
    let refreshed = world.loop_mut().refresh(stop).unwrap();
    assert!(matches!(refreshed, crate::warm::Refreshed::Sent));
    assert_eq!(
        ops(&world, "turn.model_call").len(),
        1,
        "the refresh adds no model scope"
    );
    assert_eq!(ops(&world, "turn.request").len(), 1);
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 1]);
}

#[test]
fn a_bare_switch_keeps_timing_the_ordinary_call() {
    let rig = switch_rig(vec![run_answer("Switched.", 0)]);
    let mut world = build(Build {
        observe: true,
        switch: Some(rig),
        ..plain_build(vec![run_answer("One.", 0)])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "turn.model_call").len(), 1);
    switch_model(&mut world);
    prompt(&world, "again");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "turn.model_call").len(),
        2,
        "the switched route is still timed"
    );
    assert_eq!(
        *world.attempts.lock().unwrap(),
        vec![1],
        "only the pre-switch call used the old factory"
    );
}

// One shared runner owns every handoff end to end: requested sources nest
// their note request and model scopes inside it, tool sources none.
fn handoff_settings() -> crate::HandoffSettings {
    crate::HandoffSettings {
        enabled: true,
        tokens: 1_000,
        window_fraction: 0.7,
        nudge: false,
    }
}

fn run_tokens_tool_answer(
    name: &str,
    arguments: Value,
    input: u64,
    output: u64,
    advance_ms: u64,
) -> Answer {
    let (deltas, end) = tool_call_reply(name, arguments);
    let mut reply = end.unwrap();
    reply.tokens.input = input;
    reply.tokens.output = output;
    Answer::Run {
        deltas,
        end: Ok(reply),
        advance_ms,
    }
}

fn output_limit_answer(text: &str, advance_ms: u64) -> Answer {
    let (deltas, end) = text_reply(text);
    let mut reply = end.unwrap();
    reply.finish = contract::provider::Finish::OutputLimit;
    Answer::Run {
        deltas,
        end: Ok(reply),
        advance_ms,
    }
}

fn auto_world(script: Vec<Answer>) -> World {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    build(Build {
        tools: vec![weather],
        handoff: Some(handoff_settings()),
        retry: Some(no_wait()),
        ..plain_build(script)
    })
}

#[test]
fn an_automatic_handoff_nests_its_note_scopes() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("The note.", 0),
        run_answer("Done.", 0),
    ]);
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    let handoffs = ops(&world, "turn.handoff");
    assert_eq!(handoffs.len(), 1, "{:?}", op_records(&world));
    assert_wire(&p7_records(&world));
    assert!(
        world.loop_mut().handoff.step_ran,
        "a requested handoff marks the step as run"
    );
    assert_eq!(ops(&world, "turn.request").len(), 3);
    assert_eq!(ops(&world, "turn.model_call").len(), 3);
    let kinds = kinds(&world);
    for kind in ["handoff_started", "handoff_completed", "opening_message"] {
        assert!(kinds.contains(&kind.to_owned()), "{kinds:?}");
    }
    let started = kinds
        .iter()
        .position(|kind| kind == "handoff_started")
        .unwrap();
    let completed = kinds
        .iter()
        .position(|kind| kind == "handoff_completed")
        .unwrap();
    let opening = kinds
        .iter()
        .rposition(|kind| kind == "opening_message")
        .unwrap();
    assert!(started < completed && completed < opening, "{kinds:?}");
}

#[test]
fn a_person_handoff_requests_its_note() {
    let mut world = build(plain_build(vec![
        run_answer("The note.", 0),
        run_answer("Done.", 0),
    ]));
    person_handoff(&world, Some("focus"));
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    assert_eq!(ops(&world, "turn.request").len(), 2);
    assert_eq!(ops(&world, "turn.model_call").len(), 2);
    assert!(
        world.loop_mut().handoff.step_ran,
        "a person handoff ran this step"
    );
    assert!(kinds(&world).contains(&"handoff_started".to_owned()));
}

#[test]
fn an_overflow_handoff_requests_its_note() {
    let mut world = build(Build {
        retry: Some(no_wait()),
        ..plain_build(vec![
            run_failed_answer(ErrorCode::ContextOverflow, 0),
            run_answer("The note.", 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    assert_eq!(ops(&world, "turn.request").len(), 3);
    let kinds = kinds(&world);
    assert!(kinds.contains(&"handoff_started".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"handoff_completed".to_owned()), "{kinds:?}");
}

#[test]
fn a_tool_handoff_makes_no_note_request() {
    let noting: Arc<dyn Tool> = Arc::new(ReadsTool::noting("get_weather", "abcd", "Continue."));
    let mut world = build(Build {
        tools: vec![noting],
        ..plain_build(vec![
            run_tool_answer("get_weather", json!({}), 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    assert!(
        !world.loop_mut().handoff.step_ran,
        "a tool handoff leaves the step flag unset"
    );
    assert!(
        !kinds(&world).contains(&"handoff_started".to_owned()),
        "a tool source writes no started: {:?}",
        kinds(&world)
    );
    assert!(kinds(&world).contains(&"handoff_completed".to_owned()));
    assert_eq!(
        world.provider.requests.lock().unwrap().len(),
        2,
        "no note request was built"
    );
}

#[test]
fn two_tool_notes_join_one_handoff_in_call_order() {
    let first: Arc<dyn Tool> = Arc::new(ReadsTool::noting("get_weather", "abcd", "First."));
    let second: Arc<dyn Tool> = Arc::new(ReadsTool::noting("get_humidity", "efgh", "Second."));
    let mut end = fakes::reply("");
    for (name, arguments) in [("get_weather", json!({})), ("get_humidity", json!({}))] {
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: name.into(),
            arguments: arguments.clone(),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }));
    }
    let mut world = build(Build {
        tools: vec![first, second],
        ..plain_build(vec![
            Answer::Run {
                deltas: Vec::new(),
                end: Ok(end),
                advance_ms: 0,
            },
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let completed: Vec<Value> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "handoff_completed")
        .map(|line| Value::Object(line.payload.clone()))
        .collect();
    assert_eq!(completed.len(), 1, "one joined handoff");
    let note = completed[0]["note"].clone();
    assert_eq!(note.as_array().map(Vec::len), Some(2), "{note}");
}

#[test]
fn no_trigger_writes_no_handoff_timing() {
    let mut world = build(plain_build(vec![run_answer("Hello.", 0)]));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert!(ops(&world, "turn.handoff").is_empty());
}

#[test]
fn a_disabled_handoff_writes_no_handoff_timing() {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    let mut world = build(Build {
        tools: vec![weather],
        handoff: Some(crate::HandoffSettings {
            enabled: false,
            ..Default::default()
        }),
        ..plain_build(vec![
            run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert!(ops(&world, "turn.handoff").is_empty());
}

#[test]
fn a_tool_result_without_a_note_writes_no_handoff_timing() {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    let mut world = build(Build {
        tools: vec![weather],
        ..plain_build(vec![
            run_tool_answer("get_weather", json!({}), 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert!(ops(&world, "turn.handoff").is_empty());
}

#[test]
fn an_empty_tool_note_still_completes_one_handoff() {
    let noting: Arc<dyn Tool> = Arc::new(ReadsTool::noting("get_weather", "abcd", ""));
    let mut world = build(Build {
        tools: vec![noting],
        ..plain_build(vec![
            run_tool_answer("get_weather", json!({}), 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
}

#[test]
fn a_second_overflow_in_one_step_writes_no_second_handoff() {
    let mut world = build(Build {
        retry: Some(no_wait()),
        ..plain_build(vec![
            run_failed_answer(ErrorCode::ContextOverflow, 0),
            run_answer("The note.", 0),
            run_failed_answer(ErrorCode::ContextOverflow, 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Failed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
}

#[test]
fn a_refused_note_fails_the_handoff_and_blocks() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_failed_answer(ErrorCode::InvalidRequest, 0),
        run_answer("Done.", 0),
    ]);
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let completed: Vec<Value> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "handoff_completed")
        .map(|line| Value::Object(line.payload.clone()))
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["outcome"], "failed");
}

#[test]
fn an_exhausted_note_retry_fails_the_handoff() {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    let mut world = build(Build {
        tools: vec![weather],
        handoff: Some(handoff_settings()),
        retry: Some(crate::Retry {
            attempts: 1,
            initial: Duration::ZERO,
            max: Duration::ZERO,
        }),
        ..plain_build(vec![
            run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
            run_failed_answer(ErrorCode::RateLimited, 0),
            run_failed_answer(ErrorCode::RateLimited, 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    assert_eq!(ops(&world, "turn.request").len(), 3);
}

#[test]
fn an_output_limited_note_fails_the_handoff() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        output_limit_answer("Cut off.", 0),
        run_answer("Done.", 0),
    ]);
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
}

#[test]
fn an_empty_note_fails_the_handoff() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("", 0),
        run_answer("Done.", 0),
    ]);
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
}

#[test]
fn a_cancel_before_note_admission_times_handoff_but_no_note_model() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("The note.", 7),
    ]);
    let parked = world
        .looped
        .take()
        .expect("the loop is parked in the world");
    world.looped = Some(parked.observed({
        let provider = Arc::clone(&world.provider);
        let saw = Arc::clone(&world.factory_saw);
        let diag_path = world.home.path().join("logs").join("session-s_test.log");
        let cancel = Arc::clone(&world.cancel);
        let attempts = Arc::clone(&world.attempts);
        let observe: Observe = Arc::new(move |request: &ModelRequest, attempt: NonZeroU32| {
            attempts.lock().unwrap().push(attempt.get());
            saw.lock().unwrap().push((
                count_op(&diag_path, "turn.request"),
                count_op(&diag_path, "turn.model_call"),
            ));
            // The note is the second factory entry: cancel it before its
            // subscription, after its request scope closed.
            if attempts.lock().unwrap().len() == 2 {
                cancel.cancel();
            }
            provider.call(request)
        });
        Some(observe)
    }));
    prompt(&world, "hi");
    assert_eq!(
        world.loop_mut().turn().unwrap(),
        Some(TurnOutcome::Interrupted)
    );
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    assert_eq!(
        ops(&world, "turn.model_call").len(),
        1,
        "only the turn's call was admitted"
    );
    assert_eq!(
        *world.factory_saw.lock().unwrap(),
        vec![(1, 0), (2, 1)],
        "the note request closed before its cancelled admission"
    );
}

#[test]
fn a_cancel_mid_note_times_handoff_and_the_entered_note() {
    fakes::within(
        "a cancel mid note",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let mut world = auto_world(vec![
                run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
                Answer::Hold {
                    cancel_advance_ms: 7,
                },
            ]);
            prompt(&world, "hi");
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let outcome = world
                        .looped
                        .as_mut()
                        .expect("the loop is parked in the world")
                        .turn();
                    done_tx.send(outcome).unwrap();
                });
                assert!(
                    world.clock.await_parked(
                        world.origin + Duration::from_secs(3600),
                        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    ),
                    "the note parks before the cancel lands"
                );
                world.cancel.cancel();
                let outcome = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_rx)
                    .expect("the cancelled turn ends");
                assert_eq!(outcome.unwrap(), Some(TurnOutcome::Interrupted));
            });
            assert_eq!(ops(&world, "turn.handoff").len(), 1);
            let models = ops(&world, "turn.model_call");
            assert_eq!(models.len(), 2, "{:?}", op_records(&world));
            assert_eq!(models[1].ms, 7);
        },
    );
}

#[test]
fn a_failed_handoff_start_writes_one_scope_and_no_started() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("The note.", 0),
    ]);
    arm_fail_handoff_started();
    prompt(&world, "hi");
    let Err(error) = world.loop_mut().turn() else {
        panic!("a failed record fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let kinds = kinds(&world);
    assert!(!kinds.contains(&"handoff_started".to_owned()), "{kinds:?}");
    assert!(
        !kinds.contains(&"handoff_completed".to_owned()),
        "{kinds:?}"
    );
}

#[test]
fn a_failed_handoff_completion_writes_one_scope_and_no_fabrication() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("The note.", 0),
    ]);
    arm_fail_handoff_completed();
    prompt(&world, "hi");
    let Err(error) = world.loop_mut().turn() else {
        panic!("a failed record fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let kinds = kinds(&world);
    assert!(kinds.contains(&"handoff_started".to_owned()), "{kinds:?}");
    assert!(
        !kinds.contains(&"handoff_completed".to_owned()),
        "{kinds:?}"
    );
}

#[test]
fn a_failed_restart_writes_one_scope_and_no_second_opening() {
    let mut world = auto_world(vec![
        run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
        run_answer("The note.", 0),
    ]);
    arm_fail_restarted();
    prompt(&world, "hi");
    let Err(error) = world.loop_mut().turn() else {
        panic!("a failed restart fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(ops(&world, "turn.handoff").len(), 1);
    let kinds = kinds(&world);
    assert!(kinds.contains(&"handoff_completed".to_owned()), "{kinds:?}");
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "opening_message")
            .count(),
        1,
        "no restart opening was fabricated: {kinds:?}"
    );
}

#[test]
fn a_handoff_measures_its_note_and_restart_end_to_end() {
    let weather: Arc<dyn Tool> = Arc::new(ReadsTool::reads("get_weather", "abcd"));
    let mut world = build(Build {
        tools: vec![weather],
        handoff: Some(handoff_settings()),
        retry: Some(no_wait()),
        // The restart holds 5 ms through the handoff's own forget hook.
        forget_advance_ms: Some(5),
        ..plain_build(vec![
            run_tokens_tool_answer("get_weather", json!({}), 1_000, 3, 0),
            run_answer("The note.", 3),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    let handoffs = ops(&world, "turn.handoff");
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].ms, 8, "3 ms of note and 5 ms of restart");
    let models = ops(&world, "turn.model_call");
    assert_eq!(models.len(), 3);
    assert_eq!(models[1].ms, 3, "the note scope holds only its run");
    assert_wire(&p7_records(&world));
}

#[test]
fn a_tool_handoff_measures_its_restart() {
    let noting: Arc<dyn Tool> = Arc::new(ReadsTool::noting("get_weather", "abcd", "Continue."));
    let mut world = build(Build {
        tools: vec![noting],
        forget_advance_ms: Some(5),
        ..plain_build(vec![
            run_tool_answer("get_weather", json!({}), 0),
            run_answer("Done.", 0),
        ])
    });
    prompt(&world, "hi");
    assert_eq!(
        turn(&mut world),
        Some(TurnOutcome::Completed),
        "{:?}",
        kinds(&world)
    );
    let handoffs = ops(&world, "turn.handoff");
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].ms, 5, "the tool restart contributes its hold");
}

#[test]
fn a_tool_handoff_sets_the_step_flag_only_when_it_does_not_complete() {
    let mut world = build(plain_build(vec![]));
    let turn_id = TurnId("t_test".into());
    let seed = |world: &mut World| {
        let action = ActionId("a_note".into());
        let carry = &mut world.loop_mut().handoff.carry;
        carry.call_requested(
            &action,
            &Input::ToolCall {
                action_id: action.clone(),
                model: "fake/model".into(),
                call: ToolCallRequested {
                    name: "get_weather".into(),
                    arguments: json!({}),
                    provider_id: None,
                    repair: None,
                    ran_by: None,
                    provider_item: None,
                },
            },
        );
        carry.call_completed(
            &action,
            &Input::ToolResult {
                action_id: action.clone(),
                text: "Sunny.".into(),
                is_error: false,
                images: Vec::new(),
                pdfs: Vec::new(),
            },
            None,
            None,
        );
        carry.step.last_mut().unwrap().note = Some("Continue.".into());
    };
    seed(&mut world);
    assert!(
        world.loop_mut().handoff_from_tools(&turn_id).unwrap(),
        "a completed tool handoff restarts the context"
    );
    assert!(
        !world.loop_mut().handoff.step_ran,
        "a completed tool handoff leaves the step flag unset"
    );
    seed(&mut world);
    assert!(world.cancel.arm(), "a turn must be armed to be cancelled");
    assert!(world.cancel.cancel());
    assert!(
        !world.loop_mut().handoff_from_tools(&turn_id).unwrap(),
        "a cancelled tool handoff does not complete"
    );
    assert!(
        world.loop_mut().handoff.step_ran,
        "a tool handoff that does not complete sets the step flag"
    );
}
