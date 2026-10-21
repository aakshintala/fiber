//! `before_message` at admission (`docs/extensions.md`, "The hook points"):
//! a person's or a driver's prompt or steer runs the chain before slash
//! expansion, attending, queuing or logging.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{InputItem, TurnOutcome, TurnStarted};
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall,
    CheckAnswer, CheckOutcome, Context, Hooks, Replaced,
};
use contract::inbox::{Ack, Answer, Delivery, Message};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode, JobId, SessionId, TurnId};
use fakes::{Deadline, NoRules};
use log::Log;

use crate::before_message::Hooked;
use crate::inbox::TurnInput;
use crate::jobs::Queued;
use crate::{Error, Loop, Model};

/// One hang bound for every wait: the turn runs on a thread of its own, so
/// a stuck hook fails the test instead of hanging it.
const TURN_DEADLINE: Duration = Duration::from_secs(10);
/// The idle delay the unattended check counts from the last prompt.
const IDLE: Duration = Duration::from_secs(60);

/// A scripted `Hooks` fake: seen `before_message` contents and a queue of
/// answers, with an optional shutdown to start inside `before_message`.
struct FakeHooks {
    seen: Mutex<Vec<String>>,
    answers: Mutex<VecDeque<BeforeMessageAnswer>>,
    shutdown: Mutex<Option<Arc<crate::TurnCancel>>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            answers: Mutex::new(VecDeque::new()),
            shutdown: Mutex::new(None),
        }
    }

    fn answer(&self, answer: BeforeMessageAnswer) {
        self.answers.lock().unwrap().push_back(answer);
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
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

fn passed_with_context(context: Vec<(&str, &str)>) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: context
                .into_iter()
                .map(|(extension, text)| Context {
                    state: Vec::new(),
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
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

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.seen
            .lock()
            .unwrap()
            .push(crate::conversation::text(message.content));
        if let Some(cancel) = self.shutdown.lock().unwrap().clone() {
            cancel.shutdown(143);
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

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// The replacements `on_rewrite` reported, in call order.
type Rewrites = Arc<Mutex<Vec<(CommandId, Vec<ContentPart>)>>>;

/// What a turn-level test drives: the loop, its inbox, and the handles to
/// read what it wrote and was sent.
struct World {
    looped: Option<Loop>,
    log: Arc<Log>,
    inbox: mpsc::Sender<Delivery>,
    provider: Arc<fakes::ScriptedProvider>,
    dir: PathBuf,
    rewrites: Rewrites,
    watcher: Option<log::Watcher>,
    _home: fakes::TempDir,
}

/// A `Loop` with its inbox still held by the test, `hooks` set when given,
/// and `script` as its provider's replies. `prepare` runs on the workspace
/// before the loop starts, so a skill it writes is discovered.
fn started_with(
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    prepare: impl FnOnce(&Path),
) -> World {
    let home = fakes::TempDir::new("fiber-before-message");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    prepare(&workspace);
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let provider = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    // Subscribed before the loop starts: the stream holds every line the
    // turn writes, ephemeral notices included, which the durable read skips.
    let watcher = log.watch();
    let rewrites: Rewrites = Arc::new(Mutex::new(Vec::new()));
    let recording = Arc::clone(&rewrites);
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
    .answerable(false)
    .on_rewrite(Arc::new(move |id, content| {
        recording
            .lock()
            .unwrap()
            .push((id.clone(), content.to_vec()));
    }));
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks as Arc<dyn Hooks>);
    }
    World {
        looped: Some(looped),
        log,
        inbox,
        provider,
        dir: home.path().join("s_test"),
        rewrites,
        watcher: Some(watcher),
        _home: home,
    }
}

fn started(script: Vec<fakes::Scripted>, hooks: Option<Arc<FakeHooks>>) -> World {
    started_with(script, hooks, |_| {})
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

/// An acknowledgement recording its answer. A send after the test dropped
/// the receiver is ignored, as a dropped delivery's is.
fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        })),
        rx,
    )
}

type Finished = mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, Error>)>;

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

/// Every line the world's watcher received up to and including
/// `turn_completed`: the durable lines and the ephemeral ones. The read
/// runs on its own thread under one [`TURN_DEADLINE`] naming the awaited
/// completion, so a missing `turn_completed` fails instead of hanging on
/// the retained log's open watcher.
fn streamed(world: &mut World) -> Vec<Envelope> {
    let watcher = world.watcher.take().unwrap();
    let (watcher, streamed) = fakes::within(
        "a turn_completed line or the log's end",
        Deadline::after(TURN_DEADLINE),
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

/// The complete, ordered kinds of `lines`.
fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

fn line<'a>(lines: &'a [Envelope], kind: &str) -> &'a Envelope {
    lines.iter().find(|line| line.kind == kind).unwrap()
}

/// The `turn_started` message items.
fn started_input(lines: &[Envelope]) -> Vec<InputItem> {
    let started: TurnStarted = serde_json::from_value(serde_json::Value::Object(
        line(lines, "turn_started").payload.clone(),
    ))
    .unwrap();
    started.input
}

/// The acknowledgement's rejection.
fn rejected(answer: Answer) -> contract::inbox::Rejection {
    match answer {
        Ok(_) => panic!("an acceptance, not a rejection: {answer:?}"),
        Err(rejection) => rejection,
    }
}

fn text_of(content: &[ContentPart]) -> String {
    crate::conversation::text(content)
}

#[test]
fn a_replaced_prompt_logs_and_sends_only_the_replacement() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced("my key is [redacted]", "fiber.test/redact"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "my key is sk-123"), ack))
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
    let input = started_input(&all);
    assert_eq!(input.len(), 1);
    let InputItem::Message {
        content,
        changed_by,
        ..
    } = &input[0]
    else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(text_of(content), "my key is [redacted]");
    assert_eq!(changed_by, &Some(vec!["fiber.test/redact".to_owned()]));
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].conversation.last(),
        Some(&contract::provider::Input::User {
            text: "my key is [redacted]".into(),
            images: Vec::new(),
        })
    );
    let bytes = std::fs::read(world.dir.join("events.jsonl")).unwrap();
    assert!(
        !String::from_utf8(bytes).unwrap().contains("sk-123"),
        "the original is never logged"
    );
}

#[test]
fn a_refused_prompt_is_rejected_and_starts_no_turn() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(refused("fiber.test/guard", "no keys"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (refused_ack, refused_answer) = capture();
    world
        .inbox
        .send(Delivery::Prompt(
            driver("c_1", "my key is sk-123"),
            refused_ack,
        ))
        .unwrap();
    let (ok_ack, ok_answer) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "hello"), ok_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );

    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&refused_answer)
            .expect("the refusal is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    assert_eq!(
        rejection.message,
        "Extension fiber.test/guard refused the message: no keys"
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&ok_answer)
            .expect("the second prompt's answer")
            .is_ok(),
        "the second prompt is accepted"
    );
    drop(world.looped.take());

    let all = lines(&world);
    let input = started_input(&all);
    assert_eq!(
        input.len(),
        1,
        "the refused prompt starts no turn of its own"
    );
    let InputItem::Message { content, .. } = &input[0] else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(text_of(content), "hello");
}

#[test]
fn a_refused_prompt_alone_starts_no_turn() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(refused("fiber.test/guard", "no keys"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "my key is sk-123"), ack))
        .unwrap();
    let (close_ack, _close_answered) = capture();
    world.inbox.send(Delivery::Close(close_ack)).unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(finish_turn(&mut world, &finished), None);
    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the refusal is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "turn_started"),
        "a refused prompt alone starts no turn"
    );
}

/// A `Jobs` stub with one job running and nothing else.
struct OneJob;

impl Jobs for OneJob {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError {
            path: PathBuf::from("nowhere"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no jobs in this test"),
        })
    }

    fn stop(&self, _job_id: &JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        vec![JobId("j_1".into())]
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

#[test]
fn a_refused_prompt_leaves_the_jobs_check_unarmed() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(refused("fiber.test/guard", "no keys"));
    hooks.answer(passed());
    let mut world = started(Vec::new(), Some(hooks));
    let mut looped = world.looped.take().unwrap();
    looped = looped.jobs(Arc::new(OneJob)).idle_exit(Some(IDLE));
    assert_eq!(looped.check_deadline(&[JobId("j_1".into())]), None);

    let mut input = TurnInput::of(Vec::new());
    let (ack, _) = capture();
    looped
        .admit_idle(Delivery::Prompt(driver("c_1", "no"), ack), &mut input)
        .unwrap();
    assert!(input.pieces.is_empty());
    assert_eq!(
        looped.check_deadline(&[JobId("j_1".into())]),
        None,
        "a refused prompt never marks the session attended"
    );

    let mut input = TurnInput::of(Vec::new());
    let (ack, _) = capture();
    looped
        .admit_idle(Delivery::Prompt(driver("c_2", "yes"), ack), &mut input)
        .unwrap();
    assert_eq!(input.pieces.len(), 1);
    assert!(
        looped.check_deadline(&[JobId("j_1".into())]).is_some(),
        "an admitted prompt arms the jobs check: the positive fact"
    );
    world.looped = Some(looped);
}

#[test]
fn a_blocking_hook_failure_rejects_the_prompt_and_writes_only_its_notices() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(failed("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let (close_ack, _close_answered) = capture();
    world.inbox.send(Delivery::Close(close_ack)).unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(finish_turn(&mut world, &finished), None);

    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the failure is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::HookFailed);
    assert_eq!(
        rejection.message,
        "The `before_message` hook of extension fiber.test/guard failed, \
         so the message was not sent: boom"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "turn_started"),
        "a blocked message starts no turn"
    );
}

#[test]
fn passed_context_is_logged_after_turn_started_before_the_acceptance() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed_with_context(vec![
        ("fiber.test/first", "one"),
        ("fiber.test/second", "two"),
    ]));
    hooks.answer(refused("fiber.test/guard", "late"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    // The ack records the log length when called: the contexts must be
    // written before the acceptance.
    let log = Arc::clone(&world.log);
    let (seen_tx, seen_rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        seen_tx.send((log.count(), answer)).unwrap();
    }));
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let (refused_ack, refused_answer) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "later"), refused_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&refused_answer)
            .expect("the refusal is answered")
            .is_err()
    );
    let (at_ack, answer) = Deadline::after(TURN_DEADLINE)
        .recv(&seen_rx)
        .expect("the acceptance is answered");
    assert!(answer.is_ok());
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
    assert_eq!(contexts.len(), 2, "a refused message adds no context");
    assert_eq!(contexts[0].payload["hook"], "before_message");
    assert_eq!(contexts[0].payload["extension"], "fiber.test/first");
    assert_eq!(contexts[0].payload["text"], "one");
    assert_eq!(contexts[1].payload["extension"], "fiber.test/second");
    assert_eq!(contexts[1].payload["text"], "two");
    let last_context = all
        .iter()
        .rposition(|line| line.kind == "context_added")
        .unwrap();
    assert!(
        at_ack > last_context as u64,
        "the acceptance comes after the contexts are written"
    );
    let input = started_input(&all);
    assert_eq!(input.len(), 1);
    let InputItem::Message { changed_by, .. } = &input[0] else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(changed_by, &None);
}

/// Writes a `second` skill into `workspace`, as the skill discovery reads it.
fn write_second_skill(workspace: &Path) {
    let dir = workspace.join(".agents/skills/second");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: second\ndescription: The second skill.\n---\nExpanded body.\n",
    )
    .unwrap();
}

#[test]
fn the_hook_sees_a_skill_command_before_expansion_and_its_rewrite_expands() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    hooks.answer(replaced("/second tail", "fiber.test/redact"));
    let mut world = started_with(
        vec![fakes::Scripted::text("Done.")],
        Some(hooks.clone()),
        write_second_skill,
    );
    let (first_ack, first_answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(
            driver("c_1", "/second with args"),
            first_ack,
        ))
        .unwrap();
    let (second_ack, second_answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "plain"), second_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&first_answered)
            .expect("the first prompt's answer")
            .is_ok()
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&second_answered)
            .expect("the second prompt's answer")
            .is_ok()
    );
    assert_eq!(hooks.seen(), vec!["/second with args", "plain"]);
    drop(world.looped.take());

    let all = lines(&world);
    let input = started_input(&all);
    assert_eq!(input.len(), 2);
    for item in &input {
        let InputItem::Message { content, .. } = item else {
            panic!("a message item: {input:?}");
        };
        assert!(
            text_of(content).starts_with("Expanded body"),
            "both prompts expanded through the skill: {content:?}"
        );
    }
    let texts: Vec<String> = input
        .iter()
        .map(|item| match item {
            InputItem::Message { content, .. } => text_of(content),
            InputItem::ShellCommand { .. }
            | InputItem::Jobs { .. }
            | InputItem::Handoff { .. }
            | InputItem::Rewind { .. }
            | InputItem::Unknown => panic!("a message item"),
        })
        .collect();
    assert!(
        texts[0].ends_with("with args"),
        "the direct skill command keeps its arguments: {}",
        texts[0]
    );
    assert!(
        texts[1].ends_with("tail"),
        "the expansion reads the hook's replacement: {}",
        texts[1]
    );
}

#[test]
fn no_hook_runs_for_busy_closing_or_after_a_shutdown_started() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();

    // `busy`: a prompt while a turn runs.
    let (ack, answered) = capture();
    looped
        .admit_running(
            Delivery::Prompt(driver("c_busy", "hello"), ack),
            &TurnId("t_1".into()),
        )
        .unwrap();
    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the busy rejection"),
    );
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(hooks.count(), 0);

    // `closing`: a prompt once `close` was taken.
    let (close_ack, _) = capture();
    looped.take_close(close_ack);
    let (ack, answered) = capture();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::Prompt(driver("c_late", "hello"), ack), &mut input)
        .unwrap();
    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the closing rejection"),
    );
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(hooks.count(), 0);
    world.looped = Some(looped);

    // A shutdown with hooks set: admission rejects `closing` unhooked.
    let shutdown_hooks = Arc::new(FakeHooks::new());
    shutdown_hooks.answer(passed());
    let mut world = started(Vec::new(), Some(shutdown_hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    looped.cancel.shutdown(143);
    let (ack, answered) = capture();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::Prompt(driver("c_shut", "hello"), ack), &mut input)
        .unwrap();
    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the shutdown closing rejection"),
    );
    assert_eq!(rejection.code, ErrorCode::Closing);
    let (ack, answered) = capture();
    looped
        .admit_running(
            Delivery::Steer(driver("c_steer", "hello"), ack),
            &TurnId("t_1".into()),
        )
        .unwrap();
    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the steer's closing rejection"),
    );
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(shutdown_hooks.count(), 0);
    world.looped = Some(looped);

    // No hooks set: a steer during a shutdown is accepted as today.
    let mut world = started(Vec::new(), None);
    let mut looped = world.looped.take().unwrap();
    looped.cancel.shutdown(143);
    let (ack, answered) = capture();
    looped
        .admit_running(
            Delivery::Steer(driver("c_steer", "hello"), ack),
            &TurnId("t_1".into()),
        )
        .unwrap();
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the shutdown steer's answer")
            .is_ok()
    );
    world.looped = Some(looped);
}

#[test]
fn a_steer_after_close_joins_only_a_batch_that_holds_a_message() {
    let mut world = started(Vec::new(), None);
    let mut looped = world.looped.take().unwrap();
    let (close_ack, _) = capture();
    looped.take_close(close_ack);

    // A batch that already holds a steer: the late steer joins it.
    let held = Queued::Steer(driver("c_held", "first"), Hooked::default());
    let mut input = TurnInput::of(vec![held]);
    let (ack, answered) = capture();
    looped
        .admit_idle(
            Delivery::Steer(driver("c_joins", "second"), ack),
            &mut input,
        )
        .unwrap();
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the joining steer's answer")
            .is_ok()
    );
    assert_eq!(input.pieces.len(), 2);

    // An empty batch: the same steer is refused `closing`.
    let mut empty = TurnInput::of(Vec::new());
    let (ack, answered) = capture();
    looped
        .admit_idle(
            Delivery::Steer(driver("c_refused", "hello"), ack),
            &mut empty,
        )
        .unwrap();
    assert_eq!(
        rejected(
            Deadline::after(TURN_DEADLINE)
                .recv(&answered)
                .expect("the empty batch's closing rejection")
        )
        .code,
        ErrorCode::Closing
    );
    assert!(empty.pieces.is_empty());
    world.looped = Some(looped);
}

#[test]
fn a_shutdown_inside_the_hook_admits_but_starts_no_turn() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    {
        let looped = world.looped.take().unwrap();
        hooks
            .shutdown
            .lock()
            .unwrap()
            .replace(Arc::clone(&looped.cancel));
        world.looped = Some(looped);
    }
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(finish_turn(&mut world, &finished), None);
    assert_eq!(hooks.count(), 1);
    // The admission accepted the prompt, but no turn started to accept it.
    assert!(answered.try_recv().is_err());
    drop(world.looped.take());

    let all = lines(&world);
    assert!(
        !all.iter().any(|line| line.kind == "turn_started"),
        "no turn starts once a shutdown began"
    );
}

#[test]
fn on_rewrite_hears_a_replaced_prompt_before_its_ack_and_nothing_else() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced("redacted", "fiber.test/redact"));
    hooks.answer(passed());
    hooks.answer(refused("fiber.test/guard", "no"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    // The acks record the rewrite count when called: the replacement must
    // be reported before the prompt is accepted.
    for (id, text) in [("c_1", "secret"), ("c_2", "plain"), ("c_3", "blocked")] {
        let rewrites = Arc::clone(&world.rewrites);
        let ack = Ack(Box::new(move |answer| {
            assert!(answer.is_ok() == (id != "c_3"));
            assert_eq!(
                rewrites.lock().unwrap().len(),
                1,
                "the rewrite is reported before any acceptance"
            );
        }));
        world
            .inbox
            .send(Delivery::Prompt(driver(id, text), ack))
            .unwrap();
    }
    let (steer_ack, steer_answered) = capture();
    world
        .inbox
        .send(Delivery::Steer(driver("c_4", "steer"), steer_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&steer_answered)
            .expect("the steer's answer")
            .is_ok()
    );
    drop(world.looped.take());

    let rewrites = world.rewrites.lock().unwrap().clone();
    assert_eq!(rewrites.len(), 1, "only the replaced prompt is reported");
    assert_eq!(rewrites[0].0, CommandId("c_1".into()));
    assert_eq!(
        rewrites[0].1,
        vec![ContentPart::Text {
            text: "redacted".into()
        }]
    );
}

#[test]
fn a_replaced_prompt_naming_a_refused_mcp_prompt_reports_no_rewrite() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced("/gone args", "fiber.test/redact"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    {
        let looped = world.looped.take().unwrap();
        let looped = looped.server_prompts(crate::ServerPrompts {
            rows: vec![contract::events::CommandInfo {
                extension: false,
                name: "gone".into(),
                description: "A missing prompt.".into(),
                argument_hint: None,
                tag: "gone-server".into(),
            }],
            fetch: Arc::new(|_, _, _, _| contract::tool::Output {
                content: Vec::new(),
                error: Some(contract::shapes::Failure {
                    code: ErrorCode::McpPromptFailed,
                    message: "the server refused".into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                ..contract::tool::Output::default()
            }),
        });
        world.looped = Some(looped);
    }
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "plain"), ack))
        .unwrap();
    let (close_ack, _close_answered) = capture();
    world.inbox.send(Delivery::Close(close_ack)).unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(finish_turn(&mut world, &finished), None);

    let rejection = rejected(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the expansion failure is answered"),
    );
    assert_eq!(rejection.code, ErrorCode::McpPromptFailed);
    drop(world.looped.take());
    assert!(
        world.rewrites.lock().unwrap().is_empty(),
        "a prompt the expansion rejects never reaches the door"
    );
}

#[test]
fn a_non_blocking_failure_writes_a_notice_without_a_turn_and_runs_the_original() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(noticed(ErrorCode::HookFailed, "fiber.test/soft"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "original"), ack))
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
            .is_ok()
    );
    drop(world.looped.take());

    let all = streamed(&mut world);
    let notice = line(&all, "notice");
    assert!(notice.turn_id.is_none(), "an idle notice names no turn");
    // The notice is written at admission, before the turn's preamble.
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "notice",
            "preamble_built",
            "opening_message",
            "turn_started",
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
    let input = started_input(&all);
    assert_eq!(input.len(), 1);
    let InputItem::Message { content, .. } = &input[0] else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(text_of(content), "original");
    let requests = world.provider.requests();
    assert_eq!(
        requests[0].conversation.last(),
        Some(&contract::provider::Input::User {
            text: "original".into(),
            images: Vec::new(),
        })
    );
}

#[test]
fn a_reader_admitted_idle_steer_hooks_and_joins_the_input() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    // The reader admitted at once, still queued and unstaged.
    looped
        .steering
        .push(
            CommandId("c_1".into()),
            vec![ContentPart::Text {
                text: "hello".into(),
            }],
            Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            &*world.log,
        )
        .unwrap();
    let mut input = TurnInput::of(Vec::new());
    let (ack, answered) = capture();
    looped
        .take_steer_idle(driver("c_1", "hello"), ack, &mut input)
        .unwrap();
    assert_eq!(
        hooks.seen(),
        vec!["hello".to_owned()],
        "the idle take runs the hook pass over the reader entry"
    );
    assert_eq!(
        input.pieces.len(),
        1,
        "the hooked steer lands in the turn input, not merely acked"
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the steer is answered")
            .is_ok(),
        "the idle take accepts the steer"
    );
    world.looped = Some(looped);
}

#[test]
fn a_reader_removed_idle_steer_answers_idempotently_with_no_hook_pass() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    // The reader admitted at once, then dropped: the entry left while
    // its id stays admitted, so the loop's follow-up delivery is a
    // no-op (`remove` keeps the id; `take` would forget it).
    looped
        .steering
        .push(
            CommandId("c_1".into()),
            vec![ContentPart::Text {
                text: "hello".into(),
            }],
            Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            &*world.log,
        )
        .unwrap();
    assert!(
        looped
            .steering
            .remove(&CommandId("c_1".into()), &*world.log)
    );
    let mut input = TurnInput::of(Vec::new());
    let (ack, answered) = capture();
    looped
        .take_steer_idle(driver("c_1", "hello"), ack, &mut input)
        .unwrap();
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the duplicate steer is answered")
            .is_ok(),
        "the reader's answer stands: this ack answers idempotently"
    );
    assert_eq!(
        hooks.count(),
        0,
        "no hook pass runs for an id with nothing queued"
    );
    assert!(
        input.pieces.is_empty(),
        "the duplicate gains the turn no piece"
    );
    world.looped = Some(looped);
}
