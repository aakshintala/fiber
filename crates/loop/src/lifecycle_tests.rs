//! `turn_start`, `before_model_call` and `turn_end` in the loop: what each
//! hook is shown, and how its answer reaches the turn, the request or the
//! next step.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{InputItem, TextDelta, ToolCallRequested, TurnOutcome, TurnStarted};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome,
    BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer, CheckOutcome, Context,
    ContextAnswer, ContextOutcome, Hooks,
};
use contract::inbox::{Ack, Answer, Delivery, Message};
use contract::provider::{Delta, ReplyAction};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender, Usage};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, SessionId, TurnId};
use fakes::Deadline;
use log::Log;

use crate::{Loop, Model};

/// One hang bound for every wait: the turn runs on a thread of its own, so
/// a stuck hook fails the test instead of hanging it.
const TURN_DEADLINE: Duration = Duration::from_secs(10);

/// A scripted `Hooks` fake: a queue of answers per lifecycle point, what
/// each call was shown, and an optional cancel or shutdown to start inside
/// `turn_start`'s own call.
struct FakeHooks {
    before_message: Mutex<VecDeque<BeforeMessageAnswer>>,
    turn_start: Mutex<VecDeque<ContextAnswer>>,
    before_model_call: Mutex<VecDeque<CheckAnswer>>,
    turn_end: Mutex<VecDeque<ContextAnswer>>,
    seen_input: Mutex<Vec<String>>,
    turn_start_calls: AtomicUsize,
    before_model_call_calls: AtomicUsize,
    seen_calls: Mutex<Vec<(String, Usage)>>,
    cancel_in_call: Mutex<Option<Arc<crate::TurnCancel>>>,
    turn_end_calls: AtomicUsize,
    seen_replies: Mutex<Vec<String>>,
    cancel_in_end: Mutex<Option<Arc<crate::TurnCancel>>>,
    steer_in_end: Mutex<Option<mpsc::Sender<Delivery>>>,
    cancel_in_hook: Mutex<Option<Arc<crate::TurnCancel>>>,
    shutdown_in_hook: Mutex<Option<Arc<crate::TurnCancel>>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            before_message: Mutex::new(VecDeque::new()),
            turn_start: Mutex::new(VecDeque::new()),
            before_model_call: Mutex::new(VecDeque::new()),
            turn_end: Mutex::new(VecDeque::new()),
            seen_input: Mutex::new(Vec::new()),
            turn_start_calls: AtomicUsize::new(0),
            before_model_call_calls: AtomicUsize::new(0),
            seen_calls: Mutex::new(Vec::new()),
            cancel_in_call: Mutex::new(None),
            turn_end_calls: AtomicUsize::new(0),
            seen_replies: Mutex::new(Vec::new()),
            cancel_in_end: Mutex::new(None),
            steer_in_end: Mutex::new(None),
            cancel_in_hook: Mutex::new(None),
            shutdown_in_hook: Mutex::new(None),
        }
    }

    fn answer_before_message(&self, answer: BeforeMessageAnswer) {
        self.before_message.lock().unwrap().push_back(answer);
    }

    fn answer_turn_start(&self, answer: ContextAnswer) {
        self.turn_start.lock().unwrap().push_back(answer);
    }

    fn answer_before_model_call(&self, answer: CheckAnswer) {
        self.before_model_call.lock().unwrap().push_back(answer);
    }

    fn answer_turn_end(&self, answer: ContextAnswer) {
        self.turn_end.lock().unwrap().push_back(answer);
    }
}

fn passed_message() -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: Vec::new(),
        },
        notices: Vec::new(),
    }
}

fn passed_message_with_context(context: Vec<(&str, &str)>) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: context
                .into_iter()
                .map(|(extension, text)| Context {
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
        },
        notices: Vec::new(),
    }
}

fn added(texts: Vec<(&str, &str)>) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Added(
            texts
                .into_iter()
                .map(|(extension, text)| Context {
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
        ),
        notices: Vec::new(),
    }
}

fn failed(by: &str, message: &str) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Failed(contract::hook::HookFailure {
            extension: by.to_owned(),
            message: message.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn noticed_soft(by: &str) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Added(Vec::new()),
        notices: vec![contract::events::Notice {
            code: ErrorCode::HookFailed,
            message: format!("extension {by} failed softly"),
            extension: Some(by.to_owned()),
        }],
    }
}

fn passed_call() -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Passed,
        notices: Vec::new(),
    }
}

fn refused_call(by: &str, reason: &str) -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Refused(contract::hook::Refusal {
            extension: by.to_owned(),
            reason: reason.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn failed_call(by: &str, message: &str) -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Failed(contract::hook::HookFailure {
            extension: by.to_owned(),
            message: message.to_owned(),
        }),
        notices: Vec::new(),
    }
}

impl Hooks for FakeHooks {
    fn after_tool(&self, _call: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        passed_call()
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.before_message
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed_message)
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

    fn turn_start(&self, turn: &contract::hook::TurnStart<'_>) -> ContextAnswer {
        self.turn_start_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_input.lock().unwrap().push(format!(
            "{:?}",
            turn.input
                .iter()
                .map(|item| match item {
                    InputItem::Message { content, .. } => crate::conversation::text(content),
                    InputItem::Jobs { .. } => "jobs".to_owned(),
                    InputItem::Handoff { .. } => "handoff".to_owned(),
                    InputItem::ShellCommand { .. } | InputItem::Unknown => "other".to_owned(),
                })
                .collect::<Vec<_>>()
        ));
        if let Some(cancel) = self.cancel_in_hook.lock().unwrap().clone() {
            cancel.cancel();
        }
        if let Some(cancel) = self.shutdown_in_hook.lock().unwrap().clone() {
            cancel.shutdown(143);
        }
        self.turn_start
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| added(Vec::new()))
    }

    fn before_model_call(&self, call: &contract::hook::BeforeModelCall<'_>) -> CheckAnswer {
        self.before_model_call_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_calls
            .lock()
            .unwrap()
            .push((call.model.to_owned(), call.usage.clone()));
        if let Some(cancel) = self.cancel_in_call.lock().unwrap().clone() {
            cancel.cancel();
        }
        self.before_model_call
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed_call)
    }

    fn turn_end(&self, end: &contract::hook::TurnEnd<'_>) -> ContextAnswer {
        self.turn_end_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_replies.lock().unwrap().push(end.reply.to_owned());
        if let Some(cancel) = self.cancel_in_end.lock().unwrap().clone() {
            cancel.cancel();
        }
        // A steer sent here waits in the inbox while the hook runs, so the
        // next step writes it after the hook's own messages.
        if let Some(inbox) = self.steer_in_end.lock().unwrap().clone() {
            let ack = Ack(Box::new(|_| {}));
            inbox
                .send(Delivery::Steer(driver("c_steer", "late steer"), ack))
                .unwrap();
        }
        self.turn_end
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| added(Vec::new()))
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// What a turn-level test drives: the loop, its inbox, and the handles to
/// read what it wrote and was sent.
struct World {
    looped: Option<Loop>,
    inbox: mpsc::Sender<Delivery>,
    provider: Arc<fakes::ScriptedProvider>,
    dir: std::path::PathBuf,
    watcher: Option<log::Watcher>,
    _home: fakes::TempDir,
}

/// A `Loop` with its inbox still held by the test, `hooks` set when given,
/// and `script` as its provider's replies.
fn started(script: Vec<fakes::Scripted>, hooks: Option<Arc<FakeHooks>>) -> World {
    started_with_tools(script, hooks, Vec::new())
}

/// [`started`], with `tools` registered under `builtin`.
fn started_with_tools(
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let scripted = Arc::new(fakes::ScriptedProvider::new(script));
    let provider = Arc::clone(&scripted) as Arc<dyn contract::provider::Provider>;
    assemble(scripted, provider, hooks, tools)
}

/// [`started_with_tools`], reaching the scripted provider through `wrap`:
/// the test reads the scripted provider's requests while the loop runs
/// through the wrapper.
fn started_wrapped(
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
    wrap: impl FnOnce(
        Arc<fakes::ScriptedProvider>,
        mpsc::Sender<Delivery>,
    ) -> Arc<dyn contract::provider::Provider>,
) -> (World, Arc<fakes::ScriptedProvider>) {
    let scripted = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    let provider = wrap(Arc::clone(&scripted), inbox.clone());
    (
        assemble_with(scripted.clone(), provider, inbox, rx, hooks, tools),
        scripted,
    )
}

/// A `Loop` on `provider` with its inbox still held by the test.
fn assemble(
    scripted: Arc<fakes::ScriptedProvider>,
    provider: Arc<dyn contract::provider::Provider>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let (inbox, rx) = mpsc::channel();
    assemble_with(scripted, provider, inbox, rx, hooks, tools)
}

/// [`assemble`], with the test's own inbox channel.
fn assemble_with(
    scripted: Arc<fakes::ScriptedProvider>,
    provider: Arc<dyn contract::provider::Provider>,
    inbox: mpsc::Sender<Delivery>,
    rx: mpsc::Receiver<Delivery>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let home = fakes::TempDir::new("fiber-lifecycle");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    // Subscribed before the loop starts: the stream holds every line the
    // turn writes, ephemeral notices included, which the durable read skips.
    let watcher = log.watch();
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider,
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
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap()
    .answerable(false);
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks as Arc<dyn Hooks>);
    }
    World {
        looped: Some(looped),
        inbox,
        provider: scripted,
        dir: home.path().join("s_test"),
        watcher: Some(watcher),
        _home: home,
    }
}

/// A driver message with `id` saying `text`.
fn driver(id: &str, text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId(id.into())),
        },
    }
}

/// An acknowledgement recording its answer.
fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        })),
        rx,
    )
}

/// A test tool that only reads and returns `text`.
struct Probe;

impl Tool for Probe {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".into(),
            description: "Returns a fixed output.".into(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "tool output".into(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A reply saying `text`, then calling `name` with `arguments`, billed at
/// `cost`.
fn tool_call_reply(
    text: &str,
    name: &str,
    arguments: serde_json::Value,
    cost: f64,
) -> fakes::Scripted {
    let mut end = fakes::reply(text);
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    end.cost = Some(cost);
    fakes::Scripted {
        deltas: vec![Delta::Text(TextDelta { text: text.into() })],
        end: Ok(end),
    }
}

/// A reply of `text` alone, billed at `cost`.
fn priced(text: &str, cost: f64) -> fakes::Scripted {
    let mut scripted = fakes::Scripted::text(text);
    if let Ok(reply) = &mut scripted.end {
        reply.cost = Some(cost);
    }
    scripted
}

/// `scripted` with its reply's `input` and `output` tokens set.
fn with_tokens(mut scripted: fakes::Scripted, input: u64, output: u64) -> fakes::Scripted {
    if let Ok(reply) = &mut scripted.end {
        reply.tokens.input = input;
        reply.tokens.output = output;
    }
    scripted
}

type Finished = mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)>;

/// Runs the world's turn on a thread; the inbox stays open so the turn
/// parks on it when nothing starts one.
fn run_turn(world: &mut World) -> Finished {
    let mut looped = world.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        done.send((looped, outcome)).unwrap();
    });
    finished
}

fn finish_turn(world: &mut World, finished: &Finished) -> Option<TurnOutcome> {
    let (looped, outcome) = Deadline::after(TURN_DEADLINE)
        .recv(finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    outcome.unwrap()
}

/// Every durable line the world wrote.
fn lines(world: &World) -> Vec<Envelope> {
    log::read(&world.dir).unwrap()
}

/// The complete, ordered kinds of `lines`.
fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

fn line<'a>(lines: &'a [Envelope], kind: &str) -> &'a Envelope {
    lines.iter().find(|line| line.kind == kind).unwrap()
}

/// Every line the world's watcher received up to and including
/// `turn_completed`: the durable lines and the ephemeral ones. The read
/// runs on its own thread under one [`TURN_DEADLINE`] naming the awaited
/// completion, so a missing `turn_completed` fails instead of hanging on
/// the retained log's open watcher.
fn streamed(world: &mut World) -> Vec<Envelope> {
    let watcher = world.watcher.take().unwrap();
    let (watcher, streamed) = fakes::within(
        "a turn_completed line or the log's end",
        TURN_DEADLINE,
        move || {
            let mut watcher = watcher;
            let mut streamed = Vec::new();
            while let Ok(Some(line)) = watcher.recv() {
                let done = line.kind == "turn_completed";
                streamed.push(line);
                if done {
                    break;
                }
            }
            (watcher, streamed)
        },
    );
    world.watcher = Some(watcher);
    streamed
}

#[test]
fn a_turn_start_context_is_logged_before_the_first_step_and_sent_after_the_prompt() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_message(passed_message_with_context(vec![(
        "fiber.test/before",
        "earlier",
    )]));
    hooks.answer_turn_start(added(vec![("fiber.test/inbox", "3 new messages")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the prompt's answer")
            .is_ok(),
        "the prompt is accepted"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "context_added",
            "context_added",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let contexts: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .collect();
    assert_eq!(contexts.len(), 2);
    // A `before_message` context precedes the `turn_start` one.
    assert_eq!(contexts[0].payload["hook"], "before_message");
    assert_eq!(contexts[0].payload["text"], "earlier");
    assert_eq!(contexts[1].payload["hook"], "turn_start");
    assert_eq!(contexts[1].payload["extension"], "fiber.test/inbox");
    assert_eq!(contexts[1].payload["text"], "3 new messages");
    // The hook saw the turn's prompt.
    assert_eq!(
        hooks.seen_input.lock().unwrap().clone(),
        vec!["[\"hello\"]"]
    );
    // The first request ends with the context as its own user message
    // after the prompt.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 1);
    let tail: Vec<String> = requests[0]
        .conversation
        .iter()
        .rev()
        .take(3)
        .map(|input| match input {
            contract::provider::Input::User { text, .. } => text.clone(),
            contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolCall { .. }
            | contract::provider::Input::ToolResult { .. } => {
                panic!("a user message: {input:?}")
            }
        })
        .collect();
    assert_eq!(tail, vec!["3 new messages", "earlier", "hello"]);
}

#[test]
fn a_blocking_turn_start_failure_fails_the_turn_before_any_request() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(failed("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the prompt's answer")
            .is_ok(),
        "the prompt is accepted before the hook fails"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "turn_completed",
        ]
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `turn_start` hook of extension fiber.test/guard failed, \
         so the turn did not run: boom"
    );
    assert!(
        world.provider.requests().is_empty(),
        "the provider got no request"
    );
}

#[test]
fn a_non_blocking_turn_start_failure_writes_a_notice_and_the_turn_goes_on() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(noticed_soft("fiber.test/soft"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = streamed(&mut world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "notice",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let notice = line(&all, "notice");
    assert_eq!(notice.payload["code"], "hook_failed");
    assert_eq!(notice.payload["extension"], "fiber.test/soft");
    assert_eq!(world.provider.requests().len(), 1);
}

#[test]
fn a_cancel_or_shutdown_inside_turn_start_ends_the_turn_interrupted_with_no_context() {
    for shutdown in [false, true] {
        let hooks = Arc::new(FakeHooks::new());
        hooks.answer_turn_start(added(vec![("fiber.test/inbox", "late")]));
        let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
        {
            let looped = world.looped.take().unwrap();
            let signal = Arc::clone(&looped.cancel);
            if shutdown {
                hooks.shutdown_in_hook.lock().unwrap().replace(signal);
            } else {
                hooks.cancel_in_hook.lock().unwrap().replace(signal);
            }
            world.looped = Some(looped);
        }
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("c_1", "hello"), ack))
            .unwrap();
        let finished = run_turn(&mut world);
        assert_eq!(
            finish_turn(&mut world, &finished),
            Some(TurnOutcome::Interrupted),
            "shutdown: {shutdown}"
        );
        drop(world.looped.take());

        let all = lines(&world);
        assert!(
            !all.iter().any(|line| line.kind == "context_added"),
            "no context is written: {all:?}"
        );
        assert!(
            !all.iter().any(|line| line.kind == "step_started"),
            "no step starts: {all:?}"
        );
    }
}

#[test]
fn two_turn_start_contexts_are_written_in_run_order() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(added(vec![
        ("fiber.test/first", "one"),
        ("fiber.test/second", "two"),
    ]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    let texts: Vec<&str> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .map(|line| line.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["one", "two"]);
}

#[test]
fn without_hooks_the_kinds_list_is_unchanged() {
    let mut world = started(vec![fakes::Scripted::text("Done.")], None);
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn turn_start_is_shown_the_input_without_unknown_items() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    let input = vec![
        InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        },
        InputItem::Unknown,
    ];
    let ended = looped.turn_start(&TurnId("t_test".into()), &input).unwrap();
    assert_eq!(ended, None);
    world.looped = Some(looped);
    assert_eq!(
        hooks.seen_input.lock().unwrap().clone(),
        vec!["[\"hi\"]"],
        "the Unknown item is filtered out, not an error"
    );
    assert_eq!(hooks.turn_start_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_hook_sees_each_request_with_the_model_and_the_spend_so_far() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.5),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Probe) as Arc<dyn Tool>],
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // Once per request: the tool call's, then the reply's.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    let seen = hooks.seen_calls.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "fake/model");
    assert_eq!(seen[0].1.cost, Some(0.0));
    // The second call sees the first step's cost.
    assert_eq!(seen[1].0, "fake/model");
    assert_eq!(seen[1].1.cost, Some(0.5));
    assert_eq!(world.provider.requests().len(), 2);
}

#[test]
fn a_refusal_fails_the_turn_budget_exceeded_and_stops_the_delegates() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(refused_call("fiber.test/guard", "team budget spent"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world
        .looped
        .take()
        .map(|looped| looped.jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "budget_exceeded");
    assert_eq!(
        end.payload["error"]["message"],
        "Extension fiber.test/guard refused the model request: team budget spent"
    );
    assert!(
        world.provider.requests().is_empty(),
        "the provider got no request for that step"
    );
    assert_eq!(jobs.stop_delegates_calls(), 1);
}

#[test]
fn a_blocking_before_model_call_failure_fails_the_turn_without_stopping_delegates() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(failed_call("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world
        .looped
        .take()
        .map(|looped| looped.jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `before_model_call` hook of extension fiber.test/guard failed, \
         so the request was not sent: boom"
    );
    assert!(world.provider.requests().is_empty());
    assert_eq!(jobs.stop_delegates_calls(), 0);
}

#[test]
fn a_retried_request_runs_the_hook_once() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![
            fakes::Scripted::failed(contract::shapes::Failure {
                code: ErrorCode::RateLimited,
                message: "slow down".into(),
                retry_after_ms: None,
                provider: None,
            }),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
    );
    // No backoff wait: the retry resends at once on the fake clock.
    world.looped = world.looped.take().map(|looped| {
        looped.retry(crate::Retry {
            attempts: 3,
            initial: Duration::ZERO,
            max: Duration::from_secs(60),
        })
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 1);
    // The retry resent the same request.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].conversation, requests[1].conversation);
}

#[test]
fn a_handoff_for_size_runs_the_hook_again_on_the_rebuilt_request() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![
            fakes::Scripted::failed(contract::shapes::Failure {
                code: ErrorCode::ContextOverflow,
                message: "too big".into(),
                retry_after_ms: None,
                provider: None,
            }),
            priced("the note", 0.0),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The request that did not fit, the note request the hook never sees,
    // and the rebuilt request: the hook ran on the first and the third.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    assert_eq!(world.provider.requests().len(), 3);
    let all = lines(&world);
    assert!(
        all.iter().any(|line| line.kind == "handoff_completed"),
        "the overflow handed off: {:?}",
        kinds(&all)
    );
}

#[test]
fn an_over_budget_session_never_calls_the_hook() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world.looped.take().map(|looped| {
        looped
            .jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>)
            .budget(Some(0.0))
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 0);
    let all = lines(&world);
    assert_eq!(
        line(&all, "turn_completed").payload["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(jobs.stop_delegates_calls(), 1);
}

#[test]
fn a_handoff_note_request_never_reaches_the_hook() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            with_tokens(
                tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.0),
                5000,
                5000,
            ),
            priced("the note", 0.0),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Probe) as Arc<dyn Tool>],
    );
    // Any reply overflows the context, so the next step hands off first.
    world.looped = world.looped.take().map(|looped| {
        looped.handoff(crate::HandoffSettings {
            enabled: true,
            tokens: 1,
            window_fraction: 1.0,
            nudge: false,
        })
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The step's request and the rebuilt one: the note request between them
    // never reached the hook.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    assert_eq!(world.provider.requests().len(), 3);
    let all = lines(&world);
    assert!(
        all.iter().any(|line| line.kind == "handoff_completed"),
        "an automatic handoff ran: {:?}",
        kinds(&all)
    );
}

#[test]
fn a_cancel_inside_the_hook_ends_the_turn_interrupted_even_on_a_refusal() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(refused_call("fiber.test/guard", "team budget spent"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    {
        let looped = world.looped.take().unwrap();
        hooks
            .cancel_in_call
            .lock()
            .unwrap()
            .replace(Arc::clone(&looped.cancel));
        world.looped = Some(looped);
    }
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Interrupted)
    );
    drop(world.looped.take());

    // The refusal was never applied: nothing was sent and the turn did not
    // fail.
    assert!(world.provider.requests().is_empty());
    let all = lines(&world);
    assert_eq!(
        line(&all, "turn_completed").payload["outcome"],
        "interrupted"
    );
}

/// The `turn_started` message items.
fn started_input(lines: &[Envelope]) -> Vec<InputItem> {
    let started: TurnStarted = serde_json::from_value(serde_json::Value::Object(
        line(lines, "turn_started").payload.clone(),
    ))
    .unwrap();
    started.input
}

/// A provider that sends one steer to the inbox when the first call is
/// made, so it waits at that step's end-of-turn drain, then answers from
/// its scripted provider.
struct SteerDuring {
    inner: Arc<fakes::ScriptedProvider>,
    during: Mutex<Option<mpsc::Sender<Delivery>>>,
}

impl contract::provider::Provider for SteerDuring {
    fn call(
        &self,
        request: &contract::provider::ModelRequest,
    ) -> Box<dyn contract::provider::ModelCall> {
        if let Some(inbox) = self.during.lock().unwrap().take() {
            let ack = Ack(Box::new(|_| {}));
            inbox
                .send(Delivery::Steer(driver("c_steer", "aside"), ack))
                .unwrap();
        }
        self.inner.call(request)
    }

    fn wire_tools(
        &self,
        tools: &[contract::provider::ToolDefinition],
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.inner.wire_tools(tools)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        self.inner.cost_lookup()
    }

    fn warms(&self, request: &contract::provider::ModelRequest) -> bool {
        self.inner.warms(request)
    }
}

/// A test tool whose calls end the turn asking one question.
struct Asking;

impl Tool for Asking {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".into(),
            description: "Asks one question.".into(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "a question".into(),
            }],
            control: Some(contract::events::Control {
                handoff: None,
                questions: Some(vec![contract::shapes::Question {
                    header: "Pick".into(),
                    question: "Which?".into(),
                    options: Vec::new(),
                    multi_select: None,
                }]),
                skill: None,
                name: None,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A reply of `text` cut off by the output limit.
fn cut_off(text: &str) -> fakes::Scripted {
    let mut end = fakes::reply(text);
    end.finish = contract::provider::Finish::OutputLimit;
    fakes::Scripted {
        deltas: Vec::new(),
        end: Ok(end),
    }
}

/// A reasoning-only reply thinking `thought`: no text, no call.
fn reasoning_only(thought: &str) -> fakes::Scripted {
    let mut end = fakes::reply("");
    end.actions = vec![contract::provider::ReplyAction::Reasoning(
        contract::events::ReasoningCompleted {
            text: thought.into(),
            provider_item: None,
        },
    )];
    fakes::Scripted {
        deltas: Vec::new(),
        end: Ok(end),
    }
}

#[test]
fn a_turn_end_message_continues_the_turn_and_the_hook_runs_again() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The hook saw the reply that ended the step, then the next one, which
    // ended the turn.
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        hooks.seen_replies.lock().unwrap().clone(),
        vec!["Done.", "All set."]
    );
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The message is the extension's own, with no command and no rewrites.
    let steering = line(&all, "steering_applied");
    assert_eq!(
        steering.payload["content"],
        serde_json::json!([{"type": "text", "text": "keep going"}])
    );
    assert_eq!(steering.payload["source"], "extension");
    assert_eq!(steering.payload["extension"], "fiber.test/goal");
    assert!(steering.payload.get("command_id").is_none());
    assert!(steering.payload.get("changed_by").is_none());
    // The next request carries it as a user message.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.last(),
        Some(&contract::provider::Input::User {
            text: "keep going".into(),
            images: Vec::new(),
        })
    );
}

#[test]
fn a_steer_waiting_at_the_end_of_turn_drain_skips_turn_end_that_time() {
    let hooks = Arc::new(FakeHooks::new());
    let (mut world, scripted) = started_wrapped(
        vec![fakes::Scripted::text("one"), fakes::Scripted::text("two")],
        Some(hooks.clone()),
        Vec::new(),
        |inner, inbox| {
            Arc::new(SteerDuring {
                inner,
                during: Mutex::new(Some(inbox)),
            }) as Arc<dyn contract::provider::Provider>
        },
    );
    let _ = scripted;
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The first step's end found the steer waiting and never ran the hook;
    // the second step's end did, and ended the turn.
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    let all = lines(&world);
    let steering: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .collect();
    assert_eq!(steering.len(), 1);
    assert_eq!(
        steering[0].payload["content"][0]["text"], "aside",
        "the waiting steer continued the turn"
    );
}

#[test]
fn a_cancel_inside_turn_end_drops_its_message_and_starts_no_later_turn_with_it() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    {
        let looped = world.looped.take().unwrap();
        hooks
            .cancel_in_end
            .lock()
            .unwrap()
            .replace(Arc::clone(&looped.cancel));
        world.looped = Some(looped);
    }
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Interrupted)
    );
    // The next turn holds no extension message: it never joined `queued`.
    // The fake stops cancelling now, so the next turn runs clean.
    hooks.cancel_in_end.lock().unwrap().take();
    let (next_ack, next_answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "again"), next_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&next_answered)
            .expect("the prompt's answer")
            .is_ok()
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "no message was written: {:?}",
        kinds(&all)
    );
    let started: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "turn_started")
        .collect();
    assert_eq!(started.len(), 2);
    let input = started_input(std::slice::from_ref(started[1]));
    assert_eq!(input.len(), 1);
    let InputItem::Message { content, .. } = &input[0] else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(crate::conversation::text(content), "again");
}

#[test]
fn replied_runs_no_hook_once_a_cancel_landed() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    assert!(looped.cancel.arm());
    looped.cancel.cancel();
    let ended = looped.replied(&TurnId("t_test".into()), "Done.").unwrap();
    world.looped = Some(looped);
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);
    let Some(end) = ended else {
        panic!("a cancelled turn ends");
    };
    assert_eq!(end.outcome, TurnOutcome::Interrupted);
}

#[test]
fn a_turn_that_fails_or_asks_questions_never_calls_turn_end() {
    // A failed model call ends the turn with no reply to show the hook.
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![fakes::Scripted::failed(contract::shapes::Failure {
            code: ErrorCode::QuotaExceeded,
            message: "no more".into(),
            retry_after_ms: None,
            provider: None,
        })],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);

    // A call asking questions ends the turn at the step boundary instead.
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.0),
            fakes::Scripted::text("Done."),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Asking) as Arc<dyn Tool>],
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);
    let all = lines(&world);
    let end = line(&all, "turn_completed");
    assert!(end.payload.get("questions").is_some());
}

#[test]
fn a_blocking_turn_end_failure_writes_nothing_and_fails_the_turn() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(failed("fiber.test/goal", "boom"));
    let mut world = started(vec![fakes::Scripted::text("one")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    // The failed chain holds nothing for a later turn either.
    assert!(
        world
            .looped
            .as_ref()
            .unwrap()
            .lifecycle
            .continued
            .is_empty()
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "the failure wrote no message"
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `turn_end` hook of extension fiber.test/goal failed: boom"
    );
}

#[test]
fn two_turn_end_messages_are_written_in_run_order_after_a_late_steer() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![
        ("fiber.test/first", "one"),
        ("fiber.test/second", "two"),
    ]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    // A steer sent while the hook runs waits in the inbox behind it.
    hooks
        .steer_in_end
        .lock()
        .unwrap()
        .replace(world.inbox.clone());
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    let texts: Vec<&str> = all
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .map(|line| line.payload["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        vec!["one", "two", "late steer"],
        "the hook's messages precede the steer that arrived meanwhile"
    );
}

#[test]
fn an_output_limit_reply_runs_turn_end_with_its_kept_text() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![cut_off("partial")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.seen_replies.lock().unwrap().clone(), vec!["partial"]);
}

#[test]
fn a_reasoning_only_reply_runs_turn_end_with_no_text() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![reasoning_only("hmm")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.seen_replies.lock().unwrap().clone(), vec![""]);
}

#[test]
fn a_cancel_before_the_next_steps_commit_drops_held_messages() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    // The reply ends the step with nothing waiting: its message is held for
    // the next step, not written yet.
    assert_eq!(looped.replied(&turn, "Done.").unwrap(), None);
    assert_eq!(looped.lifecycle.continued.len(), 1);
    // A cancel lands before the next step's commit: the commit is refused
    // and the held message is dropped unwritten.
    assert!(looped.cancel.arm());
    assert!(looped.cancel.cancel());
    let step = looped.step(&turn).unwrap();
    assert!(
        matches!(step, crate::Step::Ended(_)),
        "the turn ends without another request"
    );
    // A later turn is clean: nothing held over.
    assert!(looped.lifecycle.continued.is_empty());
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "no held message was written: {:?}",
        kinds(&all)
    );
}
