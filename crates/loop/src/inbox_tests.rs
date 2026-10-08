//! `extension_exec` is written once at the drain that takes it, idle or
//! mid-turn, and starts no turn (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::events::{
    Answer, ExtensionExec, ExtensionLog, Interaction, InteractionRequested, InteractionResolved,
    ResolvedBy,
};
use contract::inbox::{Ack, Delivery};
use contract::shapes::Process;
use contract::{RequestId, TurnId};
use log::Log;

use super::{TurnInput, Waited};
use crate::{Loop, Model};

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
        Arc::clone(&log),
        Arc::new(fakes::ScriptedProvider::new(Vec::new())),
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
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
            fakes::CONTEXT_WINDOW,
        ),
        rx,
        Vec::new(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials,
            credential_files: Vec::new(),
            rules: Arc::new(NoRules),
        },
    )
    .unwrap();
    let watched = log.watch();
    (looped, log, watched, home)
}

struct NoRules;

impl contract::rules::Rules for NoRules {
    fn read(&self) -> Result<contract::rules::StandingRules, contract::rules::RulesError> {
        Ok(contract::rules::StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), contract::rules::RulesError> {
        Ok(())
    }
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
    let (accepted, seen) = rx.recv_timeout(DEADLINE).expect("the ack runs");
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
    let (accepted, seen) = rx.recv_timeout(DEADLINE).expect("the ack runs");
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
    assert!(rx.recv_timeout(DEADLINE).expect("the ack runs"));
    let first = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_requested arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_requested");
    let second = watched
        .recv_timeout(DEADLINE)
        .expect("interaction_resolved arrives in time")
        .expect("the log outlives the turn")
        .expect("the log ended before interaction_resolved");
    assert_eq!(first.kind, "interaction_requested");
    assert_eq!(second.kind, "interaction_resolved");
}
