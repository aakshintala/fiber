//! Admission, atomic preparation and fatal writes at the reload boundary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use super::*;
use contract::events::PreambleReason;
use contract::inbox::Answer;
use contract::rules::{Rules, RulesError, StandingRules};
use contract::{ErrorCode, SessionId, TurnId};
use fakes::clock::FakeClock;
use fakes::{Deadline, Scripted, ScriptedProvider};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

const WAIT: Duration = Duration::from_secs(5);
thread_local! { static FAIL_RELOAD_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(super) fn fail_reload_write() -> bool {
    FAIL_RELOAD_WRITE.with(|fail| fail.replace(false))
}

#[test]
fn failed_reload_append_drops_the_ack_without_accepting_it() {
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| Ok(prepared())));
    FAIL_RELOAD_WRITE.with(|fail| fail.set(true));
    let (ack, rx) = ack();
    assert!(w.looped.take_reload(ack).is_err());
    assert!(matches!(
        Deadline::after(WAIT).recv(&rx),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

#[test]
fn a_deferred_reload_runs_once_before_the_next_prompt() {
    let mut w = world();
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    w.looped = w.looped.reloader(Arc::new(move |_| {
        called.fetch_add(1, Ordering::SeqCst);
        Ok(prepared())
    }));
    let (ack, rx) = ack();
    let offer = contract::events::RepositoryCodeOffered {
        request_id: contract::RequestId("r_reload".into()),
        items: Vec::new(),
    };
    let mut folded = crate::offer::Folded::default();
    folded.fold(&contract::events::Event::RepositoryCodeOffered(
        offer.clone(),
    ));
    w.looped.repository = crate::offer::State::resumed(folded);
    w.looped
        .log
        .append(
            &contract::events::Event::Clients(contract::events::Clients { count: 1 }),
            None,
            None,
        )
        .unwrap();
    w.looped.answerable = true;
    w.looped = w.looped.repository_code(Arc::new(EmptyRepository));
    w.inbox
        .send(contract::inbox::Delivery::Reload(ack))
        .unwrap();
    w.inbox
        .send(contract::inbox::Delivery::Reply(
            contract::commands::Reply {
                request_id: offer.request_id,
                answer: contract::commands::ReplyAnswer::Decisions {
                    decisions: Vec::new(),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    assert!(w.looped.reraise_offer().unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        w.looped.deferred.front(),
        Some(contract::inbox::Delivery::Reload(_))
    ));
    w.inbox
        .send(contract::inbox::Delivery::Close(Ack(Box::new(|_| {}))))
        .unwrap();
    assert!(w.looped.turn().unwrap().is_none());
    assert!(Deadline::after(WAIT).recv(&rx).unwrap().unwrap().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        kinds(&w.looped),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "reloaded",
            "extensions_loaded",
            "preamble_built"
        ]
    );
}
struct EmptyRepository;
impl contract::repository::RepositoryCode for EmptyRepository {
    fn unapproved(&self) -> Result<Vec<contract::repository::Unapproved>, Failure> {
        Ok(Vec::new())
    }
    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<contract::repository::Decided, Failure> {
        unreachable!("the pending offer has no items")
    }
}
struct EmptyRules;
impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }
    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

struct World {
    looped: Loop,
    inbox: mpsc::Sender<contract::inbox::Delivery>,
    _home: fakes::TempDir,
}
fn world() -> World {
    let home = fakes::TempDir::new("fiber-reload");
    let clock = FakeClock::new();
    let log = Arc::new(
        log::Log::create(home.path(), SessionId("s_reload".into()), clock.clone()).unwrap(),
    );
    let (sender, inbox) = mpsc::channel();
    let looped = Loop::start(
        crate::Session {
            log,
            inbox,
            provider: Arc::new(ScriptedProvider::new([Scripted::text("done")])),
            model: crate::Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::PromptInputs::new(
                home.path().into(),
                "/bin/sh".into(),
                "events.jsonl".into(),
                clock,
                fakes::CONTEXT_WINDOW,
            ),
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.path().display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        None,
    )
    .unwrap();
    World {
        looped,
        inbox: sender,
        _home: home,
    }
}
fn prepared() -> ReloadPrepared {
    ReloadPrepared {
        prompt: PromptPatch {
            system: Some("new system".into()),
            append: None,
            extensions: Vec::new(),
            extension_dirs: Vec::new(),
            extension_sections: Vec::new(),
            skills_disabled: Vec::new(),
            cache_lifetime: CacheLifetime::FiveMinutes,
        },
        tools: Vec::new(),
        extensions: Vec::new(),
        notices: Vec::new(),
        servers: Vec::new(),
        hooks: None,
        reviewer: Err(Failure {
            code: ErrorCode::NoModel,
            message: "new reviewer failure".into(),
            retry_after_ms: None,
            provider: None,
        }),
        reviewer_observe: None,
        credential_files: vec![PathBuf::from("/read/credential")],
        settings: ReloadSettings {
            reviewer_notes: "new notes".into(),
            limits: BlockLimits::default(),
            retry: Retry::default(),
            handoff: HandoffSettings::default(),
            budget: Some(10.0),
            idle_exit: Some(Duration::from_secs(100)),
            warm_cap: Some(2),
        },
        installed: Box::new(|| {}),
    }
}
fn ack() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}
fn kinds(looped: &Loop) -> Vec<String> {
    log::read(looped.log.dir())
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

#[test]
fn closing_busy_and_rewound_refuse_without_preparing_or_writing() {
    for state in ["closing", "busy", "rewound"] {
        let mut w = world();
        let called = Arc::new(AtomicUsize::new(0));
        let calls = called.clone();
        w.looped = w.looped.reloader(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(prepared())
        }));
        let (ack, rx) = ack();
        match state {
            "closing" => {
                w.looped.closing = true;
                w.looped.take_reload(ack).unwrap();
            }
            "busy" => w
                .looped
                .admit_running(
                    contract::inbox::Delivery::Reload(ack),
                    &TurnId("t_busy".into()),
                )
                .unwrap(),
            _ => {
                w.looped.rewound = true;
                w.looped
                    .admit_idle(
                        contract::inbox::Delivery::Reload(ack),
                        &mut crate::inbox::TurnInput::of(Vec::new()),
                    )
                    .unwrap();
            }
        }
        let rejection = Deadline::after(WAIT).recv(&rx).unwrap().unwrap_err();
        if state == "busy" {
            assert_eq!(rejection.message, "A turn is running; reload once it ends.");
        }
        assert_eq!(
            rejection.code,
            if state == "busy" {
                ErrorCode::Busy
            } else {
                ErrorCode::Closing
            }
        );
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert_eq!(kinds(&w.looped), ["session_started"]);
    }
}

#[test]
fn no_reloader_is_invalid_arguments() {
    let mut w = world();
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert_eq!(
        Deadline::after(WAIT).recv(&rx).unwrap().unwrap_err().code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

#[test]
fn applied_reload_swaps_settings_before_installation_and_accepts_after_the_event() {
    let mut w = world();
    let log = w.looped.log.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    w.looped = w.looped.reloader(Arc::new(move |_| {
        let mut next = prepared();
        let seen = recorded.clone();
        let log = log.clone();
        next.installed = Box::new(move || seen.lock().unwrap().push(log.count()));
        Ok(next)
    }));
    let (tx, rx) = mpsc::channel();
    let log = w.looped.log.clone();
    w.looped
        .take_reload(Ack(Box::new(move |answer| {
            assert_eq!(log.count(), 2);
            tx.send(answer).unwrap();
        })))
        .unwrap();
    assert!(Deadline::after(WAIT).recv(&rx).unwrap().unwrap().is_none());
    assert_eq!(*seen.lock().unwrap(), [1]);
    assert_eq!(
        kinds(&w.looped),
        [
            "session_started",
            "reloaded",
            "extensions_loaded",
            "preamble_built"
        ]
    );
    assert_eq!(w.looped.preamble_reason, PreambleReason::Reload);
    assert_eq!(w.looped.prompt.system.as_deref(), Some("new system"));
    assert_eq!(w.looped.prompt.cache_lifetime, CacheLifetime::FiveMinutes);
    assert_eq!(w.looped.reviewer_notes, "new notes");
    assert_eq!(
        w.looped.reviewer.as_ref().err().unwrap().message,
        "new reviewer failure"
    );
    assert_eq!(w.looped.reviewer_sent, None);
    assert_eq!(
        w.looped.credential_files,
        [PathBuf::from("/read/credential")]
    );
    assert_eq!(w.looped.budget, Some(10.0));
    assert_eq!(w.looped.idle_exit, Some(Duration::from_secs(100)));
    assert!(!w.looped.opened);
}

#[test]
fn failed_preamble_append_ends_after_the_reload_ack_is_accepted() {
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| Ok(prepared())));
    crate::timing_tests::arm_fail_preamble_write();
    let (ack, rx) = ack();
    assert!(w.looped.take_reload(ack).is_err());
    assert!(Deadline::after(WAIT).recv(&rx).unwrap().unwrap().is_none());
    assert_eq!(
        kinds(&w.looped),
        ["session_started", "reloaded", "extensions_loaded"]
    );
}

#[test]
fn a_preparer_cancel_rejects_without_applying() {
    // debt: Part 1 tests the cancellation rejection only; Part 2's shutdown
    // wiring must prove that cancelling the prepare wait ends the loop.
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| {
        Err(Rejection {
            code: ErrorCode::Closing,
            message: "shutdown".into(),
        })
    }));
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert_eq!(
        Deadline::after(WAIT).recv(&rx).unwrap().unwrap_err().code,
        ErrorCode::Closing
    );
    assert_eq!(kinds(&w.looped), ["session_started"]);
}
