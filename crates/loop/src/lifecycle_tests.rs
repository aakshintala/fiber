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

use contract::events::{InputItem, TurnOutcome};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome,
    BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer, CheckOutcome, Context,
    ContextAnswer, ContextOutcome, Hooks,
};
use contract::inbox::{Ack, Answer, Delivery, Message};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, Origin, Sender};
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
    turn_end_calls: AtomicUsize,
    #[allow(dead_code, reason = "read from task 2.3 on")]
    seen_replies: Mutex<Vec<String>>,
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
            turn_end_calls: AtomicUsize::new(0),
            seen_replies: Mutex::new(Vec::new()),
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

    #[allow(dead_code, reason = "used from task 2.2 on")]
    fn answer_before_model_call(&self, answer: CheckAnswer) {
        self.before_model_call.lock().unwrap().push_back(answer);
    }

    #[allow(dead_code, reason = "used from task 2.3 on")]
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

    fn before_model_call(&self, _call: &contract::hook::BeforeModelCall<'_>) -> CheckAnswer {
        self.before_model_call_calls.fetch_add(1, Ordering::SeqCst);
        self.before_model_call
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed_call)
    }

    fn turn_end(&self, end: &contract::hook::TurnEnd<'_>) -> ContextAnswer {
        self.turn_end_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_replies.lock().unwrap().push(end.reply.to_owned());
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
    let home = fakes::TempDir::new("fiber-lifecycle");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    // Subscribed before the loop starts: the stream holds every line the
    // turn writes, ephemeral notices included, which the durable read skips.
    let watcher = log.watch();
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::clone(&provider) as Arc<dyn contract::provider::Provider>,
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
            tools: Vec::new(),
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
        provider,
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
