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
use contract::inbox::{Ack, Delivery, SessionMessage};
use contract::provider::{Delta, Input, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender as From};
use contract::tool::{Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, RequestId, SessionId};
use fakes::Deadline;
use fakes::clock::{FakeClock, Mark};
use fakes::{Scripted, ScriptedProvider};
use log::{Log, Watcher};
use serde_json::{Map, Value, json};
use support::admission::Admission;

use super::Window;
use crate::{Loop, Model};

const DEADLINE: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_secs(60);

const FROM: &str = "s_0123456789abcdef";
const COMMAND: &str = "c_m1";
const TEXT: &str = "Tests pass on main.";
const NAMED: &str = "Fiber: a message from session s_0123456789abcdef, named \"builder\". Another session wrote it, not the person.\n\nTests pass on main.";
const UNNAMED: &str = "Fiber: a message from session s_0123456789abcdef. Another session wrote it, not the person.\n\nTests pass on main.";

/// A session message from `FROM`, named `builder`, sent by `COMMAND`.
fn named() -> SessionMessage {
    super::for_test(FROM, Some("builder"), COMMAND, TEXT)
}

/// The `Delivery` for `message`, as the door sends it.
fn delivery(message: SessionMessage) -> Delivery {
    Delivery::Message(message)
}

fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

fn close() -> Delivery {
    Delivery::Close(ignore())
}

/// Reads `watcher` until `done` accepts a line, under one [`DEADLINE`].
#[track_caller]
fn read_until(
    watcher: Watcher,
    what: &str,
    done: impl Fn(&Envelope) -> bool + Send + 'static,
) -> (Watcher, Vec<Envelope>) {
    let what_owned = what.to_owned();
    fakes::within(what, DEADLINE, move || {
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

    /// Runs the loop's idle wait around `offer`: `window` sees `Before`
    /// and, when nothing is found, `After`.
    fn hook(&mut self, window: impl FnMut(Window) + Send + 'static) {
        self.looped.as_mut().unwrap().window = Some(Box::new(window));
    }

    /// Runs the loop to its end on its own thread.
    fn spawn_run(&mut self) -> mpsc::Receiver<Result<(), crate::Error>> {
        let looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let _sent = done.send(looped.run());
        });
        finished
    }

    /// Runs one turn on its own thread, returning the loop with it.
    fn spawn_turn(&mut self) -> mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)> {
        let looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let mut looped = looped;
            let outcome = looped.turn();
            let _sent = done.send((looped, outcome));
        });
        finished
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
            | Input::ToolResult { .. } => None,
        })
        .collect()
}

/// Parks the loop in its idle wait, then moves past the deadline: the next
/// lines the test sends decide what the last idle check finds.
fn park_idle(world: &World) -> Mark {
    let origin = world.clock.now();
    assert!(
        world.clock.await_parked(origin + IDLE, DEADLINE),
        "the loop parked in its idle wait: {:?}",
        world.clock.parked()
    );
    world.clock.advance_marked(IDLE)
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
            let done = admission.admit(|| inbox.send(delivery(named())).is_ok());
            sent.store(done.is_some(), Ordering::SeqCst);
        });
    }
    let finished = world.spawn_run();
    let mark = park_idle(&world);
    world.send(Delivery::Cancelled);
    // The message turn runs; the loop parks again, then exits past a
    // second deadline with the hook spent.
    assert!(
        world.clock.await_any_parked_since(&mark, DEADLINE),
        "the message turn ran and the loop parked again: {:?}",
        world.clock.parked()
    );
    // Nothing closed while the loop runs on: a later message is still
    // admitted; the idle exit that ends the run closes admission.
    assert!(world.admission.admit(|| true).is_some());
    world.clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    let result = Deadline::after(DEADLINE)
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
    thread::spawn(move || {
        let mut looped = looped;
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    let origin = world.clock.now();
    assert!(
        world.clock.await_parked(origin + IDLE, DEADLINE),
        "the loop parked in its idle wait: {:?}",
        world.clock.parked()
    );
    // The clock moves first, then the wake lands, so the exit check
    // finds an empty inbox: a wake taken before the advance would park
    // again with no later wake.
    world.clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    let (looped, outcome) = Deadline::after(DEADLINE)
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
    looped.admit_message_idle(super::for_test(FROM, None, COMMAND, TEXT), &mut input);
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
            let done = admission.admit(|| inbox.send(delivery(named())).is_ok());
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
    let result = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("run returned");
    assert!(result.is_ok(), "{result:?}");
    assert!(admitted.load(Ordering::SeqCst));
    assert!(message_in.load(Ordering::SeqCst));
    let rx = Deadline::after(DEADLINE)
        .recv(&closed_rx)
        .expect("the close was sent");
    assert!(
        Deadline::after(DEADLINE)
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
    let parked = Arc::clone(&world.clock);
    fakes::within("the question wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    world.send(delivery(super::for_test(
        FROM,
        Some("builder"),
        COMMAND,
        TEXT,
    )));
    let mark = world.clock.advance_marked(IDLE);
    world.send(Delivery::Cancelled);
    // Past the delay the wait holds no deadline and writes nothing more.
    assert!(
        world.clock.await_parked_since(&mark, None, DEADLINE),
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
    let (looped, outcome) = Deadline::after(DEADLINE)
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
    fakes::within("the approval wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    world.send(delivery(super::for_test(
        FROM,
        Some("builder"),
        COMMAND,
        TEXT,
    )));
    let mark = world.clock.advance_marked(IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        world.clock.await_parked_since(&mark, None, DEADLINE),
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
    let (looped, outcome) = Deadline::after(DEADLINE)
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
fn an_offer_wait_with_a_session_message_in_the_input_does_not_end_idle() {
    let code = FakeRepo::server("mcp-x");
    let mut world = World::new(vec![Scripted::text("Done.")], Vec::new()).offered(code);
    let finished = world.spawn_turn();
    world.send(delivery(named()));
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
    let parked = Arc::clone(&world.clock);
    fakes::within("the offer wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    let mark = world.clock.advance_marked(IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        world.clock.await_parked_since(&mark, None, DEADLINE),
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
    let (looped, outcome) = Deadline::after(DEADLINE)
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
    let parked = Arc::clone(&world.clock);
    fakes::within("the offer wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    world.send(delivery(named()));
    let mark = world.clock.advance_marked(IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        world.clock.await_parked_since(&mark, None, DEADLINE),
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
    let (looped, outcome) = Deadline::after(DEADLINE)
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
    world.send(delivery(named()));
    let (_watcher, _) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    let parked = Arc::clone(&world.clock);
    fakes::within("the offer wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    world.send(close());
    let result = Deadline::after(DEADLINE)
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
    world.send(delivery(named()));
    let (_watcher, _) = read_until(
        world.watched.take().unwrap(),
        "a repository_code_offered line",
        |line| line.kind == "repository_code_offered",
    );
    fakes::within("the offer wait parked", DEADLINE, {
        let parked = Arc::clone(&world.clock);
        move || {
            while parked.parked().is_empty() {
                thread::yield_now();
            }
        }
    });
    world.send(close());
    let result = Deadline::after(DEADLINE)
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
    let parked = Arc::clone(&world.clock);
    fakes::within("the question wait parked", DEADLINE, move || {
        while parked.parked().is_empty() {
            thread::yield_now();
        }
    });
    let _mark = world.clock.advance_marked(IDLE);
    world.send(Delivery::Cancelled);
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended");
    assert!(outcome.unwrap().is_none());
    assert!(refused.load(Ordering::SeqCst));
    assert!(looped.idle_left);
    world.looped = Some(looped);
}
