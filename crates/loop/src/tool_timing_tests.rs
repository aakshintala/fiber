//! Part 8 tool phase timers (`docs/state.md`, "Diagnostic logs"): one
//! `op_timing` line per entered tool phase through the one installed timer.
//!
//! Time comes from the injected clock: synchronous fake work inside the
//! rules, the provider, the hooks and the tool sets each duration, and a
//! test advances the fake clock during a person's wait only after the wait
//! parks on it. Every receive, join and log tap carries the shared
//! must-succeed bound (`fakes::MUST_SUCCEED_WITHIN`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    AskStep, Decision, Event, FiberExited, Interaction, Notice, OnFork, PermissionRequested,
    RuleScope, StandingRule, ToolCallRequested, TurnOutcome,
};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeToolAnswer, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, HookFailure, Hooks, Refusal, StateLine,
};
use contract::inbox::{Ack, Delivery};
use contract::provider::{
    CallError, Delta, ModelCall, ModelRequest, Provider, Reply, ReplyAction, ToolDefinition,
};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender};
use contract::tool::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, CommandId, ErrorCode, RequestId, SessionId, TurnId};
use fakes::clock::FakeClock;
use fakes::{Deadline, MUST_SUCCEED_WITHIN};
use log::Log;
use serde_json::{Map, Value, json};
use support::lock;

use crate::{BlockLimits, Error, Loop, Model, Reviewer};

/// The idle delay a suspending turn waits out.
const IDLE: Duration = Duration::from_secs(60);

/// Fails the next `permission_resolved` write once with `Error::Log`.
static FAIL_DECISION_WRITE: Mutex<bool> = Mutex::new(false);

/// Arms the one-shot decision-write fault the error test drives.
pub(crate) fn arm_fail_decision_write() {
    *lock(&FAIL_DECISION_WRITE) = true;
}

/// Whether the next `permission_resolved` write fails. Consumed once.
pub(crate) fn take_fail_decision_write() -> bool {
    std::mem::replace(&mut lock(&FAIL_DECISION_WRITE), false)
}

/// The `Error::Log` a faulted decision write returns.
pub(crate) fn log_fault() -> Error {
    Error::Log(log::Error::Io {
        path: std::path::PathBuf::from("s_test/events.jsonl"),
        source: std::io::Error::other("fiber-test write fault"),
    })
}

/// Advances the clock by `ms` at each `after_tool` exit boundary, once
/// per arming: later cap and artifact work the timer must already have
/// closed before.
static AFTER_DISPATCH_ADVANCE_MS: Mutex<Option<u64>> = Mutex::new(None);

/// Arms the one-shot post-dispatch clock advance the cap-boundary test drives.
pub(crate) fn arm_after_dispatch_advance(ms: u64) {
    *lock(&AFTER_DISPATCH_ADVANCE_MS) = Some(ms);
}

/// Runs at an `after_tool` dispatch's exit, inside the timed-no-longer
/// scope. Advances the armed clock, proving later work stays outside.
pub(crate) fn after_dispatch_exit(clock: &dyn Clock) {
    if let Some(ms) = lock(&AFTER_DISPATCH_ADVANCE_MS).take() {
        clock.sleep(Duration::from_millis(ms));
    }
}
/// One scripted provider answer: the reply, after synchronous fake work of
/// `advance_ms`, all inside the timed model call, or a hold until the
/// test cancels the call.
enum Answer {
    Run {
        deltas: Vec<Delta>,
        end: Result<Reply, CallError>,
        advance_ms: u64,
    },
    Hold,
    /// Parks the call until the test sends its answer: the release queue
    /// hands a sender per held call to the test in arrival order.
    Held,
}

fn text_reply(text: &str) -> (Vec<Delta>, Result<Reply, CallError>) {
    let end = fakes::reply(text);
    (Vec::new(), Ok(end))
}

fn tool_reply(name: &str, arguments: Value) -> (Vec<Delta>, Result<Reply, CallError>) {
    let mut end = fakes::reply("");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments: arguments.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    (Vec::new(), Ok(end))
}

fn text_answer(text: &str, advance_ms: u64) -> Answer {
    let (deltas, end) = text_reply(text);
    Answer::Run {
        deltas,
        end,
        advance_ms,
    }
}

fn tool_answer(name: &str, arguments: Value, advance_ms: u64) -> Answer {
    let (deltas, end) = tool_reply(name, arguments);
    Answer::Run {
        deltas,
        end,
        advance_ms,
    }
}

/// A model answer calling two tools in order, then nothing else.
/// A model answer calling two tools in order, then nothing else.
fn tool_answer_two(first: (&str, Value), second: (&str, Value), advance_ms: u64) -> Answer {
    tool_answer_many(&[first, second], advance_ms)
}

/// A model answer calling three tools in order, then nothing else.
fn tool_answer_three(
    first: (&str, Value),
    second: (&str, Value),
    third: (&str, Value),
    advance_ms: u64,
) -> Answer {
    tool_answer_many(&[first, second, third], advance_ms)
}

/// A model answer calling each tool in order, then nothing else.
fn tool_answer_many(calls: &[(&str, Value)], advance_ms: u64) -> Answer {
    let mut end = fakes::reply("");
    let mut deltas = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: (*name).into(),
            arguments: (*arguments).clone(),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }));
        deltas.push(Delta::ToolCallArguments(
            contract::events::ToolCallArgumentsDelta {
                index: u32::try_from(index).expect("the reply fits"),
                name: Some((*name).into()),
                text: arguments.to_string(),
            },
        ));
    }
    Answer::Run {
        deltas,
        end: Ok(end),
        advance_ms,
    }
}

fn error_answer(code: ErrorCode, advance_ms: u64) -> Answer {
    Answer::Run {
        deltas: Vec::new(),
        end: Err(CallError::Failed {
            failure: Failure {
                code,
                message: "The reviewer call failed.".into(),
                provider: None,
                retry_after_ms: None,
            },
            should_retry: Some(false),
            usage: Box::new(fakes::unnamed_usage()),
        }),
        advance_ms,
    }
}

/// One scripted reviewer answer, clonable so a resume rebuilds it.
#[derive(Clone)]
enum ReviewReply {
    Text {
        text: String,
        advance_ms: u64,
    },
    Error {
        code: ErrorCode,
        advance_ms: u64,
    },
    Hold,
    /// Parks the call until the test sends its answer through the queued
    /// release, so a batch test holds every first stage in flight and
    /// releases them in whatever order it chooses.
    HoldAnswer,
}

fn review_text(text: &str, advance_ms: u64) -> ReviewReply {
    ReviewReply::Text {
        text: text.into(),
        advance_ms,
    }
}

fn review_error(code: ErrorCode, advance_ms: u64) -> ReviewReply {
    ReviewReply::Error { code, advance_ms }
}

fn review_answers(script: &[ReviewReply]) -> Vec<Answer> {
    script
        .iter()
        .map(|reply| match reply {
            ReviewReply::Text { text, advance_ms } => {
                let (deltas, end) = text_reply(text);
                Answer::Run {
                    deltas,
                    end,
                    advance_ms: *advance_ms,
                }
            }
            ReviewReply::Error { code, advance_ms } => error_answer(code.clone(), *advance_ms),
            ReviewReply::Hold => Answer::Hold,
            ReviewReply::HoldAnswer => Answer::Held,
        })
        .collect()
}

/// A provider on the model seam whose answers run synchronous fake work.
struct ScriptProvider {
    clock: Arc<FakeClock>,
    script: Mutex<VecDeque<Answer>>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    hold_pause: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    calls: AtomicUsize,
    /// Every request received, in arrival order.
    requests: Mutex<Vec<ModelRequest>>,
    /// Cloned into each held call: every hold signals entry, so the test
    /// counts them in flight.
    hold_entered: Mutex<Option<mpsc::Sender<()>>>,
    /// One sender per held call, in arrival order: the test answers each
    /// hold through these, in whatever order it chooses.
    hold_releases: Arc<Mutex<VecDeque<mpsc::Sender<Answer>>>>,
    /// One-shot pause for the next held call: the call sends `parked`
    /// once it reaches the pause, then blocks on `gate`. Consumed once;
    /// later calls run as today.
    held_pause: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl ScriptProvider {
    fn called(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Every request the provider received, in arrival order.
    fn requests(&self) -> Vec<ModelRequest> {
        lock(&self.requests).clone()
    }

    /// Holds the next call ahead of its hold: the call blocks on
    /// `release` before it stores its own release, and sends
    /// `cancelled` once its cancel has been handled. Consumed once;
    /// later calls run as today.
    fn pause_hold(&self, cancelled: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *lock(&self.hold_pause) = Some((cancelled, release));
    }

    /// Parks the next held call between its release queue and its entry
    /// signal: the call sends `parked` once it reaches the pause, then
    /// blocks on `gate`. Consumed once; later calls run as today.
    fn pause_held(&self, parked: mpsc::Sender<()>, gate: mpsc::Receiver<()>) {
        *lock(&self.held_pause) = Some((parked, gate));
    }
}

impl Provider for ScriptProvider {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        lock(&self.requests).push(request.clone());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = lock(&self.entered).take() {
            let _sent = entered.send(());
        }
        let mut signal = None;
        let mut pause = None;
        if let Some((cancelled, release)) = lock(&self.hold_pause).take() {
            signal = Some(cancelled);
            pause = Some(release);
        }
        let answer = lock(&self.script)
            .pop_front()
            .expect("the script has a reply");
        let hold = matches!(answer, Answer::Held).then(|| {
            let entered = lock(&self.hold_entered)
                .clone()
                .expect("a held call needs the test's entry signal");
            let pause = Mutex::new(lock(&self.held_pause).take());
            (entered, Arc::clone(&self.hold_releases), pause)
        });
        Box::new(ScriptCall {
            clock: Arc::clone(&self.clock),
            answer,
            release: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            signal,
            pause: Mutex::new(pause),
            hold,
        })
    }
}

/// Who wakes a held scripted call: its entry signal, the queue of
/// per-call answer senders the test drains in arrival order, and the
/// test's one-shot pause between the queue and the signal.
type HoldRelease = (
    mpsc::Sender<()>,
    Arc<Mutex<VecDeque<mpsc::Sender<Answer>>>>,
    Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
);

struct ScriptCall {
    clock: Arc<FakeClock>,
    answer: Answer,
    release: Mutex<Option<mpsc::Sender<()>>>,
    /// Set by every cancel and read after the release store, so a
    /// cancel that lands before the store is never dropped.
    cancelled: AtomicBool,
    /// Sends once the cancel has been handled, on every cancel route.
    signal: Option<mpsc::Sender<()>>,
    /// Holds the run ahead of its release store; taken out and dropped
    /// before the block, so the cancel never meets this lock.
    pause: Mutex<Option<mpsc::Receiver<()>>>,
    /// Set for held calls: queues the release, signals entry, then parks
    /// for the test's answer through the queued release.
    hold: Option<HoldRelease>,
}

impl ModelCall for ScriptCall {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        if self.hold.is_some() {
            return self.run_released(sink);
        }
        match &self.answer {
            Answer::Run {
                deltas,
                end,
                advance_ms,
            } => run_answer(&self.clock, deltas, end, *advance_ms, sink),
            // Parks until the test cancels the call: the release is the
            // signal the code under test is waiting on the cancellation.
            Answer::Hold => {
                // A wired pause holds the call ahead of its release
                // store: the test releases it only after the cancel
                // has been handled. The channel is the test's own, so
                // the block carries no deadline; a dropped sender
                // releases it too.
                let pause = lock(&self.pause).take();
                if let Some(pause) = pause {
                    let _released = pause.recv();
                }
                let (tx, rx) = mpsc::channel();
                *lock(&self.release) = Some(tx);
                // A cancel that landed before the store is seen here;
                // one after it is seen on the stored release; one in
                // between is seen on both, and the extra send is harmless.
                if self.cancelled.load(Ordering::SeqCst) {
                    return Err(CallError::Cancelled {
                        usage: Box::new(fakes::unnamed_usage()),
                    });
                }
                Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the test cancels the hold");
                Err(CallError::Cancelled {
                    usage: Box::new(fakes::unnamed_usage()),
                })
            }
            Answer::Held => {
                unreachable!("a held call parks for the test's answer before matching");
            }
        }
    }

    fn cancel(&self) {
        // First, so a hold storing its release next still sees it.
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(release) = lock(&self.release).take() {
            let _sent = release.send(());
        }
        // After the cancel is handled, on every route; never touches
        // the pause, so a cancel during the blocked run cannot deadlock.
        if let Some(signal) = &self.signal {
            let _sent = signal.send(());
        }
    }
}

impl ScriptCall {
    /// Parks a held call until the test sends its answer: queues the release,
    /// then signals entry, then runs the test's answer as the reply.
    /// A wired pause parks the call between the queue and the signal: the
    /// test owns both ends, so the block carries no deadline, and a dropped
    /// gate releases it too. A test parked there observes the queue exactly
    /// as the old signal-then-queue order left it: empty.
    /// A cancel still ends the hold cancelled, as with `Answer::Hold`.
    /// The answer block carries no deadline: the channel is the test's own — the
    /// test pops the sender and sends the answer, and dropping it releases
    /// the hold too. The test's own deadline on the entry signal and the
    /// turn's end names a hang.
    #[allow(clippy::result_large_err, reason = "test helper mirroring the seam")]
    fn run_released(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let (entered, releases, pause) = self
            .hold
            .as_ref()
            .expect("a held call is armed by the test");
        let (tx, rx) = mpsc::channel();
        lock(releases).push_back(tx);
        // A wired pause parks the call after it queues its release and
        // before it signals entry: taken out before the block, so the
        // test's drop of the gate releases it too.
        if let Some((parked, gate)) = lock(pause).take() {
            let _sent = parked.send(());
            let _released = gate.recv();
        }
        let _sent = entered.send(());
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(CallError::Cancelled {
                usage: Box::new(fakes::unnamed_usage()),
            });
        }
        let answer = rx.recv().expect("the test releases the hold");
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(CallError::Cancelled {
                usage: Box::new(fakes::unnamed_usage()),
            });
        }
        match &answer {
            Answer::Run {
                deltas,
                end,
                advance_ms,
            } => run_answer(&self.clock, deltas, end, *advance_ms, sink),
            Answer::Hold | Answer::Held => {
                panic!("a hold releases with a run answer, never another hold")
            }
        }
    }
}

/// Runs one scripted answer's fake work: its deltas, its clock advance,
/// then its end. `run` and released holds share it, so a released hold
/// replays exactly what the session would have run for that answer.
#[allow(
    clippy::result_large_err,
    reason = "the scripted reply reuses the provider's error type"
)]
fn run_answer(
    clock: &FakeClock,
    deltas: &[Delta],
    end: &Result<Reply, CallError>,
    advance_ms: u64,
    sink: &mut dyn FnMut(Delta),
) -> Result<Reply, CallError> {
    for delta in deltas {
        sink(delta.clone());
    }
    clock.sleep(Duration::from_millis(advance_ms));
    clone_end(end)
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

/// How a probe tool spills its output, fabricated in the workspace.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpillMode {
    /// No spill: ordinary output.
    None,
    /// A kept artifact file the hook sees.
    Persisted,
    /// No kept file and no partial file left behind.
    Unsaved,
    /// No kept file and a partial file the loop must fail to remove.
    UnsavedBadLeft,
}

/// What a probe tool asks its inner runner for, if anything.
#[derive(Clone, Default)]
enum InnerPlan {
    /// No inner call: an ordinary run.
    #[default]
    None,
    /// One synchronous inner call, propagating its answer.
    Call { name: String, args: Value },
    /// One detached inner call, reported off-thread.
    Forget { name: String, args: Value },
}

/// Wakes a watched leaf's cancel wait: the loop's cancel signal owns
/// no channel the test could hold, so the wake carries one.
struct CancelWake(mpsc::Sender<()>);

impl Wake for CancelWake {
    fn wake(&self) {
        let _sent = self.0.send(());
    }
}

/// A registered tool with scripted effects, schema and synchronous work.
struct ProbeTool {
    name: String,
    schema: Value,
    effects: Result<Effects, EffectsError>,
    text: String,
    run_ms: u64,
    ask_confirm: bool,
    boom_on: Option<String>,
    spill: Mutex<SpillMode>,
    jobs: Mutex<Vec<contract::jobs::JobRecord>>,
    inner_plan: Mutex<InnerPlan>,
    inner_report: Mutex<Option<mpsc::Sender<String>>>,
    inner_errors: Mutex<Vec<String>>,
    forget_gate: Mutex<Option<mpsc::Receiver<()>>>,
    run_entered: Mutex<Option<mpsc::Sender<()>>>,
    run_gate: Mutex<Option<mpsc::Receiver<()>>>,
    run_rehold: Mutex<Option<mpsc::Receiver<()>>>,
    cancel_watch: Mutex<Option<mpsc::Sender<()>>>,
    run_error: Mutex<Option<String>>,
    clock_hold: Mutex<bool>,
    clock: Mutex<Option<Arc<FakeClock>>>,
    dir: Mutex<Option<std::path::PathBuf>>,
    ran: AtomicUsize,
    ran_args: Mutex<Vec<Map<String, Value>>>,
    run_threads: Mutex<Vec<thread::ThreadId>>,
}

impl ProbeTool {
    fn reads(name: &str) -> Self {
        Self::classified(
            name,
            json!({"type": "object"}),
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
                review_when_confined: false,
                hosts: None,
            }),
            "read it",
        )
    }

    fn executes(name: &str, subject: &str) -> Self {
        Self::classified(
            name,
            json!({"type": "object"}),
            Ok(Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Executes],
                    reversible: true,
                    paths: None,
                    confined: false,
                },
                subject: Some(subject.into()),
                prefix: Some(name.into()),
                always_reviewed: false,
                review_when_confined: false,
                hosts: None,
            }),
            "ran it",
        )
    }

    fn classified(
        name: &str,
        schema: Value,
        effects: Result<Effects, EffectsError>,
        text: &str,
    ) -> Self {
        Self {
            name: name.into(),
            schema,
            effects,
            text: text.into(),
            run_ms: 0,
            ask_confirm: false,
            boom_on: None,
            spill: Mutex::new(SpillMode::None),
            jobs: Mutex::new(Vec::new()),
            inner_plan: Mutex::new(InnerPlan::None),
            inner_report: Mutex::new(None),
            inner_errors: Mutex::new(Vec::new()),
            forget_gate: Mutex::new(None),
            run_entered: Mutex::new(None),
            run_gate: Mutex::new(None),
            run_rehold: Mutex::new(None),
            cancel_watch: Mutex::new(None),
            run_error: Mutex::new(None),
            clock_hold: Mutex::new(false),
            clock: Mutex::new(None),
            dir: Mutex::new(None),
            ran: AtomicUsize::new(0),
            ran_args: Mutex::new(Vec::new()),
            run_threads: Mutex::new(Vec::new()),
        }
    }

    /// Fails the effects function when the arguments carry `key: "boom"`:
    /// the original call checks, its rewrite does not.
    fn booming(mut self, key: &str) -> Self {
        self.boom_on = Some(key.into());
        self
    }

    /// Spills the next runs' output as `mode` fabricates.
    fn spilling(&self, mode: SpillMode) {
        *lock(&self.spill) = mode;
    }

    /// Queues `jobs` on the next runs' output.
    fn queue_jobs(&self, jobs: Vec<contract::jobs::JobRecord>) {
        *lock(&self.jobs) = jobs;
    }

    fn ran_args(&self) -> Vec<Map<String, Value>> {
        lock(&self.ran_args).clone()
    }
    /// Synchronous fake work inside the tool's run, in milliseconds.
    fn with_work(mut self, ms: u64) -> Self {
        self.run_ms = ms;
        self
    }

    /// One synchronous inner call of `name` with `args` per run.
    fn calling(self, name: &str, args: Value) -> Self {
        *lock(&self.inner_plan) = InnerPlan::Call {
            name: name.into(),
            args,
        };
        self
    }

    /// One detached inner call of `name` per run, reported off-thread.
    fn forgetting(self, name: &str, args: Value) -> Self {
        *lock(&self.inner_plan) = InnerPlan::Forget {
            name: name.into(),
            args,
        };
        self
    }

    /// Reports detached inner answers on `report`.
    fn reporting(&self, report: mpsc::Sender<String>) {
        *lock(&self.inner_report) = Some(report);
    }

    /// Holds a forgetting run after it fires until the test releases `gate`:
    /// the anchor stays alive past the descendant's admission.
    fn hold_forget(&self, gate: mpsc::Receiver<()>) {
        *lock(&self.forget_gate) = Some(gate);
    }

    /// Fails the next runs with `message` as a tool error.
    fn failing_run(&self, message: &str) {
        *lock(&self.run_error) = Some(message.into());
    }

    /// Holds the next runs on the clock at entry until the test releases
    /// `gate`: the hold itself is the accused park.
    fn hold_run(self) -> Self {
        *lock(&self.clock_hold) = true;
        self
    }

    /// Re-parks a held run after the test releases its first park, without
    /// moving time, so a later step observes a fresh park.
    fn rehold_run(&self, gate: mpsc::Receiver<()>) {
        *lock(&self.run_rehold) = Some(gate);
    }

    /// Holds the next runs at entry until the test releases `gate`.
    fn gate_run(&self, entered: mpsc::Sender<()>, gate: mpsc::Receiver<()>) {
        *lock(&self.run_entered) = Some(entered);
        *lock(&self.run_gate) = Some(gate);
    }

    /// Watches the next run for its cancel: the run waits for the loop
    /// to cancel it before it waits on its gate, then sends `seen`.
    fn watch_cancel(&self, seen: mpsc::Sender<()>) {
        *lock(&self.cancel_watch) = Some(seen);
    }

    fn confirming(mut self) -> Self {
        self.ask_confirm = true;
        self
    }

    fn ran(&self) -> usize {
        self.ran.load(Ordering::SeqCst)
    }

    /// Every worker thread that entered a run, in entry order.
    fn run_threads(&self) -> Vec<thread::ThreadId> {
        lock(&self.run_threads).clone()
    }
}

impl Tool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: "Probes.".into(),
            input_schema: self.schema.clone(),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        if let Some(key) = &self.boom_on
            && arguments.get(key).and_then(Value::as_str) == Some("boom")
        {
            return Err(EffectsError::Arguments(
                "the rewrite names no ledger".into(),
            ));
        }
        self.effects.clone()
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _: &dyn Emit) -> Output {
        self.enter_run(arguments, cancel);
        let error = lock(&self.run_error).clone().map(|message| Failure {
            code: ErrorCode::ToolError,
            message,
            provider: None,
            retry_after_ms: None,
        });
        Output {
            content: vec![ContentPart::Text {
                text: self.text.clone(),
            }],
            error,
            jobs: lock(&self.jobs).clone(),
            spilled: self.spill_output(),
            ..Output::default()
        }
    }

    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        // One planned inner call first; an ordinary run otherwise.
        if !self.ask_confirm {
            let planned = match lock(&self.inner_plan).clone() {
                InnerPlan::None => None,
                InnerPlan::Call { name, args } => Some((name, args, false)),
                InnerPlan::Forget { name, args } => Some((name, args, true)),
            };
            if let Some((name, args, forget)) = planned {
                return self.run_inner(arguments, cancel, ask, name, args, forget);
            }
            return self.run(arguments, cancel, emit);
        }
        self.ran.fetch_add(1, Ordering::SeqCst);
        match ask.ask(Asking {
            interaction: Interaction::Confirm {
                prompt: "Deploy?".into(),
            },
            action_ids: Vec::new(),
            until: None,
            check: None,
            suspends: true,
        }) {
            Answered::Reply(answer) => Output {
                content: vec![ContentPart::Text {
                    text: format!("answered {answer:?}"),
                }],
                ..Output::default()
            },
            Answered::NoAnswer => Output {
                content: vec![ContentPart::Text {
                    text: "no answer".into(),
                }],
                ..Output::default()
            },
        }
    }
}

impl ProbeTool {
    /// Counts the run, records its arguments and holds it at the test's
    /// gate, then does its synchronous fake work.
    fn enter_run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel) {
        self.ran.fetch_add(1, Ordering::SeqCst);
        lock(&self.ran_args).push(arguments.clone());
        lock(&self.run_threads).push(thread::current().id());
        if let Some(entered) = lock(&self.run_entered).take() {
            let _sent = entered.send(());
        }
        if let Some(seen) = lock(&self.cancel_watch).take() {
            // A watched leaf waits here for the loop to cancel it: the
            // test sends the run gate only after `seen`, so the leaf
            // cannot return before its cancel fired. The subscription
            // keeps only a Weak, so the wake stays alive across the
            // wait; a cancel that landed before the subscription is
            // caught by the recheck.
            let (wake_tx, wake_rx) = mpsc::channel();
            let wake: Arc<dyn Wake> = Arc::new(CancelWake(wake_tx));
            cancel.subscribe(Arc::downgrade(&wake));
            if !cancel.is_cancelled() {
                Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv_or_fail(&wake_rx, "the loop cancels the leaf");
            }
            let _sent = seen.send(());
        }
        if *lock(&self.clock_hold) {
            // Parks on the injected clock while the test holds the gate:
            // advancing the clock then sets the run's duration.
            let clock = lock(&self.clock).clone().expect("the build wires a clock");
            if let Some(rehold) = lock(&self.run_rehold).take() {
                clock.wait_until(None, &mut |_| {
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv_or_fail(&rehold, "the test releases the run to re-park");
                });
            }
            clock.wait_until(None, &mut |_| {
                if let Some(gate) = lock(&self.run_gate).take() {
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv_or_fail(&gate, "the test releases the run");
                }
            });
        } else if let Some(gate) = lock(&self.run_gate).take() {
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&gate, "the test releases the run");
        }
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(self.run_ms));
        }
    }

    /// One inner call per the plan: synchronously, or detached with its
    /// answer reported off-thread.
    fn run_inner(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        ask: &dyn Ask,
        name: String,
        args: Value,
        forget: bool,
    ) -> Output {
        self.enter_run(arguments, cancel);
        let Some(inner) = ask.inner_calls() else {
            return Output {
                content: vec![ContentPart::Text {
                    text: "no inner calls".into(),
                }],
                ..Output::default()
            };
        };
        if forget {
            let report = lock(&self.inner_report).clone();
            std::thread::spawn(move || {
                let message = match inner.run("probe", &name, args) {
                    Ok(done) => format!("answered {:?}", done.status),
                    Err(error) => format!("refused {error}"),
                };
                if let Some(report) = report {
                    let _sent = report.send(message);
                }
            });
            if let Some(gate) = lock(&self.forget_gate).take() {
                Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv_or_fail(&gate, "the test releases the forgetter");
            }
            return Output {
                content: vec![ContentPart::Text {
                    text: "fired".into(),
                }],
                ..Output::default()
            };
        }
        match inner.run("probe", &name, args) {
            Ok(done) => {
                let text = crate::conversation::text(&done.content);
                Output {
                    content: vec![ContentPart::Text { text }],
                    ..Output::default()
                }
            }
            Err(error) => {
                lock(&self.inner_errors).push(error.clone());
                Output {
                    content: vec![ContentPart::Text { text: error }],
                    ..Output::default()
                }
            }
        }
    }
}

impl ProbeTool {
    /// The run's spill, fabricated in the wired workspace directory.
    fn spill_output(&self) -> Option<contract::tool::Spilled> {
        match *lock(&self.spill) {
            SpillMode::None => None,
            SpillMode::Persisted => {
                let dir = lock(&self.dir).clone().expect("the build wires a dir");
                let path = dir.join("spill.txt");
                let bytes = b"spilled output";
                std::fs::write(&path, bytes).expect("the spill file writes");
                Some(contract::tool::Spilled {
                    head_bytes: 8,
                    tail_bytes: 6,
                    head: "spilled ".into(),
                    tail: "output".into(),
                    removed: 0,
                    file: Ok(contract::tool::SpillFile {
                        path,
                        size: bytes.len() as u64,
                    }),
                })
            }
            SpillMode::Unsaved => Some(contract::tool::Spilled {
                head_bytes: 8,
                tail_bytes: 6,
                head: "spilled ".into(),
                tail: "output".into(),
                removed: 3,
                file: Err(contract::tool::Unsaved {
                    message: "the spill file was not kept".into(),
                    left: None,
                }),
            }),
            SpillMode::UnsavedBadLeft => Some(contract::tool::Spilled {
                head_bytes: 8,
                tail_bytes: 6,
                head: "spilled ".into(),
                tail: "output".into(),
                removed: 3,
                file: Err(contract::tool::Unsaved {
                    message: "the spill file was not kept".into(),
                    left: Some(std::path::PathBuf::from("/nonexistent/fiber-spill-part")),
                }),
            }),
        }
    }
}

/// Standing rules the test sets, fails and times.
struct FakeRules {
    inner: Mutex<Result<StandingRules, RulesError>>,
    read_ms: Mutex<u64>,
    clock: Mutex<Option<Arc<FakeClock>>>,
}

impl Default for FakeRules {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Ok(StandingRules::default())),
            read_ms: Mutex::new(0),
            clock: Mutex::new(None),
        }
    }
}

impl FakeRules {
    fn set(&self, rules: StandingRules) {
        *lock(&self.inner) = Ok(rules);
    }

    fn fail(&self, message: &str) {
        *lock(&self.inner) = Err(RulesError(message.into()));
    }
}

impl Rules for FakeRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(*lock(&self.read_ms)));
        }
        lock(&self.inner).clone()
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

fn ask_rule(tool: &str, prefix: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Ask,
            tool: tool.into(),
            prefix: prefix.into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

fn allow_rule(tool: &str, prefix: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Allow,
            tool: tool.into(),
            prefix: prefix.into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

fn deny_rule(tool: &str, prefix: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Deny,
            tool: tool.into(),
            prefix: prefix.into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

/// Hooks with one scripted `before_tool` answer; `check` passes and
/// `after_tool` leaves the output. Task 2 grows this with timed stages.
struct ProbeHooks {
    before: Mutex<BeforeToolOutcome>,
    check: Mutex<CheckOutcome>,
    after: Mutex<AfterToolOutcome>,
    before_ms: u64,
    check_ms: u64,
    after_ms: u64,
    before_calls: AtomicUsize,
    check_calls: AtomicUsize,
    after_calls: AtomicUsize,
    clock: Mutex<Option<Arc<FakeClock>>>,
    /// One-shot signal sent on the next `before_tool` entry, then the
    /// entry waits on `gate`: the failed-commit test's shutdown window.
    entered: Mutex<Option<mpsc::Sender<()>>>,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    /// When set, every `before_tool` answer carries one notice naming
    /// the tool, so a batch test pins notice order across calls.
    notice_each: AtomicBool,
}

impl ProbeHooks {
    fn passing() -> Self {
        Self::staged(
            BeforeToolOutcome::Unchanged,
            CheckOutcome::Passed,
            AfterToolOutcome::Unchanged,
            0,
            0,
            0,
        )
    }

    fn staged(
        before: BeforeToolOutcome,
        check: CheckOutcome,
        after: AfterToolOutcome,
        before_ms: u64,
        check_ms: u64,
        after_ms: u64,
    ) -> Self {
        Self {
            before: Mutex::new(before),
            check: Mutex::new(check),
            after: Mutex::new(after),
            before_ms,
            check_ms,
            after_ms,
            before_calls: AtomicUsize::new(0),
            check_calls: AtomicUsize::new(0),
            after_calls: AtomicUsize::new(0),
            clock: Mutex::new(None),
            entered: Mutex::new(None),
            gate: Mutex::new(None),
            notice_each: AtomicBool::new(false),
        }
    }
    fn refusing() -> Self {
        Self::staged(
            BeforeToolOutcome::Refused(Refusal {
                extension: "probe".into(),
                reason: "not this call".into(),
            }),
            CheckOutcome::Passed,
            AfterToolOutcome::Unchanged,
            0,
            0,
            0,
        )
    }

    fn rewriting(arguments: Map<String, Value>) -> Self {
        Self::staged(
            BeforeToolOutcome::Rewritten {
                arguments,
                changed_by: vec!["probe".into()],
            },
            CheckOutcome::Passed,
            AfterToolOutcome::Unchanged,
            0,
            0,
            0,
        )
    }

    /// Signals the test on entry, then waits for its release.
    fn gated(entered: mpsc::Sender<()>, gate: mpsc::Receiver<()>) -> Self {
        let hooks = Self::refusing();
        *lock(&hooks.entered) = Some(entered);
        *lock(&hooks.gate) = Some(gate);
        hooks
    }
}

impl Hooks for ProbeHooks {
    fn before_tool(&self, call: &contract::hook::BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.before_calls.fetch_add(1, Ordering::SeqCst);
        // Every entry signals and waits: entries queue on the receiver
        // itself, so one gate holds each dispatch in turn.
        if let Some(entered) = lock(&self.entered).as_ref() {
            let _sent = entered.send(());
        }
        if lock(&self.gate).is_some() {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(lock(&self.gate).as_ref().expect("the gate is armed"))
                .expect("the test releases the gate");
        }
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(self.before_ms));
        }
        let notices = if self.notice_each.load(Ordering::SeqCst) {
            vec![Notice {
                code: ErrorCode::HookFailed,
                message: format!("hook saw {}", call.tool),
                extension: Some("fiber.test/guard".into()),
            }]
        } else {
            Vec::new()
        };
        BeforeToolAnswer {
            outcome: lock(&self.before).clone(),
            notices,
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> CheckAnswer {
        self.check_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(self.check_ms));
        }
        CheckAnswer {
            outcome: lock(&self.check).clone(),
            notices: Vec::new(),
        }
    }

    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        self.after_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(self.after_ms));
        }
        AfterToolAnswer {
            outcome: lock(&self.after).clone(),
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _message: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(&self, _call: &contract::hook::BeforeModelCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
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

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// Wakes the loop's inbox, as a door does, so worker interactions and
/// clock advances are seen while a turn runs.
struct InboxWake(std::sync::Weak<mpsc::Sender<Delivery>>);

impl Wake for InboxWake {
    fn wake(&self) {
        if let Some(tx) = self.0.upgrade() {
            let _sent = tx.send(Delivery::Cancelled);
        }
    }
}

struct Setup {
    tools: Vec<Arc<ProbeTool>>,
    rules: Arc<FakeRules>,
    hooks: Option<Arc<ProbeHooks>>,
    no_reviewer: bool,
    reviewer_script: Option<Vec<ReviewReply>>,
    reviewer_cost: Option<contract::provider::Cost>,
    reviewer_wake: Option<mpsc::Sender<()>>,
    /// Cloned into each held reviewer call: every hold signals entry, so
    /// the test counts them all in flight.
    reviewer_hold_entered: Option<mpsc::Sender<()>>,
    reviewer_limits: Option<(u64, u64)>,
    budget: Option<f64>,
    answerable: bool,
    debug: bool,
    idle: Option<Duration>,
}

impl Setup {
    fn new(rules: Arc<FakeRules>) -> Self {
        Self {
            tools: Vec::new(),
            rules,
            hooks: None,
            no_reviewer: false,
            reviewer_script: None,
            reviewer_cost: None,
            reviewer_wake: None,
            reviewer_hold_entered: None,
            reviewer_limits: None,
            budget: None,
            answerable: true,
            debug: true,
            idle: None,
        }
    }
}

struct World {
    home: fakes::TempDir,
    log: Option<Arc<Log>>,
    clock: Arc<FakeClock>,
    origin: Instant,
    inbox_tx: Option<Arc<mpsc::Sender<Delivery>>>,
    cancel: Arc<crate::TurnCancel>,
    looped: Option<Loop>,
    rules: Arc<FakeRules>,
    tools: Vec<Arc<ProbeTool>>,
    provider: Arc<ScriptProvider>,
    reviewer: Option<Arc<ScriptProvider>>,
    reviewer_script: Option<Vec<ReviewReply>>,
    reviewer_cost: Option<contract::provider::Cost>,
    reviewer_limits: Option<(u64, u64)>,
    answerable: bool,
    debug: bool,
    idle: Option<Duration>,
    hooks: Option<Arc<ProbeHooks>>,
    no_reviewer: bool,
    credential_files: Vec<std::path::PathBuf>,
}

fn model() -> Model {
    Model {
        reference: "fake/model".into(),
        cost: None,
        subscription: false,
    }
}

fn build(setup: &Setup, home: fakes::TempDir, script: Vec<Answer>) -> World {
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = FakeClock::new();
    let origin = clock.origin();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let provider = Arc::new(ScriptProvider {
        clock: Arc::clone(&clock),
        script: Mutex::new(script.into_iter().collect()),
        entered: Mutex::new(None),
        calls: AtomicUsize::new(0),
        hold_pause: Mutex::new(None),
        held_pause: Mutex::new(None),
        requests: Mutex::new(Vec::new()),
        hold_entered: Mutex::new(None),
        hold_releases: Arc::new(Mutex::new(VecDeque::new())),
    });
    // Every timer reads this world's clock: tools, rules and the hook
    // gate advance it from whichever thread they run on.
    for tool in &setup.tools {
        *lock(&tool.clock) = Some(Arc::clone(&clock));
        *lock(&tool.dir) = Some(workspace.clone());
    }
    *lock(&setup.rules.clock) = Some(Arc::clone(&clock));
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let inbox_tx = Arc::new(inbox_tx);
    let cancel = Arc::new(crate::TurnCancel::default());
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
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
            tools: setup
                .tools
                .iter()
                .map(|tool| ("builtin".to_owned(), tool.clone() as Arc<dyn Tool>))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::clone(&setup.rules) as Arc<dyn Rules>,
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&cancel))
    .answerable(setup.answerable);
    if setup.debug {
        let diag = log::diag::Diag::new(
            home.path(),
            log::diag::Process::Session,
            log::diag::Level::Debug,
            Arc::clone(&dyn_clock),
        );
        diag.attach(&SessionId("s_test".into()));
        looped = looped.diagnostics(Arc::new(diag));
    }
    if setup.no_reviewer {
        looped = looped.reviewer(
            Err(Failure {
                code: ErrorCode::NoModel,
                message: crate::reviewer::NO_MODEL_MESSAGE.into(),
                provider: None,
                retry_after_ms: None,
            }),
            BlockLimits::default(),
        );
    }
    let mut reviewer: Option<Arc<ScriptProvider>> = None;
    if let Some(script) = setup.reviewer_script.as_ref() {
        let judging = Arc::new(ScriptProvider {
            clock: Arc::clone(&clock),
            script: Mutex::new(review_answers(script).into_iter().collect()),
            entered: Mutex::new(setup.reviewer_wake.clone()),
            calls: AtomicUsize::new(0),
            hold_pause: Mutex::new(None),
            held_pause: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            hold_entered: Mutex::new(setup.reviewer_hold_entered.clone()),
            hold_releases: Arc::new(Mutex::new(VecDeque::new())),
        });
        let (consecutive, session) = setup.reviewer_limits.unwrap_or((3, 20));
        looped = looped.reviewer(
            Ok(Reviewer {
                provider: Arc::clone(&judging) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/reviewer".into(),
                    cost: setup.reviewer_cost.clone(),
                    subscription: false,
                },
                cache_lifetime: contract::events::CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            BlockLimits {
                consecutive,
                session,
            },
        );
        reviewer = Some(judging);
    }
    if let Some(hooks) = &setup.hooks {
        *lock(&hooks.clock) = Some(Arc::clone(&clock));
        looped = looped.hooks(Arc::clone(hooks) as Arc<dyn Hooks>);
    }
    if let Some(budget) = setup.budget {
        looped = looped.budget(Some(budget));
    }
    looped = looped.inbox_wake(Arc::new(InboxWake(Arc::downgrade(&inbox_tx))));
    if let Some(idle) = setup.idle {
        looped = looped.idle_exit(Some(idle));
    }
    World {
        home,
        log: Some(log),
        clock,
        origin,
        inbox_tx: Some(inbox_tx),
        cancel,
        looped: Some(looped),
        rules: Arc::clone(&setup.rules),
        tools: setup.tools.iter().map(Arc::clone).collect(),
        provider: Arc::clone(&provider),
        reviewer,
        reviewer_script: setup.reviewer_script.clone(),
        reviewer_cost: setup.reviewer_cost.clone(),
        reviewer_limits: setup.reviewer_limits,
        answerable: setup.answerable,
        debug: setup.debug,
        idle: setup.idle,
        hooks: setup.hooks.clone(),
        no_reviewer: setup.no_reviewer,
        credential_files: Vec::new(),
    }
}

fn prompt(world: &World, text: &str) {
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Prompt(
            contract::inbox::Message {
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

/// Runs one turn on a detached worker and parks the loop back with its
/// outcome, also reporting the worker's thread for named clock parks. A
/// hung turn fails at the deadline instead of hanging the test.
struct Flight {
    done: mpsc::Receiver<(Result<Option<TurnOutcome>, Error>, Loop)>,
    thread: mpsc::Receiver<thread::ThreadId>,
}

fn spawn_turn(world: &mut World) -> Flight {
    let mut looped = world.looped.take().expect("the loop is parked");
    let (done_tx, done_rx) = mpsc::channel();
    let (thread_tx, thread_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = thread_tx.send(thread::current().id());
        let outcome = looped.turn();
        let _sent = done_tx.send((outcome, looped));
    });
    Flight {
        done: done_rx,
        thread: thread_rx,
    }
}

/// Borrows the loop parked in the world.
impl World {
    fn loop_mut(&mut self) -> &mut Loop {
        self.looped
            .as_mut()
            .expect("the loop is parked in the world")
    }
}

/// The bound `join_turn` waits for the turn on: the shared must-succeed bound.
const JOIN_WITHIN: Duration = MUST_SUCCEED_WITHIN;

fn join_turn(world: &mut World, flight: Flight) -> Result<Option<TurnOutcome>, Error> {
    let (outcome, looped) =
        Deadline::after(JOIN_WITHIN).recv_or_fail(&flight.done, "the turn ends");
    world.looped = Some(looped);
    outcome
}

fn turn(world: &mut World) -> Result<Option<TurnOutcome>, Error> {
    let flight = spawn_turn(world);
    join_turn(world, flight)
}

#[test]
fn join_turn_carries_the_shared_must_succeed_bound() {
    assert_eq!(
        JOIN_WITHIN, MUST_SUCCEED_WITHIN,
        "join_turn waits out the shared must-succeed bound",
    );
}

/// The next `permission_requested` on a watcher registered before the turn,
/// with its request and action ids. The shared must-succeed bound covers
/// the whole wait: every skipped line takes what remains of it, never a fresh wait.
fn wait_requested(watcher: &mut log::Watcher, deadline: &Deadline, what: &str) -> (String, String) {
    loop {
        let line = watcher
            .recv_timeout(deadline.left())
            .unwrap_or_else(|| panic!("a line arrives before the deadline: {what}"))
            .expect("the log stays readable")
            .expect("no end yet");
        if line.kind == "permission_requested" {
            let request = line.payload["request_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let action = line.action_id.clone().map(|id| id.0).unwrap_or_default();
            return (request, action);
        }
    }
}

/// The nth `permission_requested` (1-based) on the watcher: a resumed
/// turn's watcher replays the first raise, so the finishing turn waits
/// for the second, the re-raise. One deadline bounds all `n` taps.
fn wait_requested_nth(
    watcher: &mut log::Watcher,
    deadline: &Deadline,
    n: usize,
    what: &str,
) -> (String, String) {
    let mut found = (String::new(), String::new());
    for _ in 0..n {
        found = wait_requested(watcher, deadline, what);
    }
    found
}

/// Sends a person's approval answer to `request`.
fn reply(world: &World, request: &str, answer: ReplyAnswer) {
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Reply(
            contract::commands::Reply {
                request_id: RequestId(request.into()),
                answer,
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
}

fn allow() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: None,
    }
}

fn deny() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Deny,
        feedback: None,
        remember: None,
    }
}

/// Every `op_timing` record in the session's diagnostic file, in order.
fn op_records(world: &World) -> Vec<OpRecord> {
    op_records_in(world.home.path())
}

fn op_records_in(home: &std::path::Path) -> Vec<OpRecord> {
    let path = home.join("logs").join("session-s_test.log");
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
                op: data["op"].as_str().unwrap_or_default().into(),
                ms: data["ms"].as_u64().unwrap_or(u64::MAX),
                tool: data["tool"].as_str().map(str::to_owned),
                keys,
                raw: line,
            }
        })
        .collect()
}

#[derive(Debug)]
struct OpRecord {
    op: String,
    ms: u64,
    tool: Option<String>,
    keys: Vec<String>,
    raw: Value,
}

fn ops(world: &World, op: &str) -> Vec<OpRecord> {
    op_records(world)
        .into_iter()
        .filter(|line| line.op == op)
        .collect()
}

/// Every `op_timing` record carrying a tool: Part 8's phases. Part 7's
/// scopes carry none.
fn tool_records(world: &World) -> Vec<OpRecord> {
    op_records(world)
        .into_iter()
        .filter(|line| line.tool.is_some())
        .collect()
}

/// The wire promise on every Part 8 tool line: exactly the keys `op`,
/// `ms` and `tool`, an unsigned `ms`, the registered tool, and none of the
/// session's prompt, output, arguments or credential anywhere in the line.
#[track_caller]
fn assert_tool_wire(records: &[OpRecord], tool: &str) {
    assert!(!records.is_empty(), "the test proved its lines exist first");
    for record in records {
        assert_eq!(
            record.keys,
            vec!["ms".to_owned(), "op".to_owned(), "tool".to_owned()],
            "{:?}",
            record.raw
        );
        assert_eq!(record.tool.as_deref(), Some(tool), "{:?}", record.raw);
        let raw = record.raw.to_string();
        for sentinel in ["hi", "Hello.", "atlas-secret", "/secret", "sk-test", "null"] {
            assert!(
                !raw.contains(sentinel),
                "no prompt, output, argument, credential or null in {raw}"
            );
        }
    }
}

fn kinds(world: &World) -> Vec<String> {
    log::read(&world.home.path().join("s_test"))
        .unwrap()
        .iter()
        .map(|line| line.kind.clone())
        .collect()
}

/// Exits the suspending process on `request` and resumes the session in a
/// new process with `script`: the next `turn` finishes the suspended batch
/// as a fresh active segment on a fresh clock.
fn exit_and_resume(world: &mut World, request: &str, script: Vec<Answer>) {
    let log = world.log.take().expect("the log is open");
    let looped = world.looped.take().expect("the loop is parked");
    drop(looped);
    log.append(
        &Event::FiberExited(FiberExited {
            exit_code: 0,
            usage: Default::default(),
            final_message: None,
            error: None,
            suspended_on: Some(RequestId(request.into())),
            questions: None,
        }),
        None,
        None,
    )
    .unwrap();
    drop(log);
    let clock = FakeClock::new();
    let origin = clock.origin();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn Clock>;
    let log = Arc::new(
        Log::open(
            world.home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let resumed = crate::resume::resumed(log.dir()).expect("the exit suspends a batch");
    let provider = Arc::new(ScriptProvider {
        clock: Arc::clone(&clock),
        script: Mutex::new(script.into_iter().collect()),
        entered: Mutex::new(None),
        calls: AtomicUsize::new(0),
        hold_pause: Mutex::new(None),
        held_pause: Mutex::new(None),
        requests: Mutex::new(Vec::new()),
        hold_entered: Mutex::new(None),
        hold_releases: Arc::new(Mutex::new(VecDeque::new())),
    });
    let workspace = world.home.path().join("workspace");
    for tool in &world.tools {
        *lock(&tool.clock) = Some(Arc::clone(&clock));
        *lock(&tool.dir) = Some(workspace.clone());
    }
    *lock(&world.rules.clock) = Some(Arc::clone(&clock));
    let workspace = world.home.path().join("workspace");
    let credentials = world.home.path().join("credentials");
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let inbox_tx = Arc::new(inbox_tx);
    let cancel = Arc::new(crate::TurnCancel::default());
    let mut looped = Loop::resume(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
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
                Arc::clone(&dyn_clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: world
                .tools
                .iter()
                .map(|tool| ("builtin".to_owned(), tool.clone() as Arc<dyn Tool>))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: world.credential_files.clone(),
                rules: Arc::clone(&world.rules) as Arc<dyn Rules>,
            },
        },
        resumed,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&cancel))
    .answerable(world.answerable);
    if world.debug {
        let diag = log::diag::Diag::new(
            world.home.path(),
            log::diag::Process::Session,
            log::diag::Level::Debug,
            Arc::clone(&dyn_clock),
        );
        diag.attach(&SessionId("s_test".into()));
        looped = looped.diagnostics(Arc::new(diag));
    }
    if world.no_reviewer {
        looped = looped.reviewer(
            Err(Failure {
                code: ErrorCode::NoModel,
                message: crate::reviewer::NO_MODEL_MESSAGE.into(),
                provider: None,
                retry_after_ms: None,
            }),
            BlockLimits::default(),
        );
    }
    let mut reviewer: Option<Arc<ScriptProvider>> = None;
    if let Some(script) = world.reviewer_script.clone() {
        let judging = Arc::new(ScriptProvider {
            clock: Arc::clone(&clock),
            script: Mutex::new(review_answers(&script).into_iter().collect()),
            entered: Mutex::new(None),
            calls: AtomicUsize::new(0),
            hold_pause: Mutex::new(None),
            held_pause: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            hold_entered: Mutex::new(None),
            hold_releases: Arc::new(Mutex::new(VecDeque::new())),
        });
        let (consecutive, session) = world.reviewer_limits.unwrap_or((3, 20));
        looped = looped.reviewer(
            Ok(Reviewer {
                provider: Arc::clone(&judging) as Arc<dyn Provider>,
                model: Model {
                    reference: "fake/reviewer".into(),
                    cost: world.reviewer_cost.clone(),
                    subscription: false,
                },
                cache_lifetime: contract::events::CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                thinking_levels: Vec::new(),
            }),
            BlockLimits {
                consecutive,
                session,
            },
        );
        reviewer = Some(judging);
    }
    if let Some(hooks) = &world.hooks {
        *lock(&hooks.clock) = Some(Arc::clone(&clock));
        looped = looped.hooks(Arc::clone(hooks) as Arc<dyn Hooks>);
    }
    looped = looped.inbox_wake(Arc::new(InboxWake(Arc::downgrade(&inbox_tx))));
    if let Some(idle) = world.idle {
        looped = looped.idle_exit(Some(idle));
    }
    world.clock = clock;
    world.origin = origin;
    world.log = Some(log);
    world.inbox_tx = Some(inbox_tx);
    world.cancel = cancel;
    world.looped = Some(looped);
    world.provider = provider;
    world.reviewer = reviewer;
}

/// Every `permission_resolved` decision in the session log, with who decided.
fn resolved_bys(world: &World) -> Vec<(String, String)> {
    log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "permission_resolved")
        .map(|line| {
            (
                line.payload["decision"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                line.payload["decided_by"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// suspends the turn on an approval, advancing the idle delay once the
/// approval wait parks on the clock. Returns the pending request's id.
fn suspend_on_approval(world: &mut World) -> String {
    let flight = spawn_turn(world);
    let thread =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    world.clock.advance_when_parked(
        Some(world.origin + IDLE),
        Some(thread),
        IDLE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    // A clock move alone never wakes the inbox wait: a wake makes it look
    // again, find the delay passed, and unwind (`docs/testing.md`).
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    let outcome = join_turn(world, flight);
    assert_eq!(outcome.unwrap(), None, "the idle delay unwinds the turn");
    request
}

// A fast-path read times exactly one permission order, with no decision
// line, and the tool ran with the registered name alone recorded.
#[test]
fn a_fast_read_times_one_permission_without_a_decision_line() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer(
                "atlas",
                json!({"path": "/secret/ledger", "command": "atlas-secret"}),
                0,
            ),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1, "the allowed call ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1, "{:?}", op_records(&world));
    assert_eq!(records[0].ms, 0);
    assert_tool_wire(&records, "atlas");
    assert!(
        !kinds(&world).contains(&"permission_resolved".to_owned()),
        "a fast path writes no decision line"
    );
}

// The rules read runs inside the permission order: its synchronous fake
// work sets the recorded duration.
#[test]
fn a_slow_rules_read_sets_the_permission_duration() {
    let rules = Arc::new(FakeRules::default());
    *lock(&rules.read_ms) = 5;
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 5, "the judge's work is inside the order");
    assert_tool_wire(&records, "atlas");
}

// A standing allow runs the call with one permission record and a decision
// line naming the rule.
#[test]
fn a_standing_allow_runs_with_one_permission_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(allow_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("allow".to_owned(), "standing_rule".to_owned())]
    );
}

// A standing deny refuses with one permission record and never runs.
#[test]
fn a_standing_deny_refuses_with_one_permission_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(deny_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0, "the refused call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1, "the entered order is timed");
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "standing_rule".to_owned())]
    );
}

// The credential deny refuses with one permission record and never runs.
#[test]
fn a_credential_deny_refuses_with_one_permission_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object"}),
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec!["../credentials".into()]),
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        }),
        "read it",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0, "the refused call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "credential_deny".to_owned())]
    );
}

// Unreadable rules fail closed with one permission record and never run.
#[test]
fn unreadable_rules_deny_with_one_permission_record() {
    let rules = Arc::new(FakeRules::default());
    rules.fail("global rules: line 1 is not a rule");
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "standing_rule".to_owned())]
    );
}

// A call naming no registered tool never enters the permission order.
#[test]
fn an_unknown_tool_enters_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("ghost", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
    assert!(
        !kinds(&world).contains(&"tool_call_started".to_owned()),
        "the unknown call never started"
    );
}

// Arguments that are not an object never enter the permission order.
#[test]
fn non_object_arguments_enter_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!("oops"), 0),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
}

// Arguments that miss the schema never enter the permission order.
#[test]
fn schema_failing_arguments_enter_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object", "required": ["target"]}),
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
            review_when_confined: false,
            hosts: None,
        }),
        "read it",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
}

// An effects failure never enters the permission order.
#[test]
fn an_effects_failure_enters_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object"}),
        Err(EffectsError::Arguments("no such ledger".into())),
        "read it",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
}

// A hook refusal settles the call in preparation: no permission order.
#[test]
fn a_hook_refusal_never_enters_permission() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::new(ProbeHooks::refusing()));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    // The refused preparation emits its entered hook dispatch, and nothing
    // else: the permission order is never entered.
    assert_eq!(ops(&world, "tool.hooks").len(), 1);
    assert!(ops(&world, "tool.permission").is_empty());
    assert!(
        !kinds(&world).contains(&"permission_requested".to_owned()),
        "a refused call asks nobody"
    );
}

// A rewrite that fails its re-check settles the call in preparation.
#[test]
fn an_invalid_rewrite_never_enters_permission() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object", "required": ["target"]}),
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
            review_when_confined: false,
            hosts: None,
        }),
        "read it",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::new(ProbeHooks::rewriting(Map::new())));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({"target": "x"}), 0),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert_eq!(
        ops(&world, "tool.hooks").len(),
        1,
        "the entered before dispatch"
    );
    assert!(ops(&world, "tool.permission").is_empty());
}

// A person's allow holds the permission order open: the 7 ms advanced
// while the loop waits on the answer is inside the recorded duration.
#[test]
fn a_persons_allow_holds_the_wait_inside_permission() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let thread =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    assert!(
        ops(&world, "tool.permission").is_empty(),
        "no record completes while the person is waited on"
    );
    world.clock.advance_when_parked(
        None,
        Some(thread),
        Duration::from_millis(7),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    reply(&world, &request, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1, "the allowed call ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 7, "the answer wait is inside the order");
    assert_tool_wire(&records, "atlas");
    world.clock.sleep(Duration::from_secs(30));
    assert_eq!(
        ops(&world, "tool.permission")[0].ms,
        7,
        "later work stays outside the closed record"
    );
}

// A person's deny ends the order with one record and never runs.
#[test]
fn a_persons_deny_ends_permission_without_running() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    reply(&world, &request, deny());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 0, "the refused call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "person".to_owned())]
    );
}

// With nobody to answer, the approval is denied with one record.
#[test]
fn a_headless_approval_is_denied_with_one_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.answerable = false;
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
}

// A closed inbox denies the waiting approval with one record.
#[test]
fn a_closed_inbox_denies_the_waiting_approval() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    world.inbox_tx.take();
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
}

// `close` taken while waiting denies the request with one record.
#[test]
fn close_while_waiting_denies_with_one_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Close(Ack(Box::new(|_| {}))))
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0, "the closed call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "cancel".to_owned())]
    );
}

// A cancel while waiting ends the call cancelled with one record.
#[test]
fn a_cancel_while_waiting_ends_cancelled_with_one_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    // The cancel lands only once the wait parks on the clock: any earlier
    // and the unarmed signal swallows it.
    assert!(
        world
            .clock
            .await_parked_unbounded(Deadline::after(MUST_SUCCEED_WITHIN)),
        "the approval wait parks on the clock"
    );
    world.cancel.cancel();
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0, "the cancelled call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert!(
        kinds(&world).contains(&"tool_call_completed".to_owned()),
        "the cancelled call still completes"
    );
}

// A stale reply and an unfit reply are rejected; the fitting allow runs.
#[test]
fn a_stale_and_an_unfit_reply_are_rejected_before_an_allow() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    let inbox = world.inbox_tx.as_ref().expect("the inbox is open").clone();
    let answered = |request: &str, answer: ReplyAnswer| {
        let (tx, rx) = mpsc::channel();
        inbox
            .send(Delivery::Reply(
                contract::commands::Reply {
                    request_id: RequestId(request.into()),
                    answer,
                },
                Ack(Box::new(move |answer| {
                    let _sent = tx.send(answer);
                })),
            ))
            .unwrap();
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "the reply is answered")
    };
    assert!(
        answered("r_stale", allow()).is_err(),
        "a reply to another request is rejected"
    );
    assert!(
        answered(
            &request,
            ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: Some("fine".into()),
                remember: None,
            }
        )
        .is_err(),
        "an allow carrying feedback fits no standing ask"
    );
    assert!(
        answered(&request, allow()).is_ok(),
        "the fitting allow is accepted"
    );
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
}

// The idle delay unwinds the turn with the abandoned segment's record.
#[test]
fn an_idle_wait_suspends_with_the_abandoned_segment_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    assert!(!request.is_empty());
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1, "abandonment still closes the segment");
    assert_tool_wire(&records, "atlas");
    assert!(
        !kinds(&world).contains(&"tool_call_completed".to_owned()),
        "an idle approval completes nothing"
    );
}

// A shutdown while waiting abandons the segment with one record.
#[test]
fn a_shutdown_while_waiting_abandons_with_one_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    world.cancel.shutdown(0);
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
    assert!(
        !kinds(&world).contains(&"tool_call_started".to_owned()),
        "the abandoned call never started"
    );
}

// A failed decision write ends the turn with the original error.
#[test]
fn a_failed_decision_write_fails_the_turn() {
    let rules = Arc::new(FakeRules::default());
    rules.set(allow_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    arm_fail_decision_write();
    prompt(&world, "hi");
    let Err(error) = turn(&mut world) else {
        panic!("a failed decision write fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 1, "the errored order still closes");
    assert_tool_wire(&records, "atlas");
}

// A remembered session grant allows the next call with its own record.
#[test]
fn a_session_grant_allows_the_next_call() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.no_reviewer = true;
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({}), 0),
            text_answer("First.", 0),
            tool_answer("atlas", json!({}), 0),
            text_answer("Second.", 0),
        ],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the escalation is raised",
    );
    reply(
        &world,
        &request,
        ReplyAnswer::Approval {
            decision: Decision::Allow,
            feedback: None,
            remember: Some(Remember {
                scope: RememberScope::Session,
                prefix: "atlas".into(),
            }),
        },
    );
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1);
    assert_eq!(ops(&world, "tool.permission").len(), 1);
    prompt(&world, "again");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 2, "the grant allows the second call");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2, "each order owns its record");
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![
            ("allow".to_owned(), "person".to_owned()),
            ("allow".to_owned(), "session_grant".to_owned()),
        ]
    );
}

// A resumed approval starts a fresh segment: the re-raised request keeps
// its id, and the new segment times only the new process's wait.
#[test]
fn a_resumed_allow_starts_a_fresh_segment() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    assert_eq!(ops(&world, "tool.permission").len(), 1);
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let thread =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the approval is raised again",
    );
    assert_eq!(raised, request, "the same request id is raised again");
    assert_eq!(
        kinds(&world)
            .iter()
            .filter(|kind| *kind == "permission_requested")
            .count(),
        2,
        "the request is raised once per process"
    );
    world.clock.advance_when_parked(
        Some(world.origin + IDLE),
        Some(thread),
        Duration::from_millis(7),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    reply(&world, &raised, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1, "the allowed call ran once");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2, "one segment per process");
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        records[1].ms, 7,
        "the fresh segment times only the new wait, never downtime"
    );
}

// A resumed deny ends its segment with a record and never runs.
#[test]
fn a_resumed_deny_ends_its_segment_without_running() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the approval is raised again",
    );
    reply(&world, &raised, deny());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2);
    assert_tool_wire(&records, "atlas");
}

// A resumed approval with nobody to answer is denied with its record.
#[test]
fn a_resumed_headless_approval_is_denied_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    world.answerable = false;
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2);
    assert_tool_wire(&records, "atlas");
}

// A credential file configured after the suspension refuses the resumed
// approval in its own segment.
#[test]
fn a_resumed_credential_deny_refuses_in_its_segment() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object"}),
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: true,
                paths: Some(vec!["ledger.md".into()]),
                confined: false,
            },
            subject: Some("atlas run".into()),
            prefix: Some("atlas".into()),
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        }),
        "ran it",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    world.credential_files = vec![world.home.path().join("workspace").join("ledger.md")];
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0, "the refused call never ran");
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2);
    assert_tool_wire(&records, "atlas");
    assert!(resolved_bys(&world).contains(&("deny".to_owned(), "credential_deny".to_owned())));
}

// A cancel while the resumed approval waits ends it with its record.
#[test]
fn a_resumed_cancel_ends_its_segment_cancelled() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the approval is raised again",
    );
    assert!(
        world
            .clock
            .await_parked(world.origin + IDLE, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the resumed wait parks on the clock"
    );
    world.cancel.cancel();
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2);
    assert_tool_wire(&records, "atlas");
}

// Idling again on the resumed approval abandons its own segment.
#[test]
fn a_resumed_idle_abandons_its_own_segment() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let thread =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the approval is raised again",
    );
    world.clock.advance_when_parked(
        Some(world.origin + IDLE),
        Some(thread),
        IDLE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    let outcome = join_turn(&mut world, flight);
    assert_eq!(outcome.unwrap(), None);
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2, "live and resumed abandonments each close");
    assert_tool_wire(&records, "atlas");
}

// A shutdown while the resumed approval waits abandons its own segment.
#[test]
fn a_resumed_shutdown_abandons_its_own_segment() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the approval is raised again",
    );
    world.cancel.shutdown(0);
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.permission");
    assert_eq!(records.len(), 2);
    assert_tool_wire(&records, "atlas");
}

// A shutdown before the resumed decision is written settles nothing: the
// failed commit writes no line and emits no timer.
#[test]
fn a_failed_commit_on_resume_settles_nothing() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    world.hooks = Some(Arc::new(ProbeHooks::gated(entered_tx, gate_rx)));
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the hooks re-run on resume");
    world.cancel.shutdown(0);
    gate_tx.send(()).expect("the hook waits on the gate");
    let outcome = join_turn(&mut world, flight);
    assert_eq!(outcome.unwrap(), None, "the failed commit unwinds");
    assert_eq!(tool.ran(), 0);
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "only the abandoned live segment closed"
    );
    assert!(
        !kinds(&world).contains(&"permission_resolved".to_owned()),
        "the unsettled refusal writes no decision"
    );
}

// A call whose tool no longer checks completes with the check's failure
// and never opens a resumed segment: the historic name is not validated.
#[test]
fn a_resumed_schema_failure_completes_unchecked() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    world.tools = vec![Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object", "required": ["target"]}),
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some("atlas run".into()),
            prefix: Some("atlas".into()),
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        }),
        "ran it",
    ))];
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the recorded request is raised",
    );
    assert_eq!(raised, request);
    reply(&world, &raised, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "no resumed segment"
    );
    assert!(
        kinds(&world).contains(&"tool_call_completed".to_owned()),
        "the call completes with the check's failure"
    );
}

// A call naming a tool the resumed process no longer registers completes
// unchecked with no resumed segment.
#[test]
fn a_resumed_unknown_tool_completes_unchecked() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    world.tools.clear();
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the recorded request is raised",
    );
    reply(&world, &raised, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "no resumed segment"
    );
}

// A hook that refuses on resume settles the call with no resumed segment:
// the decision is a deny, and the request is never raised again.
#[test]
fn a_resumed_hook_refusal_settles_with_no_segment() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let request = suspend_on_approval(&mut world);
    world.hooks = Some(Arc::new(ProbeHooks::refusing()));
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "no resumed segment"
    );
    assert_eq!(
        resolved_bys(&world).last(),
        Some(&("deny".to_owned(), "hook".to_owned())),
        "the settled refusal is decided by the hook"
    );
}

// Dropping a suspended approval holding its guard closes the segment
// once: the abandoned segment still ends.
#[test]
fn dropping_suspended_state_closes_its_guard_once() {
    let home = fakes::TempDir::new("fiber-tool-timing");
    let clock = FakeClock::new();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn Clock>;
    let diag = log::diag::Diag::new(
        home.path(),
        log::diag::Process::Session,
        log::diag::Level::Debug,
        Arc::clone(&dyn_clock),
    );
    diag.attach(&SessionId("s_test".into()));
    let timer = log::diag::timed(
        &diag,
        contract::diag::Op::ToolPermission,
        Some("atlas"),
        None,
    )
    .expect("debug starts a timer");
    clock.sleep(Duration::from_millis(4));
    drop(crate::resume::Suspended {
        turn: TurnId("t_test".into()),
        pending: crate::suspend::Pending::Approval(PermissionRequested {
            always_reviewed: false,
            request_id: RequestId("r_test".into()),
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            step: AskStep::StandingAsk {
                standing_rule: StandingRule {
                    scope: RuleScope::Project,
                    prefix: "atlas".into(),
                },
            },
        }),
        action: ActionId("a_test".into()),
        batch: Vec::new(),
        started: None,
        permission: Some(Box::new(timer) as Box<dyn Send>),
    });
    let records: Vec<OpRecord> = op_records_in(home.path())
        .into_iter()
        .filter(|line| line.op == "tool.permission")
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 4);
    assert_tool_wire(&records, "atlas");
}

/// The next `interaction_requested` on a watcher, with its request id.
/// One deadline bounds the whole wait.
fn wait_interaction(watcher: &mut log::Watcher, deadline: &Deadline, what: &str) -> String {
    wait_interaction_nth(watcher, deadline, 1, what)
}

/// The nth `interaction_requested` (1-based): a resumed turn's watcher
/// replays the first question, so the finishing turn waits for the second.
/// One deadline bounds all `n` taps: every skipped line takes what
/// remains of it, never a fresh wait.
fn wait_interaction_nth(
    watcher: &mut log::Watcher,
    deadline: &Deadline,
    n: usize,
    what: &str,
) -> String {
    let mut found = String::new();
    let mut seen = 0;
    while seen < n {
        let line = watcher
            .recv_timeout(deadline.left())
            .unwrap_or_else(|| panic!("a line arrives before the deadline: {what}"))
            .expect("the log stays readable")
            .expect("no end yet");
        if line.kind == "interaction_requested"
            && let Some(request) = line.payload["request_id"].as_str()
        {
            found = request.to_owned();
            seen += 1;
        }
    }
    found
}

// A resumed interaction runs again with no second permission order: its
// call was already allowed before the exit.
#[test]
fn a_resumed_interaction_runs_with_no_second_permission() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").confirming());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let asked = wait_interaction(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the tool asks its question",
    );
    world.cancel.shutdown(0);
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    let outcome = join_turn(&mut world, flight);
    assert_eq!(outcome.unwrap(), None, "the shutdown unwinds the turn");
    exit_and_resume(&mut world, &asked, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let raised = wait_interaction_nth(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        2,
        "the question is raised again",
    );
    assert_eq!(raised, asked, "the same question is raised again");
    reply(&world, &raised, ReplyAnswer::Confirmed { confirmed: true });
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 2, "the call ran again with its answer");
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "only the live fast-path order is timed; the resumed run is not judged: {:?}",
        op_records(&world)
    );
}

// One entered review stage times its work, including the reviewer's
// provider call.
#[test]
fn a_first_stage_allow_times_one_reviewer_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![review_text("allow", 3)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1, "the allowed call ran");
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 3);
    assert_tool_wire(&records, "atlas");
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 1);
    assert_eq!(permission[0].ms, 3, "the order holds the stage");
    assert_eq!(
        resolved_bys(&world),
        vec![("allow".to_owned(), "reviewer".to_owned())]
    );
}

// A check carries the call to a second stage: two stages, two records.
#[test]
fn a_check_then_second_allow_times_two_reviewer_records() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![review_text("check", 3), review_text("allow: fine", 5)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1);
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|record| record.ms).collect::<Vec<_>>(),
        vec![3, 5],
        "each stage times only its own work"
    );
    assert_tool_wire(&records, "atlas");
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 1);
    assert_eq!(permission[0].ms, 8);
}

// An unreadable verdict is re-asked inside the same stage: still one record.
#[test]
fn an_unreadable_verdict_reasks_inside_one_stage_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![review_text("maybe", 3), review_text("allow", 3)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1, "the re-ask stays inside the stage");
    assert_eq!(records[0].ms, 6);
    assert_tool_wire(&records, "atlas");
}

// A failing first stage escalates to a person with its record kept.
#[test]
fn a_failing_first_stage_escalates_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![review_error(ErrorCode::Other("down".into()), 3)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the escalation is raised",
    );
    reply(&world, &request, deny());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1, "the attempted stage is timed");
    assert_eq!(records[0].ms, 3);
    assert_tool_wire(&records, "atlas");
}

// A failing second stage keeps both stages' records and escalates.
#[test]
fn a_failing_second_stage_keeps_both_records() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![
        review_text("check", 3),
        review_error(ErrorCode::Other("down".into()), 5),
    ]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the escalation is raised",
    );
    reply(&world, &request, deny());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|record| record.ms).collect::<Vec<_>>(),
        vec![3, 5]
    );
    assert_tool_wire(&records, "atlas");
}

// Cancelling a running stage completes the call cancelled with its record.
#[test]
fn cancelling_a_stage_ends_cancelled_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![ReviewReply::Hold]);
    let (entered_tx, entered_rx) = mpsc::channel();
    setup.reviewer_wake = Some(entered_tx);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({}), 0),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
            text_answer("Spare.", 0),
        ],
    );
    // Declared after `world`: a panic drops the sender first and frees
    // the held run before `world` drops.
    let (release_tx, release_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    world
        .reviewer
        .as_ref()
        .expect("a reviewer")
        .pause_hold(cancelled_tx, release_rx);
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the stage starts its request");
    assert!(
        world.cancel.cancel(),
        "the turn is armed while the stage runs"
    );
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    // The cancel is handled before the hold stores its release, so
    // the test never depends on the cancel landing after the store.
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&cancelled_rx, "the call sees the cancel");
    release_tx.send(()).expect("the hold waits");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0, "the cancelled call never ran");
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1, "the entered stage is timed");
    assert_tool_wire(&records, "atlas");
    assert!(
        kinds(&world).contains(&"tool_call_completed".to_owned()),
        "the cancelled call still completes"
    );
}

// A cancel that lands before the hold stores its release still ends
// the hold: the pause forces the early-cancel order on every run.
#[test]
fn a_cancel_before_the_hold_stores_its_release_still_ends_the_hold() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![ReviewReply::Hold]);
    let (entered_tx, entered_rx) = mpsc::channel();
    setup.reviewer_wake = Some(entered_tx);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({}), 0),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
            text_answer("Spare.", 0),
        ],
    );
    // Declared after `world`: a panic drops the sender first and frees
    // the held run before `world` drops.
    let (release_tx, release_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    world
        .reviewer
        .as_ref()
        .expect("a reviewer")
        .pause_hold(cancelled_tx, release_rx);
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the stage starts its request");
    assert!(
        world.cancel.cancel(),
        "the turn is armed while the stage runs"
    );
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&cancelled_rx, "the call sees the cancel");
    release_tx.send(()).expect("the hold waits");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0, "the cancelled call never ran");
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1, "the entered stage is timed");
    assert_tool_wire(&records, "atlas");
    assert!(
        kinds(&world).contains(&"tool_call_completed".to_owned()),
        "the cancelled call still completes"
    );
}

// A budget reached between stages times the first stage only.
#[test]
fn a_budget_reached_between_stages_times_only_the_first() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.budget = Some(1.0);
    setup.reviewer_cost = Some(contract::provider::Cost {
        input: 1_000_000.0,
        output: 1_000_000.0,
        cache_read: None,
        cache_write: None,
        tiers: Vec::new(),
    });
    setup.reviewer_script = Some(vec![review_text("check", 0), review_text("allow: fine", 0)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Failed));
    assert_eq!(tool.ran(), 0, "the over-budget call never ran");
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 1, "only the admitted stage is timed");
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "budget".to_owned())]
    );
}

// A budget already reached admits no stage and times nothing.
#[test]
fn a_budget_reached_before_any_stage_times_nothing() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.budget = Some(0.0);
    setup.reviewer_script = Some(vec![review_text("allow", 0)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Failed));
    assert_eq!(tool.ran(), 0);
    assert!(world.reviewer.as_ref().expect("a reviewer").called() == 0);
    assert!(
        ops(&world, "tool.reviewer").is_empty(),
        "a refused stage emits nothing"
    );
    assert!(
        resolved_bys(&world).is_empty(),
        "the step budget gate fails before any decision"
    );
}

// With no reviewer model the call escalates with no stage timer.
#[test]
fn no_reviewer_model_escalates_with_no_stage_timer() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.no_reviewer = true;
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the escalation is raised",
    );
    reply(&world, &request, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1);
    assert!(
        ops(&world, "tool.reviewer").is_empty(),
        "no stage ran, so no stage is timed"
    );
    assert_eq!(ops(&world, "tool.permission").len(), 1);
}

// A fast path runs no reviewer stage and times none.
#[test]
fn a_fast_path_runs_no_reviewer_stage() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert!(
        ops(&world, "tool.reviewer").is_empty(),
        "a fast path meets no reviewer"
    );
}

// A block escalated to a person splits cleanly: each stage keeps its own
// duration, and the permission order holds both stages and the wait.
#[test]
fn an_escalated_block_splits_stage_and_wait_durations() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_limits = Some((1, 20));
    setup.reviewer_script = Some(vec![review_text("check", 3), review_text("block: nope", 3)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let thread =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the escalation is raised",
    );
    world.clock.advance_when_parked(
        None,
        Some(thread),
        Duration::from_millis(11),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    reply(&world, &request, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(tool.ran(), 1);
    let records = ops(&world, "tool.reviewer");
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|record| record.ms).collect::<Vec<_>>(),
        vec![3, 3],
        "the human wait stays out of the stages"
    );
    assert_tool_wire(&records, "atlas");
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 1);
    assert_eq!(permission[0].ms, 17, "the order holds stages and wait");
}

// The handoff selection judges person messages through the same request
// path but times no tool stage.
#[test]
fn a_handoff_selection_reviews_with_no_tool_stage() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.reviewer_script = Some(vec![review_text("allow", 0), review_text("1", 0)]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "tool.reviewer").len(), 1);
    let turn_id = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .find(|line| line.kind == "turn_started")
        .expect("the turn started")
        .turn_id
        .expect("a turn id");
    world.loop_mut().reviewer_handoff(&turn_id).unwrap();
    assert_eq!(
        world.reviewer.as_ref().expect("a reviewer").called(),
        2,
        "the selection really asked the reviewer"
    );
    assert_eq!(
        ops(&world, "tool.reviewer").len(),
        1,
        "the selection times no tool stage"
    );
}

// An unchanged before and a passing check time one hook line each.
#[test]
fn unchanged_before_and_check_time_two_hook_lines() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        1,
        1,
        0,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1);
    let records = ops(&world, "tool.hooks");
    assert!(
        records.len() >= 2,
        "before and check dispatch: {:?}",
        records
    );
    assert_eq!(
        records
            .iter()
            .take(2)
            .map(|record| record.ms)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert_tool_wire(&records[..2], "atlas");
    assert_eq!(hooks.before_calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.check_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ops(&world, "tool.permission").len(), 1);
}

// A valid rewrite runs with the rewritten arguments and two hook lines.
#[test]
fn a_valid_rewrite_runs_rewritten_with_two_hook_lines() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object", "required": ["target"]}),
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
            review_when_confined: false,
            hosts: None,
        }),
        "read it",
    ));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Rewritten {
            arguments: Map::from_iter([("target".to_owned(), json!("hooked"))]),
            changed_by: vec!["probe".into()],
        },
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        1,
        1,
        0,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({"target": "orig"}), 0),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1);
    assert_eq!(
        tool.ran_args(),
        vec![Map::from_iter([("target".to_owned(), json!("hooked"))])],
        "the call runs with the rewritten arguments"
    );
    let records = ops(&world, "tool.hooks");
    assert!(records.len() >= 2);
    assert_tool_wire(&records[..2], "atlas");
    assert_eq!(ops(&world, "tool.permission").len(), 1);
}

// A before refusal emits its own dispatch line and never reaches check.
#[test]
fn a_before_refusal_emits_one_line_and_skips_check() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Refused(Refusal {
            extension: "probe".into(),
            reason: "not this call".into(),
        }),
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        1,
        0,
        0,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.hooks");
    assert_eq!(records.len(), 1, "only the entered before dispatch");
    assert_eq!(records[0].ms, 1);
    assert_tool_wire(&records, "atlas");
    assert_eq!(
        hooks.check_calls.load(Ordering::SeqCst),
        0,
        "check is not entered"
    );
    assert!(
        tool_records(&world)
            .iter()
            .all(|record| record.op == "tool.hooks")
    );
}

// A rewrite that fails its schema re-check emits before only.
#[test]
fn an_invalid_schema_rewrite_emits_before_only() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::classified(
        "atlas",
        json!({"type": "object", "required": ["target"]}),
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
            review_when_confined: false,
            hosts: None,
        }),
        "read it",
    ));
    let hooks = Arc::new(ProbeHooks::rewriting(Map::new()));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({"target": "x"}), 0),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert_eq!(ops(&world, "tool.hooks").len(), 1);
    assert_eq!(hooks.check_calls.load(Ordering::SeqCst), 0);
    assert!(
        tool_records(&world)
            .iter()
            .all(|record| record.op == "tool.hooks")
    );
}

// A rewrite that fails its effects re-check emits before only.
#[test]
fn an_invalid_effects_rewrite_emits_before_only() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(
        ProbeTool::classified(
            "atlas",
            json!({"type": "object"}),
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
                review_when_confined: false,
                hosts: None,
            }),
            "read it",
        )
        .booming("__boom__"),
    );
    let hooks = Arc::new(ProbeHooks::rewriting(Map::from_iter([(
        "__boom__".to_owned(),
        json!("boom"),
    )])));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert_eq!(ops(&world, "tool.hooks").len(), 1);
    assert_eq!(hooks.check_calls.load(Ordering::SeqCst), 0);
}

// A check refusal emits before and check, and never permission.
#[test]
fn a_check_refusal_emits_two_lines_and_no_permission() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Refused(Refusal {
            extension: "probe".into(),
            reason: "not this call".into(),
        }),
        AfterToolOutcome::Unchanged,
        1,
        1,
        0,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.hooks");
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|record| record.ms).collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert_tool_wire(&records, "atlas");
    assert!(ops(&world, "tool.permission").is_empty());
}

// A failing check emits before and check, and never permission.
#[test]
fn a_failing_check_emits_two_lines_and_no_permission() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Failed(HookFailure {
            extension: "probe".into(),
            message: "the check broke".into(),
        }),
        AfterToolOutcome::Unchanged,
        0,
        0,
        0,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 0);
    assert_eq!(ops(&world, "tool.hooks").len(), 2);
    assert!(ops(&world, "tool.permission").is_empty());
}

// A shutdown before any dispatch emits nothing.
#[test]
fn a_shutdown_before_any_dispatch_emits_nothing() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    world.cancel.shutdown(0);
    prompt(&world, "hi");
    let outcome = turn(&mut world);
    assert_eq!(outcome.unwrap(), None);
    assert_eq!(hooks.before_calls.load(Ordering::SeqCst), 0);
    assert!(tool_records(&world).is_empty());
}

// A shutdown between before and check emits before only.
#[test]
fn a_shutdown_between_before_and_check_emits_before_only() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut hooks = ProbeHooks::passing();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    *lock(&hooks.entered) = Some(entered_tx);
    *lock(&hooks.gate) = Some(gate_rx);
    hooks.before_ms = 1;
    let hooks = Arc::new(hooks);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the before hooks run");
    world.cancel.shutdown(0);
    gate_tx.send(()).expect("the hook waits on the gate");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0);
    let records = ops(&world, "tool.hooks");
    assert_eq!(records.len(), 1, "only the entered before dispatch");
    assert_eq!(records[0].ms, 1);
    assert_eq!(
        hooks.check_calls.load(Ordering::SeqCst),
        0,
        "check never runs"
    );
    assert!(ops(&world, "tool.permission").is_empty());
}

// Without hooks no dispatch is fabricated.
#[test]
fn without_hooks_no_dispatch_is_fabricated() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1);
    assert!(ops(&world, "tool.hooks").is_empty());
    assert_eq!(ops(&world, "tool.permission").len(), 1);
}

// An unchanged after dispatch adds the third hook line with its duration.
#[test]
fn an_unchanged_after_adds_the_third_hook_line() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 1);
    let records = ops(&world, "tool.hooks");
    assert_eq!(records.len(), 3, "before, check and the one after dispatch");
    assert_eq!(
        records.iter().map(|record| record.ms).collect::<Vec<_>>(),
        vec![0, 0, 1]
    );
    assert_tool_wire(&records, "atlas");
}

// A changed after dispatch keeps its single line and the turn completes.
#[test]
fn a_changed_after_keeps_its_single_line() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Changed {
            content: Some("replaced".into()),
            details: None,
            artifact: None,
        },
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "tool.hooks").len(), 3);
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 1);
}

// A failed after dispatch keeps its single line and fails the call.
#[test]
fn a_failed_after_keeps_its_single_line() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Failed(HookFailure {
            extension: "probe".into(),
            message: "the after hook broke".into(),
        }),
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "tool.hooks").len(), 3);
}

// A withheld after dispatch keeps its single line.
#[test]
fn a_withheld_after_keeps_its_single_line() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Withheld {
            extension: "probe".into(),
        },
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(ops(&world, "tool.hooks").len(), 3);
}

// A persisted spill dispatches after exactly once.
#[test]
fn a_persisted_spill_dispatches_after_once() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    tool.spilling(SpillMode::Persisted);
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ops(&world, "tool.hooks").len(),
        3,
        "before, check and the one spilled after dispatch"
    );
}

// An unsaved spill dispatches after exactly once.
#[test]
fn an_unsaved_spill_dispatches_after_once() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    tool.spilling(SpillMode::Unsaved);
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ops(&world, "tool.hooks").len(), 3);
}

// A spill the loop cannot clean up fails before any after dispatch.
#[test]
fn an_unclean_spill_fails_before_any_after_dispatch() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    tool.spilling(SpillMode::UnsavedBadLeft);
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let Err(error) = turn(&mut world) else {
        panic!("an unremovable spill fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(
        hooks.after_calls.load(Ordering::SeqCst),
        0,
        "no after dispatch"
    );
    assert_eq!(ops(&world, "tool.hooks").len(), 2, "before and check only");
}

// A shutdown suppresses the after dispatch: the completion carries no output.
#[test]
fn a_shutdown_suppresses_the_after_dispatch() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    world.cancel.shutdown(0);
    let looped = world.loop_mut();
    let shaped = looped
        .shape(
            crate::hooks::Ran {
                tool: "atlas",
                arguments: &Map::new(),
                status: contract::events::CallStatus::Completed,
                text: vec![contract::shapes::ContentPart::Text {
                    text: "read it".into(),
                }],
                images: Vec::new(),
                details: None,
                process: None,
                rewritten: None,
                spilled: None,
            },
            contract::tool::Bound::DEFAULT,
            &ActionId("a_test".into()),
            None,
        )
        .expect("a shutdown shapes empty output");
    assert!(shaped.content.is_empty());
    assert_eq!(
        hooks.after_calls.load(Ordering::SeqCst),
        0,
        "no after dispatch"
    );
    assert!(ops(&world, "tool.hooks").is_empty());
}

// Later cap work stays outside the closed after timer.
#[test]
fn later_cap_work_stays_outside_the_after_timer() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    arm_after_dispatch_advance(10);
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert!(
        world
            .clock
            .now()
            .checked_duration_since(world.origin)
            .unwrap_or_default()
            >= Duration::from_millis(10),
        "the armed boundary really advanced the clock"
    );
    let records = ops(&world, "tool.hooks");
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[2].ms, 1,
        "the timer closed before the later 10 ms of cap work"
    );
}

// A tool result carrying job records reaches the shared after dispatch once.
#[test]
fn a_job_bearing_result_reaches_one_after_dispatch() {
    use contract::JobId;
    use contract::events::{JobCompleted, Outcome};
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    tool.queue_jobs(vec![contract::jobs::JobRecord::Completed(JobCompleted {
        job_id: JobId("j_1".into()),
        status: Outcome::Completed,
        error: None,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    })]);
    let hooks = Arc::new(ProbeHooks::passing());
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ops(&world, "tool.hooks").len(),
        3,
        "before, check and the one shared after dispatch"
    );
    assert!(
        kinds(&world).contains(&"job_completed".to_owned()),
        "the job record is written with the result"
    );
}

// An autonomous job end writes its completion with no tool timer.
#[test]
fn an_autonomous_job_end_writes_no_tool_timer() {
    use contract::JobId;
    use contract::events::{JobCompleted, Outcome};
    use contract::inbox::{Claim, JobNotice};
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(&setup, home, vec![text_answer("Done.", 0)]);
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Job(JobNotice {
            completed: JobCompleted {
                job_id: JobId("j_1".into()),
                status: Outcome::Completed,
                error: None,
                process: None,
                output_tail: None,

                notice: None,
                changed_by: None,
                protected_paths: None,
            },
            claim: Claim(Box::new(|| Some(Vec::new()))),
            delegate: None,
        }))
        .unwrap();
    let flight = spawn_turn(&mut world);
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert!(
        tool_records(&world).is_empty(),
        "no tool ran, so no tool phase"
    );
    assert!(
        kinds(&world).contains(&"job_completed".to_owned()),
        "the autonomous end still completes"
    );
}

// An entered outer run times its work: the 2 ms hold sets the record.
#[test]
fn an_entered_outer_run_times_its_hold() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").hold_run());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    tool.gate_run(entered_tx, gate_rx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the run is entered");
    let worker = tool
        .run_threads()
        .into_iter()
        .next()
        .expect("the entered run reports its worker");
    assert!(
        ops(&world, "tool.run").is_empty(),
        "no record completes while the run is held"
    );
    world.clock.advance_when_parked(
        None,
        Some(worker),
        Duration::from_millis(2),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    gate_tx.send(()).expect("the run waits on the gate");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 2);
    assert_tool_wire(&records, "atlas");
}

// Synchronous tool work inside the run sets the record.
#[test]
fn synchronous_tool_work_sets_the_run_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ms, 2);
    assert_tool_wire(&records, "atlas");
}

// A tool error still closes its run record exactly once.
#[test]
fn a_tool_error_closes_its_run_record_once() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
    tool.failing_run("the ledger is locked");
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 1);
    assert_tool_wire(&records, "atlas");
}

// A cancel held out of the run completes the call cancelled with its record.
#[test]
fn a_cancel_mid_run_completes_cancelled_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").hold_run());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    tool.gate_run(entered_tx, gate_rx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the run is entered");
    assert!(
        world.cancel.cancel(),
        "the turn is armed while the run holds"
    );
    gate_tx.send(()).expect("the run waits on the gate");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 1, "the cancelled run was entered");
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 1, "the entered run is timed");
    assert_tool_wire(&records, "atlas");
    let completions: Vec<String> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "tool_call_completed")
        .map(|line| {
            line.payload["status"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(completions, vec!["cancelled".to_owned()]);
}

// A cancel before the run check starts nothing and times nothing.
#[test]
fn a_cancel_before_the_run_check_times_no_run() {
    let rules = Arc::new(FakeRules::default());
    rules.set(ask_rule("atlas", "atlas"));
    let tool = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the approval is raised",
    );
    assert!(
        world
            .clock
            .await_parked_unbounded(Deadline::after(MUST_SUCCEED_WITHIN)),
        "the approval wait parks on the clock"
    );
    assert!(world.cancel.cancel());
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::Cancelled)
        .unwrap();
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 0, "no run was entered");
    assert!(ops(&world, "tool.run").is_empty());
    assert!(
        !kinds(&world).contains(&"tool_call_started".to_owned()),
        "nothing started"
    );
}

// Two concurrent runs keep independent spans and complete in request order.
#[test]
fn concurrent_runs_keep_independent_spans_in_request_order() {
    let rules = Arc::new(FakeRules::default());
    let first = Arc::new(ProbeTool::reads("atlas").hold_run());
    let second = Arc::new(ProbeTool::reads("boreal").hold_run());
    let (rehold_tx, rehold_rx) = mpsc::channel();
    first.rehold_run(rehold_rx);
    let (first_tx, first_rx) = mpsc::channel();
    let (first_gate_tx, first_gate_rx) = mpsc::channel();
    first.gate_run(first_tx, first_gate_rx);
    let (second_tx, second_rx) = mpsc::channel();
    let (second_gate_tx, second_gate_rx) = mpsc::channel();
    second.gate_run(second_tx, second_gate_rx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&first));
    setup.tools.push(Arc::clone(&second));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer_two(("atlas", json!({})), ("boreal", json!({})), 0),
            text_answer("Done.", 0),
        ],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&first_rx, "the first run is entered");
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&second_rx, "the second run is entered");
    let first_worker = first
        .run_threads()
        .into_iter()
        .next()
        .expect("the first run reports its worker");
    let second_worker = second
        .run_threads()
        .into_iter()
        .next()
        .expect("the second run reports its worker");
    // The second finishes first: two shared milliseconds pass, then only
    // it returns. Its record lands while the first is still held.
    world.clock.advance_when_parked(
        None,
        Some(second_worker),
        Duration::from_millis(2),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    second_gate_tx.send(()).expect("the second run waits");
    // The first worker's span must not see the next advance: it lands
    // only after the second worker's guard dropped, proven by its record.
    let bound = Deadline::after(MUST_SUCCEED_WITHIN);
    loop {
        if ops(&world, "tool.run")
            .iter()
            .any(|record| record.tool.as_deref() == Some("boreal"))
        {
            break;
        }
        assert!(!bound.left().is_zero(), "the second run completes first");
        std::thread::yield_now();
    }
    rehold_tx.send(()).expect("the first run waits to re-park");
    world.clock.advance_when_parked(
        None,
        Some(first_worker),
        Duration::from_millis(3),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    first_gate_tx.send(()).expect("the first run waits");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 2);
    let boreal = records
        .iter()
        .find(|record| record.tool.as_deref() == Some("boreal"))
        .expect("the second run is timed");
    let atlas = records
        .iter()
        .find(|record| record.tool.as_deref() == Some("atlas"))
        .expect("the first run is timed");
    assert_eq!(boreal.ms, 2, "no buffering time in the second run");
    assert_eq!(atlas.ms, 5);
    assert_tool_wire(std::slice::from_ref(boreal), "boreal");
    assert_tool_wire(std::slice::from_ref(atlas), "atlas");
    // The second worker returned first, so its line precedes the first's.
    let boreal_at = op_records(&world)
        .iter()
        .position(|line| line.op == "tool.run" && line.tool.as_deref() == Some("boreal"));
    let atlas_at = op_records(&world)
        .iter()
        .position(|line| line.op == "tool.run" && line.tool.as_deref() == Some("atlas"));
    assert!(boreal_at < atlas_at, "the earlier return writes first");
    // Completions stay in request order, though the second run ended first.
    let completed: Vec<String> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "tool_call_completed")
        .map(|line| line.action_id.clone().map(|id| id.0).unwrap_or_default())
        .collect();
    assert_eq!(completed.len(), 2);
    let requested: Vec<String> = log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "tool_call_requested")
        .map(|line| line.action_id.clone().map(|id| id.0).unwrap_or_default())
        .collect();
    assert_eq!(completed, requested, "results return in request order");
}

/// Every `tool_call_completed` status in the session log, in order.
fn completed_statuses(world: &World) -> Vec<String> {
    log::read(&world.home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "tool_call_completed")
        .map(|line| {
            line.payload["status"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

// A consumer error after the run still leaves exactly one run record.
#[test]
fn a_consumer_error_after_the_run_keeps_one_record() {
    let rules = Arc::new(FakeRules::default());
    // A hook's artifact text is written after the worker returned; with
    // the artifacts path a file, that write fails the turn.
    let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Changed {
            content: None,
            details: None,
            artifact: Some("hook artifact".into()),
        },
        0,
        0,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    // Break the artifacts directory before the turn: the artifact write
    // fails, the run does not.
    let artifacts = world
        .log
        .as_ref()
        .expect("the log is open")
        .dir()
        .join("artifacts");
    if artifacts.exists() {
        std::fs::remove_dir_all(&artifacts).unwrap();
    }
    std::fs::write(&artifacts, b"not a directory").unwrap();
    prompt(&world, "hi");
    let Err(error) = turn(&mut world) else {
        panic!("a failed artifact write fails the turn");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    let records = ops(&world, "tool.run");
    assert_eq!(records.len(), 1, "the returned run is timed once");
    assert_tool_wire(&records, "atlas");
}

// An admitted inner call times its own permission order and run, with no
// reviewer and no person asked.
#[test]
fn an_admitted_inner_call_times_permission_and_run() {
    let rules = Arc::new(FakeRules::default());
    *lock(&rules.read_ms) = 5;
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal").with_work(2));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(outer.ran(), 1);
    assert_eq!(inner.ran(), 1, "the inner call ran");
    assert!(
        !kinds(&world).contains(&"permission_requested".to_owned()),
        "no reviewer and no person meet an inner call"
    );
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 2, "outer and inner orders");
    assert_eq!(
        permission[1].ms, 5,
        "the deny steps are inside the inner order"
    );
    assert_tool_wire(&permission[1..], "boreal");
    let runs = ops(&world, "tool.run");
    assert_eq!(runs.len(), 2);
    // The outer run holds the inner admission: 5 ms of deny steps plus
    // the inner run's own 2 ms. Each record names its own run.
    let outer_run = runs
        .iter()
        .find(|record| record.tool.as_deref() == Some("atlas"))
        .expect("the outer run is timed");
    let inner_run = runs
        .iter()
        .find(|record| record.tool.as_deref() == Some("boreal"))
        .expect("the inner run is timed");
    assert_eq!(outer_run.ms, 7);
    assert_eq!(inner_run.ms, 2);
    assert_tool_wire(std::slice::from_ref(inner_run), "boreal");
}

// An inner credential deny refuses with its own order record and no run.
#[test]
fn an_inner_credential_deny_refuses_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::classified(
        "boreal",
        json!({"type": "object"}),
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec!["../credentials".into()]),
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        }),
        "inner",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(inner.ran(), 0, "the refused inner call never ran");
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 2, "outer and refused inner orders");
    assert_tool_wire(&permission[1..], "boreal");
    assert!(
        ops(&world, "tool.run")
            .iter()
            .all(|record| record.tool.as_deref() == Some("atlas"))
    );
    assert_eq!(
        resolved_bys(&world),
        vec![("deny".to_owned(), "credential_deny".to_owned())]
    );
}

// An inner standing deny refuses with its own order record and no run.
#[test]
fn an_inner_standing_deny_refuses_with_its_record() {
    let rules = Arc::new(FakeRules::default());
    rules.set(deny_rule("boreal", "boreal"));
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::executes("boreal", "boreal run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(inner.ran(), 0);
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 2);
    assert_tool_wire(&permission[1..], "boreal");
}

// An inner call naming no tool enters no phase.
#[test]
fn an_inner_unknown_tool_enters_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("ghost", json!({})));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "the outer order only"
    );
    assert_eq!(ops(&world, "tool.run").len(), 1, "the outer run only");
}

// An inner call failing its schema enters no phase.
#[test]
fn an_inner_schema_failure_enters_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::classified(
        "boreal",
        json!({"type": "object", "required": ["target"]}),
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
            review_when_confined: false,
            hosts: None,
        }),
        "inner",
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(inner.ran(), 0);
    assert_eq!(ops(&world, "tool.permission").len(), 1);
    assert_eq!(ops(&world, "tool.run").len(), 1);
}

// A tool calling itself is refused with no phase of its own.
#[test]
fn a_self_call_is_refused_with_no_phase() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("atlas", json!({})));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(
        ops(&world, "tool.permission").len(),
        1,
        "the outer order only"
    );
    assert_eq!(ops(&world, "tool.run").len(), 1, "the outer run only");
}

// Eight nested levels run; the ninth is refused at the depth cap.
#[test]
fn eight_nested_levels_run_and_the_ninth_is_refused() {
    let rules = Arc::new(FakeRules::default());
    let left = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let right = Arc::new(ProbeTool::reads("boreal").calling("atlas", json!({})));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&left));
    setup.tools.push(Arc::clone(&right));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    // Five atlas runs (outer plus depths 2, 4, 6, 8) and four boreal runs.
    assert_eq!(left.ran(), 5);
    assert_eq!(right.ran(), 4);
    let permission = ops(&world, "tool.permission");
    assert_eq!(
        permission.len(),
        9,
        "the outer order and eight admitted inner orders"
    );
    let runs = ops(&world, "tool.run");
    assert_eq!(runs.len(), 9, "the outer run and eight admitted inner runs");
    for record in permission.iter().skip(1).chain(runs.iter().skip(1)) {
        assert!(
            record.tool.as_deref() == Some("atlas") || record.tool.as_deref() == Some("boreal"),
            "{:?}",
            record.raw
        );
    }
    assert!(
        log::read(&world.home.path().join("s_test"))
            .unwrap()
            .iter()
            .filter(|line| line.kind == "tool_call_completed")
            .any(|line| serde_json::to_string(&line.payload)
                .unwrap()
                .contains("depth exceeds 8")),
        "the ninth call is refused at the cap"
    );
}

// An inner worker that fails still closes its run record exactly once.
#[test]
fn an_inner_error_closes_its_run_record_once() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal").with_work(2));
    inner.failing_run("the boreal ledger is locked");
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(inner.ran(), 1);
    let runs = ops(&world, "tool.run");
    assert_eq!(runs.len(), 2);
    let inner_run = runs
        .iter()
        .find(|record| record.tool.as_deref() == Some("boreal"))
        .expect("the inner run is timed");
    assert_eq!(inner_run.ms, 2);
    assert_tool_wire(std::slice::from_ref(inner_run), "boreal");
    assert_eq!(
        completed_statuses(&world),
        vec!["failed".to_owned(), "completed".to_owned()]
    );
}

// Cancelling an inner run completes it cancelled with its records.
#[test]
fn cancelling_an_inner_run_completes_it_cancelled() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal").hold_run());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    inner.gate_run(entered_tx, gate_rx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the inner run is entered");
    assert!(world.cancel.cancel(), "the turn is armed");
    gate_tx.send(()).expect("the inner run waits");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(inner.ran(), 1, "the inner run was entered");
    let runs = ops(&world, "tool.run");
    assert_eq!(runs.len(), 2, "outer and entered inner runs");
    let inner_run = runs
        .iter()
        .find(|record| record.tool.as_deref() == Some("boreal"))
        .expect("the inner run is timed");
    assert_tool_wire(std::slice::from_ref(inner_run), "boreal");
    assert!(completed_statuses(&world).contains(&"cancelled".to_owned()));
}

// An inner call runs its entered hooks with their own lines.
#[test]
fn an_inner_call_runs_its_entered_hooks() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal"));
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        1,
        1,
        1,
    ));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    // Both calls run the full hook chain: two befores, two checks, two afters.
    assert_eq!(ops(&world, "tool.hooks").len(), 6);
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 2);
}

// Cancelling while an inner call is hooked ends it before any run.
#[test]
fn cancelling_a_hooked_inner_call_never_runs_it() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal"));
    let hooks = ProbeHooks::passing();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    *lock(&hooks.entered) = Some(entered_tx);
    *lock(&hooks.gate) = Some(gate_rx);
    let hooks = Arc::new(hooks);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    setup.hooks = Some(Arc::clone(&hooks));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    // The outer call's hooks run first; releasing them lets the inner
    // call reach its own hooks, where the cancel lands.
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "a before hook runs");
    gate_tx.send(()).expect("the hook waits");
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the inner before hook runs");
    assert!(world.cancel.cancel(), "the turn is armed");
    gate_tx.send(()).expect("the hook waits");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(inner.ran(), 0, "the cancelled inner call never ran");
    assert_eq!(
        ops(&world, "tool.permission").len(),
        2,
        "both orders entered"
    );
    assert_eq!(
        ops(&world, "tool.run").len(),
        1,
        "only the outer run was entered"
    );
}

// The three-tool world the returned-anchor tests drive: the middle
// call forgets the leaf and reports its answer, held until released.
struct ReturnedAnchor {
    world: World,
    leaf: Arc<ProbeTool>,
    leaf_entered_rx: mpsc::Receiver<()>,
    leaf_gate_tx: mpsc::Sender<()>,
    forget_tx: mpsc::Sender<()>,
    report_rx: mpsc::Receiver<String>,
}

fn returned_anchor() -> ReturnedAnchor {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let middle = Arc::new(ProbeTool::reads("boreal").forgetting("coral", json!({})));
    let leaf = Arc::new(ProbeTool::reads("coral"));
    let (forget_tx, forget_rx) = mpsc::channel();
    middle.hold_forget(forget_rx);
    let (leaf_entered_tx, leaf_entered_rx) = mpsc::channel();
    let (leaf_gate_tx, leaf_gate_rx) = mpsc::channel();
    leaf.gate_run(leaf_entered_tx, leaf_gate_rx);
    let (report_tx, report_rx) = mpsc::channel();
    middle.reporting(report_tx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&middle));
    setup.tools.push(Arc::clone(&leaf));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    ReturnedAnchor {
        world,
        leaf,
        leaf_entered_rx,
        leaf_gate_tx,
        forget_tx,
        report_rx,
    }
}

// A descendant that returns before its anchor does completes: the leaf
// answers while the held anchor is still alive, so the completion is
// written with the anchor unreturned.
#[test]
fn a_descendant_that_returns_before_its_anchor_completes() {
    let ReturnedAnchor {
        mut world,
        leaf,
        leaf_entered_rx,
        leaf_gate_tx,
        forget_tx,
        report_rx,
    } = returned_anchor();
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    // The leaf runs and answers while the held anchor is unreturned.
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&leaf_entered_rx, "the leaf run is entered");
    leaf_gate_tx.send(()).expect("the leaf run waits");
    let report = Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&report_rx, "the leaf answers");
    assert_eq!(
        report, "answered Completed",
        "a leaf that beats its anchor completes"
    );
    forget_tx.send(()).expect("the forgetter waits");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(leaf.ran(), 1, "the descendant run was entered");
    assert_eq!(ops(&world, "tool.permission").len(), 3);
    assert_eq!(ops(&world, "tool.run").len(), 3);
}

// A returned anchor cancels its still-running descendant: the middle
// call returns while the leaf still runs, and the leaf is cancelled.
#[test]
fn a_returned_anchor_cancels_its_running_descendant() {
    let ReturnedAnchor {
        mut world,
        leaf,
        leaf_entered_rx,
        leaf_gate_tx,
        forget_tx,
        report_rx,
    } = returned_anchor();
    let (seen_tx, seen_rx) = mpsc::channel();
    leaf.watch_cancel(seen_tx);
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    // The leaf is admitted while its anchor still runs, then the anchor
    // returns while the leaf still runs.
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&leaf_entered_rx, "the leaf run is entered");
    forget_tx.send(()).expect("the forgetter waits");
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&seen_rx, "the leaf sees its cancel");
    leaf_gate_tx.send(()).expect("the leaf run waits");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(leaf.ran(), 1, "the descendant run was entered");
    let report = Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&report_rx, "the leaf answers");
    assert_eq!(
        report, "answered Cancelled",
        "the orphaned leaf is cancelled"
    );
    // All three orders were entered; all three runs started.
    assert_eq!(ops(&world, "tool.permission").len(), 3);
    assert_eq!(ops(&world, "tool.run").len(), 3);
}

// A descendant released before its anchor returns is still cancelled:
// the leaf waits for its cancel ahead of its gate on every order of
// the test's two releases.
#[test]
fn a_returned_anchor_cancels_a_descendant_released_early() {
    let ReturnedAnchor {
        mut world,
        leaf,
        leaf_entered_rx,
        leaf_gate_tx,
        forget_tx,
        report_rx,
    } = returned_anchor();
    let (seen_tx, seen_rx) = mpsc::channel();
    leaf.watch_cancel(seen_tx);
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&leaf_entered_rx, "the leaf run is entered");
    // The old order: the gate is already queued when the anchor returns.
    leaf_gate_tx.send(()).expect("the leaf run waits");
    forget_tx.send(()).expect("the forgetter waits");
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&seen_rx, "the leaf sees its cancel");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(leaf.ran(), 1, "the descendant run was entered");
    let report = Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&report_rx, "the leaf answers");
    assert_eq!(
        report, "answered Cancelled",
        "the early leaf is cancelled too"
    );
    assert_eq!(ops(&world, "tool.permission").len(), 3);
    assert_eq!(ops(&world, "tool.run").len(), 3);
}

// A command's inner call runs alone with its own order and run.
#[test]
fn a_command_inner_call_runs_alone_with_its_timers() {
    use contract::inbox::InnerCallRequest;
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(&setup, home, vec![text_answer("Done.", 0)]);
    let (answer_tx, answer_rx) = mpsc::channel();
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::InnerCall(InnerCallRequest {
            extension: "probe".into(),
            command: CommandId("c_1".into()),
            name: "atlas".into(),
            arguments: json!({}),
            cancel: Arc::new(fakes::CancelToken::new()),
            taken: Box::new(|| {}),
            answer: Box::new(move |done| {
                let _sent = answer_tx.send(done.status);
            }),
        }))
        .unwrap();
    // The command runs in the idle drain; the prompt starts the turn that
    // follows it.
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 1);
    let status =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&answer_rx, "the command answers");
    assert_eq!(status, contract::events::CallStatus::Completed);
    let permission = ops(&world, "tool.permission");
    assert_eq!(permission.len(), 1);
    assert_tool_wire(&permission, "atlas");
    let runs = ops(&world, "tool.run");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].ms, 2);
    assert_tool_wire(&runs, "atlas");
}

// Cancelling a command's inner run completes it cancelled with its timers.
#[test]
fn cancelling_a_command_inner_run_completes_it_cancelled() {
    use contract::events::CallStatus;
    use contract::inbox::InnerCallRequest;
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").hold_run());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    tool.gate_run(entered_tx, gate_rx);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(&setup, home, vec![text_answer("Done.", 0)]);
    let token = Arc::new(fakes::CancelToken::new());
    let (answer_tx, answer_rx) = mpsc::channel();
    world
        .inbox_tx
        .as_ref()
        .expect("the inbox is open")
        .send(Delivery::InnerCall(InnerCallRequest {
            extension: "probe".into(),
            command: CommandId("c_1".into()),
            name: "atlas".into(),
            arguments: json!({}),
            cancel: Arc::clone(&token) as Arc<dyn contract::tool::Cancel>,
            taken: Box::new(|| {}),
            answer: Box::new(move |done| {
                let _sent = answer_tx.send(done.status);
            }),
        }))
        .unwrap();
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&entered_rx, "the command run is entered");
    token.cancel();
    gate_tx.send(()).expect("the run waits");
    assert!(join_turn(&mut world, flight).is_ok());
    assert_eq!(tool.ran(), 1, "the run was entered");
    let status =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&answer_rx, "the command answers");
    assert_eq!(status, CallStatus::Cancelled);
    assert_eq!(ops(&world, "tool.permission").len(), 1);
    assert_eq!(ops(&world, "tool.run").len(), 1);
}

// A closed slot answers cancelled without any worker or timer.
#[test]
fn a_closed_slot_answers_cancelled_without_a_worker() {
    use contract::events::CallStatus;
    use contract::tool::InnerCalls;
    let wake = Arc::new(crate::progress::SharedWake::default());
    let slot =
        crate::inner_calls::InnerSlot::new(wake, ActionId("a_test".into()), "atlas".into(), 0);
    slot.close();
    let done = slot
        .run("probe", "boreal", json!({}))
        .expect("a closed slot still answers");
    assert_eq!(done.status, CallStatus::Cancelled);
}

// At `info` the same outer work runs with no timer and no diagnostic file.
#[test]
fn info_runs_outer_work_with_no_timer() {
    let rules = Arc::new(FakeRules::default());
    let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&tool));
    setup.debug = false;
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran(), 1, "untimed work still runs");
    assert!(tool_records(&world).is_empty());
    assert!(
        !world
            .home
            .path()
            .join("logs")
            .join("session-s_test.log")
            .exists(),
        "no diagnostic file is written at info"
    );
    assert!(kinds(&world).contains(&"tool_call_completed".to_owned()));
}

// At `info` the same inner work runs with no timer and no worker context.
#[test]
fn info_runs_inner_work_with_no_timer() {
    let rules = Arc::new(FakeRules::default());
    let outer = Arc::new(ProbeTool::reads("atlas").calling("boreal", json!({})));
    let inner = Arc::new(ProbeTool::reads("boreal").with_work(2));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&outer));
    setup.tools.push(Arc::clone(&inner));
    setup.debug = false;
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)],
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(inner.ran(), 1, "untimed inner work still runs");
    assert!(tool_records(&world).is_empty());
}

// A failed diagnostics file changes neither outputs nor event sequence.
#[test]
fn a_failed_diagnostics_file_changes_no_output() {
    fn script() -> Vec<Answer> {
        vec![tool_answer("atlas", json!({}), 0), text_answer("Done.", 0)]
    }
    let run = |broken: bool| {
        let rules = Arc::new(FakeRules::default());
        let tool = Arc::new(ProbeTool::reads("atlas").with_work(2));
        let mut setup = Setup::new(Arc::clone(&rules));
        setup.tools.push(Arc::clone(&tool));
        let home = fakes::TempDir::new("fiber-tool-timing");
        let mut world = build(&setup, home, script());
        if broken {
            // The diagnostic file path becomes a directory: every
            // diagnostic write fails silently.
            let path = world.home.path().join("logs").join("session-s_test.log");
            drop(std::fs::remove_file(&path));
            std::fs::create_dir_all(world.home.path().join("logs")).unwrap();
            std::fs::create_dir(&path).unwrap();
        }
        prompt(&world, "hi");
        let outcome = turn(&mut world).unwrap();
        let kinds = kinds(&world);
        (outcome, kinds, tool.ran())
    };
    // The diagnostic file may not exist yet before the first write: run
    // the control first for its kinds, then the broken run.
    let (control_outcome, control_kinds, control_ran) = run(false);
    let (outcome, kinds, ran) = run(true);
    assert_eq!(outcome, control_outcome);
    assert_eq!(kinds, control_kinds, "the event sequence is unchanged");
    assert_eq!(ran, control_ran);
}

// An observed entry means the release is already queued: the hold queues
// its release before it signals entry. The pause parks the held call
// between the queue and the signal, so the pop below runs while the old
// signal-then-queue order would still show an empty queue: it fails there
// and passes here, with no wall-clock wait.
// A reviewer hold answers one tool call, then the turn completes.
#[test]
fn an_entered_hold_already_queues_its_release() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.reviewer_script = Some(vec![ReviewReply::HoldAnswer]);
    let (entered_tx, entered_rx) = mpsc::channel();
    setup.reviewer_hold_entered = Some(entered_tx);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer("atlas", json!({}), 0),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
        ],
    );
    let judging = Arc::clone(world.reviewer.as_ref().expect("a reviewer"));
    let (parked_tx, parked_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    judging.pause_held(parked_tx, gate_rx);
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    deadline.recv_or_fail(&parked_rx, "the held call reaches its release pause");
    let release = lock(&judging.hold_releases)
        .pop_front()
        .expect("an entered hold already queues its release");
    let _sent = gate_tx.send(());
    deadline.recv_or_fail(&entered_rx, "the held call enters");
    release
        .send(review_answers(&[review_text("allow", 0)]).remove(0))
        .expect("the hold waits");
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(atlas.ran(), 1);
}

// Three stage-1 requests fly together and resolve in request order: with
// every first stage held, all three entries land before any answer
// returns; releasing in reverse arrival order still resolves 1-2-3 before
// any `tool_call_started`.
#[test]
fn three_first_stages_fly_together_and_resolve_in_request_order() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let bruno = Arc::new(ProbeTool::executes("bruno", "bruno run"));
    let coral = Arc::new(ProbeTool::executes("coral", "coral run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.tools.push(Arc::clone(&bruno));
    setup.tools.push(Arc::clone(&coral));
    setup.reviewer_script = Some(vec![
        ReviewReply::HoldAnswer,
        ReviewReply::HoldAnswer,
        ReviewReply::HoldAnswer,
    ]);
    let (entered_tx, entered_rx) = mpsc::channel();
    setup.reviewer_hold_entered = Some(entered_tx);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer_three(
                ("atlas", json!({})),
                ("bruno", json!({})),
                ("coral", json!({})),
                0,
            ),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
            text_answer("Spare.", 0),
        ],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    for _ in 0..3 {
        deadline.recv_or_fail(&entered_rx, "each first stage enters");
    }
    let judging = world.reviewer.as_ref().expect("a reviewer");
    assert_eq!(
        judging.called(),
        3,
        "all three first stages are in flight together"
    );
    // Releases go out in reverse arrival order; the resolutions must
    // still come out in request order.
    let mut answers = review_answers(&[
        review_text("allow", 1),
        review_text("allow", 1),
        review_text("allow", 1),
    ]);
    for _ in 0..3 {
        let release = lock(&judging.hold_releases)
            .pop_back()
            .expect("each hold queues its release");
        release
            .send(answers.pop().expect("one answer per hold"))
            .expect("the hold waits");
    }
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(atlas.ran(), 1);
    assert_eq!(bruno.ran(), 1);
    assert_eq!(coral.ran(), 1);
    // The resolutions name the calls in request order, even though the
    // answers arrived in reverse.
    let lines = log::read(&world.home.path().join("s_test")).unwrap();
    let requested: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_requested")
        .map(|line| line.action_id.clone().map(|id| id.0).unwrap_or_default())
        .collect();
    let resolved: Vec<(String, String)> = lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .map(|line| {
            (
                line.action_id.clone().map(|id| id.0).unwrap_or_default(),
                line.payload["decision"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert_eq!(requested.len(), 3);
    assert_eq!(
        resolved,
        requested
            .into_iter()
            .map(|id| (id, "allow".to_owned()))
            .collect::<Vec<_>>(),
        "every resolution allows its call in request order"
    );
    assert_eq!(
        kinds(&world),
        vec![
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        "resolutions stay in request order before any start",
    );
}

// Carried hook state stays with its already-decided call: with the batch's
// first call already decided and carrying state, and two more calls still
// to judge, the state is written before the first call's start, not the
// second's. Matching any other action would hand the first call's state to
// the second call.
#[test]
fn carried_hook_state_stays_with_its_already_decided_call() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::reads("atlas"));
    let bruno = Arc::new(ProbeTool::reads("bruno"));
    let coral = Arc::new(ProbeTool::reads("coral"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.tools.push(Arc::clone(&bruno));
    setup.tools.push(Arc::clone(&coral));
    setup.hooks = Some(Arc::new(ProbeHooks::passing()));
    setup.reviewer_script = Some(Vec::new());
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(&setup, home, Vec::new());
    let turn = TurnId("t_test".into());
    let call = |name: &str| ToolCallRequested {
        name: name.into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    };
    let prepared = |tool: &Arc<ProbeTool>| crate::before_tool::Prepared {
        tool: Arc::clone(tool) as Arc<dyn Tool>,
        arguments: Map::new(),
        effects: Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        },
        rewritten: None,
    };
    let id_a = ActionId("a_a".into());
    let id_b = ActionId("a_b".into());
    let id_c = ActionId("a_c".into());
    let state = vec![StateLine {
        extension: "probe.test".into(),
        key: "carried".into(),
        value: Some((json!("kept"), OnFork::AtPoint)),
        landing: 0,
    }];
    let done = world
        .loop_mut()
        .run_batch(
            vec![
                (id_a.clone(), call("atlas"), Some(Ok(prepared(&atlas)))),
                (id_b.clone(), call("bruno"), None),
                (id_c.clone(), call("coral"), None),
            ],
            &turn,
            None,
            Some((id_a.clone(), state)),
        )
        .expect("the batch runs");
    assert!(!done, "no cancel ends the batch");
    let lines = log::read(&world.home.path().join("s_test")).unwrap();
    let started: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_started")
        .map(|line| line.action_id.clone().map(|id| id.0).unwrap_or_default())
        .collect();
    assert_eq!(
        started,
        vec!["a_a".to_owned(), "a_b".to_owned(), "a_c".to_owned()],
        "every call runs in request order"
    );
    let state_at = lines
        .iter()
        .position(|line| line.kind == "extension_state_set")
        .expect("the carried state is written");
    assert_eq!(lines[state_at].payload["key"], json!("carried"));
    let started_a_at = lines
        .iter()
        .position(|line| {
            line.kind == "tool_call_started"
                && line.action_id.as_ref().is_some_and(|id| id.0 == "a_a")
        })
        .expect("the first call starts");
    assert!(
        state_at < started_a_at,
        "the carried state lands before its call's start, not the next call's"
    );
}

// An idle unwind commits the cursor past the applied first stage: with the
// first call allowed and the second escalating into an idle wait, the shared
// cursor still advances past the applied cuts, so the next review chains
// from them. Skipping the commit would leave the cursor behind.
#[test]
fn an_idle_unwind_commits_the_cursor_past_the_applied_first_stage() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let bruno = Arc::new(ProbeTool::executes("bruno", "bruno run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.tools.push(Arc::clone(&bruno));
    setup.reviewer_script = Some(vec![
        review_text("allow", 0),
        review_text("check", 0),
        review_text("block its reason", 0),
    ]);
    setup.reviewer_limits = Some((1, 20));
    setup.idle = Some(IDLE);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![tool_answer_two(
            ("atlas", json!({})),
            ("bruno", json!({})),
            0,
        )],
    );
    prompt(&world, "hi");
    suspend_on_approval(&mut world);
    assert!(
        world.loop_mut().reviewer_sent.is_some(),
        "the unwind commits the cursor past the applied first stage"
    );
}

// A block on the second of three calls counts first: with the consecutive
// limit at two, the third call's block escalates. Stage 1 fans out, so
// its replies are uniform checks and the verdicts land serially at
// stage 2 in request order.
#[test]
fn a_second_blocks_count_lands_the_escalation_on_the_third_call() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let bruno = Arc::new(ProbeTool::executes("bruno", "bruno run"));
    let coral = Arc::new(ProbeTool::executes("coral", "coral run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.tools.push(Arc::clone(&bruno));
    setup.tools.push(Arc::clone(&coral));
    setup.reviewer_script = Some(vec![
        review_text("check", 1),
        review_text("check", 1),
        review_text("check", 1),
        review_text("allow", 1),
        review_text("block second reason", 1),
        review_text("block third reason", 1),
    ]);
    setup.reviewer_limits = Some((2, 20));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer_three(
                ("atlas", json!({})),
                ("bruno", json!({})),
                ("coral", json!({})),
                0,
            ),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
            text_answer("Spare.", 0),
        ],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the third call escalates",
    );
    reply(&world, &request, allow());
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(atlas.ran(), 1);
    assert_eq!(bruno.ran(), 0, "the denied call never ran");
    assert_eq!(coral.ran(), 1);
    let lines = log::read(&world.home.path().join("s_test")).unwrap();
    assert_eq!(
        kinds(&world),
        vec![
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "usage_recorded",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
        "each call applies in request order with its own usages",
    );
    let requested: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|line| line.kind == "permission_requested")
        .collect();
    assert_eq!(requested.len(), 1, "only the third call escalates");
    assert_eq!(
        requested[0].payload["escalation"],
        json!({"cause": "consecutive_blocks", "reason": "third reason"}),
    );
    let resolved: Vec<&str> = lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .map(|line| line.payload["decision"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(resolved, vec!["allow", "deny", "allow"]);
    assert_eq!(
        world.reviewer.as_ref().expect("a reviewer").called(),
        6,
        "three first stages plus three second stages",
    );
}

// The cache chain survives the batch: the same three-call reply plus a
// follow-up review records the same cache counts, chains the same
// `previous_end` values and lands the same `reviewer_sent` cursor whether
// the reply fans out (no budget) or takes the serial path (budget set).
#[test]
fn a_mixed_batch_matches_the_serial_event_stream() {
    struct Run {
        kinds: Vec<String>,
        resolved: Vec<(String, String)>,
        sends: usize,
        first_prevs: Vec<Option<usize>>,
        second_prevs: Vec<Option<usize>>,
        follow_prev: Option<usize>,
        follow_len: usize,
        follow_key: String,
    }

    fn run(budget: Option<f64>, script: Vec<ReviewReply>) -> Run {
        let rules = Arc::new(FakeRules::default());
        rules.set(deny_rule("bruno", "bruno"));
        let mut setup = Setup::new(Arc::clone(&rules));
        setup
            .tools
            .push(Arc::new(ProbeTool::executes("atlas", "atlas run")));
        setup
            .tools
            .push(Arc::new(ProbeTool::executes("bruno", "bruno run")));
        setup
            .tools
            .push(Arc::new(ProbeTool::executes("coral", "coral run")));
        setup.reviewer_script = Some(script);
        setup.budget = budget;
        let home = fakes::TempDir::new("fiber-tool-timing");
        let mut world = build(
            &setup,
            home,
            vec![
                tool_answer_three(
                    ("atlas", json!({})),
                    ("bruno", json!({})),
                    ("coral", json!({})),
                    0,
                ),
                text_answer("Done.", 0),
                tool_answer("atlas", json!({}), 0),
                text_answer("Done.", 0),
                text_answer("Spare.", 0),
            ],
        );
        prompt(&world, "hi");
        let outcome = turn(&mut world).unwrap();
        assert_eq!(outcome, Some(TurnOutcome::Completed));
        let kinds = kinds(&world);
        let lines = log::read(&world.home.path().join("s_test")).unwrap();
        let resolved: Vec<(String, String)> = lines
            .iter()
            .filter(|line| line.kind == "permission_resolved")
            .map(|line| {
                (
                    line.payload["decision"].as_str().unwrap_or("").to_owned(),
                    line.payload["decided_by"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        let sends = world.reviewer.as_ref().expect("a reviewer").called();
        // The cache chain each request names: first stages fan out in
        // arrival order, so their ends compare as a set; second stages
        // pre-send in request order. Both must match the serial path.
        let mut first_prevs: Vec<Option<usize>> = Vec::new();
        let mut second_prevs: Vec<Option<usize>> = Vec::new();
        for request in world.reviewer.as_ref().expect("a reviewer").requests() {
            // First stages cap their reply; second stages do not.
            if request.max_output_tokens.is_some() {
                first_prevs.push(request.previous_end);
            } else {
                second_prevs.push(request.previous_end);
            }
        }
        first_prevs.sort();
        // The next reply's review must chain past the batch exactly as it
        // chains past the serial path: one more reviewed call proves the
        // `reviewer_sent` cursor and the cache prefix match, not just the
        // batch's own ends.
        prompt(&world, "again");
        let follow_outcome = turn(&mut world).unwrap();
        assert_eq!(follow_outcome, Some(TurnOutcome::Completed));
        let follow = world
            .reviewer
            .as_ref()
            .expect("a reviewer")
            .requests()
            .last()
            .cloned()
            .expect("the follow-up review sends");
        assert!(
            follow.max_output_tokens.is_some(),
            "the follow-up review is a first stage"
        );
        Run {
            kinds,
            resolved,
            sends,
            first_prevs,
            second_prevs,
            follow_prev: follow.previous_end,
            follow_len: follow.conversation.len(),
            follow_key: follow.cache_key,
        }
    }
    let check = review_text("check", 1);
    let allow = review_text("allow", 1);
    let block = review_text("block coral reason", 1);
    let batch = run(
        None,
        vec![
            check.clone(),
            check.clone(),
            allow.clone(),
            block.clone(),
            allow.clone(),
        ],
    );
    let serial = run(
        Some(1000.0),
        vec![
            check.clone(),
            allow.clone(),
            check.clone(),
            block.clone(),
            allow.clone(),
        ],
    );
    assert_eq!(batch.sends, 4, "the denied call sends nothing");
    assert_eq!(serial.sends, 4);
    assert_eq!(
        batch.resolved,
        vec![
            ("allow".to_owned(), "reviewer".to_owned()),
            ("deny".to_owned(), "standing_rule".to_owned()),
            ("deny".to_owned(), "reviewer".to_owned()),
        ],
        "allow, standing deny, reviewer deny in request order",
    );
    assert_eq!(serial.resolved, batch.resolved);
    assert_eq!(
        batch.kinds, serial.kinds,
        "the batch's event stream matches the serial path exactly"
    );
    assert_eq!(
        batch.first_prevs, serial.first_prevs,
        "the fanned-out first stages keep the serial cache chain"
    );
    assert_eq!(
        batch.second_prevs, serial.second_prevs,
        "the pre-sent second stages keep the serial cache chain"
    );
    assert_eq!(
        batch.follow_prev, serial.follow_prev,
        "the follow-up review chains past the same cursor"
    );
    assert_eq!(
        batch.follow_len, serial.follow_len,
        "the follow-up review sends the same cached prefix"
    );
    assert_eq!(
        batch.follow_key, serial.follow_key,
        "the follow-up review keeps the serial cache key"
    );
}

// A grant installed mid-batch re-judges the calls still waiting, in
// request order: the first call blocks, escalates and is allowed with a
// remembered prefix; the second still needs its review; the third is the
// same tool under the grant, so its fanned-out second stage is discarded
// and its recorded first-stage usage stays. Six requests go out either
// way.
#[test]
fn a_grant_remembers_across_the_rest_of_the_batch() {
    let rules = Arc::new(FakeRules::default());
    let atlas = Arc::new(ProbeTool::executes("atlas", "atlas run"));
    let bruno = Arc::new(ProbeTool::executes("bruno", "bruno run"));
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.tools.push(Arc::clone(&atlas));
    setup.tools.push(Arc::clone(&bruno));
    setup.reviewer_script = Some(vec![
        review_text("check", 1),
        review_text("check", 1),
        review_text("check", 1),
        review_text("block atlas reason", 1),
        review_text("allow", 1),
        review_text("allow", 1),
    ]);
    setup.reviewer_limits = Some((1, 20));
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer_three(
                ("atlas", json!({})),
                ("bruno", json!({})),
                ("atlas", json!({})),
                0,
            ),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
        ],
    );
    prompt(&world, "hi");
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(
        &mut watcher,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        "the first call escalates",
    );
    reply(
        &world,
        &request,
        ReplyAnswer::Approval {
            decision: Decision::Allow,
            feedback: None,
            remember: Some(Remember {
                scope: RememberScope::Session,
                prefix: "atlas".into(),
            }),
        },
    );
    assert_eq!(
        join_turn(&mut world, flight).unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(atlas.ran(), 2, "both atlas calls ran");
    assert_eq!(bruno.ran(), 1);
    let lines = log::read(&world.home.path().join("s_test")).unwrap();
    let resolved: Vec<(String, String)> = lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .map(|line| {
            (
                line.payload["decision"].as_str().unwrap_or("").to_owned(),
                line.payload["decided_by"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        resolved,
        vec![
            ("allow".to_owned(), "person".to_owned()),
            ("allow".to_owned(), "reviewer".to_owned()),
            ("allow".to_owned(), "session_grant".to_owned()),
        ],
        "the grant decides the third call",
    );
    assert_eq!(
        world.reviewer.as_ref().expect("a reviewer").called(),
        6,
        "all six requests went out, the third's second stage discarded",
    );
    // The third call's first-stage usage is recorded under its own action,
    // even though its second stage never ran.
    let requested: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_requested")
        .map(|line| line.action_id.clone().map(|id| id.0).unwrap_or_default())
        .collect();
    let third = requested.get(2).expect("three calls");
    let third_usages: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|line| {
            line.kind == "usage_recorded"
                && line.payload.get("reviewer").is_some_and(|reviewer| {
                    reviewer["purpose"] == "stage_1" && reviewer["action_id"] == *third
                })
        })
        .collect();
    assert_eq!(
        third_usages.len(),
        1,
        "the mooted first stage still records its usage"
    );
    assert_eq!(kinds(&world).len(), lines.len());
}

// A reply's hook notices come first in request order, then its
// `permission_resolved` lines (`docs/permissions.md`, "What is recorded"):
// preparing every call runs its `before_tool` hooks before the first
// stage fans out, so no fanned-out usage can slip ahead of a notice.
#[test]
fn hook_notices_come_before_the_fanned_out_usages() {
    let rules = Arc::new(FakeRules::default());
    let hooks = Arc::new(ProbeHooks::staged(
        BeforeToolOutcome::Unchanged,
        CheckOutcome::Passed,
        AfterToolOutcome::Unchanged,
        0,
        0,
        0,
    ));
    hooks.notice_each.store(true, Ordering::SeqCst);
    let mut setup = Setup::new(Arc::clone(&rules));
    setup.hooks = Some(Arc::clone(&hooks));
    setup
        .tools
        .push(Arc::new(ProbeTool::executes("atlas", "atlas run")));
    setup
        .tools
        .push(Arc::new(ProbeTool::executes("bruno", "bruno run")));
    setup
        .tools
        .push(Arc::new(ProbeTool::executes("coral", "coral run")));
    setup.reviewer_script = Some(vec![
        review_text("check", 1),
        review_text("check", 1),
        review_text("check", 1),
        review_text("allow", 1),
        review_text("allow", 1),
        review_text("allow", 1),
    ]);
    let home = fakes::TempDir::new("fiber-tool-timing");
    let mut world = build(
        &setup,
        home,
        vec![
            tool_answer_three(
                ("atlas", json!({})),
                ("bruno", json!({})),
                ("coral", json!({})),
                0,
            ),
            text_answer("Done.", 0),
            text_answer("Spare.", 0),
        ],
    );
    // Notices are ephemeral: they stream but never reach the durable
    // log, so a watcher registered before the turn captures them.
    let mut watcher = world.log.as_ref().expect("the log is open").watch();
    prompt(&world, "hi");
    assert_eq!(turn(&mut world).unwrap(), Some(TurnOutcome::Completed));
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let mut streamed = Vec::new();
    while let Some(line) = watcher
        .recv_timeout(deadline.left())
        .unwrap_or_else(|| panic!("the stream ends after the turn"))
        .expect("no end yet")
    {
        let last = line.kind == "turn_completed";
        streamed.push(line);
        if last {
            break;
        }
    }
    let notices: Vec<String> = streamed
        .iter()
        .filter(|line| line.kind == "notice")
        .map(|line| line.payload["message"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(
        notices,
        vec![
            "hook saw atlas".to_owned(),
            "hook saw bruno".to_owned(),
            "hook saw coral".to_owned(),
        ],
        "one notice per call, in request order",
    );
    let first_usage = streamed
        .iter()
        .position(|line| line.kind == "usage_recorded" && line.payload.get("reviewer").is_some())
        .expect("the fanned-out stages record usage");
    let last_notice = streamed
        .iter()
        .rposition(|line| line.kind == "notice")
        .expect("hooks noticed");
    assert!(
        last_notice < first_usage,
        "every hook notice comes before any fanned-out usage",
    );
}
