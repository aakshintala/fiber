//! A finished job wakes the loop: a notice starts a turn while idle, joins a
//! running turn at the next step boundary, and is written once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    CallStatus, Clients, Decision, DelegateFinished, DelegateStarted, Event, JobCompleted,
    JobStarted, Notice, OnFork, Outcome, Parent, RepositoryCodeOffered, ShellCommand,
    ToolCallArgumentsDelta, ToolCallRequested, TurnOutcome, UsageRecorded,
};
use contract::hook::{
    AfterToolAnswer, AfterToolCall, AfterToolOutcome, HookFailure, Hooks, JobDelivery, StateLine,
};
use contract::inbox::{Ack, Claim, Delivery, JobNotice, Message, SessionMessage};
use contract::jobs::{JobRecord, Jobs, OpenError, Taken};
use contract::provider::{
    Delta, Input, ModelCall, ModelRequest, Provider, ReplyAction, ToolDefinition,
};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{
    ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, ProtectedPathFailure,
    ProtectedPaths, Sender as From, Tokens, Usage,
};
use contract::tool::{Cancel, Effects, EffectsError, Output, Settle, Tool};
use contract::{
    ActionId, CommandId, Envelope, ErrorCode, GenerationId, JobId, RequestId, Seq, SessionId,
    TurnId,
};
use fakes::Deadline;
use fakes::jobs::FakeJobs;
use fakes::{Scripted, ScriptedProvider, StubJobs};
use log::{Log, Watcher};
use serde_json::{Map, Value, json};

use super::{line_text, notice_text};
use crate::{Loop, Model};

/// Reads `watcher` on its own thread with blocking `recv` until `done`
/// accepts a line, under one [`fakes::MUST_SUCCEED_WITHIN`] naming `what`. Returns the
/// watcher and every line read, the accepted one last.
#[track_caller]
fn read_until(
    watcher: Watcher,
    what: &str,
    done: impl Fn(&Envelope) -> bool + Send + 'static,
) -> (Watcher, Vec<Envelope>) {
    let what_owned = what.to_owned();
    fakes::within(
        what,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
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
        },
    )
}
const JOB: &str = "j_5e10c0ffee123456";
const OTHER: &str = "j_0ddba11cafe00000";
const DELEGATE: &str = "j_de1e6a7e12345678";

/// How a shell job that exited 1 ends.
fn failed(id: &str) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        process: Some(Process {
            exit_code: Some(1),
            signal: None,
            timed_out: false,
        }),
        output_tail: Some("1 failing\n".into()),

        notice: None,
        changed_by: None,
        protected_paths: None,
    }
}

/// How many times a notice's claim was asked.
#[derive(Clone, Default)]
struct Asked(Arc<AtomicUsize>);

impl Asked {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// `id`'s end as the registry sends it, whose claim answers `holds`.
fn notice(id: &str, holds: bool, asked: &Asked) -> Delivery {
    let asked = Arc::clone(&asked.0);
    Delivery::Job(JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
            holds.then(Vec::new)
        })),
        delegate: None,
    })
}

fn held(id: &str) -> Delivery {
    notice(id, true, &Asked::default())
}

/// A delegate's finish for `id` with `text`.
fn delegate_finished(id: &str, text: String) -> DelegateFinished {
    DelegateFinished {
        job_id: JobId(id.into()),
        text,
        artifact: None,
        questions: None,
        usage: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        worktree: None,

        changed_by: None,
    }
}

/// `id`'s end as a delegate notice: its finish rides with its completion.
fn delegate_held(id: &str, text: String) -> Delivery {
    Delivery::Job(JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(|| Some(Vec::new()))),
        delegate: Some(delegate_finished(id, text)),
    })
}

/// What a delegate notice for `id` with `text` renders as its first wake
/// message.
fn delegate_rendered(id: &str, text: &str) -> String {
    format!("Fiber: delegate {id} finished. Its final message:\n{text}")
}

fn message(text: &str, command: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: From {
            origin: Origin::Driver,
            command_id: Some(CommandId(command.into())),
        },
    }
}

/// What a session message from `s_0123456789abcdef`, named `builder`,
/// saying `Tests pass on main.` renders as once framed.
const FRAMED: &str = "Fiber: a message from session s_0123456789abcdef, named \"builder\". Another session wrote it, not the person.\n\nTests pass on main.";

/// A session message from another session, sent by `command` with parent
/// `parent`, answered on `ack`.
fn session_message(command: &str, parent: Option<&str>, ack: Ack) -> Delivery {
    Delivery::Message(
        SessionMessage {
            from_session_id: SessionId("s_0123456789abcdef".into()),
            parent_session_id: parent.map(|parent| SessionId(parent.into())),
            command_id: CommandId(command.into()),
            text: "Tests pass on main.".into(),
            name: Some("builder".into()),
        },
        ack,
    )
}

/// `turn_started`'s input for one session message sent by `command`.
fn message_input(command: &str) -> Value {
    json!([{
        "type": "message",
        "source": "session",
        "from_session_id": "s_0123456789abcdef",
        "command_id": command,
        "content": [{"type": "text", "text": FRAMED}],
    }])
}

fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

fn prompt(text: &str) -> Delivery {
    Delivery::Prompt(message(text, "c_prompt"), ignore())
}

fn steer(text: &str) -> Delivery {
    Delivery::Steer(message(text, "c_steer"), ignore())
}

/// An ack that reports whether the command was accepted on `answered`.
fn reported(answered: Sender<bool>) -> Ack {
    Ack(Box::new(move |result| {
        let _sent = answered.send(result.is_ok());
    }))
}

/// What the notice for [`failed`] reads.
fn rendered(id: &str) -> String {
    format!("Fiber: background job {id} ended: failed.\nExit code 1.\nLast output:\n1 failing")
}

/// A reply that calls `name` with `{}`.
fn calls(name: &str) -> Scripted {
    let mut end = fakes::reply("");
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    Scripted {
        deltas: vec![Delta::ToolCallArguments(ToolCallArgumentsDelta {
            index: 0,
            name: Some(name.into()),
            text: "{}".into(),
        })],
        end: Ok(end),
    }
}

/// The scripted provider, sending each call's deliveries to the inbox as
/// that call is made: they arrive while its reply streams.
struct During {
    inner: Arc<ScriptedProvider>,
    inbox: Sender<Delivery>,
    by_call: Mutex<VecDeque<Vec<Delivery>>>,
}

impl Provider for During {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let sent = self.by_call.lock().unwrap().pop_front().unwrap_or_default();
        for delivery in sent {
            self.inbox.send(delivery).unwrap();
        }
        self.inner.call(request)
    }
}

/// A tool that sends its deliveries to the inbox while it runs.
struct Sends {
    inbox: Sender<Delivery>,
    during: Mutex<Vec<Delivery>>,
}

impl Tool for Sends {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "sends".into(),
            description: "Sends while it runs.".into(),
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
        for delivery in std::mem::take(&mut *self.during.lock().unwrap()) {
            self.inbox.send(delivery).unwrap();
        }
        Output {
            content: vec![ContentPart::Text {
                text: "ok\n".into(),
            }],
            ..Output::default()
        }
    }
}

/// A tool whose result settles a job's end, as `jobs wait` does when it
/// claims one: the taken records are written under the call before its
/// completion.
struct Settles {
    taken: Vec<Taken>,
}

impl Tool for Settles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "settles".into(),
            description: "Settles a claimed job end.".into(),
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
        let taken = self.taken.clone();
        Output {
            content: vec![ContentPart::Text {
                text: "claimed\n".into(),
            }],
            settle: Some(Settle(Arc::new(move || taken.clone()))),
            ..Output::default()
        }
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

/// A loop on a fresh log, its provider answering `script` and sending
/// `by_call[n]` as call `n` is made.
struct World {
    _home: fakes::TempDir,
    dir: std::path::PathBuf,
    log: Arc<Log>,
    inbox: Sender<Delivery>,
    provider: Arc<ScriptedProvider>,
    looped: Option<Loop>,
    /// Every line written, ephemeral ones too, from the loop's start.
    watched: Option<Watcher>,
    clock: Arc<fakes::clock::FakeClock>,
    runner: Option<thread::ThreadId>,
}

impl World {
    fn new(
        script: Vec<Scripted>,
        by_call: Vec<Vec<Delivery>>,
        tools: impl FnOnce(&Sender<Delivery>) -> Vec<Arc<dyn Tool>>,
    ) -> Self {
        let home = fakes::TempDir::under(std::path::Path::new("/tmp"), "fiber-job-notices");
        let workspace = home.path().join("workspace");
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&credentials).unwrap();
        let fake = fakes::clock::FakeClock::new();
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
                tools: tools(&inbox)
                    .into_iter()
                    .map(|tool| ("builtin".to_owned(), tool))
                    .collect(),
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
        Self {
            dir: home.path().join("s_test"),
            _home: home,
            log,
            inbox,
            provider,
            looped: Some(looped),
            watched,
            clock: fake,
            runner: None,
        }
    }

    /// The lines written since the last call, ephemeral ones too, through
    /// the next `turn_completed`: call it after a turn ends.
    #[track_caller]
    fn watched(&mut self) -> Vec<Envelope> {
        let watcher = self.watched.take().unwrap();
        let (watcher, lines) = read_until(watcher, "a turn_completed line", |line| {
            line.kind == "turn_completed"
        });
        self.watched = Some(watcher);
        // `run` starts the status observer, whose lines race the
        // loop's own and are not what these tests pin.
        lines
            .into_iter()
            .filter(|line| line.kind != "session_status")
            .collect()
    }

    /// Starts one turn on its own thread.
    fn spawn(&mut self) -> mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)> {
        let mut looped = self.looped.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn();
            let _sent = tx.send((looped, outcome));
        });
        rx
    }

    /// Waits for the turn [`World::spawn`] started, failing at [`fakes::MUST_SUCCEED_WITHIN`].
    #[track_caller]
    fn join(
        &mut self,
        rx: &mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)>,
        wait: &Deadline,
    ) -> Option<TurnOutcome> {
        let (looped, outcome) = wait.recv(rx).expect("the turn ended");
        self.looped = Some(looped);
        outcome.unwrap()
    }

    /// Runs one turn on its own thread, failing at [`fakes::MUST_SUCCEED_WITHIN`].
    #[track_caller]
    fn turn(&mut self) -> Option<TurnOutcome> {
        let rx = self.spawn();
        let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        self.join(&rx, &wait)
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

    fn requests(&self) -> Vec<ModelRequest> {
        self.provider.requests()
    }
}

fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
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

/// The `job_completed` line for `id` carries no action, names `id` and
/// reads as [`failed`].
fn assert_notice(line: &Envelope, id: &str) {
    assert_eq!(line.kind, "job_completed");
    assert_eq!(line.action_id, None);
    assert!(line.turn_id.is_some());
    assert_eq!(line.payload["job_id"], id);
    assert_eq!(line.payload["status"], "failed");
    assert_eq!(line.payload["error"]["code"], "nonzero_exit");
    assert_eq!(line.payload["output_tail"], "1 failing\n");
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

// Idle: a notice starts a turn.

fn write_extension_started(world: &mut World, id: &str) {
    world
        .looped
        .as_mut()
        .unwrap()
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId(id.into()),
                tool: None,
                extension: Some("test/scan".into()),
                description: "extension scan".into(),
                output_path: "/child/events.jsonl".into(),
            }),
            None,
            None,
        )
        .unwrap();
    write_delegate_started(world, id, "fiber");
}

#[test]
fn an_extension_end_is_claimed_hooked_and_logged_without_turn_input() {
    for running in [false, true] {
        for hold in [false, true] {
            for block in [false, true] {
                let hooks = Arc::new(RecordingHooks::default());
                hooks.answer_with(move |seen| {
                    if block {
                        withheld()
                    } else {
                        changed(&seen.content.replace("SECRET", "redacted"))
                    }
                });
                let mut world =
                    World::new(Vec::new(), Vec::new(), |_| Vec::new()).hooked(Arc::clone(&hooks));
                write_extension_started(&mut world, DELEGATE);
                let asked = Asked::default();
                let delivery = counted_delegate(DELEGATE, "SECRET", hold, &asked);
                let looped = world.looped.as_mut().unwrap();
                let before = looped.conversation.clone();
                if running {
                    looped
                        .admit_running(delivery, &TurnId("t_test".into()))
                        .unwrap();
                } else {
                    let mut input = crate::inbox::TurnInput::of(Vec::new());
                    looped.admit_idle(delivery, &mut input).unwrap();
                    assert!(input.pieces.is_empty());
                }
                assert_eq!(asked.count(), 1);
                assert!(looped.queued.is_empty());
                assert_eq!(looped.conversation, before);
                assert!(world.requests().is_empty());
                let ends: Vec<_> = log::read(&world.dir)
                    .unwrap()
                    .into_iter()
                    .filter(|line| {
                        matches!(line.kind.as_str(), "delegate_finished" | "job_completed")
                    })
                    .collect();
                assert_eq!(ends.len(), if hold { 2 } else { 0 });
                if hold {
                    assert_eq!(kinds(&ends), ["delegate_finished", "job_completed"]);
                    assert!(
                        ends.iter()
                            .all(|line| line.turn_id.is_none() && line.action_id.is_none())
                    );
                    assert_eq!(
                        ends[0].payload["text"],
                        if block {
                            withheld_line()
                        } else {
                            "redacted".into()
                        }
                    );
                    let seen = hooks.calls();
                    assert_eq!(seen[0].delivery, Some(JobDelivery::Delegate));
                    assert_eq!(seen[0].tool, "");
                    assert_eq!(seen[0].content, "SECRET");
                }
            }
        }
    }
}

#[test]
fn only_model_jobs_hold_idle_and_appear_in_the_jobs_check() {
    let jobs = Arc::new(Listed::default());
    jobs.set(&[JOB, DELEGATE]);
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new())
        .with_jobs(jobs)
        .idle(Duration::from_secs(60));
    write_extension_started(&mut world, DELEGATE);
    let looped = world.looped.as_mut().unwrap();
    let model = looped.model_running(looped.running());
    assert_eq!(model, [JobId(JOB.into())]);
    looped.start_unattended();
    assert!(looped.check_deadline(&model).is_some());
    assert!(looped.check_deadline(&[]).is_none());
    let until = looped.idle_deadline();
    assert!(looped.idle_until(until, &model).is_none());
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped.check_jobs(&mut input).unwrap();
    assert_eq!(input.pieces.len(), 1);
    assert!(
        matches!(looped.queued.back(), Some(super::Queued::Pending(ids, _)) if ids == &[JobId(JOB.into())])
    );
}

fn extension_start(id: &str) -> Delivery {
    Delivery::ExtensionJob(
        JobStarted {
            job_id: JobId(id.into()),
            tool: None,
            extension: Some("test/scan".into()),
            description: "scan".into(),
            output_path: "/child/events.jsonl".into(),
        },
        DelegateStarted {
            job_id: JobId(id.into()),
            delegate_session_id: SessionId("s_child".into()),
            harness: "fiber".into(),
            model: "fake/model".into(),
            workspace: "/w".into(),
            worktree: None,
            forked_from: None,
        },
    )
}

#[test]
fn extension_start_racing_a_running_read_neither_holds_idle_nor_notifies_on_close() {
    for close in [false, true] {
        let jobs = Arc::new(Listed::default());
        jobs.set(&[DELEGATE]);
        let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new())
            .with_jobs(jobs.clone())
            .idle(Duration::from_secs(60));
        *jobs.arriving.lock().unwrap() =
            Some((world.inbox.clone(), vec![extension_start(DELEGATE)]));
        let (stops, stopped) = mpsc::channel();
        *jobs.stops.lock().unwrap() = Some(stops);
        world.looped.as_mut().unwrap().closing = close;
        let origin = world.clock.now();
        let finished = world.spawn_run();
        if !close {
            world.advance_parked(
                Some(origin + Duration::from_secs(60)),
                Duration::from_secs(60),
            );
            world.send(Delivery::Cancelled);
        }
        let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        assert_eq!(wait.recv(&stopped).unwrap(), JobId(DELEGATE.into()));
        {
            // The held-delegate send and the list clear share one update,
            // so the settle never blocks on a notice with nothing to drain.
            jobs.running.update_running(|ids| {
                world.send(delegate_held(DELEGATE, "PRIVATE SENTINEL".into()));
                ids.clear();
            });
        }
        ran(&finished, &wait);
        let lines = log::read(&world.dir).unwrap();
        assert!(
            !lines
                .iter()
                .any(|line| matches!(line.kind.as_str(), "turn_started" | "jobs_pending_notified"))
        );
        assert!(lines.iter().any(|line| line.kind == "delegate_finished"));
        assert!(world.requests().is_empty());
    }
}

#[test]
fn jobs_check_drains_an_extension_start_after_reading_the_running_list() {
    let jobs = Arc::new(Listed::default());
    jobs.set(&[DELEGATE]);
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new()).with_jobs(jobs.clone());
    *jobs.arriving.lock().unwrap() = Some((world.inbox.clone(), vec![extension_start(DELEGATE)]));
    let looped = world.looped.as_mut().unwrap();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped.check_jobs(&mut input).unwrap();
    assert!(input.pieces.is_empty());
    assert!(looped.queued.is_empty());
    assert!(looped.handoff.carry.owned(DELEGATE));
}

struct EmptyOffer;

impl contract::repository::RepositoryCode for EmptyOffer {
    fn unapproved(&self) -> Result<Vec<contract::repository::Unapproved>, Failure> {
        Ok(Vec::new())
    }

    fn file(&self, _: &str) -> Result<String, Failure> {
        Err(Failure {
            code: ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }

    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<contract::repository::Decided, Failure> {
        unreachable!("the pending offer has no items")
    }
}

/// Drives a turn to an approval wait, unwinds it past the idle delay, exits
/// with the approval suspended, resumes, and finishes the turn. When
/// `send_start` the resume finds an extension start waiting in the inbox;
/// when `send_end` the finishing turn's first model request sends the
/// extension job's end, so `admit_running` takes it in the step drain.
fn finish_suspended_turn(send_start: bool, send_end: bool) -> World {
    let jobs = Arc::new(Listed::default());
    let mut world = World::new(
        vec![calls("gated"), calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    )
    .with_jobs(Arc::clone(&jobs) as Arc<dyn Jobs>)
    .idle(Duration::from_secs(60));
    let (id_tx, id_rx) = mpsc::channel();
    let watched = on_request(&world.log, move |id| {
        id_tx.send(id).unwrap();
    });
    world.send(prompt("go"));
    let origin = world.clock.now();
    let rx = world.spawn();
    let _suspended_on: RequestId =
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv_or_fail(&id_rx, "the approval was raised");
    world.clock.advance_when_parked(
        Some(origin + Duration::from_secs(60)),
        None,
        Duration::from_secs(60),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    world.send(Delivery::Cancelled);
    assert_eq!(
        world.join(&rx, &Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        None,
        "the idle delay unwinds the turn"
    );
    watched.join().unwrap();
    // The fake keeps the job in `running()` from the resume on. Phase 1
    // lists no job: its approval wait needs the idle deadline to unwind.
    if send_start {
        jobs.set(&[DELEGATE]);
    }
    crate::fiber_exited(
        &world.log,
        world.looped.as_ref().unwrap().summary(),
        Ok(()),
        false,
        None,
    )
    .unwrap();
    drop(world.looped.take());
    let resumed = crate::resumed(&world.dir).expect("the exit suspends a batch");
    let (tx, inbox_rx) = mpsc::channel();
    let by_call = if send_end {
        vec![vec![delegate_held(DELEGATE, "PRIVATE SENTINEL".into())]]
    } else {
        Vec::new()
    };
    let provider = Arc::new(During {
        inner: Arc::clone(&world.provider),
        inbox: tx.clone(),
        by_call: Mutex::new(by_call.into()),
    });
    let home = world.home().to_path_buf();
    let workspace = home.join("workspace");
    let credentials = home.join("credentials");
    let looped = Loop::resume(
        crate::Session {
            log: Arc::clone(&world.log),
            provider,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home,
                "/bin/sh".into(),
                world.dir.join("events.jsonl").display().to_string(),
                Arc::clone(&world.clock) as Arc<dyn contract::clock::Clock>,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: vec![("builtin".to_owned(), Arc::new(Gated) as Arc<dyn Tool>)],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(AskGated),
            },
        },
        resumed,
    )
    .unwrap()
    .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>)
    .idle_exit(Some(Duration::from_secs(60)));
    world.inbox = tx;
    world.looped = Some(looped);
    if send_start {
        world.send(extension_start(DELEGATE));
    }
    let mut watcher = world.log.watch();
    let answered = thread::spawn({
        let inbox = world.inbox.clone();
        move || {
            for _ in 0..2 {
                let (next, lines) = read_until(watcher, "a permission_requested line", |line| {
                    line.kind == "permission_requested"
                });
                watcher = next;
                let line = lines.last().unwrap();
                inbox
                    .send(allow(RequestId(
                        line.payload["request_id"].as_str().unwrap().into(),
                    )))
                    .unwrap();
            }
        }
    });
    let rx = world.spawn();
    assert_eq!(
        world.join(&rx, &Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        Some(TurnOutcome::Completed)
    );
    answered.join().unwrap();
    world
}

#[test]
fn a_resumed_suspended_turn_records_a_waiting_extension_start_first() {
    // `docs/delegates.md`, "Events": `delegate_finished` precedes `job_completed`.
    let mut world = finish_suspended_turn(true, true);
    let lines = log::read(&world.dir).unwrap();
    // The finishing turn recorded the waiting start with no turn and no action.
    let started: Vec<_> = lines
        .iter()
        .filter(|line| line.kind == "job_started")
        .collect();
    assert_eq!(started.len(), 1);
    assert!(started[0].turn_id.is_none() && started[0].action_id.is_none());
    assert_eq!(started[0].payload["extension"], json!("test/scan"));
    let delegates: Vec<_> = lines
        .iter()
        .filter(|line| line.kind == "delegate_started")
        .collect();
    assert_eq!(delegates.len(), 1);
    assert!(delegates[0].turn_id.is_none() && delegates[0].action_id.is_none());
    // The finish completes the suspended turn: no new turn started.
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "turn_started")
            .count(),
        1
    );
    // The step drain wrote the end with no turn and no wake.
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| matches!(line.kind.as_str(), "delegate_finished" | "job_completed"))
        .collect();
    assert_eq!(
        ends.iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["delegate_finished", "job_completed"]
    );
    assert_eq!(ends[0].payload["text"], json!("PRIVATE SENTINEL"));
    assert!(
        ends.iter()
            .all(|line| line.turn_id.is_none() && line.action_id.is_none())
    );
    // The step's next request equals a run without the end: the model
    // saw nothing of it, only the calls and user texts match (ids and
    // directories differ per run).
    let control = finish_suspended_turn(false, false);
    let requests = world.requests();
    let control_requests = control.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(control_requests.len(), 3);
    for (got, want) in requests[1..].iter().zip(control_requests[1..].iter()) {
        let scrub = |texts: Vec<String>, dir: &str| {
            texts
                .iter()
                .map(|text| text.replace(dir, "<home>"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            scrub(users(got), &world.home().display().to_string()),
            scrub(users(want), &control.home().display().to_string())
        );
        let calls = |request: &ModelRequest| {
            request
                .conversation
                .iter()
                .filter_map(|input| match input {
                    Input::ToolCall { call, .. } => Some(call.name.clone()),
                    Input::User { .. }
                    | Input::Assistant { .. }
                    | Input::Reasoning { .. }
                    | Input::ToolResult { .. }
                    | Input::ToolSearchResult { .. } => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(calls(got), calls(want));
    }
    // With a repository offer pending and the fake keeping the job in
    // `running()`, the offer wait still ends at the idle delay: the
    // filtered list is empty.
    let mut looped = world.looped.take().unwrap();
    let origin = world.clock.now();
    let pending = RepositoryCodeOffered {
        request_id: RequestId("r_offer".into()),
        items: Vec::new(),
    };
    let mut folded = crate::offer::Folded::default();
    folded.fold(&Event::RepositoryCodeOffered(pending));
    looped.repository = crate::offer::State::resumed(folded);
    looped = looped.repository_code(Arc::new(EmptyOffer));
    looped
        .log
        .append(&Event::Clients(Clients { count: 1 }), None, None)
        .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(looped.offer(None));
    });
    world.clock.advance_when_parked(
        Some(origin + Duration::from_secs(60)),
        None,
        Duration::from_secs(60),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    world.send(Delivery::Cancelled);
    assert!(
        !Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&finished)
            .expect("the offer wait ended")
            .unwrap()
    );
}

#[test]
fn a_notice_while_idle_starts_a_turn_named_by_its_job() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    assert_notice(&lines[2], JOB);
    assert_eq!(lines[2].turn_id, lines[0].turn_id);
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].conversation.last(),
        Some(&Input::User {
            text: rendered(JOB),
            images: Vec::new()
        })
    );
}

#[test]
fn a_delegate_notice_writes_its_finish_before_its_end() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(delegate_held(DELEGATE, "Done.".into()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["delegate_finished", "job_completed"])
    );
    // The turn's input still names the job once, with no message item.
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [DELEGATE]}])
    );
    assert_eq!(lines[2].kind, "delegate_finished");
    assert_eq!(lines[2].action_id, None);
    assert_eq!(lines[2].payload["job_id"], DELEGATE);
    assert_eq!(lines[2].payload["text"], "Done.");
    assert!(lines[2].payload.get("artifact").is_none());
    assert_eq!(lines[2].turn_id, lines[0].turn_id);
    assert_notice(&lines[3], DELEGATE);
    // The wake renders the final message first, then the notice.
    let seen = users(&world.requests()[0]);
    assert_eq!(
        seen[seen.len() - 2..],
        [delegate_rendered(DELEGATE, "Done."), rendered(DELEGATE)]
    );
}

#[test]
fn a_20_kib_final_message_is_cut_with_its_full_text_in_an_artifact() {
    let full = "z".repeat(20 * 1024);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(delegate_held(DELEGATE, full.clone()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["delegate_finished", "job_completed"])
    );
    let text = lines[2].payload["text"].as_str().unwrap();
    assert!(text.starts_with(&full[..16 * 1024]));
    assert!(text.contains("[4096 bytes cut."));
    assert_eq!(
        lines[2].payload["artifact"],
        format!("artifacts/{DELEGATE}.txt")
    );
    let kept =
        std::fs::read_to_string(world.dir.join(format!("artifacts/{DELEGATE}.txt"))).unwrap();
    assert_eq!(kept, full);
    // The model reads the logged, already-bounded line, framed by the
    // delegate-finished section.
    let seen = users(&world.requests()[0]);
    assert_eq!(seen[seen.len() - 2], delegate_rendered(DELEGATE, text));
}

#[test]
fn delegate_mode_records_its_parent_with_or_without_a_worktree() {
    fn opened(
        id: &str,
        parent: Option<Parent>,
        worktree: Option<contract::shapes::Worktree>,
    ) -> (fakes::TempDir, std::path::PathBuf) {
        let home = fakes::TempDir::new("fiber-delegate-open");
        let workspace = home.path().join("workspace");
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&credentials).unwrap();
        let fake = fakes::clock::FakeClock::new();
        let clock: Arc<dyn contract::clock::Clock> = fake.clone();
        let log =
            Arc::new(Log::create(home.path(), SessionId(id.into()), Arc::clone(&clock)).unwrap());
        let (_, rx) = mpsc::channel();
        let permissions = crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials,
            credential_files: Vec::new(),
            rules: Arc::new(AskGated),
        };
        let prompt = crate::prompt::PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".into(),
            home.path()
                .join(format!("{id}/events.jsonl"))
                .display()
                .to_string(),
            clock,
            fakes::CONTEXT_WINDOW,
        );
        let model = Model {
            reference: "fake/model".into(),
            cost: None,
            subscription: false,
        };
        match parent {
            Some(parent) => Loop::delegate(
                crate::Session {
                    log,
                    provider: Arc::new(ScriptedProvider::new(Vec::new())),
                    model,
                    prompt,
                    inbox: rx,
                    tools: Vec::new(),
                    permissions,
                },
                parent,
                worktree,
            ),
            None => Loop::start(
                crate::Session {
                    log,
                    provider: Arc::new(ScriptedProvider::new(Vec::new())),
                    model,
                    prompt,
                    inbox: rx,
                    tools: Vec::new(),
                    permissions,
                },
                None,
            ),
        }
        .unwrap();
        let dir = home.path().join(id);
        (home, dir)
    }
    let parent = Parent {
        session_id: SessionId("s_parent0000000001".into()),
        delegate_id: JobId("j_de1e6a7e12345678".into()),
    };
    let (_home, dir) = opened("s_delegate00000001", Some(parent.clone()), None);
    let lines: Vec<Envelope> = log::read(&dir)
        .unwrap()
        .into_iter()
        .filter(Envelope::is_durable)
        .collect();
    assert_eq!(lines[0].kind, "session_started");
    assert_eq!(
        lines[0].payload["parent"],
        json!({
            "session_id": "s_parent0000000001",
            "delegate_id": "j_de1e6a7e12345678",
        })
    );
    assert!(lines[0].payload.get("worktree").is_none());
    let (_home, dir) = opened("s_plain000000000001", None, None);
    let lines: Vec<Envelope> = log::read(&dir)
        .unwrap()
        .into_iter()
        .filter(Envelope::is_durable)
        .collect();
    assert_eq!(lines[0].kind, "session_started");
    assert!(lines[0].payload.get("parent").is_none());
    let worktree = contract::shapes::Worktree {
        path: "/worktrees/s_child".into(),
        branch: "fiber/s_child".into(),
        repository: Some("/repo/.git".into()),
    };
    let (_home, dir) = opened(
        "s_isolated00000001",
        Some(parent.clone()),
        Some(worktree.clone()),
    );
    let lines: Vec<_> = log::read(&dir)
        .unwrap()
        .into_iter()
        .filter(Envelope::is_durable)
        .collect();
    // docs/events.md, "Session and turn": session_started is the first line
    // of every session's log; docs/loop.md, "Starting a turn": a turn starts
    // on an inbox arrival, and none arrives here, so the opened session's
    // log holds that line alone. docs/delegates.md, "Events": a delegate's
    // session_started carries its parent.
    assert_eq!(kinds(&lines), ["session_started"]);
    assert_eq!(
        lines[0].payload["parent"],
        serde_json::to_value(parent).unwrap()
    );
    assert_eq!(
        lines[0].payload["worktree"],
        serde_json::to_value(worktree).unwrap()
    );
}

#[test]
fn notices_sent_together_start_one_turn_in_arrival_order() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(held(JOB));
    world.send(held(OTHER));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["job_completed", "job_completed"])
    );
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB, OTHER]}])
    );
    assert_notice(&lines[2], JOB);
    assert_notice(&lines[3], OTHER);
    let users = users(&world.requests()[0]);
    assert_eq!(users[users.len() - 2..], [rendered(JOB), rendered(OTHER)]);
}

#[test]
fn messages_and_notices_are_items_in_arrival_order() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(prompt("Hello."));
    world.send(held(JOB));
    world.send(steer("And this."));
    world.send(held(OTHER));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["job_completed", "job_completed"])
    );
    let input = &lines[0].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 4);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["content"][0]["text"], "Hello.");
    assert_eq!(input[1], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[2]["type"], "message");
    assert_eq!(input[2]["content"][0]["text"], "And this.");
    assert_eq!(input[3], json!({"type": "jobs", "job_ids": [OTHER]}));
    assert_notice(&lines[2], JOB);
    assert_notice(&lines[3], OTHER);
    // The messages render from `turn_started`; the notices from their own
    // lines at the step boundary.
    let users = users(&world.requests()[0]);
    assert_eq!(
        users[users.len() - 4..],
        [
            "Hello.".to_owned(),
            "And this.".to_owned(),
            rendered(JOB),
            rendered(OTHER)
        ]
    );
}

#[test]
fn a_prompt_after_a_notice_is_accepted_into_the_same_turn() {
    // `docs/loop.md`, "Starting a turn" and "One step": input items keep arrival order.
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let (answered, answer) = mpsc::channel();
    world.send(held(JOB));
    world.send(Delivery::Prompt(
        message("Hello.", "c_prompt"),
        reported(answered),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true)
    );
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    let input = &lines[0].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 2);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[1]["content"][0]["text"], "Hello.");
}

#[test]
fn a_second_prompt_after_a_notice_joins_its_turn() {
    // `docs/loop.md`, "Starting a turn" and "One step": input items keep arrival order.
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let (answered, answer) = mpsc::channel();
    world.send(held(JOB));
    world.send(prompt("Hello."));
    world.send(Delivery::Prompt(
        message("Again.", "c_again"),
        reported(answered),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true)
    );
    let input = world.turn_lines()[0].payload["input"].clone();
    let input = input.as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[1]["content"][0]["text"], "Hello.");
    assert_eq!(input[2]["content"][0]["text"], "Again.");
}

#[test]
fn a_notice_whose_claim_fails_starts_no_turn() {
    // `docs/loop.md`, "Starting a turn": only accepted inbox work starts a turn.
    let mut world = World::new(vec![Scripted::text("Hi.")], Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    world.send(notice(JOB, false, &asked));
    // Whether the loop takes the notice alone or with the prompt, only the
    // prompt starts a turn.
    let rx = world.spawn();
    world.send(prompt("Hello."));
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    assert_eq!(world.join(&rx, &wait), Some(TurnOutcome::Completed));
    assert_eq!(asked.count(), 1);
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&[]));
    let input = &lines[0].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input[0]["content"][0]["text"], "Hello.");
}

#[test]
fn a_notice_after_close_starts_no_turn_and_is_not_claimed() {
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    world.send(Delivery::Close(ignore()));
    world.send(notice(JOB, true, &asked));
    assert_eq!(world.turn(), None);
    assert_eq!(asked.count(), 0);
    assert!(world.requests().is_empty());
}

#[test]
fn a_notice_after_close_with_a_message_joins_its_turn() {
    let mut world = World::new(vec![Scripted::text("Hi.")], Vec::new(), |_| Vec::new());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(
        lines[0].payload["input"][1],
        json!({"type": "jobs", "job_ids": [JOB]})
    );
}

#[test]
fn a_prompt_turn_that_saw_close_in_its_batch_still_takes_a_steer() {
    // `docs/loop.md`, "One step": steering joins at the boundary in arrival order.
    // `close` lands in the same idle batch as the prompt, after it: the
    // turn still starts with no session message, so `after_close` stays
    // false and a person's steer during the turn is accepted.
    let (steered, steer_answer) = mpsc::channel();
    let mut world = World::new(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![vec![Delivery::Steer(
            message("And this.", "c_and"),
            reported(steered),
        )]],
        |_| Vec::new(),
    );
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&steer_answer),
        Ok(true)
    );
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&"And this.".to_owned()));
}

#[test]
fn a_session_message_turn_without_close_still_takes_a_steer() {
    // `docs/loop.md`, "One step": steering joins at the boundary in arrival order.
    // No `close`: the message starts the turn, so `after_close` stays
    // false and a person's steer during the turn is accepted.
    let (steered, steer_answer) = mpsc::channel();
    let mut world = World::new(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![vec![Delivery::Steer(
            message("And this.", "c_and"),
            reported(steered),
        )]],
        |_| Vec::new(),
    );
    world.send(session_message("c_message", None, ignore()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&steer_answer),
        Ok(true)
    );
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[0]).last(), Some(&FRAMED.to_owned()));
    assert_eq!(users(&requests[1]).last(), Some(&"And this.".to_owned()));
}

#[test]
fn a_steer_drop_naming_a_session_message_leaves_it_queued() {
    // `docs/loop.md`, "One step": inbox work joins at the next boundary.
    // A `steer_drop` never removes a session message: the drop names the
    // message's command, but the message stays and starts its turn.
    let (dropped, drop_answer) = mpsc::channel();
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(session_message("c_message", None, ignore()));
    world.send(Delivery::SteerDrop(
        CommandId("c_message".into()),
        reported(dropped),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&drop_answer),
        Ok(false)
    );
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&[]));
    assert_eq!(lines[0].payload["input"], message_input("c_message"));
}

#[test]
fn a_steer_drop_while_idle_skips_notices_and_the_prompt() {
    // `docs/loop.md`, "Starting a turn" and "One step": remaining input keeps arrival order.
    let mut world = World::new(vec![Scripted::text("Hi.")], Vec::new(), |_| Vec::new());
    world.send(held(JOB));
    world.send(Delivery::Prompt(message("Hello.", "c_same"), ignore()));
    world.send(Delivery::Steer(message("Drop me.", "c_same"), ignore()));
    let (answered, answer) = mpsc::channel();
    world.send(Delivery::SteerDrop(
        CommandId("c_same".into()),
        reported(answered),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true)
    );
    let input = world.turn_lines()[0].payload["input"].clone();
    let input = input.as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[1]["content"][0]["text"], "Hello.");
}

// The notice's text.

#[test]
fn a_notice_reads_its_status_exit_signal_error_and_output() {
    let mut killed = failed(JOB);
    killed.process = Some(Process {
        exit_code: None,
        signal: Some("SIGKILL".into()),
        timed_out: false,
    });
    killed.error = Some(Failure {
        code: ErrorCode::Signal,
        message: "Killed by SIGKILL.".into(),
        retry_after_ms: None,
        provider: None,
    });
    killed.output_tail = None;
    assert_eq!(
        notice_text(&killed),
        format!("Fiber: background job {JOB} ended: failed.\nKilled by SIGKILL.")
    );
    let mut errored = failed(JOB);
    errored.process = None;
    errored.error = Some(Failure {
        code: ErrorCode::ToolError,
        message: "The job ended without a result.\n".into(),
        retry_after_ms: None,
        provider: None,
    });
    assert_eq!(
        notice_text(&errored),
        format!(
            "Fiber: background job {JOB} ended: failed.\nThe job ended without a result.\nLast output:\n1 failing"
        )
    );
    let done = JobCompleted {
        job_id: JobId(JOB.into()),
        status: Outcome::Completed,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    };
    assert_eq!(
        notice_text(&done),
        format!("Fiber: background job {JOB} ended: completed.\nExit code 0.")
    );
    let mut hooked = failed(JOB);
    hooked.notice = Some("redacted".into());
    hooked.changed_by = Some(vec!["redactor".into()]);
    assert_eq!(notice_text(&hooked), "redacted");
    let mut withheld = failed(JOB);
    withheld.output_tail = None;
    withheld.notice = Some(String::new());
    withheld.changed_by = Some(vec!["redactor".into()]);
    assert_eq!(notice_text(&withheld), "");
    let stopped = JobCompleted {
        status: Outcome::Cancelled,
        process: None,
        ..done
    };
    assert_eq!(
        notice_text(&stopped),
        format!("Fiber: background job {JOB} ended: cancelled.")
    );
    assert_eq!(notice_text(&failed(JOB)), rendered(JOB));
}

#[test]
fn a_notice_names_restored_and_unrestored_protected_paths() {
    let mut restored = failed(JOB);
    restored.protected_paths = Some(ProtectedPaths {
        restored: vec![".fiber".to_owned()],
        failed: Vec::new(),
    });
    assert_eq!(
        notice_text(&restored),
        format!(
            "{}\nProtected path .fiber changed while a confined call ran; Fiber restored it.",
            rendered(JOB)
        )
    );
    let mut unrestored = failed(JOB);
    unrestored.protected_paths = Some(ProtectedPaths {
        restored: Vec::new(),
        failed: vec![ProtectedPathFailure {
            path: ".git/config".to_owned(),
            message: "permission denied".to_owned(),
        }],
    });
    assert_eq!(
        notice_text(&unrestored),
        format!(
            "{}\nProtected path .git/config changed while a confined call ran; Fiber could not restore it: permission denied.",
            rendered(JOB)
        )
    );
}

// Running: a notice joins at the next step boundary.

fn sending(inbox: &Sender<Delivery>, during: Vec<Delivery>) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Sends {
        inbox: inbox.clone(),
        during: Mutex::new(during),
    })]
}

/// A turn whose first step calls `name`, then `between` at the second
/// step's boundary, then a text reply.
fn two_steps_with<'a>(called: &[&'a str], between: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec![
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    all.extend_from_slice(called);
    all.push("step_started");
    all.extend_from_slice(between);
    all.extend_from_slice(&REPLY);
    all
}

const RAN: [&str; 2] = ["tool_call_started", "tool_call_completed"];

#[test]
fn a_notice_while_a_call_runs_is_written_at_the_next_step_boundary() {
    let mut world = World::new(
        vec![calls("sends"), Scripted::text("Seen.")],
        Vec::new(),
        |inbox| sending(inbox, vec![held(JOB)]),
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), two_steps_with(&RAN, &["job_completed"]));
    assert_notice(&lines[9], JOB);
    assert_eq!(lines[0].payload["input"].as_array().unwrap().len(), 1);
    // A notice is not a steering message: the queue line never moves.
    let watched = world.watched();
    assert!(watched.iter().any(|line| line.kind == "job_completed"));
    assert!(!kinds(&watched).contains(&"steering_queue"));
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert!(!users(&requests[0]).contains(&rendered(JOB)));
    // After the call's result, as the last thing the model reads.
    let conversation = &requests[1].conversation;
    assert!(matches!(
        conversation[conversation.len() - 2],
        Input::ToolResult { .. }
    ));
    assert_eq!(
        conversation.last(),
        Some(&Input::User {
            text: rendered(JOB),
            images: Vec::new()
        })
    );
}

#[test]
fn a_steer_and_a_notice_are_written_in_arrival_order() {
    for steer_first in [true, false] {
        let during = if steer_first {
            vec![steer("Also this."), held(JOB)]
        } else {
            vec![held(JOB), steer("Also this.")]
        };
        let mut world = World::new(
            vec![calls("sends"), Scripted::text("Seen.")],
            Vec::new(),
            |inbox| sending(inbox, during),
        );
        world.send(prompt("go"));
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let lines = world.turn_lines();
        let between = if steer_first {
            ["steering_applied", "job_completed"]
        } else {
            ["job_completed", "steering_applied"]
        };
        assert_eq!(kinds(&lines), two_steps_with(&RAN, &between));
        let notice = if steer_first { 10 } else { 9 };
        assert_notice(&lines[notice], JOB);
        // The queue line lists the steer alone, and is empty once applied.
        let queues: Vec<usize> = world
            .watched()
            .iter()
            .filter(|line| line.kind == "steering_queue")
            .map(|line| line.payload["messages"].as_array().unwrap().len())
            .collect();
        assert_eq!(queues, [1, 0]);
        let users = users(&world.requests()[1]);
        let expected = if steer_first {
            ["Also this.".to_owned(), rendered(JOB)]
        } else {
            [rendered(JOB), "Also this.".to_owned()]
        };
        assert_eq!(users[users.len() - 2..], expected);
    }
}

#[test]
fn a_steer_drop_while_running_finds_the_steer_behind_a_notice() {
    // `docs/loop.md`, "One step": notices and steering join in arrival order.
    let (answered, answer) = mpsc::channel();
    let mut world = World::new(
        vec![calls("sends"), Scripted::text("Seen.")],
        Vec::new(),
        |inbox| {
            sending(
                inbox,
                vec![
                    held(JOB),
                    Delivery::Steer(message("Drop me.", "c_drop"), ignore()),
                    Delivery::SteerDrop(CommandId("c_drop".into()), reported(answered)),
                ],
            )
        },
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true)
    );
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), two_steps_with(&RAN, &["job_completed"]));
    assert_notice(&lines[9], JOB);
}

#[test]
fn a_notice_whose_claim_fails_while_running_is_not_written() {
    let asked = Asked::default();
    let during = vec![notice(JOB, false, &asked)];
    let mut world = World::new(
        vec![calls("sends"), Scripted::text("Seen.")],
        Vec::new(),
        |inbox| sending(inbox, during),
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(asked.count(), 1);
    assert_eq!(kinds(&world.turn_lines()), two_steps_with(&RAN, &[]));
}

#[test]
fn a_notice_during_the_final_reply_continues_the_turn() {
    let mut world = World::new(
        vec![Scripted::text("Done."), Scripted::text("Seen.")],
        vec![vec![held(JOB)]],
        |_| Vec::new(),
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let mut expected = vec!["turn_started", "step_started"];
    expected.extend_from_slice(&REPLY[..4]);
    expected.extend_from_slice(&["step_started", "job_completed"]);
    expected.extend_from_slice(&REPLY);
    assert_eq!(kinds(&lines), expected);
    assert_notice(&lines[7], JOB);
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.last(),
        Some(&Input::User {
            text: rendered(JOB),
            images: Vec::new()
        })
    );
}

/// Watches the log for `permission_requested`, then runs `send` with its
/// request id: the signal the loop is waiting for a reply.
fn on_request(log: &Log, send: impl FnOnce(RequestId) + Send + 'static) -> thread::JoinHandle<()> {
    let watcher = log.watch();
    thread::spawn(move || {
        let (_, lines) = read_until(watcher, "a permission_requested line", |line| {
            line.kind == "permission_requested"
        });
        let line = lines.last().unwrap();
        send(RequestId(
            line.payload["request_id"].as_str().unwrap().into(),
        ));
    })
}

fn allow(request_id: RequestId) -> Delivery {
    Delivery::Reply(
        Reply {
            request_id,
            answer: ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: None,
                remember: None,
            },
        },
        ignore(),
    )
}

#[test]
fn a_notice_during_an_approval_wait_is_written_at_the_next_boundary() {
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        move |id| {
            inbox.send(held(JOB)).unwrap();
            inbox.send(allow(id)).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    let lines = world.turn_lines();
    let called = [
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
    ];
    assert_eq!(kinds(&lines), two_steps_with(&called, &["job_completed"]));
    assert_notice(&lines[11], JOB);
}

#[test]
fn a_notice_kept_by_a_cancelled_turn_starts_the_next_one() {
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let cancel = Arc::clone(&world.looped.as_ref().unwrap().cancel);
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        move |_| {
            inbox.send(held(JOB)).unwrap();
            assert!(cancel.cancel());
            inbox.send(Delivery::Cancelled).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
    let cut = world.turn_lines();
    assert!(!kinds(&cut).contains(&"job_completed"), "{:?}", kinds(&cut));
    assert_eq!(cut.last().unwrap().kind, "turn_completed");
    let _first = world.watched();
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    assert_notice(&lines[2], JOB);
    // A kept notice is not a kept steer: no queue line moves.
    let watched = world.watched();
    assert!(watched.iter().any(|line| line.kind == "job_completed"));
    assert!(!kinds(&watched).contains(&"steering_queue"));
}

#[test]
fn a_session_message_kept_by_a_cancelled_turn_runs_after_close_with_no_jobs() {
    // `docs/loop.md`, "Interrupt": queued steering starts the next turn.
    let (steered, steer_answer) = mpsc::channel();
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        vec![
            Vec::new(),
            vec![Delivery::Steer(
                message("And this.", "c_and"),
                reported(steered),
            )],
        ],
        |_| vec![Arc::new(Gated)],
    );
    let cancel = Arc::clone(&world.looped.as_ref().unwrap().cancel);
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        move |_| {
            // The message queues while the approval waits; the cancel ends
            // the turn with it still queued.
            inbox
                .send(session_message("c_message", None, ignore()))
                .unwrap();
            assert!(cancel.cancel());
            inbox.send(Delivery::Cancelled).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
    let cut = world.turn_lines();
    assert!(!kinds(&cut).contains(&"steering_applied"), "{cut:?}");
    assert_eq!(cut.last().unwrap().kind, "turn_completed");
    let _first = world.watched();
    // The close ends the wait for a turn; the kept message starts one
    // after it, while the person's steer in it is refused.
    world.send(Delivery::Close(ignore()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&steer_answer),
        Ok(false)
    );
    let lines = world.turn_lines();
    assert_eq!(lines[0].payload["input"], message_input("c_message"));
    // The queue moved into `turn_started`: it is listed empty first.
    let watched = world.watched();
    assert_eq!(kinds(&watched)[0], "steering_queue");
    assert_eq!(watched[0].payload["messages"], json!([]));
    assert_eq!(kinds(&watched)[1], "turn_started");
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&FRAMED.to_owned()));
}

// Ending with jobs running (`docs/tools.md`, "Background jobs").

/// What the ending notice reads for `ids`.
fn ending_text(ids: &[&str]) -> String {
    format!(
        "Fiber: this session is about to end, and these background jobs are still running: {}. Stop any you do not need with `jobs stop`; the rest will be waited for.",
        ids.join(", ")
    )
}

/// Opens a running job on `jobs`; it runs until its `end` is reported.
fn open_job(jobs: &FakeJobs) -> contract::jobs::Opened {
    jobs.open(contract::jobs::Opening {
        tool: "shell".into(),
        description: "npm test".into(),
        stop: contract::jobs::Stop(Box::new(|| {})),
        lines: false,
        input: None,
    })
    .unwrap()
}

/// Jobs a test lists by hand. When `racing` holds deliveries, the next
/// `running` read sends them and empties the list before it reads: a job
/// that ends just as the loop checks. When `arriving` holds deliveries, the
/// next read sends them and leaves the list: a message that arrives just as
/// the loop checks. Each read's length goes to `reads` when set.
#[derive(Default)]
struct Listed {
    running: StubJobs,
    racing: Mutex<Option<(Sender<Delivery>, Vec<Delivery>)>>,
    arriving: Mutex<Option<(Sender<Delivery>, Vec<Delivery>)>>,
    reads: Mutex<Option<Sender<usize>>>,
    stops: Mutex<Option<Sender<JobId>>>,
}

impl Listed {
    fn set(&self, ids: &[&str]) {
        self.running
            .set_running(ids.iter().map(|id| JobId((*id).into())));
    }
}

impl Jobs for Listed {
    fn open(&self, _: contract::jobs::Opening) -> Result<contract::jobs::Opened, OpenError> {
        Err(OpenError {
            path: "unused".into(),
            source: std::io::Error::other("a listed job is not opened"),
        })
    }

    fn stop(&self, id: &JobId) -> bool {
        self.stops
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|stops| stops.send(id.clone()).is_ok())
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _: contract::jobs::Foreground) {}

    fn running(&self) -> Vec<JobId> {
        // The status observer reads the jobs too; its reads are not the
        // loop's, so they neither fire a scripted arrival nor count.
        if std::thread::current().name() == Some("status") {
            return Jobs::running(&self.running);
        }
        if let Some((inbox, deliveries)) = self.racing.lock().unwrap().take() {
            // The racing send and the list clear share one update, so a
            // snapshot never sees the deliveries queued with the old list.
            self.running.update_running(|ids| {
                for delivery in deliveries {
                    inbox.send(delivery).unwrap();
                }
                ids.clear();
            });
        }
        if let Some((inbox, deliveries)) = self.arriving.lock().unwrap().take() {
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
        }
        let ids = Jobs::running(&self.running);
        if let Some(reads) = self.reads.lock().unwrap().as_ref() {
            let _sent = reads.send(ids.len());
        }
        ids
    }

    fn deliver_to(&self, _: Sender<Delivery>) {}
}

impl World {
    /// Gives the loop `jobs`, whose ends reach its inbox as the door wires
    /// them.
    fn with_jobs(mut self, jobs: Arc<dyn Jobs>) -> Self {
        jobs.deliver_to(self.inbox.clone());
        let looped = self.looped.take().unwrap().jobs(jobs);
        self.looped = Some(looped);
        self
    }

    fn idle(mut self, after: Duration) -> Self {
        let looped = self.looped.take().unwrap().idle_exit(Some(after));
        self.looped = Some(looped);
        self
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

    /// Moves the parked wait past `by`, expecting its park at `expected`:
    /// the runner and the [`fakes::MUST_SUCCEED_WITHIN`] wait are filled in.
    #[track_caller]
    fn advance_parked(
        &self,
        expected: Option<std::time::Instant>,
        by: Duration,
    ) -> fakes::clock::Mark {
        self.clock.advance_when_parked(
            expected,
            self.runner,
            by,
            &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        )
    }

    /// The durable lines of the next turn, `turn_started` through
    /// `turn_completed`.
    #[track_caller]
    fn next_turn(&mut self) -> Vec<Envelope> {
        let lines: Vec<Envelope> = self
            .watched()
            .into_iter()
            .filter(Envelope::is_durable)
            .collect();
        let from = lines
            .iter()
            .position(|line| line.kind == "turn_started")
            .expect("a turn started");
        lines[from..].to_vec()
    }

    fn home(&self) -> &std::path::Path {
        self._home.path()
    }
}

/// Suspends a fresh loop with no turns, rewrites the first line's
/// `parent` to `parent`, and resumes: `None` keeps the genuine
/// root line (the field absent), so the resume stays a root; `Some`
/// names a parent, so the resume stays a delegate. The log is
/// reopened after the rewrite: the edit changes the first line's
/// byte length, which would invalidate the open handle's offset table.
fn resumed_with_parent(parent: Option<Value>) -> (Loop, Arc<FakeJobs>, fakes::TempDir) {
    let mut world = World::new(vec![], Vec::new(), |_| Vec::new());
    // The loop's session holds the log's other half: `..` would drop
    // it too late, so both halves go before the reopen takes the lock.
    drop(world.looped.take());
    drop(world.watched.take());
    let World {
        _home: home,
        dir,
        log,
        provider,
        clock,
        ..
    } = world;
    drop(log);
    // No turn ran, so nothing was folded: the exit reports the empty
    // summary.
    crate::fiber_exited(
        &Log::open(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
        &crate::Summary::default(),
        Ok(()),
        false,
        None,
    )
    .unwrap();
    if let Some(parent) = parent {
        let path = dir.join("events.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        lines[0]["payload"]["parent"] = parent;
        let rewritten: Vec<String> = lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap())
            .collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();
    }
    let log = Arc::new(
        Log::open(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
    );
    let resumed = crate::resumed(&dir).expect("the exit suspends a batch");
    let workspace = home.path().join("workspace");
    let jobs_dir = home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = FakeJobs::new(&jobs_dir);
    let (_, inbox_rx) = mpsc::channel();
    let looped = Loop::resume(
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
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(AskGated),
            },
        },
        resumed,
    )
    .unwrap()
    .budget(Some(1.0))
    .jobs(Arc::clone(&jobs) as Arc<dyn Jobs>);
    // The home directory outlives the loop's log handle: dropping it
    // first would take the session directory with it.
    (looped, jobs, home)
}

/// A $100 copy against a $1 budget, as the relayed watcher line:
/// unaffordable either way, so the budget end trips on it.
fn hundred_copy() -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId("g_cost".into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        input_bytes: 0,
        input_media: None,
        web_searches: None,
        cost: Some(100.0),
        subscription: None,
        extension: None,
        origin_session_id: Some(SessionId("s_grandchild".into())),
        reviewer: None,
    }
}

#[test]
fn a_resumed_delegate_budget_end_with_a_claimed_trip_shuts_down_with_the_tree_stop_code() {
    // The first line names a parent: the resume stays a delegate, so
    // its budget end preempts its failed drain with the tree stop.
    let (mut looped, jobs, _home) = resumed_with_parent(Some(
        json!({"session_id": "s_parent", "delegate_id": "j_parent"}),
    ));
    looped
        .copy_usage(&SessionId("s_grandchild".into()), hundred_copy())
        .unwrap();
    let completed = looped.budget_end(crate::ended(TurnOutcome::Failed, None));
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    assert!(completed.error.is_none());
    assert_eq!(looped.cancel.shutdown_code(), Some(143));
    // Once for the copy's publish, once for the budget end itself.
    assert_eq!(jobs.stop_delegates_calls(), 2);
}

#[test]
fn a_resumed_root_budget_end_with_a_claimed_trip_shuts_down_nothing() {
    // The genuine root line carries no parent field: the resume stays
    // a root, so its budget end reports the failed turn and keeps
    // serving, whatever its trip holds.
    let (mut looped, jobs, _home) = resumed_with_parent(None);
    looped
        .copy_usage(&SessionId("s_child".into()), hundred_copy())
        .unwrap();
    let completed = looped.budget_end(crate::ended(TurnOutcome::Failed, None));
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    assert_eq!(looped.cancel.shutdown_code(), None);
    // Once for the copy's publish, once for the budget end itself.
    assert_eq!(jobs.stop_delegates_calls(), 2);
}

#[test]
fn a_resumed_root_with_an_explicitly_null_parent_shuts_down_nothing() {
    // An explicit null is no parent: the resume stays a root even
    // though the field is present.
    let (mut looped, jobs, _home) = resumed_with_parent(Some(Value::Null));
    looped
        .copy_usage(&SessionId("s_child".into()), hundred_copy())
        .unwrap();
    let completed = looped.budget_end(crate::ended(TurnOutcome::Failed, None));
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    assert_eq!(looped.cancel.shutdown_code(), None);
    assert_eq!(jobs.stop_delegates_calls(), 2);
}

#[track_caller]
fn ran(finished: &mpsc::Receiver<Result<(), crate::Error>>, wait: &Deadline) {
    let result = wait.recv(finished).expect("run returned");
    assert!(result.is_ok(), "{result:?}");
}

/// A whole session's durable kinds: its opening lines, then `turns`.
fn session_of(turns: &[Vec<&str>]) -> Vec<String> {
    ["session_started", "preamble_built", "opening_message"]
        .into_iter()
        .chain(turns.iter().flatten().copied())
        .map(str::to_owned)
        .collect()
}

fn durable_kinds(world: &World) -> Vec<String> {
    log::read(&world.dir)
        .unwrap()
        .into_iter()
        .filter(Envelope::is_durable)
        .map(|line| line.kind)
        .collect()
}

#[test]
fn close_with_a_job_running_gives_the_ending_notice_then_waits_for_the_job() {
    // `docs/tools.md`, "Background jobs": the session waits for running job ends.
    let world = World::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("Waiting."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();

    let first = world.next_turn();
    assert_eq!(kinds(&first), one_step_with(&[]));

    let ending = world.next_turn();
    assert_eq!(kinds(&ending), one_step_with(&["jobs_pending_notified"]));
    // The notice is a message from Fiber, with no `command_id`.
    assert_eq!(
        ending[0].payload["input"],
        json!([{
            "type": "message",
            "source": "fiber",
            "content": [{"type": "text", "text": ending_text(&[&id])}],
        }])
    );
    assert_eq!(
        Value::Object(ending[2].payload.clone()),
        json!({"job_ids": [id], "reason": "ending"})
    );
    assert_eq!(ending[2].turn_id, ending[0].turn_id);
    assert_eq!(ending[2].action_id, None);
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    // The request carries the logged message once: `jobs_pending_notified`
    // renders nothing.
    let notice = Input::User {
        text: ending_text(&[&id]),
        images: Vec::new(),
    };
    assert_eq!(requests[1].conversation.last(), Some(&notice));
    assert_eq!(
        requests[1]
            .conversation
            .iter()
            .filter(|input| **input == notice)
            .count(),
        1
    );

    // The loop is still there: the job's end starts a turn.
    (job.end.0)(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(
        last[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [id]}])
    );
    assert_notice(&last[2], &id);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    let requests = world.requests();
    assert_eq!(requests.len(), 3);
    // A resume renders the notice from the log as the loop sent it.
    let rebuilt =
        crate::conversation::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
    let sent = &requests[2].conversation;
    assert_eq!(rebuilt[..sent.len()], sent[..]);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn close_with_no_job_running_exits_with_no_notice() {
    // `docs/tools.md`, "Background jobs": the session ends when no job runs.
    let world = World::new(vec![Scripted::text("Done.")], Vec::new(), |_| Vec::new());
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    (job.end.0)(failed(&job.started.job_id.0));
    // The end before the loop's inbox was wired sends nothing.
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(durable_kinds(&world), session_of(&[one_step_with(&[])]));
    assert_eq!(world.requests().len(), 1);
}

#[test]
fn the_ending_notice_is_given_once_even_when_another_job_starts() {
    // `docs/tools.md`, "Background jobs": each running job end wakes the model.
    let world = World::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("Waiting."),
            Scripted::text("One."),
            Scripted::text("Two."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let first_job = open_job(&jobs);
    let first_id = first_job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let _first = world.next_turn();
    let ending = world.next_turn();
    assert_eq!(ending[2].payload["job_ids"], json!([first_id]));

    let second_job = open_job(&jobs);
    let second_id = second_job.started.job_id.0.clone();
    (first_job.end.0)(failed(&first_id));
    let one = world.next_turn();
    assert_eq!(kinds(&one), one_step_with(&["job_completed"]));
    assert_notice(&one[2], &first_id);

    (second_job.end.0)(failed(&second_id));
    let two = world.next_turn();
    assert_eq!(kinds(&two), one_step_with(&["job_completed"]));
    assert_notice(&two[2], &second_id);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn after_close_a_prompt_is_rejected_and_a_steer_does_not_join_a_job_turn() {
    let world = World::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("Waiting."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = Arc::new(Listed::default());
    jobs.set(&[JOB]);
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let _first = world.next_turn();
    let _ending = world.next_turn();
    let (prompted, prompt_answer) = mpsc::channel();
    let (steered, steer_answer) = mpsc::channel();
    // The end and both commands are waiting together at the next read.
    *jobs.racing.lock().unwrap() = Some((
        world.inbox.clone(),
        vec![
            held(JOB),
            Delivery::Prompt(message("Again.", "c_again"), reported(prompted)),
            Delivery::Steer(message("And.", "c_and"), reported(steered)),
        ],
    ));
    world.send(Delivery::Cancelled);
    let last = world.next_turn();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&prompt_answer),
        Ok(false)
    );
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&steer_answer),
        Ok(false)
    );
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(
        last[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

// Idle with jobs running (`docs/invocation.md`, "Lifecycle").

const IDLE: Duration = Duration::from_secs(60);

/// The jobs check's text, naming `ids`.
fn check_text(ids: &[&str]) -> String {
    format!(
        "Fiber: no one has prompted this session for a while, and these background jobs are still running: {}. Read each job's output file, and stop with `jobs stop` any that look hung or that you no longer need.",
        ids.join(", ")
    )
}

/// The input of a turn the jobs check or the ending notice starts: one
/// message from Fiber.
fn fiber_input(text: &str) -> Value {
    json!([{
        "type": "message",
        "source": "fiber",
        "content": [{"type": "text", "text": text}],
    }])
}

/// The check turn in `lines` names `id`: a Fiber message as its input,
/// then `jobs_pending_notified` with `reason` `unattended` after its first
/// `step_started`.
fn assert_check(lines: &[Envelope], id: &str) {
    assert_eq!(lines[0].payload["input"], fiber_input(&check_text(&[id])));
    assert_eq!(lines[2].kind, "jobs_pending_notified");
    assert_eq!(
        Value::Object(lines[2].payload.clone()),
        json!({"job_ids": [id], "reason": "unattended"})
    );
    assert_eq!(lines[2].turn_id, lines[0].turn_id);
    assert_eq!(lines[2].action_id, None);
}

#[test]
fn the_idle_delay_with_a_job_running_gives_the_jobs_check_once_and_never_exits() {
    // `docs/invocation.md`, "Lifecycle": an idle session with running jobs checks again.
    let world = World::new(
        vec![Scripted::text("Checked."), Scripted::text("Seen.")],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    // Unattended since the wait began, with a job running: the check is
    // due one delay later.
    let m1 = world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let check = world.next_turn();
    assert_eq!(kinds(&check), one_step_with(&["jobs_pending_notified"]));
    assert_check(&check, &id);
    // The request carries the logged message once; the line renders
    // nothing.
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    let text = check_text(&[&id]);
    assert_eq!(users(&requests[0]).last(), Some(&text));
    assert_eq!(
        users(&requests[0])
            .iter()
            .filter(|user| **user == text)
            .count(),
        1
    );
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "after the check the wait has no deadline while the job runs: {:?}",
        clock.parked()
    );
    // Another two delays with no prompt: no second check, and the session
    // does not end while the job runs. A wake makes the wait look again.
    let m2 = world.advance_parked(None, IDLE + IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        clock.await_parked_since(&m2, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the wake finds no second check and no exit: {:?}",
        clock.parked()
    );
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    (job.end.0)(failed(&id));
    let turn = world.next_turn();
    assert_eq!(kinds(&turn), one_step_with(&["job_completed"]));
    // The idle delay starts once the job's turn is over and no job runs.
    let restarted = origin + IDLE + IDLE + IDLE + IDLE;
    world.advance_parked(Some(restarted), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 2);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn a_prompt_arms_the_jobs_check_again_from_when_it_was_taken() {
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("Hi."),
            Scripted::text("Checked again."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    let m1 = world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "after the check the wait has no deadline: {:?}",
        clock.parked()
    );
    // Half a delay later a prompt arrives: the clock restarts from it.
    world.advance_parked(None, IDLE / 2);
    world.send(Delivery::Cancelled);
    world.send(prompt("Hello."));
    let prompted = world.next_turn();
    assert_eq!(kinds(&prompted), one_step_with(&[]));
    let rearmed = origin + IDLE + IDLE / 2 + IDLE;
    world.advance_parked(Some(rearmed), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.advance_parked(Some(rearmed + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn a_steer_in_the_check_turn_arms_the_check_again() {
    // `docs/invocation.md`, "Lifecycle": a turn re-arms the unattended check.
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("Noted."),
            Scripted::text("Checked again."),
            Scripted::text("Seen."),
        ],
        vec![vec![steer("Still here.")]],
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let check = world.next_turn();
    let mut steered = one_step_with(&["jobs_pending_notified"]);
    steered.pop();
    steered.extend(["step_started", "steering_applied"]);
    steered.extend(REPLY);
    assert_eq!(kinds(&check), steered);
    assert_check(&check, &id);
    // The steer was taken as the check came due: the next is one delay on.
    world.advance_parked(Some(origin + IDLE + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.advance_parked(Some(origin + IDLE * 3), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 4);
}

#[test]
fn a_steer_while_idle_arms_the_check_again() {
    // `docs/invocation.md`, "Lifecycle": a turn re-arms the unattended check.
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("Noted."),
            Scripted::text("Checked again."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    world.send(steer("Still here."));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&[]));
    world.advance_parked(Some(origin + IDLE + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.advance_parked(Some(origin + IDLE * 3), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 4);
}

#[test]
fn a_session_message_while_idle_does_not_arm_the_check_again() {
    // `docs/invocation.md`, "Lifecycle": a session message does not re-arm the check.
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("Noted."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    // A session message is neither a prompt nor a steer, so it leaves the
    // check given (`docs/invocation.md`, "Lifecycle").
    world.send(session_message("c_message", None, ignore()));
    let turn = world.next_turn();
    assert_eq!(kinds(&turn), one_step_with(&[]));
    assert_eq!(turn[0].payload["input"], message_input("c_message"));
    // Past another delay no second check comes due: the wait holds no
    // deadline while the job runs.
    let m1 = world.advance_parked(None, IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "a session message leaves the check given: {:?}",
        clock.parked()
    );
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    // The final wait's deadline is set when it parks: await it before
    // moving the clock, or a wake taken early parks again with no later
    // wake and `run` never returns.
    world.advance_parked(Some(origin + IDLE * 3), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 3);
    let requests = world.requests();
    assert_eq!(users(&requests[1]).last(), Some(&FRAMED.to_owned()));
}

#[test]
fn a_session_message_in_the_check_turn_does_not_arm_the_check_again() {
    // `docs/invocation.md`, "Lifecycle": a session message does not re-arm the check.
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("Noted."),
            Scripted::text("Seen."),
        ],
        vec![vec![session_message("c_message", None, ignore())]],
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    // The message joins the check turn at its next step boundary, applied
    // without arming the check again.
    let check = world.next_turn();
    let mut steered = one_step_with(&["jobs_pending_notified"]);
    steered.pop();
    steered.extend(["step_started", "steering_applied"]);
    steered.extend(REPLY);
    assert_eq!(kinds(&check), steered);
    assert_check(&check, &id);
    let applied = check
        .iter()
        .find(|line| line.kind == "steering_applied")
        .expect("the message applies");
    assert_eq!(applied.payload["source"], "session");
    assert_eq!(applied.payload["command_id"], "c_message");
    assert_eq!(applied.payload["content"][0]["text"], json!(FRAMED));
    // Past another delay no second check comes due.
    let m1 = world.advance_parked(None, IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "a session message leaves the check given: {:?}",
        clock.parked()
    );
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    // The final wait's deadline is set when it parks: await it before
    // moving the clock, or a wake taken early parks again with no later
    // wake and `run` never returns.
    world.advance_parked(Some(origin + IDLE * 3), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 3);
}

#[test]
fn a_jobs_end_does_not_arm_the_check_again() {
    let world = World::new(
        vec![
            Scripted::text("Checked."),
            Scripted::text("One."),
            Scripted::text("Two."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let first = open_job(&jobs);
    let second = open_job(&jobs);
    let one = first.started.job_id.0.clone();
    let two = second.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    let m1 = world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let check = world.next_turn();
    assert_eq!(
        check[0].payload["input"],
        fiber_input(&check_text(&[&one, &two]))
    );
    assert_eq!(
        Value::Object(check[2].payload.clone()),
        json!({"job_ids": [one, two], "reason": "unattended"})
    );
    // One job ends; its turn does not arm the check, so the other running
    // past another delay gives none.
    (first.end.0)(failed(&one));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "a job's end leaves the check unarmed: {:?}",
        clock.parked()
    );
    let m2 = world.advance_parked(None, IDLE + IDLE);
    world.send(Delivery::Cancelled);
    assert!(
        clock.await_parked_since(&m2, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "two delays on, no check comes due: {:?}",
        clock.parked()
    );
    (second.end.0)(failed(&two));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.advance_parked(Some(origin + IDLE * 4), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn with_jobs_given_and_none_running_the_idle_delay_exits() {
    let world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    let jobs = FakeJobs::new(world.home());
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    world.advance_parked(Some(origin + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert!(world.requests().is_empty());
    assert_eq!(durable_kinds(&world), ["session_started"]);
}

#[test]
fn with_no_idle_delay_a_job_running_gets_no_check() {
    let world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone());
    let clock = Arc::clone(&world.clock);
    let finished = world.spawn_run();
    let m1 = world.advance_parked(None, IDLE * 10);
    world.send(Delivery::Cancelled);
    assert!(
        clock.await_parked_since(&m1, None, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "ten delays on, no check comes due: {:?}",
        clock.parked()
    );
    (job.end.0)(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.send(Delivery::Close(ignore()));
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[one_step_with(&["job_completed"])])
    );
}

#[test]
fn a_steer_arriving_as_the_check_turn_starts_follows_jobs_pending_notified() {
    let world = World::new(vec![Scripted::text("Checked.")], Vec::new(), |_| Vec::new());
    let jobs = Arc::new(Listed::default());
    jobs.set(&[JOB]);
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    // The steer lands in the inbox as the wait reads the jobs past the
    // delay, after its last look at the inbox: the check turn's first step
    // drains it.
    let inbox = world.inbox.clone();
    let hook_jobs = Arc::clone(&jobs);
    clock.advance_when_parked_then(
        Some(origin + IDLE),
        world.runner,
        IDLE,
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            *hook_jobs.arriving.lock().unwrap() = Some((inbox, vec![steer("Still here.")]));
        },
    );
    world.send(Delivery::Cancelled);
    let lines = world.watched();
    let from = lines
        .iter()
        .position(|line| line.kind == "turn_started")
        .expect("a turn started");
    assert_eq!(
        kinds(&lines[from..]),
        [
            "turn_started",
            "step_started",
            "jobs_pending_notified",
            "steering_queue",
            "steering_queue",
            "steering_applied",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        lines[from + 2].payload,
        json!({"job_ids": [JOB], "reason": "unattended"})
            .as_object()
            .unwrap()
            .clone()
    );
    jobs.set(&[]);
    world.send(Delivery::Close(ignore()));
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
}

#[test]
fn a_job_listed_while_idle_holds_the_deadline_gets_the_check_and_its_end_restarts_it() {
    // `docs/invocation.md`, "Lifecycle": an unattended running job gets one check.
    let world = World::new(vec![Scripted::text("Checked.")], Vec::new(), |_| Vec::new());
    let jobs = Arc::new(Listed::default());
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    let (reads, read) = mpsc::channel();
    let hook_jobs = Arc::clone(&jobs);
    clock.advance_when_parked_then(
        Some(origin + IDLE),
        world.runner,
        IDLE,
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            *hook_jobs.reads.lock().unwrap() = Some(reads);
            hook_jobs.set(&[JOB]);
        },
    );
    world.send(Delivery::Cancelled);
    // The wait read the job running past the deadline, and did not end:
    // the session was unattended that long, so the check is given.
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    fakes::within(
        "the wait to read the jobs",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            while wait.recv(&read).expect("the wait read the jobs") == 0 {}
        },
    );
    assert_check(&world.next_turn(), JOB);
    // The job ends with its final state already claimed: no turn starts,
    // and the delay counts from when the wait saw no job running.
    jobs.set(&[]);
    world.send(notice(JOB, false, &Asked::default()));
    assert!(
        clock.await_parked(
            origin + IDLE + IDLE,
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "{:?}",
        clock.parked()
    );
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    world.advance_parked(Some(origin + IDLE + IDLE), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 1);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[one_step_with(&["jobs_pending_notified"])])
    );
}

#[test]
fn an_approval_wait_with_a_job_running_outlasts_the_idle_deadline() {
    let world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let (asked, request) = mpsc::channel();
    let watcher = on_request(&world.log, move |request_id| {
        asked.send(request_id).unwrap();
    });
    world.send(prompt("go"));
    let finished = world.spawn_run();
    let _request_id = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&request)
        .expect("the call asked");
    watcher.join().unwrap();
    world.advance_parked(None, IDLE + Duration::from_secs(1));
    world.send(Delivery::Cancelled);
    let (rejected, answer) = mpsc::channel();
    world.send(Delivery::Reply(
        Reply {
            request_id: RequestId("r_absent".into()),
            answer: ReplyAnswer::Approval {
                decision: Decision::Deny,
                feedback: None,
                remember: None,
            },
        },
        reported(rejected),
    ));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(false)
    );
    // The job's end waits for the next step; the delay restarts from it.
    (job.end.0)(failed(&id));
    let restarted = origin + IDLE + Duration::from_secs(1) + IDLE;
    assert!(
        clock.await_parked(restarted, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "{:?}",
        clock.parked()
    );
    world.advance_parked(Some(restarted), IDLE);
    world.send(Delivery::Cancelled);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    // The idle exit writes nothing for the pending call.
    assert_eq!(
        durable_kinds(&world),
        session_of(&[vec![
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
        ]])
    );
}

#[test]
fn a_job_that_ends_as_the_loop_checks_still_starts_its_turn() {
    let world = World::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("Waiting."),
            Scripted::text("Seen."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = Arc::new(Listed::default());
    jobs.set(&[JOB]);
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let _first = world.next_turn();
    let ending = world.next_turn();
    assert_eq!(ending[2].payload["job_ids"], json!([JOB]));
    // The end is in the inbox by the time the list no longer names it.
    *jobs.racing.lock().unwrap() = Some((world.inbox.clone(), vec![held(JOB)]));
    world.send(Delivery::Cancelled);
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_notice(&last[2], JOB);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn after_close_a_kept_notice_starts_a_turn_and_a_kept_steer_does_not() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let jobs = Arc::new(Listed::default());
    world = world.with_jobs(jobs);
    {
        // What a turn cut short before `close` left queued.
        let looped = world.looped.as_mut().unwrap();
        looped.closing = true;
        looped.queued.push_back(crate::jobs::Queued::Steer(
            message("Kept.", "c_kept"),
            crate::before_message::Hooked::default(),
        ));
        looped
            .queued
            .push_back(crate::jobs::Queued::Job(Vec::new(), failed(JOB), None));
    }
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    assert_notice(&lines[2], JOB);
    assert!(!users(&world.requests()[0]).contains(&"Kept.".to_owned()));
    assert_eq!(world.turn(), None);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[one_step_with(&["job_completed"])])
    );
}

#[test]
fn a_steer_in_a_turn_started_after_close_is_rejected_closing() {
    // `docs/loop.md`, "One step": steering joins only while the turn is open.
    let (during_ending, ending_answer) = mpsc::channel();
    let (during_job, job_answer) = mpsc::channel();
    let world = World::new(
        vec![
            Scripted::text("Done."),
            Scripted::text("Waiting."),
            Scripted::text("Seen."),
        ],
        vec![
            Vec::new(),
            vec![Delivery::Steer(
                message("During the notice.", "c_one"),
                reported(during_ending),
            )],
            vec![Delivery::Steer(
                message("During the job's turn.", "c_two"),
                reported(during_job),
            )],
        ],
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let first = world.next_turn();
    assert_eq!(kinds(&first), one_step_with(&[]));
    let ending = world.next_turn();
    assert_eq!(kinds(&ending), one_step_with(&["jobs_pending_notified"]));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&ending_answer),
        Ok(false)
    );
    (job.end.0)(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&job_answer),
        Ok(false)
    );
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 3);
    assert_eq!(
        durable_kinds(&world),
        session_of(&[
            one_step_with(&[]),
            one_step_with(&["jobs_pending_notified"]),
            one_step_with(&["job_completed"]),
        ])
    );
}

#[test]
fn the_turn_in_flight_when_close_arrives_still_takes_a_steer() {
    // `docs/loop.md`, "One step": steering joins at the next boundary.
    let (steered, answer) = mpsc::channel();
    let world = World::new(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![vec![
            Delivery::Close(ignore()),
            Delivery::Steer(message("And this.", "c_and"), reported(steered)),
        ]],
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    let finished = world.spawn_run();
    let turn = world.next_turn();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&answer),
        Ok(true)
    );
    let mut expected = vec![
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "step_started",
        "steering_applied",
    ];
    expected.extend_from_slice(&REPLY);
    assert_eq!(kinds(&turn), expected);
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    assert_eq!(world.requests().len(), 2);
    assert_eq!(durable_kinds(&world), session_of(&[expected]));
}

#[test]
fn a_session_message_taken_with_close_in_one_idle_batch_runs_its_turn() {
    // `docs/loop.md`, "Starting a turn" and "Ending a turn": input starts a turn
    // before close ends the run.
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(session_message("c_message", None, ignore()));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    // The message starts the turn the close ends the run after.
    assert_eq!(durable_kinds(&world), session_of(&[one_step_with(&[])]));
    let lines = world.turn_lines();
    assert_eq!(lines[0].payload["input"], message_input("c_message"));
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(users(&requests[0]).last(), Some(&FRAMED.to_owned()));
}

#[test]
fn a_session_message_queued_before_close_is_applied_by_the_turn_in_flight() {
    // `docs/loop.md`, "One step": inbox messages are applied in arrival order.
    let world = World::new(
        vec![Scripted::text("Hi."), Scripted::text("Steered.")],
        vec![vec![
            session_message("c_message", None, ignore()),
            Delivery::Close(ignore()),
        ]],
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    let finished = world.spawn_run();
    let turn = world.next_turn();
    let mut expected = vec![
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "step_started",
        "steering_applied",
    ];
    expected.extend_from_slice(&REPLY);
    assert_eq!(kinds(&turn), expected);
    let applied = turn
        .iter()
        .find(|line| line.kind == "steering_applied")
        .expect("the message applies");
    assert_eq!(applied.payload["source"], "session");
    assert_eq!(applied.payload["command_id"], "c_message");
    assert_eq!(applied.payload["content"][0]["text"], json!(FRAMED));
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
    let requests = world.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(users(&requests[1]).last(), Some(&FRAMED.to_owned()));
    assert_eq!(durable_kinds(&world), session_of(&[expected]));
}

// Monitors: a batch of lines wakes the model as a job's end does.

/// A monitor's batch for `id`.
fn line(id: &str, lines: &str, suppressed: Option<u64>) -> contract::events::JobLine {
    contract::events::JobLine {
        job_id: JobId(id.into()),
        lines: lines.into(),
        suppressed,

        changed_by: None,
    }
}

fn batch(id: &str, lines: &str, suppressed: Option<u64>) -> Delivery {
    Delivery::JobLine(line(id, lines, suppressed))
}

const SUPPRESSED: &str = "3 earlier deliveries were suppressed by the rate limit; \
restart the monitor with a more selective filter if you need them.";

#[test]
fn a_job_line_reads_as_the_monitors_batch_and_any_suppressed_count() {
    assert_eq!(
        line_text(&line(JOB, "build ok\ntests ok", None)),
        format!("Fiber: monitor {JOB} printed:\nbuild ok\ntests ok")
    );
    assert_eq!(
        line_text(&line(JOB, "build ok", Some(3))),
        format!("Fiber: monitor {JOB} printed:\nbuild ok\n{SUPPRESSED}")
    );
}

#[test]
fn a_batch_while_idle_starts_a_turn_named_by_its_monitor() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(batch(JOB, "build ok\ntests ok", Some(3)));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_line"]));
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    assert_eq!(lines[2].action_id, None);
    assert_eq!(lines[2].turn_id, lines[0].turn_id);
    assert_eq!(
        Value::Object(lines[2].payload.clone()),
        json!({"job_id": JOB, "lines": "build ok\ntests ok", "suppressed": 3})
    );
    let requests = world.requests();
    assert_eq!(
        requests[0].conversation.last(),
        Some(&Input::User {
            text: format!("Fiber: monitor {JOB} printed:\nbuild ok\ntests ok\n{SUPPRESSED}"),
            images: Vec::new()
        })
    );
    // A resume renders the batch from the log as the loop sent it.
    let rebuilt =
        crate::conversation::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
    let sent = &requests[0].conversation;
    assert_eq!(rebuilt[..sent.len()], sent[..]);
}

#[test]
fn a_monitors_batches_and_its_end_are_one_jobs_item_naming_it_once() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(batch(JOB, "one", None));
    world.send(batch(OTHER, "two", None));
    world.send(batch(JOB, "three", None));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["job_line", "job_line", "job_line", "job_completed"])
    );
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB, OTHER]}])
    );
    let texts: Vec<&str> = lines[2..5]
        .iter()
        .map(|line| line.payload["lines"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["one", "two", "three"]);
    assert_notice(&lines[5], JOB);
    let users = users(&world.requests()[0]);
    assert_eq!(
        users[users.len() - 4..],
        [
            format!("Fiber: monitor {JOB} printed:\none"),
            format!("Fiber: monitor {OTHER} printed:\ntwo"),
            format!("Fiber: monitor {JOB} printed:\nthree"),
            rendered(JOB),
        ]
    );
    let rebuilt =
        crate::conversation::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
    let sent = &world.requests()[0].conversation;
    assert_eq!(rebuilt[..sent.len()], sent[..]);
}

#[test]
fn a_message_between_batches_splits_the_jobs_items() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(batch(JOB, "one", None));
    world.send(steer("And this."));
    world.send(batch(JOB, "two", None));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let input = lines[0].payload["input"].as_array().unwrap().clone();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[2], json!({"type": "jobs", "job_ids": [JOB]}));
}

#[test]
fn a_batch_while_a_call_runs_is_written_at_the_next_step_boundary() {
    let mut world = World::new(
        vec![calls("sends"), Scripted::text("Seen.")],
        Vec::new(),
        |inbox| {
            sending(
                inbox,
                vec![batch(JOB, "ready", None), steer("Also."), held(JOB)],
            )
        },
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        two_steps_with(&RAN, &["job_line", "steering_applied", "job_completed"])
    );
    assert_eq!(lines[9].payload["lines"], "ready");
    assert_eq!(lines[9].action_id, None);
    let requests = world.requests();
    assert!(
        !users(&requests[0])
            .iter()
            .any(|text| text.contains("printed:"))
    );
    let users = users(&requests[1]);
    assert!(
        users.contains(&format!("Fiber: monitor {JOB} printed:\nready")),
        "{users:?}"
    );
    let rebuilt =
        crate::conversation::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
    let sent = &requests[1].conversation;
    assert_eq!(rebuilt[..sent.len()], sent[..]);
}

#[test]
fn a_batch_after_close_with_no_job_running_starts_no_turn() {
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    world.send(Delivery::Close(ignore()));
    world.send(batch(JOB, "late", None));
    assert_eq!(world.turn(), None);
    assert!(world.requests().is_empty());
}

#[test]
fn a_batch_after_close_while_its_job_runs_starts_a_turn() {
    let world = World::new(
        vec![
            Scripted::text("Waiting."),
            Scripted::text("Seen."),
            Scripted::text("Done."),
        ],
        Vec::new(),
        |_| Vec::new(),
    );
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    let id = job.started.job_id.0.clone();
    let mut world = world.with_jobs(jobs.clone());
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    let ending = world.next_turn();
    assert_eq!(kinds(&ending), one_step_with(&["jobs_pending_notified"]));
    world.send(batch(&id, "still going", None));
    let woken = world.next_turn();
    assert_eq!(kinds(&woken), one_step_with(&["job_line"]));
    assert_eq!(woken[2].payload["lines"], "still going");
    (job.end.0)(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    ran(&finished, &wait);
}

/// `id`'s end as a delegate notice whose claim answers `holds` and counts
/// its asks in `asked`.
fn counted_delegate(id: &str, text: &str, holds: bool, asked: &Asked) -> Delivery {
    let asked = Arc::clone(&asked.0);
    Delivery::Job(JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
            holds.then(Vec::new)
        })),
        delegate: Some(delegate_finished(id, text.into())),
    })
}

#[test]
fn admit_job_queues_the_notice_without_asking_its_claim() {
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    world
        .looped
        .as_mut()
        .unwrap()
        .admit_job(
            counted_job(DELEGATE, true, &asked),
            &TurnId("t_test".into()),
        )
        .unwrap();
    assert_eq!(asked.count(), 0, "admission asks nothing");
}

/// `id`'s end as a notice whose claim answers `holds` and counts its asks
/// in `asked`.
fn counted_job(id: &str, holds: bool, asked: &Asked) -> JobNotice {
    let asked = Arc::clone(&asked.0);
    JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
            holds.then(Vec::new)
        })),
        delegate: None,
    }
}

#[test]
fn a_notice_admitted_during_an_approval_wait_is_claimed_once_at_the_next_boundary() {
    // `docs/loop.md`, "One step": the notice joins at the next boundary.
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let asked = Asked::default();
    let (noticed_tx, noticed_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        let counted = asked.clone();
        move |id| {
            inbox
                .send(counted_delegate(DELEGATE, "Done.", true, &counted))
                .unwrap();
            noticed_tx.send(()).unwrap();
            // The approval still waits: the write, and its claim, are
            // still ahead.
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&release_rx)
                .expect("the test releases the approval");
            inbox.send(allow(id)).unwrap();
        }
    });
    world.send(prompt("go"));
    let rx = world.spawn();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&noticed_rx)
        .expect("the notice went out");
    assert_eq!(asked.count(), 0, "no claim before the next step boundary");
    release_tx.send(()).unwrap();
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    assert_eq!(world.join(&rx, &wait), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(asked.count(), 1, "the claim is asked once, at the write");
    let lines = world.turn_lines();
    let called = [
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
    ];
    assert_eq!(
        kinds(&lines),
        two_steps_with(&called, &["delegate_finished", "job_completed"])
    );
    let finished = lines
        .iter()
        .find(|line| line.kind == "delegate_finished")
        .expect("a delegate_finished line");
    assert_eq!(finished.payload["job_id"], DELEGATE);
    assert_eq!(finished.payload["text"], "Done.");
    assert_eq!(finished.turn_id, lines[0].turn_id);
    let completed = lines
        .iter()
        .find(|line| line.kind == "job_completed")
        .expect("a job_completed line");
    assert_notice(completed, DELEGATE);
}

#[test]
fn a_notice_refused_at_the_write_writes_nothing() {
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let asked = Asked::default();
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        let counted = asked.clone();
        move |id| {
            inbox
                .send(counted_delegate(DELEGATE, "Done.", false, &counted))
                .unwrap();
            inbox.send(allow(id)).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(asked.count(), 1, "the claim is asked once, at the write");
    let lines = world.turn_lines();
    let called = [
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
    ];
    assert_eq!(kinds(&lines), two_steps_with(&called, &[]));
}

#[test]
fn a_refused_notice_kept_by_a_cancelled_turn_starts_no_turn() {
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let asked = Asked::default();
    let cancel = Arc::clone(&world.looped.as_ref().unwrap().cancel);
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        let counted = asked.clone();
        move |_| {
            inbox.send(notice(JOB, false, &counted)).unwrap();
            assert!(cancel.cancel());
            inbox.send(Delivery::Cancelled).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Interrupted));
    answered.join().unwrap();
    assert_eq!(asked.count(), 0, "a cancelled turn claims nothing it kept");
    let cut = world.turn_lines();
    assert!(!kinds(&cut).contains(&"job_completed"), "{:?}", kinds(&cut));
    // The next turn takes the leftover notice, refuses it, and starts
    // nothing on it.
    world.send(Delivery::Close(ignore()));
    assert_eq!(world.turn(), None);
    assert_eq!(asked.count(), 1, "the take claims the kept notice once");
}

#[test]
fn a_restarted_job_is_owed_another_orphaned_end() {
    use contract::events::{Event, JobStarted};
    let started = || {
        Event::JobStarted(JobStarted {
            job_id: JobId(JOB.into()),
            tool: None,
            extension: None,
            description: "a job".into(),
            output_path: "jobs/j_5e10c0ffee123456.jsonl".into(),
        })
    };
    let mut orphans = super::Orphans::default();
    orphans.fold(&started());
    orphans.fold(&Event::JobCompleted(failed(JOB)));
    // A `job_started` reopens its job: the run it starts is owed its own
    // orphaned end.
    orphans.fold(&started());
    let done = orphans.finish();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].job_id.0, JOB);
    assert_eq!(done[0].error.as_ref().unwrap().code, ErrorCode::Orphaned);
}

#[test]
fn a_job_completed_after_its_restart_owes_nothing() {
    use contract::events::{Event, JobStarted};
    let started = || {
        Event::JobStarted(JobStarted {
            job_id: JobId(JOB.into()),
            tool: None,
            extension: None,
            description: "a job".into(),
            output_path: "jobs/j_5e10c0ffee123456.jsonl".into(),
        })
    };
    let mut orphans = super::Orphans::default();
    orphans.fold(&started());
    orphans.fold(&Event::JobCompleted(failed(JOB)));
    orphans.fold(&started());
    orphans.fold(&Event::JobCompleted(failed(JOB)));
    assert!(orphans.finish().is_empty());
}

#[test]
fn a_rewound_line_settles_no_job() {
    use contract::events::{Event, JobStarted, Rewound};
    let started = Event::JobStarted(JobStarted {
        job_id: JobId(JOB.into()),
        tool: None,
        extension: None,
        description: "a job".into(),
        output_path: "jobs/j_5e10c0ffee123456.jsonl".into(),
    });
    let rewound = Event::Rewound(Rewound {
        new_session_id: SessionId("s_0000000000000001".into()),
        seq: Seq(3),
        from_session_id: None,
        summary: None,
    });
    // A `rewound` settles no job: the job a rewind stopped has its own
    // `job_completed` before it, so the open job is still owed its
    // orphaned end.
    let mut orphans = super::Orphans::default();
    orphans.fold(&started);
    orphans.fold(&rewound);
    let done = orphans.finish();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].job_id.0, JOB);
    assert_eq!(done[0].error.as_ref().unwrap().code, ErrorCode::Orphaned);
}

#[test]
fn admit_idle_claims_a_notice_at_admission() {
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    let looped = world.looped.as_mut().unwrap();
    // A refused claim starts no turn on the notice.
    let refused = Asked::default();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .admit_idle(notice(JOB, false, &refused), &mut input)
        .unwrap();
    assert!(input.pieces.is_empty(), "a refused claim starts no turn");
    assert_eq!(refused.count(), 1, "the claim is asked at admission");
    // A held claim joins the turn's input as the job's end.
    let held = Asked::default();
    looped
        .admit_idle(notice(JOB, true, &held), &mut input)
        .unwrap();
    assert!(
        matches!(input.pieces.as_slice(), [super::Queued::Job(..)]),
        "a held claim starts one turn with the notice"
    );
    assert_eq!(held.count(), 1, "the claim is asked at admission");
}

/// A resumed run's start records, queued under `action`: the action of a
/// call the loop never started, so the writer's turn applies.
fn resumed_ahead(id: &str, action: &ActionId) -> Vec<Taken> {
    vec![
        Taken {
            record: contract::jobs::JobRecord::Started(JobStarted {
                job_id: JobId(id.into()),
                tool: Some("delegate_message".into()),
                extension: None,
                description: "resumed".into(),
                output_path: format!("artifacts/{id}.log"),
            }),
            action: Some(action.clone()),
        },
        Taken {
            record: contract::jobs::JobRecord::DelegateStarted(DelegateStarted {
                job_id: JobId(id.into()),
                delegate_session_id: SessionId("s_d000000000000001".into()),
                harness: "fiber".into(),
                model: "fiber:fake/m".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            action: Some(action.clone()),
        },
    ]
}

/// `id`'s end as a delegate notice whose claim took `ahead` before the
/// notice's own finish and completion.
fn ahead_notice(id: &str, text: &str, ahead: Vec<Taken>, asked: &Asked) -> JobNotice {
    let asked = Arc::clone(&asked.0);
    JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
            Some(ahead.clone())
        })),
        delegate: Some(delegate_finished(id, text.into())),
    }
}

/// [`ahead_notice`] as a delivery.
fn ahead_delegate(id: &str, text: &str, ahead: Vec<Taken>, asked: &Asked) -> Delivery {
    Delivery::Job(ahead_notice(id, text, ahead, asked))
}

/// The four lines a notice whose claim took a resumed run's start writes:
/// the two starts under `action`, then the finish and the completion with
/// no action, all under the turn that wrote them.
fn assert_ahead_lines(lines: &[Envelope], id: &str, action: &ActionId, at: usize) {
    let four = &lines[at..at + 4];
    assert_eq!(
        [
            four[0].kind.as_str(),
            four[1].kind.as_str(),
            four[2].kind.as_str(),
            four[3].kind.as_str()
        ],
        [
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed"
        ]
    );
    assert_eq!(four[0].action_id.as_ref(), Some(action));
    assert_eq!(four[1].action_id.as_ref(), Some(action));
    assert_eq!(four[2].action_id, None);
    assert_eq!(four[3].action_id, None);
    let turn = four[0].turn_id.clone().unwrap();
    for line in &four[1..] {
        assert_eq!(line.turn_id.as_ref(), Some(&turn));
    }
    assert_eq!(four[0].payload["job_id"], id);
    assert_eq!(four[1].payload["job_id"], id);
    assert_eq!(four[2].payload["job_id"], id);
    assert_eq!(four[3].payload["job_id"], id);
}

#[test]
fn a_notice_claims_write_what_it_took_ahead_at_the_next_boundary() {
    let mut world = World::new(
        vec![calls("gated"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Gated)],
    );
    let asked = Asked::default();
    let action = ActionId("a_aaaaaaaaaaaaaaaa".into());
    let ahead = resumed_ahead(DELEGATE, &action);
    let answered = on_request(&world.log, {
        let inbox = world.inbox.clone();
        let counted = asked.clone();
        move |id| {
            inbox
                .send(ahead_delegate(DELEGATE, "Done.", ahead.clone(), &counted))
                .unwrap();
            inbox.send(allow(id)).unwrap();
        }
    });
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(asked.count(), 1, "the claim is asked once, at the write");
    let lines = world.turn_lines();
    let called = [
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
    ];
    assert_eq!(
        kinds(&lines),
        two_steps_with(
            &called,
            &[
                "job_started",
                "delegate_started",
                "delegate_finished",
                "job_completed"
            ]
        )
    );
    let at = lines
        .iter()
        .position(|line| line.kind == "job_started")
        .expect("a job_started line");
    assert_ahead_lines(&lines, DELEGATE, &action, at);
}

#[test]
fn a_notice_kept_for_the_next_turn_writes_what_its_claim_took_ahead() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    let action = ActionId("a_aaaaaaaaaaaaaaaa".into());
    // Taken while no turn runs: the next turn moves it into its input
    // through `take_queued`, which claims it at the move.
    world
        .looped
        .as_mut()
        .unwrap()
        .admit_job(
            ahead_notice(DELEGATE, "Done.", resumed_ahead(DELEGATE, &action), &asked),
            &TurnId("t_test".into()),
        )
        .unwrap();
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(asked.count(), 1, "the claim is asked once, at the move");
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&[
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed"
        ])
    );
    // The turn's input still names the job once, with its message after.
    let input = lines[0].payload["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [DELEGATE]}));
    assert_eq!(input[1]["type"], "message");
    let at = lines
        .iter()
        .position(|line| line.kind == "job_started")
        .expect("a job_started line");
    assert_ahead_lines(&lines, DELEGATE, &action, at);
}

#[test]
fn a_notice_while_idle_writes_what_its_claim_took_ahead_first() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    let action = ActionId("a_aaaaaaaaaaaaaaaa".into());
    world.send(ahead_delegate(
        DELEGATE,
        "Done.",
        resumed_ahead(DELEGATE, &action),
        &asked,
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(asked.count(), 1, "the claim is asked once, at admission");
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&[
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed"
        ])
    );
    // The turn's input names the job once, with no message item.
    assert_eq!(
        lines[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [DELEGATE]}])
    );
    let at = lines
        .iter()
        .position(|line| line.kind == "job_started")
        .expect("a job_started line");
    assert_ahead_lines(&lines, DELEGATE, &action, at);
}

fn shell(command: &str, output: &str) -> Delivery {
    Delivery::Shell(
        ShellCommand {
            command: command.into(),
            output: output.into(),
            artifact: None,
            process: Process {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
            },
        },
        ignore(),
    )
}

#[test]
fn two_sent_shells_come_before_the_drain_s_messages() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(shell("echo b", "b\n"));
    world.send(shell("echo a", "a\n"));
    world.send(prompt("Hello."));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let input = lines[0].payload["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["type"], "shell_command");
    assert_eq!(input[1]["type"], "shell_command");
    let first = input[0]["seq"].as_u64().unwrap();
    let second = input[1]["seq"].as_u64().unwrap();
    assert!(first < second, "held lines keep seq order");
    assert_eq!(input[2]["content"][0]["text"], "Hello.");
    let logged: Vec<u64> = log::read(&world.dir)
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == "shell_command")
        .map(|line| line.seq.map(|seq| seq.0).unwrap())
        .collect();
    assert_eq!(logged, vec![first, second]);
    let sent = users(&world.requests()[0]);
    assert_eq!(
        sent[sent.len() - 3..],
        [
            "! echo b\nb".to_owned(),
            "! echo a\na".to_owned(),
            "Hello.".to_owned(),
        ]
    );
}

#[test]
fn a_turn_with_nothing_held_names_no_shell() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    world.send(prompt("Hello."));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let input = lines[0].payload["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["content"][0]["text"], "Hello.");
}

#[test]
fn a_shell_sent_during_a_turn_joins_the_next_turn_only() {
    let mut world = World::new(
        vec![
            calls("sends"),
            Scripted::text("Seen."),
            Scripted::text("Again."),
        ],
        Vec::new(),
        |inbox| sending(inbox, vec![shell("echo hi", "hi\n")]),
    );
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let first = world.turn_lines();
    assert!(
        first
            .iter()
            .filter(|line| line.kind == "turn_started")
            .flat_map(|line| line.payload["input"].as_array().unwrap().clone())
            .all(|item| item["type"] != "shell_command"),
        "the running turn names no shell"
    );
    assert!(
        !users(&world.requests()[0]).contains(&"! echo hi\nhi".to_owned()),
        "the running turn's next request reads no shell"
    );
    assert!(
        !users(&world.requests()[1]).contains(&"! echo hi\nhi".to_owned()),
        "the running turn's last request reads no shell"
    );
    assert!(
        durable_kinds(&world).contains(&"shell_command".to_owned()),
        "the drain still writes the line"
    );
    world.send(prompt("again"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let second = world.turn_lines();
    let input = second[0].payload["input"].as_array().unwrap().clone();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0]["type"], "shell_command");
    assert_eq!(input[1]["content"][0]["text"], "again");
    assert!(
        users(&world.requests()[2]).contains(&"! echo hi\nhi".to_owned()),
        "the next turn reads the shell"
    );
}

#[test]
fn loop_write_folds_started_jobs_and_fiber_delegates_into_the_cache() {
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new());
    let looped = world.looped.as_mut().unwrap();
    looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_1".into()),
                tool: Some("shell".into()),
                extension: None,
                description: "sleep 1".into(),
                output_path: "artifacts/j_1.log".into(),
            }),
            None,
            None,
        )
        .unwrap();
    looped
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_2".into()),
                tool: None,
                extension: Some("e".into()),
                description: "delegated".into(),
                output_path: "artifacts/j_2.log".into(),
            }),
            None,
            None,
        )
        .unwrap();
    looped
        .write(
            &Event::DelegateStarted(DelegateStarted {
                job_id: JobId("j_2".into()),
                delegate_session_id: SessionId("s_d".into()),
                harness: "fiber".into(),
                model: "fake/m".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            None,
            None,
        )
        .unwrap();
    looped
        .write(
            &Event::DelegateStarted(DelegateStarted {
                job_id: JobId("j_1".into()),
                delegate_session_id: SessionId("s_o".into()),
                harness: "codex".into(),
                model: "fake/m".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            None,
            None,
        )
        .unwrap();
    let shell = looped.ending.starts.get(&JobId("j_1".into())).unwrap();
    assert_eq!(shell.tool, "shell");
    assert_eq!(
        shell.output_path,
        std::path::PathBuf::from("artifacts/j_1.log")
    );
    assert!(!shell.fiber);
    let delegated = looped.ending.starts.get(&JobId("j_2".into())).unwrap();
    assert_eq!(delegated.tool, "");
    assert!(delegated.fiber);
}

// Deliveries: every after_tool hook sees a job's end before it is logged.

/// One `after_tool` call a delivery dispatch made, as the hook saw it.
#[derive(Debug, Clone)]
struct SeenCall {
    delivery: Option<JobDelivery>,
    tool: String,
    status: CallStatus,
    content: String,
    process: Option<Process>,
    spilled: Option<(PathBuf, u64)>,
}

/// What the recording hook answers one call with.
type AnswerFn = Box<dyn Fn(&SeenCall) -> AfterToolAnswer + Send>;

/// The `Hooks` fake deliveries dispatch to: it records each call and
/// answers as the test scripts, defaulting to [`Unchanged`].
#[derive(Default)]
struct RecordingHooks {
    seen: Mutex<Vec<SeenCall>>,
    answer: Mutex<Option<AnswerFn>>,
    staged: Mutex<Vec<StateLine>>,
    /// The registered `after_tool` chain the `output_file` pass sees:
    /// each hook's extension and whether it is `blocking`. A probe over
    /// a path that is not a regular file synthesizes its answer from
    /// this chain, as the session's own hooks do, without running hooks.
    chain: Mutex<Vec<(String, bool)>>,
    /// An answer the next probe over a non-regular path returns instead
    /// of the chain's synthesis: covers answers no chain produces.
    probe_override: Mutex<Option<AfterToolAnswer>>,
}

impl RecordingHooks {
    /// Answers every call with `answer`.
    fn answer_with(&self, answer: impl Fn(&SeenCall) -> AfterToolAnswer + Send + 'static) {
        *self.answer.lock().unwrap() = Some(Box::new(answer));
    }

    /// Registers the `after_tool` chain the non-regular probe
    /// synthesizes from: each hook's extension and its `blocking` flag.
    fn set_chain(&self, chain: &[(&str, bool)]) {
        *self.chain.lock().unwrap() = chain
            .iter()
            .map(|(name, blocking)| ((*name).to_owned(), *blocking))
            .collect();
    }

    /// Makes the next non-regular probe return `answer`.
    fn override_probe(&self, answer: AfterToolAnswer) {
        *self.probe_override.lock().unwrap() = Some(answer);
    }

    /// Stages carried state lines for the next `take_change_state`.
    fn stage_state(&self, lines: Vec<StateLine>) {
        *self.staged.lock().unwrap() = lines;
    }

    fn calls(&self) -> Vec<SeenCall> {
        self.seen.lock().unwrap().clone()
    }
}

/// No change, no attribution, no notices: the delivery passes through.
fn unchanged() -> AfterToolAnswer {
    AfterToolAnswer {
        outcome: AfterToolOutcome::Unchanged,
        changed_by: Vec::new(),
        notices: Vec::new(),
    }
}

/// A redaction of `content`, attributed to the redactor.
fn changed(content: &str) -> AfterToolAnswer {
    AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: Some(content.into()),
            details: None,
            artifact: None,
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    }
}

const WITHHOLD_EXTENSIONS: [&str; 2] = ["a", "b"];

/// The chain's block, naming `redactor` with bogus attribution the loop
/// must drop.
fn withheld() -> AfterToolAnswer {
    AfterToolAnswer {
        outcome: AfterToolOutcome::Withheld {
            extension: "redactor".into(),
        },
        changed_by: WITHHOLD_EXTENSIONS
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        notices: Vec::new(),
    }
}

/// A hook of the delivery extension's own that could not run.
fn failed_hook() -> AfterToolAnswer {
    AfterToolAnswer {
        outcome: AfterToolOutcome::Failed(HookFailure {
            extension: "redactor".into(),
            message: "boom".into(),
        }),
        changed_by: Vec::new(),
        notices: Vec::new(),
    }
}

/// The withheld line a `withheld` answer writes.
fn withheld_line() -> String {
    "Output withheld: the `after_tool` hook of extension redactor failed.".into()
}

/// The withheld line a `failed_hook` answer writes.
fn failed_line() -> String {
    "Output withheld: the `after_tool` hook of extension redactor could not run: boom.".into()
}

impl Hooks for RecordingHooks {
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        let seen = SeenCall {
            delivery: call.delivery,
            tool: call.tool.to_owned(),
            status: call.status,
            content: call.content.to_owned(),
            process: call.process.cloned(),
            spilled: call
                .spilled
                .as_ref()
                .map(|spilled| (spilled.path.clone(), spilled.size)),
        };
        self.seen.lock().unwrap().push(seen.clone());
        // A probe over a path that is not a regular file synthesizes
        // the chain's failure without running hooks: the scripted answer
        // covers ordinary calls only.
        if call.not_regular {
            if let Some(answer) = self.probe_override.lock().unwrap().take() {
                return answer;
            }
            return contract::hook::not_regular_answer(&self.chain.lock().unwrap());
        }
        match self.answer.lock().unwrap().as_ref() {
            Some(answer) => answer(&seen),
            None => unchanged(),
        }
    }

    fn take_change_state(&self) -> Vec<StateLine> {
        std::mem::take(&mut *self.staged.lock().unwrap())
    }

    fn deliver_to(&self, _: Sender<Delivery>) {}

    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> contract::hook::ContextAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        // Never consulted on the delivery path: the neutral answer.
        contract::hook::BeforeHandoffAnswer {
            outcome: contract::hook::BeforeHandoffOutcome::NoNote,
            notices: Vec::new(),
        }
    }
}

impl World {
    /// Gives the loop a recording `after_tool` fake; the test scripts its
    /// answers and reads its calls through the returned hooks.
    fn hooked(mut self, hooks: Arc<RecordingHooks>) -> Self {
        let dyned: Arc<dyn Hooks> = hooks;
        let looped = self.looped.take().unwrap().hooks(dyned);
        self.looped = Some(looped);
        self
    }
}

/// Writes a `job_started` for `job` straight to the loop's log, as the
/// transcript's own line would.
fn write_started(world: &mut World, job: &str, tool: Option<&str>) {
    world
        .looped
        .as_mut()
        .unwrap()
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId(job.into()),
                tool: tool.map(str::to_owned),
                extension: None,
                description: "test job".into(),
                output_path: "artifacts/test.log".into(),
            }),
            None,
            None,
        )
        .unwrap();
}

/// `id`'s end with an earlier run's end taken ahead of it.
fn superseded(id: &str) -> Delivery {
    Delivery::Job(JobNotice {
        completed: failed(id),
        claim: Claim(Box::new(move || {
            Some(vec![Taken {
                record: JobRecord::Completed(failed("j_earlier")),
                action: None,
            }])
        })),
        delegate: None,
    })
}

#[test]
fn a_completion_dispatch_names_its_delivery_tool_status_process_and_built_content() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    write_started(&mut world, "j_earlier", Some("watch"));
    world.send(superseded(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Completion));
    assert_eq!(calls[0].tool, "watch");
    assert_eq!(calls[0].status, CallStatus::Failed);
    assert_eq!(calls[0].content, notice_text(&failed("j_earlier")));
    assert_eq!(calls[0].process, failed("j_earlier").process);
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    assert_eq!(calls[1].tool, "shell");
    assert_eq!(calls[1].status, CallStatus::Failed);
    assert_eq!(calls[1].content, notice_text(&failed(JOB)));
    assert_eq!(calls[1].process, failed(JOB).process);
}

#[test]
fn a_monitor_dispatch_names_its_delivery_and_has_no_process() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(batch(JOB, "build ok", None));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Monitor));
    assert_eq!(calls[0].tool, "shell");
    assert_eq!(calls[0].status, CallStatus::Completed);
    assert_eq!(calls[0].content, "build ok");
    assert_eq!(calls[0].process, None);
}

#[test]
fn a_delegate_dispatch_hands_the_hook_the_full_text() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, DELEGATE, Some("shell"));
    world.send(delegate_held(DELEGATE, "Done.".into()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Delegate));
    assert_eq!(calls[0].tool, "shell");
    assert_eq!(calls[0].status, CallStatus::Failed);
    assert_eq!(calls[0].content, "Done.");
    assert_eq!(calls[0].process, failed(DELEGATE).process);
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
}

#[test]
fn a_delivery_without_a_started_job_names_no_tool() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Completion));
    assert_eq!(calls[0].tool, "");
}

#[test]
fn a_redacted_completion_names_the_redactor_and_keeps_the_token_out_of_the_log() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    let mut end = failed(JOB);
    end.output_tail = Some("token TOKEN-1234 here\n".into());
    world.send(notice_with(end));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].content.contains("TOKEN-1234"));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(
        lines[2].payload["notice"],
        format!(
            "Fiber: background job {JOB} ended: failed.\nExit code 1.\nLast output:\ntoken [redacted] here"
        )
    );
    assert!(lines[2].payload.get("output_tail").is_none());
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
    let raw = std::fs::read(world.dir.join("events.jsonl")).unwrap();
    assert!(!raw.windows(10).any(|window| window == b"TOKEN-1234"));
    let seen = users(&world.requests()[0]);
    assert_eq!(
        seen[seen.len() - 1],
        format!(
            "Fiber: background job {JOB} ended: failed.\nExit code 1.\nLast output:\ntoken [redacted] here"
        )
    );
}

/// `id`'s end with `completed`, so a test can vary what the job printed.
fn notice_with(completed: JobCompleted) -> Delivery {
    Delivery::Job(JobNotice {
        completed,
        claim: Claim(Box::new(|| Some(Vec::new()))),
        delegate: None,
    })
}

#[test]
fn a_redacted_monitor_batch_keeps_its_suppressed_count() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(batch(JOB, "token TOKEN-1234 here", Some(3)));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_line"]));
    assert_eq!(lines[2].payload["lines"], "token [redacted] here");
    assert_eq!(lines[2].payload["suppressed"], 3);
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
}

#[test]
fn a_redacted_delegate_text_reaches_the_log_and_artifact_without_the_token() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, DELEGATE, Some("shell"));
    world.send(delegate_held(DELEGATE, "token TOKEN-1234 here".into()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].content.contains("TOKEN-1234"));
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["text"], "token [redacted] here");
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
    // A short text writes no artifact: the redaction rides the line.
    assert!(lines[2].payload.get("artifact").is_none());
    let raw = std::fs::read(world.dir.join("events.jsonl")).unwrap();
    assert!(!raw.windows(10).any(|window| window == b"TOKEN-1234"));
}

#[test]
fn a_redacted_ahead_completion_is_logged_redacted() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    let mut earlier = failed("j_earlier");
    earlier.output_tail = Some("token TOKEN-1234 here\n".into());
    world.send(Delivery::Job(JobNotice {
        completed: failed(JOB),
        claim: Claim(Box::new(move || {
            Some(vec![Taken {
                record: JobRecord::Completed(earlier.clone()),
                action: None,
            }])
        })),
        delegate: None,
    }));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls().len(), 2);
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["job_completed", "job_completed"])
    );
    assert_eq!(lines[2].payload["job_id"], "j_earlier");
    assert!(
        lines[2].payload["notice"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
    let raw = std::fs::read(world.dir.join("events.jsonl")).unwrap();
    assert!(!raw.windows(10).any(|window| window == b"TOKEN-1234"));
}

#[test]
fn a_claimed_end_is_logged_as_the_completion_hook_left_it() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut end = failed(JOB);
    end.output_tail = Some("token TOKEN-1234 here\n".into());
    let taken = vec![Taken {
        record: JobRecord::Completed(end),
        action: None,
    }];
    let mut world = World::new(
        vec![calls("settles"), Scripted::text("Seen.")],
        Vec::new(),
        |_| vec![Arc::new(Settles { taken })],
    )
    .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The claim's end ran through the completion hook before it was
    // logged: the hook saw the raw tail, and the call's own dispatch
    // followed it.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Completion));
    assert!(calls[0].content.contains("TOKEN-1234"));
    assert_eq!(calls[1].delivery, None);
    let completed = world
        .turn_lines()
        .into_iter()
        .find(|line| line.kind == "job_completed")
        .expect("the claimed end is logged");
    assert_eq!(
        completed.payload["notice"],
        format!(
            "Fiber: background job {JOB} ended: failed.\nExit code 1.\nLast output:\ntoken [redacted] here"
        )
    );
    assert!(completed.payload.get("output_tail").is_none());
    assert_eq!(completed.payload["changed_by"], json!(["redactor"]));
    let raw = std::fs::read(world.dir.join("events.jsonl")).unwrap();
    assert!(!raw.windows(10).any(|window| window == b"TOKEN-1234"));
}

#[test]
fn an_unchanged_answer_keeps_the_completed_as_built() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls().len(), 1);
    let lines = world.turn_lines();
    assert_notice(&lines[2], JOB);
    assert!(lines[2].payload.get("notice").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn a_details_only_answer_keeps_the_completed_as_built() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: None,
            details: Some(json!({"matches": ["TOKEN-1234"]})),
            artifact: None,
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_notice(&lines[2], JOB);
    assert!(lines[2].payload.get("notice").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn a_content_none_answer_keeps_the_completed_as_built() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: None,
            details: None,
            artifact: None,
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_notice(&lines[2], JOB);
    assert!(lines[2].payload.get("notice").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn an_answer_returning_the_same_content_counts_as_changed() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: Some(seen.content.clone()),
            details: None,
            artifact: None,
        },
        changed_by: Vec::new(),
        notices: Vec::new(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    // Changed, though nothing differs: the notice replaces the tail, and
    // the empty attribution writes no key.
    assert_eq!(lines[2].payload["notice"], notice_text(&failed(JOB)));
    assert!(lines[2].payload.get("output_tail").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn an_empty_replacement_empties_the_notice_but_names_the_redactor() {
    for delivery in ["completion", "monitor", "delegate"] {
        let hooks = Arc::new(RecordingHooks::default());
        hooks.answer_with(|_| changed(""));
        let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
            .hooked(Arc::clone(&hooks));
        write_started(&mut world, JOB, Some("shell"));
        match delivery {
            "completion" => world.send(held(JOB)),
            "monitor" => world.send(batch(JOB, "build ok", None)),
            _ => world.send(delegate_held(DELEGATE, "Done.".into())),
        }
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let lines = world.turn_lines();
        match delivery {
            "completion" => {
                assert_eq!(lines[2].payload["notice"], "");
                assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
            }
            "monitor" => {
                assert_eq!(lines[2].payload["lines"], "");
                assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
            }
            _ => {
                assert_eq!(lines[2].payload["text"], "");
                assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
            }
        }
    }
}

#[test]
fn a_withheld_completion_names_no_one_and_keeps_no_tail() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| withheld());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["notice"], withheld_line());
    assert!(lines[2].payload.get("output_tail").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
    let seen = users(&world.requests()[0]);
    assert_eq!(seen[seen.len() - 1], withheld_line());
}

#[test]
fn a_withheld_monitor_batch_keeps_its_suppressed_count() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| withheld());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(batch(JOB, "build ok", Some(3)));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["lines"], withheld_line());
    assert_eq!(lines[2].payload["suppressed"], 3);
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn a_withheld_delegate_writes_the_line_twice_and_no_artifact() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| withheld());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, DELEGATE, Some("shell"));
    world.send(delegate_held(DELEGATE, "Done.".into()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["text"], withheld_line());
    assert!(lines[2].payload.get("artifact").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
    assert_eq!(lines[3].payload["notice"], withheld_line());
    assert!(lines[3].payload.get("changed_by").is_none());
    assert!(!world.dir.join(format!("artifacts/{DELEGATE}.txt")).exists());
}

#[test]
fn an_artifact_only_delegate_change_writes_the_hook_artifact_leaving_text() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| artifacted("HOOK-ARTIFACT"));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, DELEGATE, Some("shell"));
    world.send(delegate_held(DELEGATE, "Done.".into()));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The delegate dispatch ran; the artifact-only answer changes the
    // finish, so `changed_by` names the hook. Flipping the artifact
    // write back to ignored leaves the text changed or the artifact
    // missing and fails below.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Delegate));
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["text"], "Done.");
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
    let artifact = lines[2].payload["artifact"]
        .as_str()
        .expect("an artifact-only delegate change writes its artifact");
    assert_eq!(artifact, format!("artifacts/{DELEGATE}.txt"));
    assert_eq!(
        std::fs::read_to_string(world.dir.join(artifact)).unwrap(),
        "HOOK-ARTIFACT"
    );
    // The completion dispatch saw the same artifact-only answer and
    // passed through untouched: no notice, no attribution.
    assert!(lines[3].payload.get("notice").is_none());
    assert!(lines[3].payload.get("changed_by").is_none());
}

#[test]
fn a_failed_hook_names_its_extension_and_what_went_wrong() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| failed_hook());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(lines[2].payload["notice"], failed_line());
    assert!(lines[2].payload.get("output_tail").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn a_non_blocking_failure_writes_its_notice_before_the_unchanged_end() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| AfterToolAnswer {
        outcome: AfterToolOutcome::Unchanged,
        changed_by: Vec::new(),
        notices: vec![Notice {
            code: ErrorCode::HookFailed,
            message: "redactor blinked".into(),
            extension: Some("redactor".into()),
        }],
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    // The notice is ephemeral: it reaches watchers, never the log file.
    let all = world.watched();
    let notice = all
        .iter()
        .position(|line| line.kind == "notice")
        .expect("the failure notice is emitted");
    let end = all
        .iter()
        .position(|line| line.kind == "job_completed")
        .expect("the unchanged end is emitted");
    assert!(notice < end);
    assert_eq!(all[notice].payload["message"], "redactor blinked");
    assert_eq!(all[notice].turn_id, all[end].turn_id);
    // The delivery passes through untouched.
    assert_notice(&lines[2], JOB);
}

#[test]
fn a_hooks_carried_state_is_written_before_the_changed_end() {
    let hooks = Arc::new(RecordingHooks::default());
    let staged = Arc::clone(&hooks);
    hooks.answer_with(move |seen| {
        // Staged when the delivery answers: earlier hook points in the
        // turn drain `take_change_state` first.
        staged.stage_state(vec![StateLine {
            extension: "redactor".into(),
            key: "last_seen".into(),
            value: Some((json!(JOB), OnFork::AtPoint)),
            landing: 0,
        }]);
        changed(&seen.content.replace("TOKEN-1234", "[redacted]"))
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    let mut end = failed(JOB);
    end.output_tail = Some("token TOKEN-1234 here\n".into());
    world.send(notice_with(end));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(
        kinds(&lines),
        one_step_with(&["extension_state_set", "job_completed"])
    );
    assert_eq!(lines[2].payload["extension"], "redactor");
    assert_eq!(lines[2].payload["key"], "last_seen");
    assert!(
        lines[3].payload["notice"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
}

#[test]
fn an_artifact_only_output_file_change_writes_its_carried_state() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| artifacted("ARTIFACT"));
    hooks.stage_state(vec![StateLine {
        extension: "redactor".into(),
        key: "last_seen".into(),
        value: Some((json!(JOB), OnFork::AtPoint)),
        landing: 0,
    }]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    // A real output file, so the `output_file` pass reads it and the
    // artifact-only answer replaces it.
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"old\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The artifact-only change commits the carried state: without the
    // fix the state line is never written. The `output_file` pass runs
    // at intake, before `turn_started`, so the state line precedes the
    // turn's lines and the test reads the whole log.
    let all: Vec<String> = log::read(&world.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(
        all.contains(&"extension_state_set".to_owned()),
        "an artifact-only output-file change writes its carried state: {all:?}"
    );
    let stated = log::read(&world.dir)
        .unwrap()
        .into_iter()
        .find(|line| line.kind == "extension_state_set")
        .expect("the carried state is written");
    assert_eq!(stated.payload["extension"], "redactor");
    assert_eq!(stated.payload["key"], "last_seen");
    assert_eq!(output_bytes(&world, OUT), b"ARTIFACT");
}

#[test]
fn an_artifact_only_completion_answer_writes_no_carried_state() {
    let hooks = Arc::new(RecordingHooks::default());
    let staged = Arc::clone(&hooks);
    hooks.answer_with(move |seen| {
        if seen.delivery == Some(JobDelivery::OutputFile) {
            return unchanged();
        }
        // Staged when the completion answers: the artifact-only answer
        // must not commit it, since a completion ignores artifact text.
        staged.stage_state(vec![StateLine {
            extension: "redactor".into(),
            key: "last_seen".into(),
            value: Some((json!(JOB), OnFork::AtPoint)),
            landing: 0,
        }]);
        artifacted("ARTIFACT")
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"old\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The completion ignores the artifact text, so the artifact-only
    // answer commits no carried state: under the
    // `artifact_counts || artifact.is_some()` mutant the state line is
    // written. The output file is untouched, so only the completion
    // pass stages state.
    let all: Vec<String> = log::read(&world.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(
        !all.contains(&"extension_state_set".to_owned()),
        "an artifact-only completion answer writes no carried state: {all:?}"
    );
    let end = world
        .turn_lines()
        .into_iter()
        .find(|line| line.kind == "job_completed")
        .expect("the unchanged end is logged");
    assert!(end.payload.get("notice").is_none());
    assert!(end.payload.get("changed_by").is_none());
    assert_eq!(output_bytes(&world, OUT), b"old\n");
}

#[test]
fn a_monitor_replacement_is_cut_as_a_batch_is() {
    let replacement = (0..50)
        .map(|_| "x".repeat(99))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(replacement.len(), 4999);
    // Exactly 3,000 characters across short lines: no line hits the
    // per-line cut, and the whole is kept.
    let three_k = (0..5)
        .map(|_| "x".repeat(499))
        .chain(std::iter::once("x".repeat(500)))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(three_k.chars().count(), 3000);
    let three_k_over = format!("{three_k}\ny");
    let three_k_over_expected = format!("{three_k}\n[cut: 2 more characters]");
    for (lines, expected) in [
        (replacement.clone(), {
            let mut cut: String = replacement.chars().take(3000).collect();
            cut.push_str("\n[cut: 1999 more characters]");
            cut
        }),
        (three_k.clone(), three_k.clone()),
        (three_k_over.clone(), three_k_over_expected.clone()),
        (
            format!("{}\n{}", "x".repeat(501), "ok"),
            format!("{} [cut]\nok", "x".repeat(500)),
        ),
        ("x".repeat(500), "x".repeat(500)),
    ] {
        let hooks = Arc::new(RecordingHooks::default());
        hooks.answer_with(move |_| changed(&lines));
        let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
            .hooked(Arc::clone(&hooks));
        write_started(&mut world, JOB, Some("shell"));
        world.send(batch(JOB, "build ok", None));
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let seen = world.turn_lines();
        assert_eq!(seen[2].payload["lines"], expected);
        assert_eq!(seen[2].payload["changed_by"], json!(["redactor"]));
    }
}

#[test]
fn an_already_cut_batch_returned_as_is_is_kept() {
    let lines = format!("{}\n[cut: 5 more characters]", "x".repeat(3000));
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(move |seen| AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: Some(seen.content.clone()),
            details: None,
            artifact: None,
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(batch(JOB, &lines, None));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let seen = world.turn_lines();
    assert_eq!(seen[2].payload["lines"], lines);
}

#[test]
fn an_expanded_delegate_text_is_cut_with_its_full_text_in_an_artifact() {
    let full = "z".repeat(20 * 1024);
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    let dir = world.dir.clone();
    hooks.answer_with(move |seen| {
        // The delegate dispatch sees the full text before any cut or
        // artifact exists; the completion dispatch passes through.
        if seen.delivery == Some(JobDelivery::Delegate) {
            assert_eq!(seen.content.len(), 20 * 1024);
            assert!(!dir.join(format!("artifacts/{DELEGATE}.txt")).exists());
            return changed(&"y".repeat(40 * 1024));
        }
        unchanged()
    });
    write_started(&mut world, DELEGATE, Some("shell"));
    world.send(delegate_held(DELEGATE, full));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let text = lines[2].payload["text"].as_str().unwrap();
    assert!(text.starts_with(&"y".repeat(16 * 1024)));
    assert!(text.contains("bytes cut."));
    assert_eq!(
        lines[2].payload["artifact"],
        format!("artifacts/{DELEGATE}.txt")
    );
    let kept =
        std::fs::read_to_string(world.dir.join(format!("artifacts/{DELEGATE}.txt"))).unwrap();
    assert_large_str_eq(&kept, &"y".repeat(40 * 1024));
    let seen = users(&world.requests()[0]);
    assert_eq!(seen[seen.len() - 2], delegate_rendered(DELEGATE, text));
}

#[test]
fn an_expanded_completion_notice_is_cut_with_its_full_text_in_an_artifact() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| changed(&"y".repeat(20 * 1024)));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let notice = lines[2].payload["notice"].as_str().unwrap();
    assert!(notice.starts_with(&"y".repeat(16 * 1024)));
    assert!(notice.contains("bytes cut."));
    assert!(lines[2].payload.get("output_tail").is_none());
    assert_eq!(lines[2].payload["changed_by"], json!(["redactor"]));
    let kept =
        std::fs::read_to_string(world.dir.join(format!("artifacts/{JOB}-notice.txt"))).unwrap();
    assert_large_str_eq(&kept, &"y".repeat(20 * 1024));
}

#[test]
fn an_overlong_completion_returned_as_is_is_kept_whole() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: Some(seen.content.clone()),
            details: None,
            artifact: None,
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    let mut end = failed(JOB);
    end.output_tail = Some(format!("{}\n", "x".repeat(20 * 1024)));
    world.send(notice_with(end));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    let notice = lines[2].payload["notice"].as_str().unwrap();
    // The built notice is already over the cap; the equal replacement is
    // kept whole rather than cut again.
    assert!(notice.len() > 16 * 1024);
    assert!(!notice.contains("bytes cut."));
    assert!(lines[2].payload.get("output_tail").is_none());
}

#[test]
fn without_hooks_nothing_runs_and_the_completed_stays_raw() {
    let world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let mut world = world;
    write_started(&mut world, JOB, Some("shell"));
    let mut end = failed(JOB);
    end.output_tail = Some("token TOKEN-1234 here\n".into());
    world.send(notice_with(end));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    assert_eq!(lines[2].payload["output_tail"], "token TOKEN-1234 here\n");
    assert!(lines[2].payload.get("notice").is_none());
    assert!(lines[2].payload.get("changed_by").is_none());
}

#[test]
fn mark_orphans_runs_no_hook_and_writes_the_end_raw() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started(&mut world, JOB, Some("shell"));
    let mut end = failed(JOB);
    end.output_tail = Some("token TOKEN-1234 here\n".into());
    world
        .looped
        .as_mut()
        .unwrap()
        .mark_orphans(vec![end])
        .unwrap();
    assert!(hooks.calls().is_empty());
    let lines: Vec<Envelope> = log::read(&world.dir).unwrap();
    assert_eq!(lines[lines.len() - 1].kind, "job_completed");
    assert_eq!(
        lines[lines.len() - 1].payload["output_tail"],
        "token TOKEN-1234 here\n"
    );
}

// The output_file pass: the job's whole output through after_tool at intake.

/// Writes `job`'s start with `output_path`, relative to the log dir.
fn write_started_at(world: &mut World, job: &str, tool: Option<&str>, output_path: &str) {
    world
        .looped
        .as_mut()
        .unwrap()
        .write(
            &Event::JobStarted(JobStarted {
                job_id: JobId(job.into()),
                tool: tool.map(str::to_owned),
                extension: None,
                description: "test job".into(),
                output_path: output_path.into(),
            }),
            None,
            None,
        )
        .unwrap();
}

/// Writes `bytes` to `output_path` under the session directory.
fn write_output(world: &World, output_path: &str, bytes: &[u8]) {
    let path = world.dir.join(output_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
}

/// The bytes now at `output_path` under the session directory.
fn output_bytes(world: &World, output_path: &str) -> Vec<u8> {
    std::fs::read(world.dir.join(output_path)).unwrap()
}

/// Asserts two large strings equal by length and first differing offset,
/// without printing either whole on failure.
fn assert_large_str_eq(actual: &str, expected: &str) {
    assert_eq!(actual.len(), expected.len(), "large string length");
    let offset = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
}

/// Asserts two large byte strings equal by length and first differing
/// offset, without printing either whole on failure.
fn assert_large_bytes_eq(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "large bytes length");
    let offset = actual
        .iter()
        .zip(expected.iter())
        .position(|(left, right)| left != right);
    assert_eq!(offset, None, "first differing byte offset");
}

/// A `delegate_started` for `job` with `harness`.
fn write_delegate_started(world: &mut World, job: &str, harness: &str) {
    world
        .looped
        .as_mut()
        .unwrap()
        .write(
            &Event::DelegateStarted(DelegateStarted {
                job_id: JobId(job.into()),
                delegate_session_id: SessionId("s_o".into()),
                harness: harness.into(),
                model: "fake/m".into(),
                workspace: "/w".into(),
                worktree: None,
                forked_from: None,
            }),
            None,
            None,
        )
        .unwrap();
}

/// A replacing answer: the artifact text the hook returns.
fn artifacted(text: &str) -> AfterToolAnswer {
    AfterToolAnswer {
        outcome: AfterToolOutcome::Changed {
            content: None,
            details: None,
            artifact: Some(text.into()),
        },
        changed_by: vec!["redactor".into()],
        notices: Vec::new(),
    }
}

const OUT: &str = "artifacts/out.log";

#[test]
fn an_output_file_at_the_bound_is_handed_as_content_and_past_it_as_path() {
    for (size, as_content) in [
        (0, true),
        (1_048_575, true),
        (1_048_576, true),
        (1_048_577, false),
    ] {
        let hooks = Arc::new(RecordingHooks::default());
        let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
            .hooked(Arc::clone(&hooks));
        write_started_at(&mut world, JOB, Some("shell"), OUT);
        write_output(&world, OUT, &vec![b'x'; size]);
        world.send(held(JOB));
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let calls = hooks.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
        assert_eq!(calls[0].tool, "shell");
        assert_eq!(calls[0].status, CallStatus::Failed);
        assert!(calls[0].process.is_some());
        if as_content {
            assert_eq!(calls[0].content.len(), size);
            assert_eq!(calls[0].spilled, None);
        } else {
            assert_eq!(calls[0].content, "");
            assert_eq!(calls[0].spilled, Some((world.dir.join(OUT), 1_048_577)));
        }
        assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    }
}

/// The bytes at `OUT` after an end whose hook answers `how`.
fn output_after(size: usize, how: &str) -> Vec<u8> {
    let hooks = Arc::new(RecordingHooks::default());
    let tag = how.to_owned();
    hooks.answer_with(move |_| match tag.as_str() {
        "artifact" => artifacted("ARTIFACT"),
        "content" => changed("REPLACED"),
        _ => unchanged(),
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, &vec![b'x'; size]);
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    output_bytes(&world, OUT)
}

#[test]
fn an_artifact_answer_replaces_a_small_file_and_content_does_too() {
    assert_eq!(output_after(100, "artifact"), b"ARTIFACT");
    assert_eq!(output_after(100, "content"), b"REPLACED");
}

#[test]
fn a_large_file_is_replaced_by_artifact_only() {
    assert_eq!(output_after(1_048_577, "artifact"), b"ARTIFACT");
    assert_large_bytes_eq(&output_after(1_048_577, "content"), &vec![b'x'; 1_048_577]);
}

#[test]
fn an_unchanged_answer_leaves_the_bytes_and_the_inode() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"kept\n");
    #[cfg(unix)]
    let inode_before = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(world.dir.join(OUT)).unwrap().ino()
    };
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(output_bytes(&world, OUT), b"kept\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(world.dir.join(OUT)).unwrap().ino(),
            inode_before
        );
    }
}

#[test]
#[cfg(unix)]
fn a_replaced_file_keeps_its_bits_in_a_new_inode_with_no_sibling_left() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| changed("new\n"));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"old\n");
    std::fs::set_permissions(world.dir.join(OUT), std::fs::Permissions::from_mode(0o640)).unwrap();
    let inode_before = std::fs::metadata(world.dir.join(OUT)).unwrap().ino();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let stat = std::fs::metadata(world.dir.join(OUT)).unwrap();
    assert_eq!(stat.mode() & 0o777, 0o640);
    assert_ne!(stat.ino(), inode_before);
    assert_eq!(output_bytes(&world, OUT), b"new\n");
    let siblings: Vec<_> = std::fs::read_dir(world.dir.join("artifacts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        !siblings
            .iter()
            .any(|name| name.to_string_lossy().ends_with(".fiber-redact")),
        "{siblings:?}"
    );
}

#[test]
#[cfg(unix)]
fn a_replacement_that_cannot_be_written_fails_the_turn_leaving_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| changed("new\n"));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"old\n");
    let dir = world.dir.join("artifacts");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    world.send(held(JOB));
    let rx = world.spawn();
    let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the turn ended");
    assert!(outcome.is_err(), "{outcome:?}");
    world.looped = Some(looped);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output_bytes(&world, OUT), b"old\n");
    let siblings: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        !siblings
            .iter()
            .any(|name| name.to_string_lossy().ends_with(".fiber-redact")),
        "{siblings:?}"
    );
}

#[test]
fn a_withheld_or_failed_output_hook_replaces_the_file_with_its_line() {
    for (answer, line) in [
        (withheld(), withheld_line()),
        (failed_hook(), failed_line()),
    ] {
        let hooks = Arc::new(RecordingHooks::default());
        hooks.answer_with(move |_| answer.clone());
        let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
            .hooked(Arc::clone(&hooks));
        write_started_at(&mut world, JOB, Some("shell"), OUT);
        write_output(&world, OUT, b"token TOKEN-1234\n");
        world.send(held(JOB));
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let calls = hooks.calls();
        assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
        assert_eq!(output_bytes(&world, OUT), format!("{line}\n").into_bytes());
    }
}

#[test]
fn a_non_blocking_output_failure_keeps_the_file_and_gives_a_notice_with_no_turn() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|_| AfterToolAnswer {
        outcome: AfterToolOutcome::Unchanged,
        changed_by: Vec::new(),
        notices: vec![Notice {
            code: ErrorCode::HookFailed,
            message: "redactor blinked".into(),
            extension: Some("redactor".into()),
        }],
    });
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"kept\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(output_bytes(&world, OUT), b"kept\n");
    // The notice is ephemeral: it reaches watchers, never the log file.
    let all = world.watched();
    let notice = all
        .iter()
        .find(|line| line.kind == "notice")
        .expect("the failure notice is emitted");
    assert_eq!(notice.payload["message"], "redactor blinked");
    assert_eq!(notice.turn_id, None);
}

#[test]
fn a_missing_output_file_calls_no_output_hook_and_is_replaced_with_the_unreadable_line() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The read failed before any dispatch: only the completion hook ran.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delivery, Some(JobDelivery::Completion));
    assert_eq!(
        output_bytes(&world, OUT),
        b"Output withheld: the job's output file could not be read.\n"
    );
    // The completion itself is logged as the hook left it.
    assert_notice(&world.turn_lines()[2], JOB);
}

#[test]
#[cfg(unix)]
fn a_symlink_at_the_output_path_is_replaced_itself_leaving_the_target() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    hooks.set_chain(&[("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, "artifacts/target.log", b"token TOKEN-1234\n");
    std::os::unix::fs::symlink(world.dir.join("artifacts/target.log"), world.dir.join(OUT))
        .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // No hook ran on the link: the read never follows it, and the
    // blocking chain withholds the path itself in the probe's place.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", withheld_line()).into_bytes()
    );
    assert_eq!(
        output_bytes(&world, "artifacts/target.log"),
        b"token TOKEN-1234\n"
    );
}

#[test]
#[cfg(unix)]
fn a_path_swapped_for_a_link_before_open_is_refused() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    hooks.set_chain(&[("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"old\n");
    write_output(&world, "artifacts/target.log", b"target\n");
    let out = world.dir.join(OUT);
    let target = world.dir.join("artifacts/target.log");
    world
        .looped
        .as_mut()
        .unwrap()
        .redact_output_with(&failed(JOB), None, move || {
            std::fs::remove_file(&out).unwrap();
            std::os::unix::fs::symlink(&target, &out).unwrap();
        })
        .unwrap();
    // The no-follow open refused the link: no hook ran on it, and the
    // blocking chain withholds the path itself in the probe's place.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", withheld_line()).into_bytes()
    );
    assert_eq!(output_bytes(&world, "artifacts/target.log"), b"target\n");
}

#[test]
#[cfg(unix)]
fn a_pre_existing_sibling_symlink_never_redirects_the_redaction_write() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    write_output(&world, "artifacts/target.log", b"target\n");
    // A job leaves the redaction sibling as a link to another file: an
    // exclusive create must not follow it. Without `create_new` the
    // write truncates the target and the rename leaves a link behind.
    std::os::unix::fs::symlink(
        world.dir.join("artifacts/target.log"),
        world.dir.join("artifacts/out.log.fiber-redact"),
    )
    .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(output_bytes(&world, OUT), b"token [redacted]\n");
    assert_eq!(output_bytes(&world, "artifacts/target.log"), b"target\n");
    assert!(
        !std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the redacted output file replaces any sibling link"
    );
}

#[test]
#[cfg(unix)]
fn a_socket_at_the_output_path_is_replaced_without_blocking() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.set_chain(&[("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    std::fs::create_dir_all(world.dir.join("artifacts")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(world.dir.join(OUT)).unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", withheld_line()).into_bytes()
    );
}

#[test]
#[cfg(unix)]
fn a_fifo_at_the_output_path_does_not_block_and_is_replaced() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.set_chain(&[("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    std::fs::create_dir_all(world.dir.join("artifacts")).unwrap();
    fakes::fifo(world.dir.join(OUT));
    // The non-blocking open refuses to wait for a writer: the turn ends
    // at the deadline below instead of hanging on the FIFO.
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    // `after_tool` runs on every job delivery, and once more on the job's
    // whole output file when the job ends (`docs/extensions.md`, "Hooks"):
    // this job has one `completion` delivery plus the `output_file` pass.
    assert_eq!(calls.len(), 2);
    // No doc promises the two passes' relative order, so this test asserts
    // them as a set; the order is pinned by
    // `the_output_pass_runs_before_the_completion_hook_and_the_file_is_already_replaced`.
    let output = calls
        .iter()
        .find(|call| call.delivery == Some(JobDelivery::OutputFile))
        .expect("the output_file pass ran");
    assert_eq!(output.content, "");
    assert!(
        calls
            .iter()
            .any(|call| call.delivery == Some(JobDelivery::Completion)),
        "the completion delivery ran"
    );
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", withheld_line()).into_bytes()
    );
}

#[test]
fn a_directory_at_the_output_path_fails_the_turn() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.set_chain(&[("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    std::fs::create_dir_all(world.dir.join(OUT)).unwrap();
    world.send(held(JOB));
    let rx = world.spawn();
    let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the turn ended");
    assert!(outcome.is_err(), "{outcome:?}");
    world.looped = Some(looped);
    assert!(world.dir.join(OUT).is_dir());
}

#[test]
fn an_output_path_nested_under_artifacts_fails_the_turn_leaving_the_file() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), "artifacts/sub/out.log");
    write_output(&world, "artifacts/sub/out.log", b"token TOKEN-1234\n");
    world.send(held(JOB));
    let rx = world.spawn();
    let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the turn ended");
    // The nested in-session path cannot escape to the fallback: the read
    // fails before any dispatch, and the unreadable-line replacement fails
    // too, so the turn fails with the file untouched.
    assert!(outcome.is_err(), "{outcome:?}");
    world.looped = Some(looped);
    assert_eq!(
        output_bytes(&world, "artifacts/sub/out.log"),
        b"token TOKEN-1234\n"
    );
}

#[test]
fn an_output_path_that_escapes_artifacts_fails_the_turn_leaving_the_log() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), "artifacts/../events.jsonl");
    world.send(held(JOB));
    let rx = world.spawn();
    let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the turn ended");
    // A traversal cannot reach the fallback: the turn fails instead of
    // reading the log as output and replacing it with the unreadable line.
    assert!(outcome.is_err(), "{outcome:?}");
    world.looped = Some(looped);
    let events = std::fs::read_to_string(world.dir.join("events.jsonl")).unwrap();
    assert!(!events.contains("could not be read"), "{events}");
}

#[test]
fn an_absolute_output_path_outside_the_session_keeps_the_fallback() {
    let outside = fakes::TempDir::new("fiber-output-outside");
    let path = outside.path().join("out.log");
    std::fs::write(&path, b"token TOKEN-1234\n").unwrap();
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), &path.display().to_string());
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // A job's output file passes through `after_tool` when it ends
    // (`docs/tools.md`, "Background jobs"). The external path still reads
    // and replaces at the recorded path itself.
    let output = hooks
        .calls()
        .into_iter()
        .find(|call| call.delivery == Some(JobDelivery::OutputFile))
        .expect("the output file reaches after_tool");
    assert_eq!(output.content, "token TOKEN-1234\n");
    assert_eq!(std::fs::read(&path).unwrap(), b"token [redacted]\n");
}

#[test]
#[cfg(unix)]
fn a_non_regular_output_file_with_a_blocking_hook_is_withheld_naming_it() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.set_chain(&[("lint", false), ("redactor", true)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, "artifacts/target.log", b"token TOKEN-1234\n");
    std::os::unix::fs::symlink(world.dir.join("artifacts/target.log"), world.dir.join(OUT))
        .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The probe handed no content over, and the first blocking hook in
    // run order withholds the path itself: a failure fails closed.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", withheld_line()).into_bytes()
    );
    assert_eq!(
        output_bytes(&world, "artifacts/target.log"),
        b"token TOKEN-1234\n"
    );
}

#[test]
#[cfg(unix)]
fn a_non_regular_output_file_with_only_non_blocking_hooks_is_kept_with_a_notice_each() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.set_chain(&[("fmt", false), ("lint", false)]);
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, "artifacts/target.log", b"token TOKEN-1234\n");
    std::os::unix::fs::symlink(world.dir.join("artifacts/target.log"), world.dir.join(OUT))
        .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // The probe handed no content over: every hook's failure is
    // non-blocking, so the link stays and each hook is named in a notice.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert_eq!(calls[1].delivery, Some(JobDelivery::Completion));
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        output_bytes(&world, "artifacts/target.log"),
        b"token TOKEN-1234\n"
    );
    let mut extensions: Vec<_> = world
        .watched()
        .iter()
        .filter(|line| line.kind == "notice")
        .map(|line| {
            line.payload["extension"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    extensions.sort();
    assert_eq!(extensions, vec!["fmt".to_owned(), "lint".to_owned()]);
}

#[test]
#[cfg(unix)]
fn a_non_regular_output_path_with_no_after_tool_hook_is_left_alone() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, "artifacts/target.log", b"token TOKEN-1234\n");
    std::os::unix::fs::symlink(world.dir.join("artifacts/target.log"), world.dir.join(OUT))
        .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    // No hook is registered: there is no pass to fail, so the link
    // stays, the target is untouched, and no notice names anyone.
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        output_bytes(&world, "artifacts/target.log"),
        b"token TOKEN-1234\n"
    );
    assert!(world.watched().iter().all(|line| line.kind != "notice"));
}

#[test]
#[cfg(unix)]
fn a_failed_probe_withholds_the_non_regular_path() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.override_probe(failed_hook());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, "artifacts/target.log", b"token TOKEN-1234\n");
    std::os::unix::fs::symlink(world.dir.join("artifacts/target.log"), world.dir.join(OUT))
        .unwrap();
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert!(
        std::fs::symlink_metadata(world.dir.join(OUT))
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(
        output_bytes(&world, OUT),
        format!("{}\n", failed_line()).into_bytes()
    );
    assert_eq!(
        output_bytes(&world, "artifacts/target.log"),
        b"token TOKEN-1234\n"
    );
}

#[test]
fn a_file_grown_past_the_bound_between_stat_and_read_is_handed_as_path() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, &vec![b'x'; 1_048_000]);
    let out = world.dir.join(OUT);
    world
        .looped
        .as_mut()
        .unwrap()
        .redact_output_with(&failed(JOB), None, move || {
            let grown = std::fs::read(&out).unwrap();
            std::fs::write(&out, [grown, vec![b'y'; 2_000]].concat()).unwrap();
        })
        .unwrap();
    // The capped read saw 1,048,577 bytes of a 1,050,000-byte file: the
    // hook is handed its path and size, and no content.
    let calls = hooks.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "");
    assert_eq!(calls[0].spilled, Some((world.dir.join(OUT), 1_050_000)));
    assert_eq!(output_bytes(&world, OUT).len(), 1_050_000);
}

#[test]
fn a_capped_read_never_returns_more_than_the_bound_plus_one() {
    let dir = fakes::TempDir::new("fiber-read");
    let path = dir.path().join("big.log");
    std::fs::write(&path, vec![b'y'; 4 * 1024 * 1024]).unwrap();
    match crate::deliveries::read_external(&path) {
        Ok(log::OutputRead::Large { size }) => assert_eq!(size, 4 * 1024 * 1024),
        read => panic!("a 4 MiB file reads large, got {read:?}"),
    }
}

#[test]
#[cfg(target_os = "linux")]
fn invalid_utf8_is_handed_lossy_and_an_unchanged_answer_keeps_the_bytes() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"ok \xff\xfe end\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(calls[0].delivery, Some(JobDelivery::OutputFile));
    assert_eq!(calls[0].content, "ok \u{fffd}\u{fffd} end\n");
    assert_eq!(output_bytes(&world, OUT), b"ok \xff\xfe end\n");
}

#[test]
fn a_fiber_delegate_gets_no_output_pass_but_another_harness_does() {
    for harness in ["fiber", "codex"] {
        let hooks = Arc::new(RecordingHooks::default());
        hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
        let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
            .hooked(Arc::clone(&hooks));
        write_started_at(&mut world, DELEGATE, None, OUT);
        write_delegate_started(&mut world, DELEGATE, harness);
        write_output(&world, OUT, b"token TOKEN-1234\n");
        world.send(delegate_held(DELEGATE, "Done.".into()));
        assert_eq!(world.turn(), Some(TurnOutcome::Completed));
        let outputs: Vec<_> = hooks
            .calls()
            .into_iter()
            .filter(|call| call.delivery == Some(JobDelivery::OutputFile))
            .collect();
        if harness == "fiber" {
            // A Fiber delegate's events.jsonl is another session's log.
            assert!(outputs.is_empty());
            assert_eq!(output_bytes(&world, OUT), b"token TOKEN-1234\n");
        } else {
            assert_eq!(outputs.len(), 1);
            assert_eq!(output_bytes(&world, OUT), b"token [redacted]\n");
        }
    }
}

#[test]
fn a_job_the_log_never_started_gets_no_output_pass() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    // No job_started: the pass has no output path to read.
    write_output(&world, OUT, b"token TOKEN-1234\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert!(
        hooks
            .calls()
            .iter()
            .all(|call| call.delivery != Some(JobDelivery::OutputFile))
    );
    assert_eq!(output_bytes(&world, OUT), b"token TOKEN-1234\n");
}

#[test]
fn a_claim_refused_notice_still_runs_the_pass_once_and_writes_no_end() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Hi.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    let asked = Asked::default();
    world.send(notice(JOB, false, &asked));
    let rx = world.spawn();
    world.send(prompt("Hello."));
    assert_eq!(
        world.join(&rx, &Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(asked.count(), 1);
    // The intake redacted the file even though the claim was refused.
    assert_eq!(output_bytes(&world, OUT), b"token [redacted]\n");
    assert!(
        hooks
            .calls()
            .iter()
            .any(|call| call.delivery == Some(JobDelivery::OutputFile))
    );
    let lines = world.turn_lines();
    assert!(lines.iter().all(|line| line.kind != "job_completed"));
}

#[test]
fn a_running_claim_refused_notice_still_runs_the_pass() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    let asked = Asked::default();
    world
        .looped
        .as_mut()
        .unwrap()
        .admit_job(counted_job(JOB, false, &asked), &TurnId("t_test".into()))
        .unwrap();
    // The pass ran at intake, under the running turn.
    assert_eq!(output_bytes(&world, OUT), b"token [redacted]\n");
    world.send(prompt("go"));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(asked.count(), 1, "the claim is asked once, at the write");
    let lines = world.turn_lines();
    assert!(lines.iter().all(|line| line.kind != "job_completed"));
}

#[test]
fn an_end_after_close_with_nothing_waiting_runs_no_pass() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(Vec::new(), Vec::new(), |_| Vec::new()).hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    let asked = Asked::default();
    world.send(Delivery::Close(ignore()));
    world.send(notice(JOB, true, &asked));
    assert_eq!(world.turn(), None);
    assert_eq!(asked.count(), 0);
    assert!(hooks.calls().is_empty());
    assert_eq!(output_bytes(&world, OUT), b"token TOKEN-1234\n");
}

#[test]
fn a_pass_under_shutdown_reads_nothing() {
    let hooks = Arc::new(RecordingHooks::default());
    hooks.answer_with(|seen| changed(&seen.content.replace("TOKEN-1234", "[redacted]")));
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    world.looped.as_mut().unwrap().cancel.shutdown(143);
    world
        .looped
        .as_mut()
        .unwrap()
        .redact_output(&failed(JOB), None)
        .unwrap();
    assert!(hooks.calls().is_empty());
    assert_eq!(output_bytes(&world, OUT), b"token TOKEN-1234\n");
}

#[test]
fn without_hooks_a_missing_file_is_never_read() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert!(!world.dir.join(OUT).exists());
    assert_notice(&world.turn_lines()[2], JOB);
}

#[test]
fn the_output_pass_runs_before_the_completion_hook_and_the_file_is_already_replaced() {
    let hooks = Arc::new(RecordingHooks::default());
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new())
        .hooked(Arc::clone(&hooks));
    let dir = world.dir.clone();
    hooks.answer_with(move |seen| match seen.delivery {
        Some(JobDelivery::OutputFile) => changed("redacted\n"),
        _ => {
            let at_completion = std::fs::read(dir.join(OUT)).unwrap();
            assert_eq!(at_completion, b"redacted\n");
            unchanged()
        }
    });
    write_started_at(&mut world, JOB, Some("shell"), OUT);
    write_output(&world, OUT, b"token TOKEN-1234\n");
    world.send(held(JOB));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    let calls = hooks.calls();
    assert_eq!(
        calls.iter().map(|call| call.delivery).collect::<Vec<_>>(),
        [Some(JobDelivery::OutputFile), Some(JobDelivery::Completion)]
    );
    assert_eq!(output_bytes(&world, OUT), b"redacted\n");
    // The completion itself passed through: the raw tail is logged.
    assert_notice(&world.turn_lines()[2], JOB);
}
