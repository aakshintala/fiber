//! `extension_exec` is written once at the drain that takes it, idle or
//! mid-turn, and starts no turn (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::GenerationId;
use contract::SessionId;
use contract::commands::Name;
use contract::events::UsageRecorded;
use contract::events::{
    Answer, CallStatus, Control, ExtensionExec, ExtensionLog, Interaction, InteractionRequested,
    InteractionResolved, NamedBy, ResolvedBy, ShellCommand,
};
use contract::inbox::{Ack, Delivery};
use contract::shapes::Process;
use contract::shapes::Tokens;
use contract::shapes::Worktree;
use contract::{ErrorCode, RequestId, TurnId};
use log::Log;

use super::{TurnInput, Waited};
use crate::naming::{Named, PINNED};
use crate::rewind::command::REWOUND_CLOSING;
use crate::{Loop, Model};
use fakes::{Deadline, NoRules};

const DEADLINE: Duration = Duration::from_secs(10);

fn logged(message: &str) -> ExtensionLog {
    ExtensionLog {
        extension: "fiber.test/notes".into(),
        message: message.into(),
    }
}

fn diag_text(home: &fakes::TempDir) -> String {
    std::fs::read_to_string(home.path().join("logs").join("session-s_test.log")).unwrap_or_default()
}

fn saved_kinds(home: &fakes::TempDir) -> Vec<String> {
    log::read(&home.path().join("s_test"))
        .unwrap_or_default()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

fn exec(program: &str) -> ExtensionExec {
    ExtensionExec {
        extension: "fiber.test/notes".into(),
        program: program.into(),
        args: vec!["status".into()],
        cwd: "/w".into(),
        process: Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        },
    }
}

fn started() -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    started_with(None)
}

/// [`started`] with `worktree` on `session_started`.
fn started_with(worktree: Option<Worktree>) -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    let home = fakes::TempDir::new("fiber-exec-inbox");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    let (_inbox, rx) = mpsc::channel::<Delivery>();
    let looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
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
            inbox: rx,
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        worktree,
    )
    .unwrap();
    let watched = log.watch();
    (looped, log, watched, home)
}

struct CommandTool(&'static str);
impl contract::tool::Tool for CommandTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.0.into(),
            description: "command fixture".into(),
            input_schema: serde_json::json!({"type":"object"}),
            deferred: false,
            hosted: None,
        }
    }
    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }
    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        contract::tool::Output::default()
    }
}

fn inner_delivery(name: &str) -> Delivery {
    Delivery::InnerCall(contract::inbox::InnerCallRequest {
        extension: "ext".into(),
        command: contract::CommandId("c_1".into()),
        name: name.into(),
        arguments: serde_json::json!({}),
        cancel: Arc::new(fakes::CancelToken::new()),
        taken: Box::new(|| {}),
        answer: Box::new(|_| {}),
    })
}

#[test]
fn running_admission_holds_a_command_request_without_writing_it() {
    let (mut looped, _log, _watched, home) = started();
    let watchdog = fakes::Watchdog::matching(&home.path().display().to_string());
    looped
        .admit_running(inner_delivery("a"), &TurnId("t_1".into()))
        .unwrap();
    assert_eq!(looped.deferred.len(), 1);
    assert!(matches!(
        looped.deferred.front(),
        Some(Delivery::InnerCall(_))
    ));
    assert_eq!(saved_kinds(&home), ["session_started"]);
    watchdog.stand_down(Duration::from_secs(3));
}

struct Unanswered(Arc<std::sync::atomic::AtomicBool>);
impl Drop for Unanswered {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn an_alone_request_after_command_end_or_shutdown_writes_nothing_and_never_calls_taken() {
    for shutdown in [false, true] {
        let (mut looped, _log, _watched, home) = started();
        let watchdog = fakes::Watchdog::matching(&home.path().display().to_string());
        let source = Arc::new(fakes::CancelToken::new());
        if shutdown {
            looped.cancel.shutdown(143);
        } else {
            source.cancel();
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Unanswered(dropped.clone());
        let taken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let taking = taken.clone();
        let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let answering = answered.clone();
        let request = contract::inbox::InnerCallRequest {
            extension: "ext".into(),
            command: contract::CommandId("c_1".into()),
            name: "a".into(),
            arguments: serde_json::json!({}),
            cancel: source,
            taken: Box::new(move || {
                taking.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
            answer: Box::new(move |_| {
                answering.store(true, std::sync::atomic::Ordering::SeqCst);
                drop(guard);
            }),
        };
        fakes::within(
            "drop stopped command request",
            Duration::from_secs(3),
            move || {
                looped.run_alone(request).unwrap();
            },
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!taken.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!answered.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(saved_kinds(&home), ["session_started"]);
        watchdog.stand_down(Duration::from_secs(3));
    }
}

#[test]
fn the_drain_runs_commands_in_order_and_preserves_interleaved_model_switches() {
    for order in [
        vec!["switch", "a"],
        vec!["a", "switch"],
        vec!["a", "switch", "b"],
    ] {
        let (mut looped, _log, _watched, home) = started();
        let watchdog = fakes::Watchdog::matching(&home.path().display().to_string());
        for name in ["a", "b"] {
            let tool: Arc<dyn contract::tool::Tool> = Arc::new(CommandTool(name));
            looped.tools.insert(
                name.into(),
                ("test".into(), tool.clone(), tool.definition()),
            );
        }
        for item in &order {
            let delivery = if *item == "switch" {
                Delivery::Model(
                    serde_json::from_value(serde_json::json!({"model":"fake/other"})).unwrap(),
                    ignore(),
                )
            } else {
                inner_delivery(item)
            };
            looped.deferred.push_back(delivery);
        }
        let looped = fakes::within(
            "boundary command drain",
            Duration::from_secs(3),
            move || {
                looped.drain(&TurnId("t_1".into())).unwrap();
                looped
            },
        );
        assert_eq!(looped.deferred.len(), 1);
        assert!(matches!(
            looped.deferred.front(),
            Some(Delivery::Model(_, _))
        ));
        let lines = log::read(&home.path().join("s_test")).unwrap();
        let requested: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == "tool_call_requested")
            .map(|line| line.payload["name"].as_str().unwrap())
            .collect();
        let expected: Vec<_> = order
            .iter()
            .copied()
            .filter(|item| *item != "switch")
            .collect();
        assert_eq!(requested, expected);
        assert!(lines.iter().all(|line| line.turn_id.is_none()));
        let mut kinds = vec!["session_started"];
        for _ in &requested {
            kinds.extend([
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed",
            ]);
        }
        assert_eq!(
            lines
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>(),
            kinds
        );
        watchdog.stand_down(Duration::from_secs(3));
    }
}

#[test]
fn start_records_the_worktree_on_session_started() {
    let (_looped, _log, _watched, home) = started_with(Some(Worktree {
        path: "/w".to_owned(),
        branch: "fiber/s_test".to_owned(),
        repository: Some("/r/.git".to_owned()),
    }));
    let lines = log::read(&home.path().join("s_test")).unwrap();
    assert_eq!(lines[0].kind, "session_started");
    assert_eq!(
        lines[0].payload["worktree"],
        serde_json::json!({"path": "/w", "branch": "fiber/s_test", "repository": "/r/.git"}),
    );
    let (_looped, _log, _watched, home) = started();
    let lines = log::read(&home.path().join("s_test")).unwrap();
    assert_eq!(lines[0].kind, "session_started");
    assert!(!lines[0].payload.contains_key("worktree"));
}

#[test]
fn an_extension_exec_idle_is_written_once_and_starts_no_turn() {
    let (mut looped, _log, mut watched, _home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::ExtensionExec(exec("git")), &mut input)
        .unwrap();
    assert!(
        input.pieces.is_empty(),
        "extension_exec starts no turn idle"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension_exec arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before extension_exec");
    assert_eq!(line.kind, "extension_exec");
    assert!(line.turn_id.is_none(), "extension_exec carries no turn");
}

#[test]
fn an_extension_exec_mid_turn_is_written_once_and_queues_nothing() {
    let (mut looped, _log, mut watched, _home) = started();
    let turn = contract::TurnId("t_test".into());
    looped
        .admit_running(Delivery::ExtensionExec(exec("git")), &turn)
        .unwrap();
    assert!(
        looped.queued.is_empty(),
        "extension_exec queues nothing mid-turn"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension_exec arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before extension_exec");
    assert_eq!(line.kind, "extension_exec");
    assert!(line.turn_id.is_none(), "extension_exec carries no turn");
}

#[test]
fn an_extension_log_idle_is_live_ephemeral_and_diagnostic_and_starts_no_turn() {
    let (mut looped, _log, mut watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::ExtensionLog(logged("hello")), &mut input)
        .unwrap();
    assert!(input.pieces.is_empty(), "extension_log starts no turn idle");
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension_log arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before extension_log");
    assert_eq!(line.kind, "extension_log");
    assert!(line.turn_id.is_none(), "extension_log carries no turn");
    assert!(line.seq.is_none(), "extension_log is ephemeral");
    assert_eq!(line.payload["extension"], "fiber.test/notes");
    assert_eq!(line.payload["message"], "hello");
    assert!(
        !saved_kinds(&home).contains(&"extension_log".to_owned()),
        "extension_log is never saved"
    );
    assert!(
        diag_text(&home).ends_with("\"message\":\"fiber.test/notes: hello\"}\n"),
        "one line in logs/session-s_test.log"
    );
}

#[test]
fn an_extension_log_mid_turn_is_live_ephemeral_and_diagnostic_and_queues_nothing() {
    let (mut looped, _log, mut watched, home) = started();
    let turn = contract::TurnId("t_test".into());
    looped
        .admit_running(Delivery::ExtensionLog(logged("hello")), &turn)
        .unwrap();
    assert!(
        looped.queued.is_empty(),
        "extension_log queues nothing mid-turn"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension_log arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before extension_log");
    assert_eq!(line.kind, "extension_log");
    assert!(line.turn_id.is_none(), "extension_log carries no turn");
    assert!(line.seq.is_none(), "extension_log is ephemeral");
    assert!(
        !saved_kinds(&home).contains(&"extension_log".to_owned()),
        "extension_log is never saved"
    );
    assert!(
        diag_text(&home).ends_with("\"message\":\"fiber.test/notes: hello\"}\n"),
        "one line in logs/session-s_test.log"
    );
}

fn usage(generation: &str) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.to_owned()),
        model: "scripted/s.json".to_owned(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 3,
        },
        input_bytes: 100,
        input_media: None,
        web_searches: None,
        cost: Some(0.01),
        subscription: None,
        extension: Some("fiber.test/notes".to_owned()),
        origin_session_id: None,
        reviewer: None,
    }
}

/// An extension's `host.model` usage line is written once at the idle drain
/// and starts no turn.
#[test]
fn an_extension_usage_idle_is_written_once_and_starts_no_turn() {
    let (mut looped, _log, mut watched, _home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::ExtensionUsage(usage("g_usage")), &mut input)
        .unwrap();
    assert!(
        input.pieces.is_empty(),
        "extension usage starts no turn idle"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("extension usage arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before extension usage");
    assert_eq!(line.kind, "usage_recorded");
    assert!(line.turn_id.is_none(), "extension usage carries no turn");
    assert_eq!(line.payload["generation_id"], "g_usage");
    assert_eq!(line.payload["extension"], "fiber.test/notes");
}

fn asked(id: &str) -> InteractionRequested {
    InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: Interaction::Confirm {
            prompt: "Overwrite?".into(),
        },
        action_ids: None,
        extension: Some("fiber.test/notes".into()),
        resumes: false,
    }
}

fn resolved(id: &str) -> InteractionResolved {
    InteractionResolved {
        request_id: RequestId(id.into()),
        by: ResolvedBy::Person,
        answer: Answer::Confirmed { confirmed: true },
    }
}

#[test]
fn an_interaction_idle_is_written_once_and_starts_no_turn() {
    let (mut looped, _log, mut watched, _home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped
        .admit_idle(Delivery::Interaction(asked("r_1")), &mut input)
        .unwrap();
    assert!(input.pieces.is_empty(), "interaction starts no turn idle");
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_requested arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_requested");
    assert_eq!(line.kind, "interaction_requested");
    assert!(
        line.turn_id.is_none(),
        "interaction_requested carries no turn"
    );
    assert!(
        line.action_id.is_none(),
        "interaction_requested carries no action"
    );
    assert_eq!(line.payload["extension"], "fiber.test/notes");
}

#[test]
fn an_interaction_mid_turn_is_written_once_and_queues_nothing() {
    let (mut looped, _log, mut watched, _home) = started();
    let turn = TurnId("t_test".into());
    looped
        .admit_running(Delivery::Interaction(asked("r_1")), &turn)
        .unwrap();
    assert!(
        looped.queued.is_empty(),
        "interaction queues nothing mid-turn"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_requested arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_requested");
    assert_eq!(line.kind, "interaction_requested");
    assert!(
        line.turn_id.is_none(),
        "interaction_requested carries no turn"
    );
}

#[test]
fn a_resolved_idle_is_written_then_acked() {
    let (mut looped, _log, mut watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    let (tx, rx) = mpsc::channel();
    let dir = home.path().join("s_test");
    let ack = Ack(Box::new(move |answer| {
        let seen = log::read(&dir)
            .unwrap_or_default()
            .into_iter()
            .map(|line| line.kind)
            .collect::<Vec<_>>();
        let _sent = tx.send((answer.is_ok(), seen));
    }));
    looped
        .admit_idle(Delivery::Resolved(resolved("r_1"), ack), &mut input)
        .unwrap();
    assert!(input.pieces.is_empty(), "resolved starts no turn idle");
    let (accepted, seen) = Deadline::after(DEADLINE).recv(&rx).expect("the ack runs");
    assert!(accepted, "resolved accepts its ack");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("interaction_resolved"),
        "the ack runs only once its line is in the log"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_resolved arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_resolved");
    assert_eq!(line.kind, "interaction_resolved");
    assert!(
        line.turn_id.is_none(),
        "interaction_resolved carries no turn"
    );
}

#[test]
fn a_resolved_mid_turn_is_written_then_acked_and_queues_nothing() {
    let (mut looped, _log, mut watched, home) = started();
    let turn = TurnId("t_test".into());
    let (tx, rx) = mpsc::channel();
    let dir = home.path().join("s_test");
    let ack = Ack(Box::new(move |answer| {
        let seen = log::read(&dir)
            .unwrap_or_default()
            .into_iter()
            .map(|line| line.kind)
            .collect::<Vec<_>>();
        let _sent = tx.send((answer.is_ok(), seen));
    }));
    looped
        .admit_running(Delivery::Resolved(resolved("r_1"), ack), &turn)
        .unwrap();
    assert!(looped.queued.is_empty(), "resolved queues nothing mid-turn");
    let (accepted, seen) = Deadline::after(DEADLINE).recv(&rx).expect("the ack runs");
    assert!(accepted, "resolved accepts its ack");
    assert_eq!(
        seen.last().map(String::as_str),
        Some("interaction_resolved"),
        "the ack runs only once its line is in the log"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_resolved arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_resolved");
    assert_eq!(line.kind, "interaction_resolved");
}

#[test]
fn interaction_lines_while_an_approval_waits_are_written_and_the_wait_goes_on() {
    let (mut looped, _log, mut watched, _home) = started();
    let pending = RequestId("r_approval".into());
    let turn = TurnId("t_test".into());
    let waited = looped
        .take_while_waiting(&pending, Delivery::Interaction(asked("r_1")), &turn)
        .unwrap();
    assert!(matches!(waited, Waited::Again));
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    let waited = looped
        .take_while_waiting(&pending, Delivery::Resolved(resolved("r_1"), ack), &turn)
        .unwrap();
    assert!(matches!(waited, Waited::Again));
    assert!(Deadline::after(DEADLINE).recv(&rx).expect("the ack runs"));
    let wait = Deadline::after(DEADLINE);
    let first = watched
        .recv_timeout(wait.left())
        .expect("interaction_requested arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_requested");
    let second = watched
        .recv_timeout(wait.left())
        .expect("interaction_resolved arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_resolved");
    assert_eq!(first.kind, "interaction_requested");
    assert_eq!(second.kind, "interaction_resolved");
}

fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

fn reply(request_id: &str) -> Delivery {
    Delivery::Reply(
        contract::commands::Reply {
            request_id: RequestId(request_id.into()),
            answer: contract::commands::ReplyAnswer::Approval {
                decision: contract::events::Decision::Allow,
                feedback: None,
                remember: None,
            },
        },
        ignore(),
    )
}

fn prompt() -> Delivery {
    Delivery::Prompt(
        contract::inbox::Message {
            content: Vec::new(),
            sender: contract::shapes::Sender {
                origin: contract::shapes::Origin::Driver,
                command_id: None,
            },
        },
        ignore(),
    )
}

fn steer() -> Delivery {
    Delivery::Steer(
        contract::inbox::Message {
            content: Vec::new(),
            sender: contract::shapes::Sender {
                origin: contract::shapes::Origin::Driver,
                command_id: None,
            },
        },
        ignore(),
    )
}

fn close() -> Delivery {
    Delivery::Close(ignore())
}

fn name(text: &str) -> Delivery {
    Delivery::Name(Name { text: text.into() }, ignore())
}

fn control_with(name: Option<&str>) -> Control {
    Control {
        handoff: None,
        questions: None,
        skill: None,
        name: name.map(str::to_owned),
    }
}

/// The ack's answer, failing at [`DEADLINE`].
fn answered(rx: &mpsc::Receiver<contract::inbox::Answer>) -> contract::inbox::Answer {
    Deadline::after(DEADLINE).recv(rx).expect("the ack runs")
}

fn held_kinds(looped: &Loop) -> Vec<&str> {
    looped
        .deferred
        .iter()
        .map(|held| match held {
            Delivery::Prompt(..) => "prompt",
            Delivery::Steer(..) => "steer",
            Delivery::Reply(reply, _) => {
                if reply.request_id.0 == "r_1" {
                    "reply r_1"
                } else {
                    "reply other"
                }
            }
            Delivery::Close(_) => "close",
            Delivery::SteerDrop(..)
            | Delivery::Name(..)
            | Delivery::Message(_)
            | Delivery::Model(..)
            | Delivery::Reload(..)
            | Delivery::Credential(..)
            | Delivery::Rewind(..)
            | Delivery::Handoff(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::InnerCall(_)
            | Delivery::ExtensionLog(_)
            | Delivery::ExtensionUsage(_)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::Shell(..)
            | Delivery::Cancelled => "other",
        })
        .collect()
}

#[test]
fn take_held_answer_returns_the_matching_reply_and_keeps_the_rest_in_order() {
    let (mut looped, _log, _watched, _home) = started();
    looped.deferred.push_back(prompt());
    looped.deferred.push_back(steer());
    looped.deferred.push_back(reply("r_2"));
    looped.deferred.push_back(reply("r_1"));
    let taken = looped.take_held_answer(&RequestId("r_1".into()));
    assert!(matches!(taken, Some(Delivery::Reply(reply, _)) if reply.request_id.0 == "r_1"));
    assert_eq!(held_kinds(&looped), ["prompt", "steer", "reply other"]);
}

#[test]
fn take_held_answer_prefers_an_earlier_close_to_a_later_reply() {
    let (mut looped, _log, _watched, _home) = started();
    looped.deferred.push_back(reply("r_9"));
    looped.deferred.push_back(close());
    looped.deferred.push_back(reply("r_1"));
    let taken = looped.take_held_answer(&RequestId("r_1".into()));
    assert!(matches!(taken, Some(Delivery::Close(_))));
    assert_eq!(held_kinds(&looped), ["reply other", "reply r_1"]);
}

#[test]
fn take_held_answer_with_only_stale_replies_returns_none_and_keeps_them() {
    let (mut looped, _log, _watched, _home) = started();
    looped.deferred.push_back(reply("r_9"));
    looped.deferred.push_back(reply("r_8"));
    assert!(looped.take_held_answer(&RequestId("r_1".into())).is_none());
    assert_eq!(held_kinds(&looped), ["reply other", "reply other"]);
}

#[test]
fn a_person_name_idle_is_written_then_acked_and_pins() {
    let (mut looped, _log, mut watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    let (tx, rx) = mpsc::channel();
    let dir = home.path().join("s_test");
    let ack = Ack(Box::new(move |answer| {
        let seen = log::read(&dir)
            .unwrap_or_default()
            .into_iter()
            .map(|line| line.kind)
            .collect::<Vec<_>>();
        let _sent = tx.send((answer, seen));
    }));
    looped
        .admit_idle(
            Delivery::Name(
                Name {
                    text: "Mine".into(),
                },
                ack,
            ),
            &mut input,
        )
        .unwrap();
    assert!(input.pieces.is_empty(), "a name starts no turn idle");
    let (answer, seen) = Deadline::after(DEADLINE).recv(&rx).expect("the ack runs");
    assert!(
        matches!(answer, Ok(None)),
        "a name accepts its ack with no answer"
    );
    assert_eq!(
        seen,
        ["session_started", "session_named"],
        "the ack runs only once its line is in the log"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("session_named arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before session_named");
    assert_eq!(line.kind, "session_named");
    assert_eq!(line.payload["name"], "Mine");
    assert_eq!(line.payload["by"], "person");
    assert!(
        line.turn_id.is_none(),
        "the person's session_named carries no turn"
    );
    assert!(
        line.action_id.is_none(),
        "the person's session_named carries no action"
    );
    let Named::Pinned(failure) = looped
        .naming
        .judge(CallStatus::Completed, Some(&control_with(Some("x"))))
    else {
        panic!("the person's name pins the session");
    };
    assert_eq!(failure.code, ErrorCode::NamePinned);
    assert_eq!(failure.message, PINNED);
}

#[test]
fn an_empty_person_name_writes_null_and_unpins() {
    let (mut looped, _log, _watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped.admit_idle(name("Mine"), &mut input).unwrap();
    looped.admit_idle(name(""), &mut input).unwrap();
    assert_eq!(
        saved_kinds(&home),
        ["session_started", "session_named", "session_named"],
    );
    let lines = log::read(&home.path().join("s_test")).unwrap_or_default();
    let cleared = lines
        .iter()
        .filter(|line| line.kind == "session_named")
        .nth(1)
        .expect("the clear writes a second session_named");
    assert!(
        cleared.payload["name"].is_null(),
        "a clear writes name null"
    );
    assert_eq!(cleared.payload["by"], "person");
    let Named::Write(wrote) = looped
        .naming
        .judge(CallStatus::Completed, Some(&control_with(Some("x"))))
    else {
        panic!("the clear unpins the session");
    };
    assert_eq!(wrote.by, NamedBy::Model);
}

#[test]
fn a_person_name_after_close_is_accepted() {
    let (mut looped, _log, _watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    looped.take_close(ignore());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    looped
        .admit_idle(
            Delivery::Name(
                Name {
                    text: "Mine".into(),
                },
                ack,
            ),
            &mut input,
        )
        .unwrap();
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a name after close is accepted"
    );
    assert_eq!(saved_kinds(&home), ["session_started", "session_named"]);
    assert!(
        matches!(
            looped
                .naming
                .judge(CallStatus::Completed, Some(&control_with(Some("x")))),
            Named::Pinned(_)
        ),
        "the name after close pins"
    );
}

#[test]
fn a_person_name_mid_turn_is_written_with_no_turn_and_queues_nothing() {
    let (mut looped, _log, mut watched, home) = started();
    let turn = TurnId("t_test".into());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    looped
        .admit_running(
            Delivery::Name(
                Name {
                    text: "Mine".into(),
                },
                ack,
            ),
            &turn,
        )
        .unwrap();
    assert!(looped.queued.is_empty(), "a name queues nothing mid-turn");
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a name mid-turn is accepted"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("session_named arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before session_named");
    assert_eq!(line.kind, "session_named");
    assert_eq!(line.payload["name"], "Mine");
    assert_eq!(line.payload["by"], "person");
    assert!(
        line.turn_id.is_none(),
        "the person's session_named carries no turn"
    );
    assert_eq!(
        saved_kinds(&home),
        ["session_started", "session_named"],
        "a name writes no steering_queue line"
    );
    assert!(
        matches!(
            looped
                .naming
                .judge(CallStatus::Completed, Some(&control_with(Some("x")))),
            Named::Pinned(_)
        ),
        "the mid-turn name pins"
    );
}

#[test]
fn a_person_name_while_an_approval_waits_is_written_and_the_wait_goes_on() {
    let (mut looped, _log, mut watched, home) = started();
    let pending = RequestId("r_approval".into());
    let turn = TurnId("t_test".into());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    let waited = looped
        .take_while_waiting(
            &pending,
            Delivery::Name(
                Name {
                    text: "Mine".into(),
                },
                ack,
            ),
            &turn,
        )
        .unwrap();
    assert!(matches!(waited, Waited::Again));
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a name during an approval wait is accepted"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("session_named arrives in time")
        .expect("the log outlives the wait")
        .expect("the log ended before session_named");
    assert_eq!(line.kind, "session_named");
    assert_eq!(line.payload["name"], "Mine");
    assert_eq!(line.payload["by"], "person");
    assert_eq!(saved_kinds(&home), ["session_started", "session_named"]);
}

#[test]
fn a_person_name_after_rewound_is_refused_and_writes_nothing() {
    let (mut looped, _log, _watched, home) = started();
    looped.rewound = true;
    let mut input = TurnInput::of(Vec::new());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    looped
        .admit_idle(
            Delivery::Name(
                Name {
                    text: "Mine".into(),
                },
                ack,
            ),
            &mut input,
        )
        .unwrap();
    match answered(&rx) {
        Err(rejection) => {
            assert_eq!(rejection.code, ErrorCode::Closing);
            assert_eq!(rejection.message, REWOUND_CLOSING);
        }
        Ok(_) => panic!("a name after rewound is refused"),
    }
    assert_eq!(saved_kinds(&home), ["session_started"]);
    assert!(
        matches!(
            looped
                .naming
                .judge(CallStatus::Completed, Some(&control_with(Some("x")))),
            Named::Write(_)
        ),
        "a refused name leaves the session unpinned"
    );
}

fn shell_delivery(
    command: &str,
    output: &str,
) -> (Delivery, mpsc::Receiver<contract::inbox::Answer>) {
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    (
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
            ack,
        ),
        rx,
    )
}

#[test]
fn a_sent_shell_idle_is_written_then_acked_and_starts_no_turn() {
    let (mut looped, _log, mut watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    let (delivery, rx) = shell_delivery("echo hi", "hi\n");
    looped.admit_idle(delivery, &mut input).unwrap();
    assert!(input.pieces.is_empty(), "a sent shell starts no turn idle");
    assert!(input.prompts.is_empty(), "a sent shell answers at its take");
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a sent shell is accepted with no answer"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("shell_command arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before shell_command");
    assert_eq!(line.kind, "shell_command");
    assert_eq!(line.payload["command"], "echo hi");
    assert_eq!(line.payload["output"], "hi\n");
    assert_eq!(line.payload["process"]["exit_code"], 0);
    assert!(
        line.turn_id.is_none(),
        "a sent shell_command carries no turn"
    );
    assert!(
        line.action_id.is_none(),
        "a sent shell_command carries no action"
    );
    assert_eq!(saved_kinds(&home), ["session_started", "shell_command"]);
    assert!(
        looped.conversation.is_empty(),
        "the take renders nothing into the conversation"
    );
}

#[test]
fn a_sent_shell_mid_turn_is_written_with_no_turn_and_queues_nothing() {
    let (mut looped, _log, mut watched, home) = started();
    let turn = TurnId("t_test".into());
    let (delivery, rx) = shell_delivery("echo hi", "hi\n");
    looped.admit_running(delivery, &turn).unwrap();
    assert!(
        looped.queued.is_empty(),
        "a sent shell queues nothing mid-turn"
    );
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a sent shell mid-turn is accepted"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("shell_command arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before shell_command");
    assert_eq!(line.kind, "shell_command");
    assert_eq!(line.payload["command"], "echo hi");
    assert!(
        line.turn_id.is_none(),
        "a sent shell_command carries no turn"
    );
    assert!(
        looped.conversation.is_empty(),
        "a mid-turn take never enters that turn's conversation"
    );
    assert_eq!(saved_kinds(&home), ["session_started", "shell_command"]);
}

#[test]
fn three_sent_shells_are_written_in_inbox_order() {
    let (mut looped, _log, _watched, home) = started();
    let mut input = TurnInput::of(Vec::new());
    for command in ["a", "b", "c"] {
        let (delivery, rx) = shell_delivery(command, "out\n");
        looped.admit_idle(delivery, &mut input).unwrap();
        assert!(
            matches!(answered(&rx), Ok(None)),
            "each sent shell is accepted"
        );
    }
    let lines = log::read(&home.path().join("s_test")).unwrap_or_default();
    let commands: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == "shell_command")
        .map(|line| line.payload["command"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(commands, ["a", "b", "c"]);
}

#[test]
fn a_sent_shell_while_an_approval_waits_is_written_and_the_wait_goes_on() {
    let (mut looped, _log, mut watched, home) = started();
    let pending = RequestId("r_approval".into());
    let turn = TurnId("t_test".into());
    let (delivery, rx) = shell_delivery("echo hi", "hi\n");
    let waited = looped
        .take_while_waiting(&pending, delivery, &turn)
        .unwrap();
    assert!(matches!(waited, Waited::Again));
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a sent shell during an approval wait is accepted"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("shell_command arrives in time")
        .expect("the log outlives the wait")
        .expect("the log ended before shell_command");
    assert_eq!(line.kind, "shell_command");
    assert_eq!(line.payload["command"], "echo hi");
    assert_eq!(saved_kinds(&home), ["session_started", "shell_command"]);
}

#[test]
fn a_sent_shell_queued_at_settle_is_written_and_accepted() {
    let (mut looped, _log, mut watched, home) = started();
    looped.cancel.shutdown(143);
    let (delivery, rx) = shell_delivery("echo hi", "hi\n");
    looped.deferred.push_back(delivery);
    looped.settled(Ok(())).unwrap();
    assert!(
        matches!(answered(&rx), Ok(None)),
        "a sent shell at settle is accepted"
    );
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("shell_command arrives in time")
        .expect("the log outlives the settle")
        .expect("the log ended before shell_command");
    assert_eq!(line.kind, "shell_command");
    assert_eq!(line.payload["command"], "echo hi");
    assert_eq!(saved_kinds(&home), ["session_started", "shell_command"]);
}

#[test]
fn a_sent_shell_after_rewound_is_refused_and_writes_nothing() {
    let (mut looped, _log, _watched, home) = started();
    looped.rewound = true;
    let mut input = TurnInput::of(Vec::new());
    let (delivery, rx) = shell_delivery("echo hi", "hi\n");
    looped.admit_idle(delivery, &mut input).unwrap();
    match answered(&rx) {
        Err(rejection) => {
            assert_eq!(rejection.code, ErrorCode::Closing);
            assert_eq!(rejection.message, REWOUND_CLOSING);
        }
        Ok(_) => panic!("a sent shell after rewound is refused"),
    }
    assert_eq!(saved_kinds(&home), ["session_started"]);
}
