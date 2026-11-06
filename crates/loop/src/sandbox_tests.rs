//! The `sandbox` command's take: flip, then report on the notice path.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::SessionId;
use contract::commands::SandboxArgs;
use contract::events::EventKind;
use contract::inbox::{Ack, Delivery};
use log::Log;

use super::{ApplySandbox, Availability, Cause, SANDBOX_OFF, SANDBOX_ON};
use crate::inbox::TurnInput;
use crate::{Loop, Model};
use fakes::{Deadline, NoRules};

const DEADLINE: Duration = Duration::from_secs(10);

fn started() -> (Loop, Arc<Log>, log::Watcher, fakes::TempDir) {
    let home = fakes::TempDir::new("fiber-loop-sandbox");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(home.path().join("credentials")).unwrap();
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
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap();
    let watched = log.watch();
    (looped, log, watched, home)
}

/// The seam answering `available`: records every requested flag.
fn available() -> (ApplySandbox, Arc<std::sync::Mutex<Vec<bool>>>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let apply: ApplySandbox = Arc::new({
        let seen = Arc::clone(&seen);
        move |enabled| {
            seen.lock().unwrap().push(enabled);
            Availability::Available
        }
    });
    (apply, seen)
}

/// Admits one `sandbox` command idle and returns its answer. The toggle's
/// reply is an ephemeral `notice`: it reaches watchers, never the log.
fn admit(looped: &mut Loop, enabled: bool) -> contract::inbox::Answer {
    let mut input = TurnInput::of(Vec::new());
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    looped
        .admit_idle(Delivery::Sandbox(SandboxArgs { enabled }, ack), &mut input)
        .unwrap();
    assert!(
        input.pieces.is_empty(),
        "a sandbox toggle starts no turn idle"
    );
    Deadline::after(DEADLINE).recv(&rx).expect("the ack runs")
}

/// The toggle's reply off the watcher: the next line is its `notice`.
fn replied(watched: &mut log::Watcher) -> String {
    let line = watched
        .recv_timeout(DEADLINE)
        .expect("the notice arrives in time")
        .expect("the log outlives the drain")
        .expect("the log ended before the notice");
    assert_eq!(line.kind(), EventKind::Notice);
    assert!(line.turn_id.is_none(), "the notice carries no turn");
    assert!(line.action_id.is_none(), "the notice carries no action");
    line.payload["message"]
        .as_str()
        .expect("a notice carries a message")
        .to_owned()
}

#[test]
fn enabling_an_available_sandbox_writes_the_on_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let (apply, seen) = available();
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(*seen.lock().unwrap(), [true], "the seam flips first");
    assert_eq!(replied(&mut watched), SANDBOX_ON);
}

#[test]
fn disabling_writes_the_off_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let (apply, seen) = available();
    looped.sandbox_apply = Some(apply);
    let answer = admit(&mut looped, false);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(*seen.lock().unwrap(), [false]);
    assert_eq!(replied(&mut watched), SANDBOX_OFF);
}

#[test]
fn an_unreported_unavailability_writes_its_notice_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    let notice = contract::events::Notice {
        code: contract::ErrorCode::ToolError,
        message: "sandbox unavailable: Landlock ABI is below 2".to_owned(),
        extension: None,
    };
    looped.sandbox_apply = Some(Arc::new(move |enabled| {
        assert!(enabled);
        Availability::Unavailable {
            cause: Cause::AbiBelow2,
            notice: Some(notice.clone()),
        }
    }));
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    assert_eq!(
        replied(&mut watched),
        "sandbox unavailable: Landlock ABI is below 2"
    );
}

#[test]
fn an_already_reported_unavailability_writes_nothing_and_accepts() {
    let (mut looped, _log, mut watched, _home) = started();
    looped.sandbox_apply = Some(Arc::new(|_| Availability::Unavailable {
        cause: Cause::AbiBelow2,
        notice: None,
    }));
    let answer = admit(&mut looped, true);
    assert!(
        matches!(answer, Ok(None)),
        "the toggle accepts with no result"
    );
    // The ack runs after the append, on the same thread, so an empty queue
    // here means nothing was written: no wait, no clock.
    assert!(watched.try_recv().unwrap().is_none(), "no second line");
}

#[test]
fn a_toggle_after_close_rejects_closing() {
    let (mut looped, _log, _watched, _home) = started();
    let (apply, _) = available();
    looped.sandbox_apply = Some(apply);
    looped.closing = true;
    let answer = admit(&mut looped, true);
    assert!(
        matches!(
            &answer,
            Err(rejection) if rejection.code == contract::ErrorCode::Closing
        ),
        "after close the toggle is rejected: {answer:?}"
    );
}

#[test]
fn a_toggle_without_a_switch_is_refused() {
    let (mut looped, _log, _watched, _home) = started();
    assert!(looped.sandbox_apply.is_none());
    let answer = admit(&mut looped, true);
    assert!(
        matches!(
            &answer,
            Err(rejection) if rejection.code == contract::ErrorCode::InvalidArguments
        ),
        "with no seam the toggle is refused: {answer:?}"
    );
}

#[test]
fn every_cause_names_itself() {
    for (cause, name) in [
        (Cause::UnsupportedPlatform, "UnsupportedPlatform"),
        (Cause::AbiBelow2, "AbiBelow2"),
        (Cause::SandboxExecUnavailable, "SandboxExecUnavailable"),
        (Cause::DenyInsideWrite, "DenyInsideWrite"),
        (Cause::UnresolvableDeny, "UnresolvableDeny"),
        (Cause::UnresolvedWrite, "UnresolvedWrite"),
    ] {
        assert_eq!(format!("{cause:?}"), name);
    }
}
