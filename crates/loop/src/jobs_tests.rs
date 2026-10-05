//! A finished job wakes the loop: a notice starts a turn while idle, joins a
//! running turn at the next step boundary, and is written once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    Decision, JobCompleted, Outcome, ToolCallArgumentsDelta, ToolCallRequested, TurnOutcome,
};
use contract::inbox::{Ack, Claim, Delivery, JobNotice, Message};
use contract::jobs::{Jobs, OpenError};
use contract::provider::{
    Delta, Input, ModelCall, ModelRequest, Provider, ReplyAction, ToolDefinition,
};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{
    ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender as From,
};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, JobId, RequestId, SessionId};
use fakes::jobs::FakeJobs;
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use serde_json::{Map, Value, json};

use super::{line_text, notice_text};
use crate::{Loop, Model};

const DEADLINE: Duration = Duration::from_secs(10);
const JOB: &str = "j_5e10c0ffee123456";
const OTHER: &str = "j_0ddba11cafe00000";

/// How a shell job that exited 1 ends.
fn failed(id: &str) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after: None,
            provider: None,
        }),
        process: Some(Process {
            exit_code: Some(1),
            signal: None,
            timed_out: false,
        }),
        output_tail: Some("1 failing\n".into()),
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
            holds
        })),
    })
}

fn held(id: &str) -> Delivery {
    notice(id, true, &Asked::default())
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

/// A tool a standing rule asks a person about.
struct Gated;

impl Tool for Gated {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "gated".into(),
            description: "Needs a person's allow.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
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
    watched: mpsc::Receiver<Envelope>,
    clock: Arc<fakes::clock::FakeClock>,
}

impl World {
    fn new(
        script: Vec<Scripted>,
        by_call: Vec<Vec<Delivery>>,
        tools: impl FnOnce(&Sender<Delivery>) -> Vec<Arc<dyn Tool>>,
    ) -> Self {
        let home = fakes::TempDir::new("fiber-job-notices");
        let workspace = home.path().join("workspace");
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&credentials).unwrap();
        let fake = fakes::clock::FakeClock::new();
        let clock: Arc<dyn contract::clock::Clock> = fake.clone();
        let log = Arc::new(
            Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        // The watcher blocks without a deadline, so its lines cross a
        // channel and [`World::watched`] carries the deadline.
        let mut watcher = log.watch();
        let (forward, watched) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    return;
                }
            }
        });
        let provider = Arc::new(ScriptedProvider::new(script));
        let (inbox, rx) = mpsc::channel();
        let during = During {
            inner: Arc::clone(&provider),
            inbox: inbox.clone(),
            by_call: Mutex::new(by_call.into()),
        };
        let looped = Loop::start(
            Arc::clone(&log),
            Arc::new(during),
            Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                clock,
            ),
            rx,
            tools(&inbox)
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                rules: Arc::new(AskGated),
            },
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
        }
    }

    /// The lines written since the last call, ephemeral ones too, through
    /// the next `turn_completed`: call it after a turn ends.
    fn watched(&mut self) -> Vec<Envelope> {
        let mut lines = Vec::new();
        loop {
            let line = self
                .watched
                .recv_timeout(DEADLINE)
                .expect("a turn_completed line");
            let done = line.kind == "turn_completed";
            lines.push(line);
            if done {
                return lines;
            }
        }
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

    /// Waits for the turn [`World::spawn`] started, failing at [`DEADLINE`].
    fn join(
        &mut self,
        rx: &mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)>,
    ) -> Option<TurnOutcome> {
        let (looped, outcome) = rx.recv_timeout(DEADLINE).expect("the turn ended");
        self.looped = Some(looped);
        outcome.unwrap()
    }

    /// Runs one turn on its own thread, failing at [`DEADLINE`].
    fn turn(&mut self) -> Option<TurnOutcome> {
        let rx = self.spawn();
        self.join(&rx)
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
            Input::User { text } => Some(text.clone()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => None,
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
            text: rendered(JOB)
        })
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
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let (answered, answer) = mpsc::channel();
    world.send(held(JOB));
    world.send(Delivery::Prompt(
        message("Hello.", "c_prompt"),
        reported(answered),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(true));
    let lines = world.turn_lines();
    assert_eq!(kinds(&lines), one_step_with(&["job_completed"]));
    let input = &lines[0].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 2);
    assert_eq!(input[0], json!({"type": "jobs", "job_ids": [JOB]}));
    assert_eq!(input[1]["content"][0]["text"], "Hello.");
}

#[test]
fn a_second_prompt_after_a_notice_is_busy() {
    let mut world = World::new(vec![Scripted::text("Seen.")], Vec::new(), |_| Vec::new());
    let (answered, answer) = mpsc::channel();
    world.send(held(JOB));
    world.send(prompt("Hello."));
    world.send(Delivery::Prompt(
        message("Again.", "c_again"),
        reported(answered),
    ));
    assert_eq!(world.turn(), Some(TurnOutcome::Completed));
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(false));
    let input = world.turn_lines()[0].payload["input"].clone();
    assert_eq!(input.as_array().unwrap().len(), 2);
}

#[test]
fn a_notice_whose_claim_fails_starts_no_turn() {
    let mut world = World::new(vec![Scripted::text("Hi.")], Vec::new(), |_| Vec::new());
    let asked = Asked::default();
    world.send(notice(JOB, false, &asked));
    // Whether the loop takes the notice alone or with the prompt, only the
    // prompt starts a turn.
    let rx = world.spawn();
    world.send(prompt("Hello."));
    assert_eq!(world.join(&rx), Some(TurnOutcome::Completed));
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
fn a_steer_drop_while_idle_skips_notices_and_the_prompt() {
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
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(true));
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
        retry_after: None,
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
        retry_after: None,
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
    };
    assert_eq!(
        notice_text(&done),
        format!("Fiber: background job {JOB} ended: completed.\nExit code 0.")
    );
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
            text: rendered(JOB)
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
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(true));
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
            text: rendered(JOB)
        })
    );
}

/// Watches the log for `permission_requested`, then runs `send` with its
/// request id: the signal the loop is waiting for a reply.
fn on_request(log: &Log, send: impl FnOnce(RequestId) + Send + 'static) -> thread::JoinHandle<()> {
    let mut watcher = log.watch();
    thread::spawn(move || {
        // The watcher blocks without a deadline, so its lines cross a
        // channel and the wait below carries the deadline.
        let (forward, waiting) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            let line = waiting
                .recv_timeout(DEADLINE)
                .expect("a permission_requested line");
            if line.kind == "permission_requested" {
                send(RequestId(
                    line.payload["request_id"].as_str().unwrap().into(),
                ));
                return;
            }
        }
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
    ids: Mutex<Vec<JobId>>,
    racing: Mutex<Option<(Sender<Delivery>, Vec<Delivery>)>>,
    arriving: Mutex<Option<(Sender<Delivery>, Vec<Delivery>)>>,
    reads: Mutex<Option<Sender<usize>>>,
}

impl Listed {
    fn set(&self, ids: &[&str]) {
        *self.ids.lock().unwrap() = ids.iter().map(|id| JobId((*id).into())).collect();
    }
}

impl Jobs for Listed {
    fn open(&self, _: contract::jobs::Opening) -> Result<contract::jobs::Opened, OpenError> {
        Err(OpenError::Io {
            path: "unused".into(),
            source: std::io::Error::other("a listed job is not opened"),
        })
    }

    fn stop(&self, _: &JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _: contract::jobs::Foreground) {}

    fn running(&self) -> Vec<JobId> {
        if let Some((inbox, deliveries)) = self.racing.lock().unwrap().take() {
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
            self.ids.lock().unwrap().clear();
        }
        if let Some((inbox, deliveries)) = self.arriving.lock().unwrap().take() {
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
        }
        let ids = self.ids.lock().unwrap().clone();
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
        thread::spawn(move || {
            let _sent = done.send(looped.run());
        });
        finished
    }

    /// The durable lines of the next turn, `turn_started` through
    /// `turn_completed`.
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

fn ran(finished: &mpsc::Receiver<Result<(), crate::Error>>) {
    let result = finished.recv_timeout(DEADLINE).expect("run returned");
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
    job.end.end(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(
        last[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [id]}])
    );
    assert_notice(&last[2], &id);
    ran(&finished);
    let requests = world.requests();
    assert_eq!(requests.len(), 3);
    // A resume renders the notice from the log as the loop sent it.
    let rebuilt = crate::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
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
    let world = World::new(vec![Scripted::text("Done.")], Vec::new(), |_| Vec::new());
    let jobs = FakeJobs::new(world.home());
    let job = open_job(&jobs);
    job.end.end(failed(&job.started.job_id.0));
    // The end before the loop's inbox was wired sends nothing.
    let mut world = world.with_jobs(jobs.clone());
    world.send(prompt("Hello."));
    world.send(Delivery::Close(ignore()));
    let finished = world.spawn_run();
    ran(&finished);
    assert_eq!(durable_kinds(&world), session_of(&[one_step_with(&[])]));
    assert_eq!(world.requests().len(), 1);
}

#[test]
fn the_ending_notice_is_given_once_even_when_another_job_starts() {
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
    first_job.end.end(failed(&first_id));
    let one = world.next_turn();
    assert_eq!(kinds(&one), one_step_with(&["job_completed"]));
    assert_notice(&one[2], &first_id);

    second_job.end.end(failed(&second_id));
    let two = world.next_turn();
    assert_eq!(kinds(&two), one_step_with(&["job_completed"]));
    assert_notice(&two[2], &second_id);
    ran(&finished);
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
    assert_eq!(prompt_answer.recv_timeout(DEADLINE), Ok(false));
    assert_eq!(steer_answer.recv_timeout(DEADLINE), Ok(false));
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(
        last[0].payload["input"],
        json!([{"type": "jobs", "job_ids": [JOB]}])
    );
    ran(&finished);
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
    assert!(
        clock.await_parked(origin + IDLE, DEADLINE),
        "{:?}",
        clock.parked()
    );
    clock.advance(IDLE);
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
    // Another two delays with no prompt: no second check, and the session
    // does not end while the job runs. A wake makes the wait look again.
    clock.advance(IDLE + IDLE);
    world.send(Delivery::Cancelled);
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    job.end.end(failed(&id));
    let turn = world.next_turn();
    assert_eq!(kinds(&turn), one_step_with(&["job_completed"]));
    // The idle delay starts once the job's turn is over and no job runs.
    let restarted = origin + IDLE + IDLE + IDLE + IDLE;
    assert!(
        clock.await_parked(restarted, DEADLINE),
        "{:?}",
        clock.parked()
    );
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    // Half a delay later a prompt arrives: the clock restarts from it.
    clock.advance(IDLE / 2);
    world.send(Delivery::Cancelled);
    world.send(prompt("Hello."));
    let prompted = world.next_turn();
    assert_eq!(kinds(&prompted), one_step_with(&[]));
    let rearmed = origin + IDLE + IDLE / 2 + IDLE;
    assert!(
        clock.await_parked(rearmed, DEADLINE),
        "{:?}",
        clock.parked()
    );
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    job.end.end(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    assert!(clock.await_parked(rearmed + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    let check = world.next_turn();
    let mut steered = one_step_with(&["jobs_pending_notified"]);
    steered.pop();
    steered.extend(["step_started", "steering_applied"]);
    steered.extend(REPLY);
    assert_eq!(kinds(&check), steered);
    assert_check(&check, &id);
    // The steer was taken as the check came due: the next is one delay on.
    assert!(
        clock.await_parked(origin + IDLE + IDLE, DEADLINE),
        "{:?}",
        clock.parked()
    );
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    job.end.end(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    assert!(clock.await_parked(origin + IDLE * 3, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
    assert_eq!(world.requests().len(), 4);
}

#[test]
fn a_steer_while_idle_arms_the_check_again() {
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    world.send(steer("Still here."));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&[]));
    assert!(clock.await_parked(origin + IDLE + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    assert_check(&world.next_turn(), &id);
    job.end.end(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    assert!(clock.await_parked(origin + IDLE * 3, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
    assert_eq!(world.requests().len(), 4);
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    clock.advance(IDLE);
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
    first.end.end(failed(&one));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    clock.advance(IDLE + IDLE);
    world.send(Delivery::Cancelled);
    second.end.end(failed(&two));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    assert!(clock.await_parked(origin + IDLE * 4, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    clock.advance(IDLE * 10);
    world.send(Delivery::Cancelled);
    job.end.end(failed(&id));
    assert_eq!(kinds(&world.next_turn()), one_step_with(&["job_completed"]));
    world.send(Delivery::Close(ignore()));
    ran(&finished);
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
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    // The steer lands in the inbox as the wait reads the jobs past the
    // delay, after its last look at the inbox: the check turn's first step
    // drains it.
    *jobs.arriving.lock().unwrap() = Some((world.inbox.clone(), vec![steer("Still here.")]));
    clock.advance(IDLE);
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
            "steering_applied",
            "steering_queue",
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
    ran(&finished);
}

#[test]
fn a_job_listed_while_idle_holds_the_deadline_gets_the_check_and_its_end_restarts_it() {
    let world = World::new(vec![Scripted::text("Checked.")], Vec::new(), |_| Vec::new());
    let jobs = Arc::new(Listed::default());
    let mut world = world.with_jobs(jobs.clone()).idle(IDLE);
    let clock = Arc::clone(&world.clock);
    let origin = clock.now();
    let finished = world.spawn_run();
    assert!(clock.await_parked(origin + IDLE, DEADLINE));
    let (reads, read) = mpsc::channel();
    *jobs.reads.lock().unwrap() = Some(reads);
    jobs.set(&[JOB]);
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    // The wait read the job running past the deadline, and did not end:
    // the session was unattended that long, so the check is given.
    while read.recv_timeout(DEADLINE).expect("the wait read the jobs") == 0 {}
    assert_check(&world.next_turn(), JOB);
    // The job ends with its final state already claimed: no turn starts,
    // and the delay counts from when the wait saw no job running.
    jobs.set(&[]);
    world.send(notice(JOB, false, &Asked::default()));
    assert!(
        clock.await_parked(origin + IDLE + IDLE, DEADLINE),
        "{:?}",
        clock.parked()
    );
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    let _request_id = request.recv_timeout(DEADLINE).expect("the call asked");
    watcher.join().unwrap();
    clock.advance(IDLE + Duration::from_secs(1));
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
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(false));
    // The job's end waits for the next step; the delay restarts from it.
    job.end.end(failed(&id));
    let restarted = origin + IDLE + Duration::from_secs(1) + IDLE;
    assert!(
        clock.await_parked(restarted, DEADLINE),
        "{:?}",
        clock.parked()
    );
    clock.advance(IDLE);
    world.send(Delivery::Cancelled);
    ran(&finished);
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
    ran(&finished);
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
        looped
            .queued
            .push_back(crate::jobs::Queued::Steer(message("Kept.", "c_kept")));
        looped
            .queued
            .push_back(crate::jobs::Queued::Job(failed(JOB)));
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
    assert_eq!(ending_answer.recv_timeout(DEADLINE), Ok(false));
    job.end.end(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    assert_eq!(job_answer.recv_timeout(DEADLINE), Ok(false));
    ran(&finished);
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
    assert_eq!(answer.recv_timeout(DEADLINE), Ok(true));
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
    ran(&finished);
    assert_eq!(world.requests().len(), 2);
    assert_eq!(durable_kinds(&world), session_of(&[expected]));
}

// Monitors: a batch of lines wakes the model as a job's end does.

/// A monitor's batch for `id`.
fn line(id: &str, lines: &str, suppressed: Option<u64>) -> contract::events::JobLine {
    contract::events::JobLine {
        job_id: JobId(id.into()),
        lines: lines.into(),
        suppressed,
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
            text: format!("Fiber: monitor {JOB} printed:\nbuild ok\ntests ok\n{SUPPRESSED}")
        })
    );
    // A resume renders the batch from the log as the loop sent it.
    let rebuilt = crate::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
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
    let rebuilt = crate::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
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
    let rebuilt = crate::rebuild(&log::read(&world.dir).unwrap(), "fake/model").unwrap();
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
    job.end.end(failed(&id));
    let last = world.next_turn();
    assert_eq!(kinds(&last), one_step_with(&["job_completed"]));
    ran(&finished);
}
