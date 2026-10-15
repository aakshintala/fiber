//! Part 8 tool phase timers (`docs/state.md`, "Diagnostic logs"): one
//! `op_timing` line per entered tool phase through the one installed timer.
//!
//! Time comes from the injected clock: synchronous fake work inside the
//! rules, the provider, the hooks and the tool sets each duration, and a
//! test advances the fake clock during a person's wait only after the wait
//! parks on it. Every receive, join and log tap carries one named 5 s
//! deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    AskStep, Decision, Event, FiberExited, Interaction, PermissionRequested, RuleScope,
    StandingRule, ToolCallRequested, TurnOutcome,
};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeToolAnswer, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, Hooks, Refusal,
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
use log::Log;
use serde_json::{Map, Value, json};

use crate::{BlockLimits, Error, Loop, Model};

/// How long a test waits for a turn, a request or a parked wait: the one
/// named deadline every receive and join in this file carries.
const WAIT: Duration = Duration::from_secs(5);

/// The idle delay a suspending turn waits out.
const IDLE: Duration = Duration::from_secs(60);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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

/// One scripted provider answer: the reply, after synchronous fake work of
/// `advance_ms`, all inside the timed model call.
struct Answer {
    deltas: Vec<Delta>,
    end: Result<Reply, CallError>,
    advance_ms: u64,
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
    Answer {
        deltas,
        end,
        advance_ms,
    }
}

fn tool_answer(name: &str, arguments: Value, advance_ms: u64) -> Answer {
    let (deltas, end) = tool_reply(name, arguments);
    Answer {
        deltas,
        end,
        advance_ms,
    }
}

/// A provider on the model seam whose answers run synchronous fake work.
struct ScriptProvider {
    clock: Arc<FakeClock>,
    script: Mutex<VecDeque<Answer>>,
}

impl Provider for ScriptProvider {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let _ = request;
        Box::new(ScriptCall {
            clock: Arc::clone(&self.clock),
            answer: lock(&self.script)
                .pop_front()
                .expect("the script has a reply"),
        })
    }
}

struct ScriptCall {
    clock: Arc<FakeClock>,
    answer: Answer,
}

impl ModelCall for ScriptCall {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        for delta in &self.answer.deltas {
            sink(delta.clone());
        }
        self.clock
            .sleep(Duration::from_millis(self.answer.advance_ms));
        clone_end(&self.answer.end)
    }

    fn cancel(&self) {}
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

/// A registered tool with scripted effects, schema and synchronous work.
struct ProbeTool {
    name: String,
    schema: Value,
    effects: Result<Effects, EffectsError>,
    text: String,
    run_ms: u64,
    ask_confirm: bool,
    clock: Mutex<Option<Arc<FakeClock>>>,
    ran: AtomicUsize,
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
                },
                subject: None,
                prefix: None,
                always_reviewed: false,
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
                },
                subject: Some(subject.into()),
                prefix: Some(name.into()),
                always_reviewed: false,
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
            clock: Mutex::new(None),
            ran: AtomicUsize::new(0),
        }
    }

    /// Raises a suspending confirmation and reports its answer.
    fn confirming(mut self) -> Self {
        self.ask_confirm = true;
        self
    }

    fn ran(&self) -> usize {
        self.ran.load(Ordering::SeqCst)
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

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        self.effects.clone()
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.ran.fetch_add(1, Ordering::SeqCst);
        if let Some(clock) = lock(&self.clock).clone() {
            clock.sleep(Duration::from_millis(self.run_ms));
        }
        Output {
            content: vec![ContentPart::Text {
                text: self.text.clone(),
            }],
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
        if !self.ask_confirm {
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
    /// One-shot signal sent on the next `before_tool` entry, then the
    /// entry waits on `gate`: the failed-commit test's shutdown window.
    entered: Mutex<Option<mpsc::Sender<()>>>,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
}

impl ProbeHooks {
    fn refusing() -> Self {
        Self {
            before: Mutex::new(BeforeToolOutcome::Refused(Refusal {
                extension: "probe".into(),
                reason: "not this call".into(),
            })),
            entered: Mutex::new(None),
            gate: Mutex::new(None),
        }
    }

    fn rewriting(arguments: Map<String, Value>) -> Self {
        Self {
            before: Mutex::new(BeforeToolOutcome::Rewritten {
                arguments,
                changed_by: vec!["probe".into()],
            }),
            entered: Mutex::new(None),
            gate: Mutex::new(None),
        }
    }

    /// Signals the test on entry, then waits for its release.
    fn gated(entered: mpsc::Sender<()>, gate: mpsc::Receiver<()>) -> Self {
        Self {
            before: Mutex::new(BeforeToolOutcome::Refused(Refusal {
                extension: "probe".into(),
                reason: "not this call".into(),
            })),
            entered: Mutex::new(Some(entered)),
            gate: Mutex::new(Some(gate)),
        }
    }
}

impl Hooks for ProbeHooks {
    fn before_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> BeforeToolAnswer {
        if let Some(entered) = lock(&self.entered).take() {
            let _sent = entered.send(());
        }
        if let Some(gate) = lock(&self.gate).take() {
            fakes::Deadline::after(WAIT).recv(&gate).expect("the test releases the gate");
        }
        BeforeToolAnswer {
            outcome: lock(&self.before).clone(),
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Unchanged,
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
    hooks: Option<Arc<dyn Hooks>>,
    no_reviewer: bool,
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
    answerable: bool,
    debug: bool,
    idle: Option<Duration>,
    hooks: Option<Arc<dyn Hooks>>,
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
    });
    // Every timer reads this world's clock: tools, rules and the hook
    // gate advance it from whichever thread they run on.
    for tool in &setup.tools {
        *lock(&tool.clock) = Some(Arc::clone(&clock));
    }
    *lock(&setup.rules.clock) = Some(Arc::clone(&clock));
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let inbox_tx = Arc::new(inbox_tx);
    let cancel = Arc::new(crate::TurnCancel::default());
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider,
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
    if let Some(hooks) = &setup.hooks {
        looped = looped.hooks(Arc::clone(hooks));
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

fn join_turn(world: &mut World, flight: Flight) -> Result<Option<TurnOutcome>, Error> {
    let (outcome, looped) =
        fakes::Deadline::after(WAIT).recv_or_fail(&flight.done, "the turn ends");
    world.looped = Some(looped);
    outcome
}

fn turn(world: &mut World) -> Result<Option<TurnOutcome>, Error> {
    let flight = spawn_turn(world);
    join_turn(world, flight)
}

/// The next `permission_requested` on a watcher registered before the turn,
/// with its request and action ids. Each tap carries the deadline.
fn wait_requested(watcher: &mut log::Watcher, what: &str) -> (String, String) {
    loop {
        let line = watcher
            .recv_timeout(WAIT)
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
/// for the second, the re-raise. Each tap carries the deadline.
fn wait_requested_nth(watcher: &mut log::Watcher, n: usize, what: &str) -> (String, String) {
    let mut found = (String::new(), String::new());
    for _ in 0..n {
        found = wait_requested(watcher, what);
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
    });
    for tool in &world.tools {
        *lock(&tool.clock) = Some(Arc::clone(&clock));
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
            provider,
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
    if let Some(hooks) = &world.hooks {
        looped = looped.hooks(Arc::clone(hooks));
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
    let thread = fakes::Deadline::after(WAIT).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(&mut watcher, "the approval is raised");
    world.clock.advance_when_parked(
        Some(world.origin + IDLE),
        Some(thread),
        IDLE,
        &fakes::Deadline::after(WAIT),
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
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
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
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
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
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
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
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
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
    assert!(tool_records(&world).is_empty(), "{:?}", op_records(&world));
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
    let thread = fakes::Deadline::after(WAIT).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (request, _) = wait_requested(&mut watcher, "the approval is raised");
    assert!(
        ops(&world, "tool.permission").is_empty(),
        "no record completes while the person is waited on"
    );
    world.clock.advance_when_parked(
        None,
        Some(thread),
        Duration::from_millis(7),
        &fakes::Deadline::after(WAIT),
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
    let (request, _) = wait_requested(&mut watcher, "the approval is raised");
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
    let _ = wait_requested(&mut watcher, "the approval is raised");
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
    let _ = wait_requested(&mut watcher, "the approval is raised");
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
    let _ = wait_requested(&mut watcher, "the approval is raised");
    // The cancel lands only once the wait parks on the clock: any earlier
    // and the unarmed signal swallows it.
    assert!(
        world.clock.await_parked_unbounded(WAIT),
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
    let (request, _) = wait_requested(&mut watcher, "the approval is raised");
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
        fakes::Deadline::after(WAIT).recv_or_fail(&rx, "the reply is answered")
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
    let _ = wait_requested(&mut watcher, "the approval is raised");
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
    let (request, _) = wait_requested(&mut watcher, "the escalation is raised");
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
    let thread = fakes::Deadline::after(WAIT).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(&mut watcher, 2, "the approval is raised again");
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
        &fakes::Deadline::after(WAIT),
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
    let (raised, _) = wait_requested_nth(&mut watcher, 2, "the approval is raised again");
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
            },
            subject: Some("atlas run".into()),
            prefix: Some("atlas".into()),
            always_reviewed: false,
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
    let _ = wait_requested_nth(&mut watcher, 2, "the approval is raised again");
    assert!(
        world.clock.await_parked(world.origin + IDLE, WAIT),
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
    let thread = fakes::Deadline::after(WAIT).recv_or_fail(&flight.thread, "the turn starts");
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let _ = wait_requested_nth(&mut watcher, 2, "the approval is raised again");
    world.clock.advance_when_parked(
        Some(world.origin + IDLE),
        Some(thread),
        IDLE,
        &fakes::Deadline::after(WAIT),
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
    let _ = wait_requested_nth(&mut watcher, 2, "the approval is raised again");
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
    fakes::Deadline::after(WAIT).recv_or_fail(&entered_rx, "the hooks re-run on resume");
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
            },
            subject: Some("atlas run".into()),
            prefix: Some("atlas".into()),
            always_reviewed: false,
        }),
        "ran it",
    ))];
    exit_and_resume(&mut world, &request, vec![text_answer("Done.", 0)]);
    let flight = spawn_turn(&mut world);
    let mut watcher = world.log.as_ref().expect("the log is open").watch_all();
    let (raised, _) = wait_requested_nth(&mut watcher, 2, "the recorded request is raised");
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
    let (raised, _) = wait_requested_nth(&mut watcher, 2, "the recorded request is raised");
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
            request_id: RequestId("r_test".into()),
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
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
fn wait_interaction(watcher: &mut log::Watcher, what: &str) -> String {
    wait_interaction_nth(watcher, 1, what)
}

/// The nth `interaction_requested` (1-based): a resumed turn's watcher
/// replays the first question, so the finishing turn waits for the second.
fn wait_interaction_nth(watcher: &mut log::Watcher, n: usize, what: &str) -> String {
    let mut found = String::new();
    let mut seen = 0;
    while seen < n {
        let line = watcher
            .recv_timeout(WAIT)
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
    let asked = wait_interaction(&mut watcher, "the tool asks its question");
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
    let raised = wait_interaction_nth(&mut watcher, 2, "the question is raised again");
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
