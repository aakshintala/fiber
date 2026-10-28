//! A session message under the admission lock (`docs/tools.md`, "Messaging
//! other sessions"): the door admits it under the same lock the loop's last
//! idle check closes under, so a message sent before that check runs, and
//! one sent after is refused. A message held while an approval, a question
//! or an offer waits keeps the session from being idle.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{Decision, TurnOutcome};
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, Context, Hooks, Replaced,
};
use contract::inbox::{Ack, Answer, Delivery, Message, SessionMessage};
use contract::provider::{Delta, Input, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender as From};
use contract::tool::{Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, RequestId, SessionId, TurnId};
use fakes::Deadline;
use fakes::clock::{FakeClock, Mark};
use fakes::{Scripted, ScriptedProvider};
use log::{Log, Watcher};
use serde_json::{Map, Value, json};
use support::admission::Admission;

use super::Window;
use crate::{Loop, Model};

const IDLE: Duration = Duration::from_secs(60);

const FROM: &str = "s_0123456789abcdef";
const PARENT: &str = "s_parent00000001";
const COMMAND: &str = "c_m1";
const TEXT: &str = "Tests pass on main.";
const NAMED: &str = "Fiber: a message from session s_0123456789abcdef, named \"builder\". Another session wrote it, not the person.\n\nTests pass on main.";
const UNNAMED: &str = "Fiber: a message from session s_0123456789abcdef. Another session wrote it, not the person.\n\nTests pass on main.";

/// A session message from `FROM` with parent `PARENT`, named `builder`,
/// sent by `COMMAND`.
fn named() -> SessionMessage {
    super::for_test(FROM, Some(PARENT), Some("builder"), COMMAND, TEXT)
}

/// The `Delivery` for `message`, answered on `ack`, as the door sends it.
fn delivery(message: SessionMessage, ack: Ack) -> Delivery {
    Delivery::Message(message, ack)
}

fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

/// An acknowledgement recording its answer on the returned channel. A send
/// after the test dropped the receiver is ignored, as a dropped
/// delivery's is.
fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        })),
        rx,
    )
}

/// The answer `rx` carries, waited under one [`MUST_SUCCEED_WITHIN`] naming `what`.
#[track_caller]
fn answered(rx: &mpsc::Receiver<Answer>, what: &str) -> Answer {
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(rx)
        .unwrap_or_else(|_| panic!("{what}"))
}

/// The rejection `answer` carries, panicking on an acceptance.
#[track_caller]
fn rejected(answer: Answer) -> contract::inbox::Rejection {
    match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("an acceptance is no rejection: {answer:?}"),
    }
}

/// A hook run parked until the test frees it: the test is told on
/// `entered`, moves the clock past the registered timeout, then frees
/// the run on `release`.
struct Park {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

/// A scripted `Hooks` fake: seen raw texts and sender ids, a queue of
/// answers, and an optional one-shot park inside `before_message`.
struct FakeHooks {
    seen: Mutex<Vec<String>>,
    seen_ids: Mutex<Vec<(Option<String>, Option<String>)>>,
    answers: Mutex<VecDeque<BeforeMessageAnswer>>,
    park: Mutex<Option<Park>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            seen_ids: Mutex::new(Vec::new()),
            answers: Mutex::new(VecDeque::new()),
            park: Mutex::new(None),
        }
    }

    fn answer(&self, answer: BeforeMessageAnswer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn seen_ids(&self) -> Vec<(Option<String>, Option<String>)> {
        self.seen_ids.lock().unwrap().clone()
    }
}

fn passed() -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: Vec::new(),
        },
        notices: Vec::new(),
    }
}

fn replaced(text: &str, by: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: Some(Replaced {
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                changed_by: vec![by.to_owned()],
            }),
            context: Vec::new(),
        },
        notices: Vec::new(),
    }
}

fn refused(by: &str, reason: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Refused(contract::hook::Refusal {
            extension: by.to_owned(),
            reason: reason.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn failed(by: &str, message: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Failed(contract::hook::HookFailure {
            extension: by.to_owned(),
            message: message.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn passed_with_context(extension: &str, text: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: vec![Context {
                state: Vec::new(),
                extension: extension.to_owned(),
                text: text.to_owned(),
            }],
        },
        notices: Vec::new(),
    }
}

fn noticed(code: ErrorCode, extension: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: Vec::new(),
        },
        notices: vec![contract::events::Notice {
            code,
            message: format!("extension {extension} failed softly"),
            extension: Some(extension.to_owned()),
        }],
    }
}

impl Hooks for FakeHooks {
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.seen
            .lock()
            .unwrap()
            .push(crate::conversation::text(message.content));
        self.seen_ids.lock().unwrap().push((
            message
                .sender
                .map(|sender| sender.from_session_id.0.clone()),
            message
                .sender
                .and_then(|sender| sender.parent_session_id.map(|id| id.0.clone())),
        ));
        if let Some(park) = self.park.lock().unwrap().take() {
            // Parked: the test moves the fake clock past the registered
            // timeout, then releases the run.
            let _sent = park.entered.send(());
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&park.release)
                .expect("the test releases the parked hook");
        }
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed)
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

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
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

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

fn close() -> Delivery {
    Delivery::Close(ignore())
}

/// Reads `watcher` until `done` accepts a line, under one [`MUST_SUCCEED_WITHIN`].
#[track_caller]
fn read_until(
    watcher: Watcher,
    what: &str,
    done: impl Fn(&Envelope) -> bool + Send + 'static,
) -> (Watcher, Vec<Envelope>) {
    let what_owned = what.to_owned();
    fakes::within(what, Deadline::after(MUST_SUCCEED_WITHIN), move || {
        let mut watcher = watcher;
        let mut lines = Vec::new();
        loop {
            match watcher.recv() {
                Ok(Some(line)) => {
                    let last = done(&line);
                    lines.push(line);
                    if last {
                        return (watcher, lines);
                    }
                }
                Ok(None) => panic!("the log ended before {what_owned}"),
                Err(err) => panic!("the log failed before {what_owned}: {err}"),
            }
        }
    })
}

/// A reply that calls `name` with `{}`.
fn calls(name: &str) -> Scripted {
    let mut end = fakes::reply("");
    end.actions
        .push(ReplyAction::ToolCall(contract::events::ToolCallRequested {
            name: name.into(),
            arguments: json!({}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }));
    Scripted {
        deltas: vec![Delta::ToolCallArguments(
            contract::events::ToolCallArgumentsDelta {
                index: 0,
                name: Some(name.into()),
                text: "{}".into(),
            },
        )],
        end: Ok(end),
    }
}

/// The scripted provider, sending each call's deliveries to the inbox as
/// that call is made.
struct During {
    inner: Arc<ScriptedProvider>,
    inbox: Sender<Delivery>,
    by_call: Mutex<VecDeque<Vec<Delivery>>>,
}

impl Provider for During {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn contract::provider::ModelCall> {
        let sent = self.by_call.lock().unwrap().pop_front().unwrap_or_default();
        for delivery in sent {
            self.inbox.send(delivery).unwrap();
        }
        self.inner.call(request)
    }
}

/// A tool a standing rule asks a person about.
struct Gated;

impl Tool for Gated {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "gated".into(),
            description: "Needs a person's allow.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: false,
                paths: None,
                confined: false,
            },
            subject: Some("npm publish".into()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "ok\n".into(),
            }],
            ..Output::default()
        }
    }
}

/// Asks a person about every `gated` call.
struct AskGated;

impl Rules for AskGated {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules {
            global: vec![Rule {
                decision: RuleDecision::Ask,
                tool: "gated".into(),
                prefix: "npm publish".into(),
                added: None,
                session_id: None,
            }],
            project: Vec::new(),
        })
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A tool whose call raises one suspending `confirm` and returns its answer
/// as text.
struct Asks;

impl Tool for Asks {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "asks".into(),
            description: "Asks the person.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
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
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        panic!("the loop runs a call through run_asking")
    }

    fn run_asking(
        &self,
        _: &Map<String, Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        let answered = ask.ask(Asking {
            interaction: contract::events::Interaction::Confirm {
                prompt: "Deploy?".into(),
            },
            action_ids: Vec::new(),
            until: None,
            check: None,
            suspends: true,
        });
        let _ = answered;
        Output {
            content: vec![ContentPart::Text {
                text: "answered\n".into(),
            }],
            ..Output::default()
        }
    }
}

/// A wake that puts `Delivery::Cancelled` in the inbox, so an interaction a
/// tool call raises can be answered.
struct TestWake(Sender<Delivery>);

impl contract::clock::Wake for TestWake {
    fn wake(&self) {
        let _sent = self.0.send(Delivery::Cancelled);
    }
}

/// A fake repository: one unapproved item, every decision recorded.
struct FakeRepo {
    list: Mutex<Vec<Unapproved>>,
}

impl FakeRepo {
    fn server(name: &str) -> Arc<Self> {
        Self::with(name, false)
    }

    fn required(name: &str) -> Arc<Self> {
        Self::with(name, true)
    }

    fn with(name: &str, required: bool) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(vec![Unapproved {
                offered: contract::events::OfferedItem {
                    files: vec![],
                    kind: contract::events::OfferedKind::McpServer,
                    name: name.to_owned(),
                    hash: format!("h_{name}"),
                    required,
                    summary: format!("{name}: what an install shows"),
                    version: None,
                    diff: None,
                },
                never: false,
            }]),
        })
    }
}

impl RepositoryCode for FakeRepo {
    fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
        Err(contract::shapes::Failure {
            code: contract::ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }
    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
        Ok(self.list.lock().unwrap().clone())
    }

    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<Decided, Failure> {
        Ok(Decided::Recorded)
    }
}

/// A loop on a fresh log with an admission, its provider answering
/// `script` and sending `by_call[n]` as call `n` is made.
struct World {
    _home: fakes::TempDir,
    dir: std::path::PathBuf,
    log: Arc<Log>,
    inbox: Sender<Delivery>,
    provider: Arc<ScriptedProvider>,
    admission: Arc<Admission>,
    looped: Option<Loop>,
    watched: Option<Watcher>,
    clock: Arc<FakeClock>,
    runner: Option<thread::ThreadId>,
}

impl World {
    fn new(script: Vec<Scripted>, by_call: Vec<Vec<Delivery>>) -> Self {
        Self::with_tools(script, by_call, Vec::new(), Arc::new(AskGated))
    }

    fn with_tools(
        script: Vec<Scripted>,
        by_call: Vec<Vec<Delivery>>,
        tools: Vec<Arc<dyn Tool>>,
        rules: Arc<dyn Rules>,
    ) -> Self {
        let home = fakes::TempDir::new("fiber-session-message");
        let workspace = home.path().join("workspace");
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&credentials).unwrap();
        let fake = FakeClock::new();
        let clock: Arc<dyn contract::clock::Clock> = fake.clone();
        let log = Arc::new(
            Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        let watched = Some(log.watch());
        let provider = Arc::new(ScriptedProvider::new(script));
        let (inbox, rx) = mpsc::channel();
        let during = During {
            inner: Arc::clone(&provider),
            inbox: inbox.clone(),
            by_call: Mutex::new(by_call.into()),
        };
        let admission = Arc::new(Admission::default());
        let looped = Loop::start(
            crate::Session {
                log: Arc::clone(&log),
                provider: Arc::new(during),
                model: Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: crate::prompt::PromptInputs::new(
                    home.path().to_path_buf(),
                    "/bin/sh".into(),
                    home.path()
                        .join("s_test/events.jsonl")
                        .display()
                        .to_string(),
                    clock,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: tools
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
        .idle_exit(Some(IDLE))
        .admission(Arc::clone(&admission));
        Self {
            dir: home.path().join("s_test"),
            _home: home,
            log,
            inbox,
            provider,
            admission,
            looped: Some(looped),
            watched,
            clock: fake,
            runner: None,
        }
    }

    /// Wires an inbox wake, so a tool interaction can be answered.
    fn woken(mut self) -> Self {
        let wake: Arc<dyn contract::clock::Wake> = Arc::new(TestWake(self.inbox.clone()));
        let looped = self.looped.take().unwrap().inbox_wake(wake);
        self.looped = Some(looped);
        self
    }

    /// Wires `code` as the repository's code, with one client watching, so
    /// its offer can be answered.
    fn offered(mut self, code: Arc<dyn RepositoryCode>) -> Self {
        self.log
            .append(
                &contract::events::Event::Clients(contract::events::Clients { count: 1 }),
                None,
                None,
            )
            .unwrap();
        let looped = self.looped.take().unwrap().repository_code(code);
        self.looped = Some(looped);
        self
    }

    /// Wires `hooks` as the loop's `before_message` chain.
    fn hook_with(&mut self, hooks: Arc<FakeHooks>) {
        self.looped = Some(self.looped.take().unwrap().hooks(hooks as Arc<dyn Hooks>));
    }

    /// Runs the loop's idle wait around `offer`: `window` sees `Before`
    /// and, when nothing is found, `After`.
    fn hook(&mut self, window: impl FnMut(Window) + Send + 'static) {
        self.looped.as_mut().unwrap().window = Some(Box::new(window));
    }

    /// Runs the loop to its end on its own thread.
    fn spawn_run(&mut self) -> mpsc::Receiver<Result<(), crate::Error>> {
        let looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _sent = done.send(looped.run().result);
        });
        self.runner = Some(handle.thread().id());
        finished
    }

    /// Runs one turn on its own thread, returning the loop with it.
    fn spawn_turn(&mut self) -> mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)> {
        let looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut looped = looped;
            let outcome = looped.turn();
            let _sent = done.send((looped, outcome));
        });
        self.runner = Some(handle.thread().id());
        finished
    }

    /// Moves the parked wait past `by`, expecting its park at `expected`:
    /// the runner and the [`MUST_SUCCEED_WITHIN`] wait are filled in.
    #[track_caller]
    fn advance_parked(&self, expected: Option<std::time::Instant>, by: Duration) -> Mark {
        self.clock.advance_when_parked(
            expected,
            self.runner,
            by,
            &Deadline::after(MUST_SUCCEED_WITHIN),
        )
    }

    fn send(&self, delivery: Delivery) {
        self.inbox.send(delivery).unwrap();
    }

    /// The durable lines from the last `turn_started` on.
    fn turn_lines(&self) -> Vec<Envelope> {
        let lines: Vec<Envelope> = log::read(&self.dir)
            .unwrap()
            .into_iter()
            .filter(Envelope::is_durable)
            .collect();
        let from = lines
            .iter()
            .rposition(|line| line.kind == "turn_started")
            .expect("a turn started");
        lines[from..].to_vec()
    }

    fn durable_kinds(&self) -> Vec<String> {
        log::read(&self.dir)
            .unwrap()
            .into_iter()
            .filter(Envelope::is_durable)
            .map(|line| line.kind)
            .collect()
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.provider.requests()
    }
}

fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

/// A whole session's durable kinds: its opening lines, then `turns`.
fn session_of(turns: &[Vec<&str>]) -> Vec<String> {
    ["session_started", "preamble_built", "opening_message"]
        .into_iter()
        .chain(turns.iter().flatten().copied())
        .map(str::to_owned)
        .collect()
}

/// A step whose reply is text alone, then the turn's end.
const REPLY: [&str; 5] = [
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// `turn_started`, `step_started`, then `between`, then [`REPLY`].
fn one_step_with<'a>(between: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["turn_started", "step_started"];
    all.extend_from_slice(between);
    all.extend_from_slice(&REPLY);
    all
}

/// The user messages `request` sends, in order.
fn users(request: &ModelRequest) -> Vec<String> {
    request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.clone()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect()
}

/// Parks the loop in its idle wait, then moves past the deadline: the next
/// lines the test sends decide what the last idle check finds.
#[track_caller]
fn park_idle(world: &World) -> Mark {
    let origin = world.clock.now();
    world.advance_parked(Some(origin + IDLE), IDLE)
}

#[test]
fn a_message_sent_just_before_the_last_idle_check_is_run() {
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    let admitted = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicBool::new(false));
    {
        let admission = Arc::clone(&world.admission);
        let inbox = world.inbox.clone();
        let admitted = Arc::clone(&admitted);
        let sent = Arc::clone(&sent);
        world.hook(move |at| {
            if !matches!(at, Window::Before) || admitted.load(Ordering::SeqCst) {
                return;
            }
            admitted.store(true, Ordering::SeqCst);
            // The door admits under the same lock the check closes under.
            let done = admission.admit(|| inbox.send(delivery(named(), ignore())).is_ok());
            sent.store(done.is_some(), Ordering::SeqCst);
        });
    }
    let origin = world.clock.now();
    let finished = world.spawn_run();
    let mark = park_idle(&world);
    world.send(Delivery::Cancelled);
    // The message turn runs; the loop parks again, then exits past a
    // second deadline with the hook spent.
    assert!(
        world
            .clock
            .await_any_parked_since(&mark, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the message turn ran and the loop parked again: {:?}",
        world.clock.parked()
    );
    // Nothing closed while the loop runs on: a later message is still
    // admitted; the idle exit that ends the run closes admission.
    assert!(world.admission.admit(|| true).is_some());
    world.advance_parked(Some(origin + IDLE + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("run returned");
    assert!(result.is_ok(), "{result:?}");
    assert!(sent.load(Ordering::SeqCst));
    assert!(world.admission.admit(|| true).is_none());
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&[]));
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(users(&requests[0]).last(), Some(&NAMED.to_owned()));
}

#[test]
fn a_message_sent_just_after_the_last_idle_check_is_refused() {
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    let refused = Arc::new(AtomicBool::new(false));
    let called = Arc::new(AtomicBool::new(false));
    {
        let admission = Arc::clone(&world.admission);
        let refused = Arc::clone(&refused);
        let called = Arc::clone(&called);
        world.hook(move |at| {
            if !matches!(at, Window::After) {
                return;
            }
            // The flag is already set: the send never runs.
            let done = admission.admit(|| {
                called.store(true, Ordering::SeqCst);
            });
            refused.store(done.is_none(), Ordering::SeqCst);
        });
    }
    let looped = world.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    world.runner = Some(handle.thread().id());
    let origin = world.clock.now();
    // The clock moves first, then the wake lands, so the exit check
    // finds an empty inbox: a wake taken before the advance would park
    // again with no later wake.
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let (mut looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    assert!(outcome.unwrap().is_none());
    assert!(refused.load(Ordering::SeqCst));
    assert!(!called.load(Ordering::SeqCst));
    assert!(looped.inbox.try_recv().is_err());
    assert_eq!(world.durable_kinds(), ["session_started"]);
    world.looped = Some(looped);
}

#[test]
fn an_idle_message_frames_an_unnamed_sender_by_its_id_alone() {
    let mut world = World::new(Vec::new(), Vec::new());
    let mut looped = world.looped.take().unwrap();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .admit_message_idle(
            super::for_test(FROM, None, None, COMMAND, TEXT),
            ignore(),
            &mut input,
        )
        .unwrap();
    assert_eq!(input.pieces.len(), 1);
    let crate::jobs::Queued::Steer(message, _) = &input.pieces[0] else {
        panic!("an idle message joins the input as a steer");
    };
    assert_eq!(
        message.content,
        vec![ContentPart::Text {
            text: UNNAMED.to_owned(),
        }]
    );
    world.looped = Some(looped);
}

#[test]
fn a_message_accepted_before_a_close_that_lands_ahead_of_the_last_idle_check_is_run() {
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    let admitted = Arc::new(AtomicBool::new(false));
    let message_in = Arc::new(AtomicBool::new(false));
    let (closed_tx, closed_rx) = mpsc::channel();
    {
        let admission = Arc::clone(&world.admission);
        let inbox = world.inbox.clone();
        let admitted = Arc::clone(&admitted);
        let message_in = Arc::clone(&message_in);
        world.hook(move |at| {
            if !matches!(at, Window::Before) || admitted.load(Ordering::SeqCst) {
                return;
            }
            admitted.store(true, Ordering::SeqCst);
            // The door admits the message, then another door closes: the
            // check finds the flag already set and still reads.
            let done = admission.admit(|| inbox.send(delivery(named(), ignore())).is_ok());
            message_in.store(done.is_some(), Ordering::SeqCst);
            admission.close();
            let (ack, rx) = mpsc::channel();
            inbox
                .send(Delivery::Close(Ack(Box::new(move |answer| {
                    let _sent = ack.send(answer.map(|_| ()));
                }))))
                .unwrap();
            let _sent = closed_tx.send(rx);
        });
    }
    let finished = world.spawn_run();
    let mark = park_idle(&world);
    world.send(Delivery::Cancelled);
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("run returned");
    assert!(result.is_ok(), "{result:?}");
    assert!(admitted.load(Ordering::SeqCst));
    assert!(message_in.load(Ordering::SeqCst));
    let rx = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&closed_rx)
        .expect("the close was sent");
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("the close was answered")
            .is_ok()
    );
    // One turn for the message, then the close ends the run.
    assert_eq!(world.durable_kinds(), session_of(&[one_step_with(&[])]));
    let lines = world.turn_lines();
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
    // The flag stayed set: nothing more enters.
    assert!(world.admission.admit(|| true).is_none());
    drop(mark);
}

#[test]
fn a_question_wait_with_a_session_message_queued_does_not_end_idle() {
    let mut world = World::with_tools(
        vec![calls("asks"), Scripted::text("Done.")],
        Vec::new(),
        vec![Arc::new(Asks)],
        Arc::new(AskGated),
    )
    .woken();
    let origin = world.clock.now();
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, lines) = read_until(
        world.watched.take().unwrap(),
        "an interaction_requested line",
        |line| line.kind == "interaction_requested",
    );
    world.watched = Some(watcher);
    assert_eq!(lines.last().unwrap().payload["resumes"], true);
    let request = RequestId(
        lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    // The loop is parked in the wait before the clock moves.
    let inbox = world.inbox.clone();
    let mark = world.clock.advance_when_parked_then(
        Some(origin + IDLE),
        world.runner,
        IDLE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            inbox
                .send(delivery(
                    super::for_test(FROM, None, Some("builder"), COMMAND, TEXT),
                    ignore(),
                ))
                .unwrap();
        },
    );
    world.send(Delivery::Cancelled);
    // Past the delay the wait holds no deadline and writes nothing more.
    assert!(
        world
            .clock
            .await_parked_since(&mark, None, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the wait stays parked with no deadline: {:?}",
        world.clock.parked()
    );
    match finished.try_recv() {
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => panic!("the turn thread ended early"),
        Ok(_) => panic!("the turn ended early"),
    }
    world.send(Delivery::Reply(
        Reply {
            request_id: request,
            answer: ReplyAnswer::Confirmed { confirmed: true },
        },
        ignore(),
    ));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let applied: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].payload["source"], "session");
    assert_eq!(applied[0].payload["from_session_id"], FROM);
    assert_eq!(applied[0].payload["command_id"], COMMAND);
    assert_eq!(applied[0].payload["content"][0]["text"], json!(NAMED));
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&NAMED.to_owned()));
}

#[test]
fn an_approval_wait_with_a_session_message_queued_does_not_end_idle() {
    let mut world = World::with_tools(
        vec![calls("gated"), Scripted::text("Done.")],
        Vec::new(),
        vec![Arc::new(Gated)],
        Arc::new(AskGated),
    );
    let origin = world.clock.now();
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, lines) = read_until(
        world.watched.take().unwrap(),
        "a permission_requested line",
        |line| line.kind == "permission_requested",
    );
    world.watched = Some(watcher);
    let request = RequestId(
        lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let inbox = world.inbox.clone();
    let mark = world.clock.advance_when_parked_then(
        Some(origin + IDLE),
        world.runner,
        IDLE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            inbox
                .send(delivery(
                    super::for_test(FROM, None, Some("builder"), COMMAND, TEXT),
                    ignore(),
                ))
                .unwrap();
        },
    );
    world.send(Delivery::Cancelled);
    assert!(
        world
            .clock
            .await_parked_since(&mark, None, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the wait stays parked with no deadline: {:?}",
        world.clock.parked()
    );
    match finished.try_recv() {
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => panic!("the turn thread ended early"),
        Ok(_) => panic!("the turn ended early"),
    }
    world.send(Delivery::Reply(
        Reply {
            request_id: request,
            answer: ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: None,
                remember: None,
            },
        },
        ignore(),
    ));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let applied: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].payload["source"], "session");
    assert_eq!(applied[0].payload["command_id"], COMMAND);
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&NAMED.to_owned()));
}

#[test]
fn a_session_message_during_an_approval_wait_is_answered_at_once() {
    // While the loop waits on a permission approval, a session message
    // is taken and answered at once (`docs/loop.md`, "One step"): the
    // delivery itself wakes the wait, so no reply, cancel or clock move
    // lands first. The approval still waits afterwards.
    let mut world = World::with_tools(
        vec![calls("gated"), Scripted::text("Done.")],
        Vec::new(),
        vec![Arc::new(Gated)],
        Arc::new(AskGated),
    );
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, lines) = read_until(
        world.watched.take().unwrap(),
        "a permission_requested line",
        |line| line.kind == "permission_requested",
    );
    world.watched = Some(watcher);
    let request = RequestId(
        lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let parked = Arc::clone(&world.clock);
    fakes::within(
        "the approval wait parked",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            while parked.parked().is_empty() {
                thread::yield_now();
            }
        },
    );
    let (ack, rx) = capture();
    world.send(delivery(
        super::for_test(FROM, None, Some("builder"), COMMAND, TEXT),
        ack,
    ));
    // Answered while the approval is still pending: nothing else lands
    // in between, and the clock never moves.
    let answer = answered(&rx, "the approval wait answers the session message at once");
    assert!(answer.is_ok(), "the message is accepted: {answer:?}");
    match finished.try_recv() {
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => panic!("the turn thread ended early"),
        Ok(_) => panic!("the turn ended early"),
    }
    world.send(Delivery::Reply(
        Reply {
            request_id: request,
            answer: ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: None,
                remember: None,
            },
        },
        ignore(),
    ));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let applied: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].payload["source"], "session");
    assert_eq!(applied[0].payload["command_id"], COMMAND);
}

#[test]
fn a_session_message_during_a_question_wait_is_answered_at_once() {
    // While the loop waits on an interaction answer, a session message
    // is taken and answered at once: the delivery itself wakes the
    // wait, so no reply, cancel or clock move lands first. The question
    // still waits afterwards.
    let mut world = World::with_tools(
        vec![calls("asks"), Scripted::text("Done.")],
        Vec::new(),
        vec![Arc::new(Asks)],
        Arc::new(AskGated),
    )
    .woken();
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, _) = read_until(
        world.watched.take().unwrap(),
        "an interaction_requested line",
        |line| line.kind == "interaction_requested",
    );
    world.watched = Some(watcher);
    let parked = Arc::clone(&world.clock);
    fakes::within(
        "the question wait parked",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            while parked.parked().is_empty() {
                thread::yield_now();
            }
        },
    );
    let (ack, rx) = capture();
    world.send(delivery(
        super::for_test(FROM, None, Some("builder"), COMMAND, TEXT),
        ack,
    ));
    // Answered while the question is still pending: nothing else lands
    // in between, and the clock never moves.
    let answer = answered(&rx, "the question wait answers the session message at once");
    assert!(answer.is_ok(), "the message is accepted: {answer:?}");
    match finished.try_recv() {
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => panic!("the turn thread ended early"),
        Ok(_) => panic!("the turn ended early"),
    }
}

#[test]
fn an_offer_wait_with_a_session_message_in_the_input_does_not_end_idle() {
    let code = FakeRepo::server("mcp-x");
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new()).offered(code);
    let finished = world.spawn_turn();
    world.send(delivery(named(), ignore()));
    let (watcher, lines) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    world.watched = Some(watcher);
    let request = RequestId(
        lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    // The offer waits with the message in the turn's input.
    let mark = world.advance_parked(None, IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        world
            .clock
            .await_parked_since(&mark, None, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the wait stays parked with no deadline: {:?}",
        world.clock.parked()
    );
    world.send(Delivery::Reply(
        Reply {
            request_id: request,
            answer: ReplyAnswer::Decisions {
                decisions: vec![contract::events::OfferDecision::Approve],
            },
        },
        ignore(),
    ));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    // The turn started with the message as its input.
    let lines = world.turn_lines();
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
}

#[test]
fn an_offer_wait_holding_a_session_message_that_arrived_during_it_does_not_end_idle() {
    let code = FakeRepo::server("mcp-x");
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new()).offered(code);
    let origin = world.clock.now();
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, lines) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    world.watched = Some(watcher);
    let request = RequestId(
        lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let inbox = world.inbox.clone();
    let mark = world.clock.advance_when_parked_then(
        Some(origin + IDLE),
        world.runner,
        IDLE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            inbox.send(delivery(named(), ignore())).unwrap();
        },
    );
    world.send(Delivery::Cancelled);
    assert!(
        world
            .clock
            .await_parked_since(&mark, None, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the wait stays parked with no deadline: {:?}",
        world.clock.parked()
    );
    world.send(Delivery::Reply(
        Reply {
            request_id: request,
            answer: ReplyAnswer::Decisions {
                decisions: vec![contract::events::OfferDecision::Approve],
            },
        },
        ignore(),
    ));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    // The prompt started the turn; the message held aside joins it.
    let lines = world.turn_lines();
    let input = lines[0].payload["input"].as_array().unwrap().clone();
    assert_eq!(input.len(), 2);
    assert_eq!(input[1]["source"], "session");
    assert_eq!(input[1]["command_id"], COMMAND);
    assert_eq!(input[1]["content"][0]["text"], json!(NAMED));
}

#[test]
fn an_offer_wait_with_a_session_message_in_the_input_ends_on_close_and_runs_it() {
    let code = FakeRepo::server("mcp-x");
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new()).offered(code);
    let finished = world.spawn_run();
    world.send(delivery(named(), ignore()));
    let (_watcher, _) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    let parked = Arc::clone(&world.clock);
    fakes::within(
        "the offer wait parked",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            while parked.parked().is_empty() {
                thread::yield_now();
            }
        },
    );
    world.send(close());
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("run returned");
    assert!(result.is_ok(), "{result:?}");
    // The close ended the offer wait, and the turn still ran the message.
    let lines = world.turn_lines();
    assert_eq!(lines[0].kind, "turn_started");
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
}

#[test]
fn a_required_offer_failure_with_a_held_message_fails_the_run() {
    // A session message accepted before an offer wait with an unapproved
    // `required` item, then ordinary `close`: the wait ends, the run fails
    // on the required item, and the held message never runs.
    let code = FakeRepo::required("mcp-x");
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new()).offered(code);
    let finished = world.spawn_run();
    world.send(delivery(named(), ignore()));
    let (_watcher, _) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    fakes::within(
        "the offer wait parked",
        Deadline::after(MUST_SUCCEED_WITHIN),
        {
            let parked = Arc::clone(&world.clock);
            move || {
                while parked.parked().is_empty() {
                    thread::yield_now();
                }
            }
        },
    );
    world.send(close());
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("run returned");
    let error = result.expect_err("the required item fails the run");
    assert!(
        matches!(error, crate::Error::RepositoryCode(_)),
        "{error:?}"
    );
    assert!(
        world
            .durable_kinds()
            .iter()
            .all(|kind| kind != "turn_started"),
        "no turn ran: {:?}",
        world.durable_kinds()
    );
}

#[test]
fn a_question_wait_refuses_a_message_sent_just_after_its_last_idle_check() {
    let mut world = World::with_tools(
        vec![calls("asks"), Scripted::text("Done.")],
        Vec::new(),
        vec![Arc::new(Asks)],
        Arc::new(AskGated),
    )
    .woken();
    let origin = world.clock.now();
    let refused = Arc::new(AtomicBool::new(false));
    {
        let admission = Arc::clone(&world.admission);
        let refused = Arc::clone(&refused);
        world.hook(move |at| {
            if !matches!(at, Window::After) {
                return;
            }
            let done = admission.admit(|| {});
            refused.store(done.is_none(), Ordering::SeqCst);
        });
    }
    let finished = world.spawn_turn();
    world.send(Delivery::Prompt(
        contract::inbox::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: From {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_prompt".into())),
            },
        },
        ignore(),
    ));
    let (watcher, _) = read_until(
        world.watched.take().unwrap(),
        "an interaction_requested line",
        |line| line.kind == "interaction_requested",
    );
    world.watched = Some(watcher);
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    assert!(outcome.unwrap().is_none());
    assert!(refused.load(Ordering::SeqCst));
    assert!(looped.idle_left);
    world.looped = Some(looped);
}

#[test]
fn a_session_message_queued_as_a_steer_keeps_the_session_from_idle() {
    let mut world = World::new(Vec::new(), Vec::new());
    let mut looped = world.looped.take().unwrap();
    // Only the first disjunct holds: one session steer in `queued`, no
    // steering session, nothing deferred, nothing in the input.
    looped.queued.push_back(crate::jobs::Queued::Steer(
        Message {
            content: vec![ContentPart::Text { text: TEXT.into() }],
            sender: From {
                origin: Origin::Session {
                    from_session_id: contract::SessionId("s_other".into()),
                },
                command_id: Some(CommandId(COMMAND.into())),
            },
        },
        crate::before_message::Hooked::default(),
    ));
    assert!(
        looped.holds_message(),
        "a queued session steer alone waits to join a turn"
    );
    world.looped = Some(looped);
}

/// The framed rewrite of `text` for the named sender `FROM`.
fn framed_rewrite(text: &str) -> String {
    format!(
        "Fiber: a message from session {FROM}, named \"builder\". Another session wrote it, not the person.\n\n{text}"
    )
}

#[test]
fn a_refused_session_message_answers_message_refused_and_queues_nothing() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(refused("e", "no"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    let (ack, answers) = capture();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .admit_idle(delivery(named(), ack), &mut input)
        .unwrap();
    let rejection = rejected(answered(&answers, "the refusal"));
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    assert_eq!(rejection.message, "Extension e refused the message: no");
    assert!(input.pieces.is_empty(), "a refusal queues nothing");
    assert!(
        looped.steering.snapshot().is_empty(),
        "a refusal leaves no queue entry"
    );
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    assert_eq!(
        hooks.seen(),
        vec![TEXT.to_owned()],
        "the hook saw the raw text"
    );
    assert_eq!(
        hooks.seen_ids(),
        vec![(Some(FROM.to_owned()), Some(PARENT.to_owned()))],
        "the hook saw the sender and its parent"
    );
    assert_eq!(world.durable_kinds(), ["session_started"]);
    world.looped = Some(looped);
}

#[test]
fn a_blocking_hook_failure_answers_hook_failed_and_queues_nothing() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(failed("e", "boom"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    let (ack, answers) = capture();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .admit_idle(delivery(named(), ack), &mut input)
        .unwrap();
    let rejection = rejected(answered(&answers, "the failure"));
    assert_eq!(rejection.code, ErrorCode::HookFailed);
    assert_eq!(
        rejection.message,
        "The `before_message` hook of extension e failed, so the message was not sent: boom"
    );
    assert!(input.pieces.is_empty(), "a failure queues nothing");
    assert!(
        looped.steering.snapshot().is_empty(),
        "a failure leaves no queue entry"
    );
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    assert_eq!(world.durable_kinds(), ["session_started"]);
    world.looped = Some(looped);
}

#[test]
fn a_hook_parked_past_its_registered_timeout_answers_hook_failed() {
    // The registered timeout the parked run outlives.
    const TIMEOUT: Duration = Duration::from_secs(30);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let hooks = Arc::new(FakeHooks::new());
    *hooks.park.lock().unwrap() = Some(Park {
        entered: entered_tx,
        release: release_rx,
    });
    hooks.answer(failed("e", "timed out"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(hooks);
    let looped = world.looped.take().unwrap();
    let (ack, answers) = capture();
    let taken = thread::spawn(move || {
        let mut input = crate::inbox::TurnInput::of(Vec::new());
        let mut looped = looped;
        let outcome = looped.admit_idle(delivery(named(), ack), &mut input);
        (looped, outcome, input.pieces.is_empty())
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&entered_rx)
        .expect("the hook parked");
    assert!(
        answers.try_recv().is_err(),
        "the sender waits while the hook runs"
    );
    // Only after the parked signal does the clock move past the timeout.
    // No code under test waits on the clock here: the timeout is the scripted answer and admit_idle reads no clock while the hook runs (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    world.clock.advance(TIMEOUT + Duration::from_secs(1));
    release_tx.send(()).unwrap();
    let rejection = rejected(answered(&answers, "the timeout failure"));
    assert_eq!(rejection.code, ErrorCode::HookFailed);
    assert_eq!(
        rejection.message,
        "The `before_message` hook of extension e failed, so the message was not sent: timed out"
    );
    let (looped, outcome, empty) = taken.join().expect("the take ended");
    outcome.unwrap();
    assert!(empty, "a timed-out hook queues nothing");
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    world.looped = Some(looped);
}

#[test]
fn a_non_blocking_hook_failure_still_delivers_and_logs_its_notice() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(noticed(ErrorCode::HookFailed, "e"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let finished = world.spawn_turn();
    let (ack, answers) = capture();
    world.send(delivery(named(), ack));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(
        answered(&answers, "the acceptance"),
        Ok(None),
        "a non-blocking failure still delivers"
    );
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    let (watcher, streamed) = read_until(
        world.watched.take().unwrap(),
        "a turn_completed line",
        |line| line.kind == "turn_completed",
    );
    world.watched = Some(watcher);
    let notice = streamed
        .iter()
        .find(|line| line.kind == "notice")
        .expect("the soft failure is logged as a notice");
    assert!(notice.turn_id.is_none(), "an idle notice names no turn");
    assert_eq!(notice.payload["code"], "hook_failed");
    assert_eq!(notice.payload["extension"], "e");
    let lines = world.turn_lines();
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": NAMED}],
        }])
    );
}

#[test]
fn a_rewritten_session_message_frames_its_replacement_with_changed_by() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced("Say cheese.", "e"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let finished = world.spawn_turn();
    let (ack, answers) = capture();
    world.send(delivery(named(), ack));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(answered(&answers, "the acceptance"), Ok(None));
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    assert_eq!(
        hooks.seen(),
        vec![TEXT.to_owned()],
        "the hook saw the raw text"
    );
    let lines = world.turn_lines();
    assert_eq!(
        lines[0].payload["input"],
        json!([{
            "type": "message",
            "source": "session",
            "from_session_id": FROM,
            "command_id": COMMAND,
            "content": [{"type": "text", "text": framed_rewrite("Say cheese.")}],
            "changed_by": ["e"],
        }])
    );
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        users(&requests[0]).last(),
        Some(&framed_rewrite("Say cheese."))
    );
}

#[test]
fn a_session_message_context_joins_the_turn_as_context_added() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed_with_context("e", "Remember this."));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let finished = world.spawn_turn();
    let (ack, answers) = capture();
    world.send(delivery(named(), ack));
    let (looped, outcome) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(answered(&answers, "the acceptance"), Ok(None));
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    let lines = world.turn_lines();
    let added: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "context_added")
        .collect();
    assert_eq!(added.len(), 1, "the hook's context is logged");
    assert_eq!(added[0].payload["hook"], "before_message");
    assert_eq!(added[0].payload["extension"], "e");
    assert_eq!(added[0].payload["text"], "Remember this.");
}

#[test]
fn a_running_rewrite_stages_its_verdict_on_the_shared_queue_entry() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced("Say cheese.", "e"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    let (ack, answers) = capture();
    let turn = TurnId("t_1".into());
    looped.admit_message_running(named(), ack, &turn).unwrap();
    assert_eq!(answered(&answers, "the acceptance"), Ok(None));
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    let id = CommandId(COMMAND.into());
    let origin = Origin::Session {
        from_session_id: SessionId(FROM.into()),
    };
    assert!(
        looped.steering.is_staged(&id, &origin),
        "the session entry stages from the start"
    );
    assert!(
        !looped.steering.is_admitted(&id),
        "the session id stays stale to is_admitted"
    );
    // The staged claim drains the entry while the id stays stale.
    struct Discard;
    impl Emit for Discard {
        fn emit(&self, _event: &contract::events::Event) {}
    }
    let taken = looped.steering.take_staged(&Discard);
    assert_eq!(taken.len(), 1, "take_staged drains the session entry");
    assert_eq!(
        taken[0]
            .staged
            .as_ref()
            .and_then(|staged| staged.changed_by.clone()),
        Some(vec!["e".to_owned()]),
        "the verdict's staged metadata rides the entry"
    );
    assert!(
        !looped.steering.is_admitted(&id),
        "the drained session id stays stale"
    );
    // The queued holder carries the same post-hook content.
    assert_eq!(looped.queued.len(), 1);
    let crate::jobs::Queued::Steer(message, hooked) = &looped.queued[0] else {
        panic!("a running message queues its holder");
    };
    assert_eq!(
        message.content,
        vec![ContentPart::Text {
            text: framed_rewrite("Say cheese."),
        }]
    );
    assert_eq!(hooked.changed_by, Some(vec!["e".to_owned()]));
    world.looped = Some(looped);
}

#[test]
fn a_running_refusal_answers_message_refused_and_queues_nothing() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(refused("e", "no"));
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    let (ack, answers) = capture();
    let turn = TurnId("t_1".into());
    looped.admit_message_running(named(), ack, &turn).unwrap();
    let rejection = rejected(answered(&answers, "the refusal"));
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    assert_eq!(rejection.message, "Extension e refused the message: no");
    assert!(looped.queued.is_empty(), "a refusal queues no holder");
    assert!(
        looped.steering.snapshot().is_empty(),
        "a refusal leaves no queue entry"
    );
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    world.looped = Some(looped);
}

#[test]
fn a_shutdown_before_the_hook_runs_answers_closing() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    looped.cancel.shutdown(143);
    let (ack, answers) = capture();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .admit_idle(delivery(named(), ack), &mut input)
        .unwrap();
    let rejection = rejected(answered(&answers, "the closing rejection"));
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert!(input.pieces.is_empty(), "a closing take queues nothing");
    assert_eq!(hooks.seen().len(), 0, "no hook ran");
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    world.looped = Some(looped);
}

#[test]
fn a_running_shutdown_before_the_hook_runs_answers_closing() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new());
    world.hook_with(Arc::clone(&hooks));
    let mut looped = world.looped.take().unwrap();
    looped.cancel.shutdown(143);
    let (ack, answers) = capture();
    let turn = TurnId("t_1".into());
    looped.admit_message_running(named(), ack, &turn).unwrap();
    let rejection = rejected(answered(&answers, "the closing rejection"));
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert!(looped.queued.is_empty(), "a closing take queues no holder");
    assert_eq!(hooks.seen().len(), 0, "no hook ran");
    assert!(answers.try_recv().is_err(), "the ack fires exactly once");
    world.looped = Some(looped);
}

/// A bare loop on a temporary tree, with its log.
fn bare() -> (Loop, fakes::TempDir) {
    let home = fakes::TempDir::new("fiber-add-dir-approval");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let fake = FakeClock::new();
    let clock: Arc<dyn contract::clock::Clock> = fake.clone();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel();
    let looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::new(ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                clock,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(AskGated),
            },
        },
        None,
    )
    .unwrap();
    (looped, home)
}

#[test]
fn add_directory_while_an_approval_waits_is_written_and_a_later_call_sees_it() {
    let (mut looped, home) = bare();
    let shared = home.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let pending = RequestId("r_approval".into());
    let turn = TurnId("t_test".into());
    let (ack, answers) = capture();
    let waited = looped
        .take_while_waiting(
            &pending,
            Delivery::AddDirectory(
                contract::commands::AddDirectoryArgs {
                    path: shared.display().to_string(),
                },
                ack,
            ),
            &turn,
        )
        .unwrap();
    assert!(matches!(waited, crate::inbox::Waited::Again));
    assert!(matches!(
        answered(&answers, "add_directory is accepted"),
        Ok(None)
    ));
    // The event reaches the log before the ack runs.
    let kinds: Vec<String> = log::read(&home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(kinds.contains(&"directory_added".to_owned()), "{kinds:?}");
    // A later call with paths under the added root fast-paths against
    // the loop's folded roots.
    let verdict = crate::permission::judge(
        "write",
        &Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Writes],
                reversible: false,
                paths: Some(vec![shared.join("new.rs").display().to_string()]),
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        },
        &Ok(contract::rules::StandingRules::default()),
        &[],
        &crate::known_hosts::KnownHosts::default(),
        &looped.workspace,
        looped.changes.added(),
        home.path(),
        &looped.credentials,
        &[],
    );
    assert!(matches!(verdict, crate::permission::Verdict::Allow(None)));
}
