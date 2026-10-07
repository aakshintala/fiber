//! What the loop's tests and its `turn` jig share: scripted replies on the
//! provider seam, and a session wired to them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "test code, helpers included; each test binary uses some helpers"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Wake;
use contract::emit::Emit;
use contract::events::{
    CacheLifetime, Event, ReasoningCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested,
    TurnOutcome,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{
    CallError, Delta, ModelCall, ModelRequest, Provider, Reply, ReplyAction, ToolDefinition,
};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender as From};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, RequestId, SessionId};
use fakes::clock::FakeClock;
use fakes::{BlockingProvider, Scripted, ScriptedProvider, reply};
use log::{Log, Watcher};
use r#loop::{BlockLimits, Loop, Model, Reviewer, TurnCancel};
use serde_json::{Map, Value, json};

/// How long a turn may take before a test fails instead of hanging.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// The model reference the scripted provider answers as.
pub(crate) const MODEL: &str = "fake/model-1";

/// The model reference a scripted reviewer answers as.
pub(crate) const REVIEWER_MODEL: &str = "fake/reviewer-1";

/// Watches the log for the turn's `permission_requested`, then runs `send`
/// with its request id: the signal the loop is waiting for a reply. The wait
/// is bounded by [`DEADLINE`]: a turn that never asks fails naming the
/// missing `permission_requested`, and the thread's end drops its inbox
/// sender, releasing a loop still waiting for a reply.
pub(crate) fn on_request(
    session: &Session,
    send: impl FnOnce(RequestId) + Send + 'static,
) -> thread::JoinHandle<()> {
    let mut watcher = session.log.watch();
    thread::spawn(move || {
        loop {
            let line = watcher
                .recv_timeout(DEADLINE)
                .expect("a permission_requested line in time")
                .expect("the log outlives the request")
                .expect("the log ended before permission_requested");
            if line.kind == "permission_requested" {
                send(RequestId(
                    line.payload["request_id"].as_str().unwrap().into(),
                ));
                return;
            }
        }
    })
}

/// A reply with readable reasoning, then `text`.
pub(crate) fn reasoning_reply(thought: &str, text: &str) -> Scripted {
    let mut end = reply(text);
    end.actions.insert(
        0,
        ReplyAction::Reasoning(ReasoningCompleted {
            text: thought.into(),
            provider_item: Some(reasoning_item(thought)),
        }),
    );
    Scripted {
        deltas: vec![
            Delta::Reasoning(TextDelta {
                text: thought.into(),
            }),
            Delta::Text(TextDelta { text: text.into() }),
        ],
        end: Ok(end),
    }
}

/// A reply that calls `names`, each with `{"city": "Paris"}`, after `text`.
pub(crate) fn tool_call_reply(text: &str, names: &[&str]) -> Scripted {
    let calls: Vec<(&str, Value)> = names
        .iter()
        .map(|n| (*n, json!({"city": "Paris"})))
        .collect();
    calls_reply(text, &calls)
}

/// A reply that makes `calls`, each a tool's name and its arguments, after
/// `text`.
pub(crate) fn calls_reply(text: &str, calls: &[(&str, Value)]) -> Scripted {
    let mut end = reply(text);
    let mut deltas = vec![Delta::Text(TextDelta { text: text.into() })];
    for (index, (name, arguments)) in calls.iter().enumerate() {
        deltas.push(Delta::ToolCallArguments(ToolCallArgumentsDelta {
            index: u32::try_from(index).unwrap(),
            name: Some((*name).into()),
            text: arguments.to_string(),
        }));
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: (*name).into(),
            arguments: arguments.clone(),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }));
    }
    Scripted {
        deltas,
        end: Ok(end),
    }
}

/// Notes shared by the tools in one step, and a signal when one is added.
/// A tool that waits for another blocks here instead of polling.
pub(crate) struct Trace {
    notes: Mutex<Vec<String>>,
    changed: Condvar,
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            notes: Mutex::default(),
            changed: Condvar::new(),
        }
    }
}

impl Trace {
    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Vec<String>>> {
        self.notes.lock()
    }

    fn push(&self, line: String) {
        let mut notes = self.notes.lock().unwrap();
        notes.push(line);
        self.changed.notify_all();
    }

    /// Blocks until `line` has been pushed, and fails the test at [`DEADLINE`].
    fn wait_for(&self, line: &str) {
        let notes = self.notes.lock().unwrap();
        let (notes, _) = self
            .changed
            .wait_timeout_while(notes, DEADLINE, |notes| {
                !notes.iter().any(|note| note == line)
            })
            .unwrap();
        assert!(
            notes.iter().any(|note| note == line),
            "timed out waiting for {line}"
        );
    }
}

/// A test tool registered through the tool seam, standing in for a built-in.
/// Its schema takes a required string `city` and an optional integer `days`.
pub(crate) struct TestTool {
    pub(crate) name: &'static str,
    /// What its effects function returns.
    pub(crate) effects: Result<DeclaredEffects, EffectsError>,
    /// The call's primary argument, as its tool reads it.
    pub(crate) subject: Option<String>,
    /// The widening a rule would offer.
    pub(crate) prefix: Option<String>,
    /// What a call returns.
    pub(crate) output: Output,
    pub(crate) bound: Bound,
    /// Waited on by every call before it returns.
    pub(crate) barrier: Option<Arc<Barrier>>,
    /// A tool whose call a call of this one waits to see finish, in `trace`.
    pub(crate) after: Option<&'static str>,
    /// What happened, in order, shared between tools: `effects <name>`,
    /// `run <name>`, `done <name>`.
    pub(crate) trace: Arc<Trace>,
    /// Scripted steps `run` performs after recording its arguments: emits
    /// through the call's emitter and rendezvous with the test, in order.
    pub(crate) script: Vec<Script>,
    /// The arguments each call ran with.
    pub(crate) ran: Mutex<Vec<Map<String, Value>>>,
    /// What [`Cancel::is_cancelled`] returned at each call.
    pub(crate) cancelled: Mutex<Vec<bool>>,
}

impl TestTool {
    /// A tool whose calls only read, and return `text`.
    pub(crate) fn reads(name: &'static str, text: &str) -> Self {
        Self::declaring(name, text, vec![Effect::Reads], None)
    }

    /// A tool whose calls declare `effects` on `paths`, and return `text`.
    pub(crate) fn declaring(
        name: &'static str,
        text: &str,
        effects: Vec<Effect>,
        paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            name,
            effects: Ok(DeclaredEffects {
                effects,
                reversible: true,
                paths,
            }),
            subject: Some(String::new()),
            prefix: None,
            output: Output {
                content: vec![ContentPart::Text { text: text.into() }],
                ..Output::default()
            },
            bound: Bound::DEFAULT,
            barrier: None,
            after: None,
            trace: Arc::default(),
            script: Vec::new(),
            ran: Mutex::default(),
            cancelled: Mutex::default(),
        }
    }

    /// A tool whose calls fail with `code`.
    pub(crate) fn failing(name: &'static str, code: contract::ErrorCode) -> Self {
        let mut tool = Self::reads(name, "It broke.");
        tool.output.error = Some(Failure {
            code,
            message: "It broke.".into(),
            retry_after: None,
            provider: None,
        });
        tool
    }

    pub(crate) fn ran(&self) -> Vec<Map<String, Value>> {
        self.ran.lock().unwrap().clone()
    }

    fn note(&self, what: &str) {
        self.trace.push(format!("{what} {}", self.name));
    }
}

impl Tool for TestTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: format!("The test tool {}.", self.name),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"},
                    "days": {"type": "integer"}
                },
                "required": ["city"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        self.note("effects");
        self.effects.clone().map(|declared| Effects {
            declared,
            subject: self.subject.clone(),
            prefix: self.prefix.clone(),
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        self.note("run");
        self.cancelled.lock().unwrap().push(cancel.is_cancelled());
        self.ran.lock().unwrap().push(arguments.clone());
        for step in &self.script {
            match step {
                Script::Emit(event) => emit.emit(event),
                // On a missed release the wait above expires and the call
                // proceeds, so the turn ends and the test reports instead
                // of hanging.
                Script::Wait(gate) => {
                    gate.wait();
                }
                Script::WaitCancel => {
                    let latch: Arc<CancelLatch> = Arc::default();
                    let shared: Arc<dyn Wake> = latch.clone();
                    cancel.subscribe(Arc::downgrade(&shared));
                    // After subscribing: a cancel before the subscription
                    // never replays, so a set signal skips the wait.
                    if !cancel.is_cancelled() {
                        latch.wait();
                    }
                    self.cancelled.lock().unwrap().push(cancel.is_cancelled());
                }
            }
        }
        if let Some(other) = self.after {
            self.trace.wait_for(&format!("done {other}"));
        }
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        self.note("done");
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

/// A tool named `name` whose calls overwrite `target` with `content`,
/// declaring the absolute path. A stand-in for the `write` and `edit`
/// tools in section-file tests.
pub(crate) struct WriteFile {
    pub(crate) name: &'static str,
    pub(crate) target: PathBuf,
    pub(crate) content: String,
}

impl Tool for WriteFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "The test file write tool.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Writes],
                reversible: true,
                paths: Some(vec![self.target.display().to_string()]),
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        std::fs::write(&self.target, &self.content).unwrap();
        Output {
            content: vec![ContentPart::Text {
                text: "Wrote it.".into(),
            }],
            ..Output::default()
        }
    }
}

/// One step of a test tool's scripted run: emit an event through the
/// call's emitter, or rendezvous with the test at a gate.
pub(crate) enum Script {
    /// Emits `Event` through the call's emitter.
    Emit(Box<Event>),
    /// Blocks until the test opens the gate, or [`DEADLINE`] passes.
    Wait(Arc<Gate>),
    /// Blocks until the call's [`Cancel`] fires, or [`DEADLINE`] passes,
    /// then records what [`Cancel::is_cancelled`] says: the flag a tool
    /// that stops on cancel sees.
    WaitCancel,
}

/// What [`Script::WaitCancel`] waits on: set when the call's `Cancel`
/// fires. The wait carries [`DEADLINE`], so a missed cancel ends the call
/// instead of hanging the turn.
#[derive(Default)]
struct CancelLatch {
    fired: Mutex<bool>,
    changed: Condvar,
}

impl Wake for CancelLatch {
    fn wake(&self) {
        *self.fired.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

impl CancelLatch {
    fn wait(&self) {
        let fired = self.fired.lock().unwrap();
        if *fired {
            return;
        }
        let (fired, _) = self
            .changed
            .wait_timeout_while(fired, DEADLINE, |fired| !*fired)
            .unwrap();
        // A missed cancel ends the call instead of hanging the turn, and
        // the panic names the wait (`docs/testing.md`, "Waits and
        // timeouts").
        assert!(*fired, "timed out waiting for the call's cancel");
    }
}

/// A test gate: the tool blocks until the test opens it, or a deadline
/// passes. The deadline keeps a test failure before the release from/// hanging scoped-thread cleanup: the tool proceeds either way, the turn
/// ends, and the test's own assertion reports (`docs/testing.md`, "Waits
/// and timeouts").
#[derive(Debug, Default)]
pub(crate) struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
    expired: AtomicBool,
}

impl Gate {
    /// Blocks until [`Gate::open`] or [`DEADLINE`]; true when opened. Either
    /// way the caller proceeds, so the turn always ends. A missed release
    /// is recorded for [`Gate::check`].
    pub(crate) fn wait(&self) -> bool {
        let open = self.open.lock().unwrap();
        let (open, _) = self
            .changed
            .wait_timeout_while(open, DEADLINE, |open| !*open)
            .unwrap();
        let opened = *open;
        if !opened {
            self.expired.store(true, Ordering::SeqCst);
        }
        opened
    }

    /// Releases whoever waits in [`Gate::wait`].
    pub(crate) fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    /// Asserts the gate opened before its deadline, naming it on expiry.
    /// Called after the turn, so cleanup finishes first.
    pub(crate) fn check(&self, name: &str) {
        assert!(
            !self.expired.load(Ordering::SeqCst),
            "gate {name} expired at its deadline"
        );
    }
}

/// The reasoning item a reasoning reply carries, as a provider sent it.
pub(crate) fn reasoning_item(thought: &str) -> Value {
    json!({"type": "reasoning", "encrypted_content": "gAAA-opaque", "summary": thought})
}

/// A driver's message.
pub(crate) fn message(text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: From {
            origin: Origin::Driver,
            command_id: Some(CommandId(format!("c_{text}"))),
        },
    }
}

/// An acknowledgement that ignores its answer. `fiber ask` uses one, and so
/// does a test that does not check the answer.
pub(crate) fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

/// A driver's prompt on the loop's inbox.
pub(crate) fn delivery(text: &str) -> Delivery {
    Delivery::Prompt(message(text), ignore())
}

/// A driver's steering message on the loop's inbox.
pub(crate) fn steer(text: &str) -> Delivery {
    Delivery::Steer(message(text), ignore())
}

/// A person's `handoff` on the loop's inbox, with `instructions`.
pub(crate) fn handoff(id: &str, instructions: Option<&str>) -> Delivery {
    Delivery::Handoff(
        CommandId(id.into()),
        contract::commands::Handoff {
            instructions: instructions.map(str::to_owned),
        },
        ignore(),
    )
}

/// Standing rules in memory: `read` returns what the test set.
pub(crate) struct FakeRules {
    rules: Mutex<Result<StandingRules, RulesError>>,
}

impl FakeRules {
    pub(crate) fn empty() -> Self {
        Self {
            rules: Mutex::new(Ok(StandingRules {
                global: Vec::new(),
                project: Vec::new(),
            })),
        }
    }

    /// What `read` returns from now on.
    pub(crate) fn set(&self, rules: StandingRules) {
        *self.rules.lock().unwrap() = Ok(rules);
    }

    /// `read` fails with `error` from now on.
    pub(crate) fn fail(&self, error: RulesError) {
        *self.rules.lock().unwrap() = Err(error);
    }
}

impl Rules for FakeRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        self.rules.lock().unwrap().clone()
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        // Only step 7 (#294) offers a rule to remember; nothing here does.
        Ok(())
    }
}

/// The scripted provider, sending `during` to the inbox as the first call is
/// made, so it arrives while that reply streams.
struct Seam {
    inner: Arc<dyn Provider>,
    /// Taken by the first call, so the loop sees the inbox close once the
    /// test drops its own sender.
    during: Mutex<Option<(Vec<Delivery>, Sender<Delivery>)>>,
}

impl Provider for Seam {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        if let Some((deliveries, inbox)) = self.during.lock().unwrap().take() {
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
        }
        self.inner.call(request)
    }
}

/// A provider that answers from a scripted provider and fires the
/// session's cancel once the `fire_at`-th call's reply is in hand. The
/// fire happens on the loop's own thread, synchronously inside `run`, so
/// the cancel lands after the reply but before `turn_completed`.
struct FireCancel {
    inner: Arc<ScriptedProvider>,
    cancel: Arc<TurnCancel>,
    fire_at: usize,
    calls: AtomicUsize,
}

impl Provider for FireCancel {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let call = self.inner.call(request);
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fire_at {
            Box::new(FireAfterReply {
                inner: call,
                cancel: Arc::clone(&self.cancel),
            })
        } else {
            call
        }
    }
}

/// A provider that advances the clock by `by` while its first call runs,
/// then answers from its scripted provider, ignoring cancel: the advance
/// lands during the failing call, before any backoff wait starts.
struct AdvanceClock {
    inner: Arc<ScriptedProvider>,
    clock: Arc<FakeClock>,
    by: Duration,
    calls: AtomicUsize,
}

impl Provider for AdvanceClock {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        Box::new(AdvanceCall {
            inner: self.inner.call(request),
            clock: Arc::clone(&self.clock),
            by: self.by,
            first,
        })
    }
}

/// The call that advances the clock while it runs.
struct AdvanceCall {
    inner: Box<dyn ModelCall>,
    clock: Arc<FakeClock>,
    by: Duration,
    first: bool,
}

impl ModelCall for AdvanceCall {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        if self.first {
            self.clock.advance(self.by);
        }
        self.inner.run(sink)
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

/// The call that fires the cancel once its reply is in hand.
struct FireAfterReply {
    inner: Box<dyn ModelCall>,
    cancel: Arc<TurnCancel>,
}

impl ModelCall for FireAfterReply {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let ended = self.inner.run(sink);
        self.cancel.cancel();
        ended
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

/// A provider that cancels the session's turn when its `at`-th call (1-based)
/// is made, then answers from its scripted provider: the call ends
/// `cancelled` at once, deterministically, on the loop's own thread.
struct CancelAtCall {
    inner: Arc<ScriptedProvider>,
    cancel: Arc<TurnCancel>,
    at: usize,
    calls: AtomicUsize,
}

impl Provider for CancelAtCall {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let call = self.inner.call(request);
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            self.cancel.cancel();
        }
        call
    }
}

/// `scripted` with its reply's `input` and `output` tokens set.
pub(crate) fn with_tokens(mut scripted: Scripted, input: u64, output: u64) -> Scripted {
    if let Ok(reply) = &mut scripted.end {
        reply.tokens.input = input;
        reply.tokens.output = output;
    }
    scripted
}

/// A session on a fresh log, its loop wired to a scripted provider.
pub(crate) struct Session {
    pub(crate) provider: Arc<ScriptedProvider>,
    pub(crate) log: Arc<Log>,
    /// The clock the log stamps `ts` with, driving every deadline.
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) dir: PathBuf,
    /// The workspace, an empty directory.
    pub(crate) workspace: PathBuf,
    /// Fiber home's `credentials/` directory.
    pub(crate) credentials: PathBuf,
    /// The standing rules the loop reads.
    pub(crate) rules: Arc<FakeRules>,
    pub(crate) inbox: Sender<Delivery>,
    lines: Watcher,
    pub(crate) looped: Option<Loop>,
    /// Cancels the session's running turn, as a driver does.
    pub(crate) cancel: Arc<TurnCancel>,
    _home: TempDir,
}

impl Session {
    /// Answers each model call with the next of `script`. The first model
    /// call sends `during` to the inbox, when there is one.
    pub(crate) fn new(script: Vec<Scripted>, during: Option<Message>) -> Self {
        Self::with_tools(script, during, Vec::new())
    }

    /// As [`Session::new`], with the session's one reasoning setting.
    pub(crate) fn with_thinking(
        script: Vec<Scripted>,
        thinking: Option<contract::ThinkingLevel>,
    ) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        Self::assemble(
            Arc::clone(&scripted) as Arc<dyn Provider>,
            Vec::new(),
            Vec::new(),
            unpriced(),
            scripted,
            Arc::new(TurnCancel::default()),
            Vec::new(),
            thinking,
        )
    }

    /// As [`Session::new`], with `tools` registered.
    pub(crate) fn with_tools(
        script: Vec<Scripted>,
        during: Option<Message>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::open(
            script,
            during
                .into_iter()
                .map(|message| Delivery::Steer(message, ignore()))
                .collect(),
            tools,
            unpriced(),
        )
    }

    /// As [`Session::with_tools`], and the first model call sends `during`.
    pub(crate) fn with_tools_injecting(
        script: Vec<Scripted>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::open(script, during, tools, unpriced())
    }

    /// As [`Session::with_tools`], with `files` as the configured `file`
    /// credential sources (`docs/permissions.md`, "Credentials").
    pub(crate) fn with_credential_files(
        script: Vec<Scripted>,
        tools: Vec<Arc<dyn Tool>>,
        files: Vec<PathBuf>,
    ) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        Self::assemble_with(
            Arc::clone(&scripted) as Arc<dyn Provider>,
            Vec::new(),
            tools,
            unpriced(),
            scripted,
            Arc::new(TurnCancel::default()),
            (FakeClock::new(), 0),
            Vec::new(),
            CacheLifetime::OneHour,
            None,
            files,
        )
    }

    /// As [`Session::new`], and the first model call sends `during`.
    pub(crate) fn injecting(script: Vec<Scripted>, during: Vec<Delivery>) -> Self {
        Self::open(script, during, Vec::new(), unpriced())
    }

    /// As [`Session::new`], with the preamble's cache lifetime set to
    /// `lifetime` (`docs/prompt-cache.md`, "Cache lifetime").
    pub(crate) fn with_cache_lifetime(script: Vec<Scripted>, lifetime: CacheLifetime) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        Self::assemble_with(
            Arc::clone(&scripted) as Arc<dyn Provider>,
            Vec::new(),
            Vec::new(),
            unpriced(),
            scripted,
            Arc::new(TurnCancel::default()),
            (FakeClock::new(), 0),
            Vec::new(),
            lifetime,
            None,
            Vec::new(),
        )
    }

    /// As [`Session::with_cache_lifetime`], reaching the scripted provider
    /// through what `wrap` makes of it and the session's clock.
    pub(crate) fn wrapped(
        script: Vec<Scripted>,
        lifetime: CacheLifetime,
        wrap: impl FnOnce(Arc<ScriptedProvider>, Arc<FakeClock>) -> Arc<dyn Provider>,
    ) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        let clock = FakeClock::new();
        let provider = wrap(Arc::clone(&scripted), Arc::clone(&clock));
        Self::assemble_with(
            provider,
            Vec::new(),
            Vec::new(),
            unpriced(),
            scripted,
            Arc::new(TurnCancel::default()),
            (clock, 0),
            Vec::new(),
            lifetime,
            None,
            Vec::new(),
        )
    }

    /// As [`Session::with_tools`], reaching `model`. `during` is sent, in
    /// order, when the first model call is made.
    pub(crate) fn open(
        script: Vec<Scripted>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
        model: Model,
    ) -> Self {
        Self::open_sectioned(script, during, tools, model, Vec::new())
    }

    /// As [`Session::open`], with `sections` as the prompt's extension
    /// sections: each extension's name, its files' paths, and its budget.
    pub(crate) fn open_sectioned(
        script: Vec<Scripted>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
        model: Model,
        sections: Vec<(String, Vec<std::path::PathBuf>, Option<u64>)>,
    ) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        Self::assemble(
            Arc::clone(&scripted) as Arc<dyn Provider>,
            during,
            tools,
            model,
            scripted,
            Arc::new(TurnCancel::default()),
            sections,
            None,
        )
    }

    /// As [`Session::new`], with `sections` as the prompt's extension
    /// sections.
    pub(crate) fn sectioned(
        script: Vec<Scripted>,
        sections: Vec<(String, Vec<std::path::PathBuf>, Option<u64>)>,
    ) -> Self {
        Self::open_sectioned(script, Vec::new(), Vec::new(), unpriced(), sections)
    }

    /// As [`Session::with_tools`], with `sections` as the prompt's
    /// extension sections.
    pub(crate) fn with_tools_sectioned(
        script: Vec<Scripted>,
        tools: Vec<Arc<dyn Tool>>,
        sections: Vec<(String, Vec<std::path::PathBuf>, Option<u64>)>,
    ) -> Self {
        Self::open_sectioned(script, Vec::new(), tools, unpriced(), sections)
    }

    /// A session whose `fire_at`-th model call (1-based) fires the session's
    /// cancel once its reply is in hand, then answers from `script`: the
    /// cancel lands after the last reply but before `turn_completed`,
    /// deterministically, on the loop's own thread.
    pub(crate) fn cancelling_after_reply(script: Vec<Scripted>, fire_at: usize) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        let cancel = Arc::new(TurnCancel::default());
        let hook = Arc::new(FireCancel {
            inner: Arc::clone(&scripted),
            cancel: Arc::clone(&cancel),
            fire_at,
            calls: AtomicUsize::new(0),
        });
        Self::assemble(
            hook,
            Vec::new(),
            Vec::new(),
            unpriced(),
            scripted,
            cancel,
            Vec::new(),
            None,
        )
    }

    /// A session whose `at`-th model call (1-based) is cancelled as it is
    /// made, then answers from `script`.
    pub(crate) fn cancelling_at_call(
        script: Vec<Scripted>,
        at: usize,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        let cancel = Arc::new(TurnCancel::default());
        let hook = Arc::new(CancelAtCall {
            inner: Arc::clone(&scripted),
            cancel: Arc::clone(&cancel),
            at,
            calls: AtomicUsize::new(0),
        });
        Self::assemble(
            hook,
            Vec::new(),
            tools,
            unpriced(),
            scripted,
            cancel,
            Vec::new(),
            None,
        )
    }

    /// A session whose first model call blocks until cancelled, sending
    /// `during` to the inbox when that call is made. Later calls answer
    /// "After." at once. Returns the session and the blocking provider,
    /// which signals when its call starts to block.
    pub(crate) fn blocking(during: Vec<Delivery>) -> (Self, Arc<BlockingProvider>) {
        let blocking = Arc::new(BlockingProvider::default());
        let session = Self::assemble(
            Arc::clone(&blocking) as Arc<dyn Provider>,
            during,
            Vec::new(),
            unpriced(),
            // No scripted call is ever made; `requests` stays empty.
            Arc::new(ScriptedProvider::new(Vec::new())),
            Arc::new(TurnCancel::default()),
            Vec::new(),
            None,
        );
        (session, blocking)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the one assembly takes each session input; tests pass thinking through it"
    )]
    fn assemble(
        provider: Arc<dyn Provider>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
        model: Model,
        scripted: Arc<ScriptedProvider>,
        cancel: Arc<TurnCancel>,
        sections: Vec<(String, Vec<PathBuf>, Option<u64>)>,
        thinking: Option<contract::ThinkingLevel>,
    ) -> Self {
        Self::assemble_with(
            provider,
            during,
            tools,
            model,
            scripted,
            cancel,
            (FakeClock::new(), 0),
            sections,
            CacheLifetime::OneHour,
            thinking,
            Vec::new(),
        )
    }

    /// A session whose first model call advances the clock by `by` while it
    /// runs, then answers from `script`: the advance lands during the
    /// failing call, before any backoff wait starts.
    pub(crate) fn advancing(script: Vec<Scripted>, by: Duration) -> Self {
        let clock = FakeClock::new();
        let inner = Arc::new(ScriptedProvider::new(script));
        let provider: Arc<dyn Provider> = Arc::new(AdvanceClock {
            inner: Arc::clone(&inner),
            clock: Arc::clone(&clock),
            by,
            calls: AtomicUsize::new(0),
        });
        Self::assemble_with(
            provider,
            Vec::new(),
            Vec::new(),
            unpriced(),
            inner,
            Arc::new(TurnCancel::default()),
            (clock, 0),
            Vec::new(),
            CacheLifetime::OneHour,
            None,
            Vec::new(),
        )
    }

    /// As [`Session::with_tools`], for a model whose context window is
    /// `window` tokens.
    pub(crate) fn windowed(script: Vec<Scripted>, tools: Vec<Arc<dyn Tool>>, window: u64) -> Self {
        let scripted = Arc::new(ScriptedProvider::new(script));
        Self::assemble_with(
            Arc::clone(&scripted) as Arc<dyn Provider>,
            Vec::new(),
            tools,
            unpriced(),
            scripted,
            Arc::new(TurnCancel::default()),
            (FakeClock::new(), window),
            Vec::new(),
            CacheLifetime::OneHour,
            None,
            Vec::new(),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the one assembly takes each session input; tests pass sections through it"
    )]
    fn assemble_with(
        provider: Arc<dyn Provider>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
        model: Model,
        scripted: Arc<ScriptedProvider>,
        cancel: Arc<TurnCancel>,
        (clock, window): (Arc<FakeClock>, u64),
        sections: Vec<(String, Vec<PathBuf>, Option<u64>)>,
        cache_lifetime: CacheLifetime,
        thinking: Option<contract::ThinkingLevel>,
        credential_files: Vec<PathBuf>,
    ) -> Self {
        let home = TempDir::new();
        let workspace = home.0.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let credentials = home.0.join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let id = SessionId("s_test".into());
        let log = Arc::new(Log::create(&home.0, id.clone(), clock.clone()).unwrap());
        let lines = log.watch();
        let (inbox, rx) = mpsc::channel();
        let seam = Seam {
            inner: provider,
            during: Mutex::new((!during.is_empty()).then(|| (during, inbox.clone()))),
        };
        let rules = Arc::new(FakeRules::empty());
        let dir = home.0.join("s_test");
        let session_log = dir.join("events.jsonl").display().to_string();
        let owned: Arc<FakeClock> = Arc::clone(&clock);
        let prompt_clock: Arc<dyn contract::clock::Clock> = owned;
        let looped = Loop::start(
            Arc::clone(&log),
            Arc::new(seam),
            model,
            {
                let mut prompt = r#loop::PromptInputs::new(
                    home.0.clone(),
                    "/bin/sh".into(),
                    session_log,
                    prompt_clock,
                );
                prompt.credential = Some("work".into());
                prompt.context_window = (window != 0).then_some(window);
                prompt.extension_sections = sections;
                prompt.cache_lifetime = cache_lifetime;
                prompt.thinking = thinking;
                prompt
            },
            rx,
            tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            r#loop::Permissions {
                workspace: workspace.display().to_string(),
                credentials: credentials.clone(),
                credential_files,
                rules: rules.clone(),
            },
        )
        .unwrap()
        .cancelled_by(Arc::clone(&cancel));
        Self {
            provider: scripted,
            dir: home.0.join(&id.0),
            workspace,
            credentials,
            rules,
            log,
            clock,
            inbox,
            lines,
            looped: Some(looped),
            cancel,
            _home: home,
        }
    }

    /// Whether a person can answer an approval.
    pub(crate) fn answerable(mut self, yes: bool) -> Self {
        self.looped = self.looped.take().map(|looped| looped.answerable(yes));
        self
    }

    /// Judges step 7's calls with a scripted reviewer answering `script`,
    /// with the default block limits. Returns the reviewer, so a test can
    /// read the requests it was given.
    pub(crate) fn reviewer(&mut self, script: Vec<Scripted>) -> Arc<ScriptedProvider> {
        self.reviewer_limits(script, BlockLimits::default())
    }

    /// As [`Session::reviewer`], with `limits`.
    pub(crate) fn reviewer_limits(
        &mut self,
        script: Vec<Scripted>,
        limits: BlockLimits,
    ) -> Arc<ScriptedProvider> {
        self.reviewer_priced(script, limits, None)
    }

    /// As [`Session::reviewer`], with `limits` and the reviewer's prices.
    pub(crate) fn reviewer_priced(
        &mut self,
        script: Vec<Scripted>,
        limits: BlockLimits,
        cost: Option<contract::provider::Cost>,
    ) -> Arc<ScriptedProvider> {
        self.reviewer_with(script, limits, cost, CacheLifetime::OneHour)
    }

    /// As [`Session::reviewer`], with the reviewer's cache lifetime.
    pub(crate) fn reviewer_cached(
        &mut self,
        script: Vec<Scripted>,
        cache_lifetime: CacheLifetime,
    ) -> Arc<ScriptedProvider> {
        self.reviewer_with(script, BlockLimits::default(), None, cache_lifetime)
    }

    fn reviewer_with(
        &mut self,
        script: Vec<Scripted>,
        limits: BlockLimits,
        cost: Option<contract::provider::Cost>,
        cache_lifetime: CacheLifetime,
    ) -> Arc<ScriptedProvider> {
        let provider = Arc::new(ScriptedProvider::new(script));
        let looped = self.looped.take().unwrap().reviewer(
            Ok(Reviewer {
                provider: provider.clone(),
                model: Model {
                    reference: REVIEWER_MODEL.into(),
                    cost,
                    subscription: false,
                },
                cache_lifetime,
            }),
            limits,
        );
        self.looped = Some(looped);
        provider
    }

    /// Sends a person's answer to a pending approval.
    pub(crate) fn reply(&self, reply: contract::commands::Reply) {
        self.inbox.send(Delivery::Reply(reply, ignore())).unwrap();
    }

    /// Caps the session's billed spend at `usd` US dollars.
    pub(crate) fn budget(mut self, usd: Option<f64>) -> Self {
        self.looped = self.looped.take().map(|looped| looped.budget(usd));
        self
    }

    /// Sets automatic handoff's triggers.
    pub(crate) fn handoff(mut self, settings: r#loop::HandoffSettings) -> Self {
        self.looped = self.looped.take().map(|looped| looped.handoff(settings));
        self
    }

    /// Runs `forget` on every completed handoff.
    pub(crate) fn on_handoff(mut self, forget: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.looped = self.looped.take().map(|looped| looped.on_handoff(forget));
        self
    }

    /// Retries a failed model call with `retry`.
    pub(crate) fn retry(mut self, retry: r#loop::Retry) -> Self {
        self.looped = self.looped.take().map(|looped| looped.retry(retry));
        self
    }

    /// Runs one turn on its own thread, failing the test if it outlives
    /// [`DEADLINE`].
    pub(crate) fn turn(&mut self) -> Option<TurnOutcome> {
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = finished
            .recv_timeout(DEADLINE)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    /// Every request the loop built, in order.
    pub(crate) fn requests(&self) -> Vec<ModelRequest> {
        self.provider.requests()
    }

    /// Every line emitted since the last call, ephemeral ones included,
    /// through the next `turn_completed`.
    pub(crate) fn lines(&mut self) -> Vec<Envelope> {
        let mut lines = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(DEADLINE)
                .expect("a turn_completed line in time")
                .expect("the log outlives the turn")
                .expect("the log ended before turn_completed");
            // `run` starts the status observer, whose lines race the
            // loop's own; `tests/status.rs` reads them.
            if line.kind == "session_status" {
                continue;
            }
            let last = line.kind == "turn_completed";
            lines.push(line);
            if last {
                return lines;
            }
        }
    }
}

/// A model with no declared prices.
fn unpriced() -> Model {
    Model {
        reference: MODEL.into(),
        cost: None,
        subscription: false,
    }
}

/// The kinds of `lines`, in order.
pub(crate) fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

/// A live tap on a session's log: its own watcher, readable mid-turn with
/// a deadline, alongside `Session::lines`, which drains after the turn.
/// Lines read here stay in the session's own queue: each watcher has one.
pub(crate) struct Tap {
    watcher: Mutex<Watcher>,
    buffered: Mutex<Vec<Envelope>>,
}

impl Tap {
    /// Taps `log` from now on.
    pub(crate) fn new(log: &Arc<Log>) -> Self {
        Self {
            watcher: Mutex::new(log.watch()),
            buffered: Mutex::default(),
        }
    }

    /// The next line of `kind`, buffering the rest for later calls, and
    /// failing the test at [`DEADLINE`].
    pub(crate) fn wait_for(&self, kind: &str) -> Envelope {
        self.wait_until(|line| line.kind == kind)
    }

    /// The next `tool_call_delta` whose text is `text`, buffering the
    /// rest, and failing the test at [`DEADLINE`].
    pub(crate) fn wait_for_delta(&self, text: &str) -> Envelope {
        self.wait_until(|line| delta_text(line) == Some(text))
    }

    /// The next line `matches` accepts, buffering the rest, and failing the
    /// test at [`DEADLINE`]. Takes the watcher lock and the buffer lock one
    /// at a time, never nested, and never holds the buffer lock across the
    /// wait.
    fn wait_until(&self, matches: impl Fn(&Envelope) -> bool) -> Envelope {
        loop {
            if let Some(found) = take_from(&self.buffered, &matches) {
                return found;
            }
            let line = self
                .watcher
                .lock()
                .unwrap()
                .recv_timeout(DEADLINE)
                .expect("a line in time")
                .expect("the log outlives the tap")
                .expect("the log ended before the awaited line");
            if matches(&line) {
                return line;
            }
            self.buffered.lock().unwrap().push(line);
        }
    }

    /// Every line received and buffered so far, without waiting.
    pub(crate) fn pending(&self) -> Vec<Envelope> {
        let mut lines = std::mem::take(&mut *self.buffered.lock().unwrap());
        let mut watcher = self.watcher.lock().unwrap();
        loop {
            match watcher.try_recv() {
                Ok(Some(line)) => lines.push(line),
                Ok(None) => break,
                Err(e) => panic!("the tap's watcher failed: {e}"),
            }
        }
        lines
    }
}

/// Removes and returns the first buffered line `matches` accepts, if any.
fn take_from(
    buffered: &Mutex<Vec<Envelope>>,
    matches: impl Fn(&Envelope) -> bool,
) -> Option<Envelope> {
    let mut buffered = buffered.lock().unwrap();
    let found = buffered.iter().position(&matches)?;
    Some(buffered.remove(found))
}

/// The text of a `tool_call_delta` line, if it carries any.
fn delta_text(line: &Envelope) -> Option<&str> {
    (line.kind == "tool_call_delta")
        .then(|| line.payload.get("text")?.as_str())
        .flatten()
}

/// A directory removed when dropped.
pub(crate) struct TempDir(pub(crate) PathBuf, fakes::TempDir);

impl TempDir {
    pub(crate) fn new() -> Self {
        let held = fakes::TempDir::new("fiber-loop");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}
