//! #1411: an ask taken after a cancel landed is declined, with no
//! `interaction_requested` written for it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::thread;

use contract::clock::Wake;
use contract::events::Interaction;
use contract::inbox::Delivery;
use contract::rules::{Rules, RulesError, StandingRules};
use contract::tool::{Answered, Ask, Asking};
use contract::{ActionId, Envelope, SessionId, TurnId};
use log::Log;

use crate::progress::{SharedWake, Stream};
use crate::{Loop, Model};

/// Rules that hold nothing.
struct Still;

impl Rules for Still {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A wake that drops every wake-up.
struct Awake;

impl Wake for Awake {
    fn wake(&self) {}
}

fn confirm() -> Interaction {
    Interaction::Confirm {
        prompt: "Deploy?".into(),
    }
}

fn asking() -> Asking {
    Asking {
        interaction: confirm(),
        action_ids: Vec::new(),
        until: None,
        check: None,
        suspends: false,
    }
}

/// An answerable loop with an inbox wake when `woken`, holding its scratch
/// directory alive in `home`.
fn looped(answerable: bool, woken: bool) -> (Loop, fakes::TempDir) {
    let home = fakes::TempDir::new("fiber-interactions-cancel");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
    );
    let (_, rx) = mpsc::channel::<Delivery>();
    let rules: Arc<dyn Rules> = Arc::new(Still);
    let mut looped = Loop::start(
        log,
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
            credentials: home.path().join("credentials"),
            credential_files: Vec::new(),
            rules,
        },
        None,
    )
    .unwrap()
    .answerable(answerable);
    if woken {
        looped = looped.inbox_wake(Arc::new(Awake));
    }
    (looped, home)
}

fn durable(home: &fakes::TempDir) -> Vec<Envelope> {
    log::read(&home.path().join("s_test"))
        .unwrap()
        .into_iter()
        .filter(Envelope::is_durable)
        .collect()
}

fn kinds(lines: &[Envelope]) -> Vec<String> {
    lines.iter().map(|line| line.kind.clone()).collect()
}

#[test]
fn a_cancel_landing_before_the_take_declines_the_ask() {
    let (mut looped, home) = looped(true, true);
    assert!(looped.cancel.arm());
    let wake = Arc::new(SharedWake::default());
    let action = ActionId("a_1".into());
    let stream = Arc::new(Stream::new(Arc::clone(&wake), action.clone(), true));
    // A running call raises from its own thread and blocks, as it does.
    let asker = Arc::clone(&stream);
    let answered = thread::spawn(move || asker.ask(asking()));
    // Each raise bumps the wake, so the park never misses one.
    let clock = fakes::clock::FakeClock::new();
    loop {
        if format!("{:?}", stream.asking()).contains("\"raised\"") {
            break;
        }
        wake.park(clock.as_ref(), None);
    }
    // The cancel lands after the take, before the decision read.
    let cancel = Arc::clone(&looped.cancel);
    super::AFTER_TAKE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(cancel.cancel());
        }) as Box<dyn FnOnce()>);
    });
    let turn = TurnId("t_1".into());
    let calls = [(&action, Some(stream.as_ref()))];
    looped.serve_interactions(&calls, &turn).unwrap();
    let lines = durable(&home);
    assert_eq!(kinds(&lines), ["session_started", "interaction_resolved"]);
    let resolved = &lines[1];
    assert_eq!(resolved.payload["by"], "fiber");
    assert_eq!(resolved.payload["declined"], true);
    assert!(stream.asking().pending().is_none());
    assert_eq!(answered.join().unwrap(), Answered::NoAnswer);
}

#[test]
fn unanswerable_when_no_answer_can_come() {
    // (answerable, inbox wake, cancelled, unanswerable): each row flips
    // one disjunct of `interactions_unanswerable`.
    let rows = [
        (true, true, false, false),
        (false, true, false, true),
        (true, false, false, true),
        (true, true, true, true),
    ];
    for (answerable, woken, cancelled, expected) in rows {
        let (looped, _home) = looped(answerable, woken);
        if cancelled {
            assert!(looped.cancel.arm());
            assert!(looped.cancel.cancel());
        }
        assert_eq!(
            looped.interactions_unanswerable(),
            expected,
            "answerable={answerable} woken={woken} cancelled={cancelled}"
        );
    }
}
