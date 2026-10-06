//! `extension_exec` is written once at the drain that takes it, idle or
//! mid-turn, and starts no turn (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::events::ExtensionExec;
use contract::inbox::Delivery;
use contract::shapes::Process;
use log::Log;

use super::TurnInput;
use crate::{Loop, Model};

const DEADLINE: Duration = Duration::from_secs(10);

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
        ),
        rx,
        Vec::new(),
        crate::Permissions {
            workspace: workspace.display().to_string(),
            credentials,
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
    let mut input = TurnInput {
        pieces: Vec::new(),
        prompt: None,
    };
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
