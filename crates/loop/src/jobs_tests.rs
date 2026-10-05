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

use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    Decision, JobCompleted, Outcome, ToolCallArgumentsDelta, ToolCallRequested, TurnOutcome,
};
use contract::inbox::{Ack, Claim, Delivery, JobNotice, Message};
use contract::provider::{
    Delta, Input, ModelCall, ModelRequest, Provider, ReplyAction, ToolDefinition,
};
use contract::rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
use contract::shapes::{
    ContentPart, DeclaredEffects, Effect, Failure, Origin, Process, Sender as From,
};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, JobId, RequestId, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use serde_json::{Map, Value, json};

use super::notice_text;
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
            command_id: CommandId(command.into()),
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
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
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
