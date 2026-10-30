//! One call's ask slot: a raise reaches the loop thread and wakes it, an
//! answer releases the asker, and the release guard leaves no asker
//! blocked, now or later. Every receive is bounded, so a block fails the
//! test instead of hanging it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::clock::Wake;
use contract::commands::ReplyAnswer;
use contract::events::{
    Answer, Event, Interaction, InteractionRequested, InteractionResolved, ResolvedBy,
    ToolCallStarted,
};
use contract::inbox::Delivery;
use contract::shapes::{DeclaredEffects, Effect, True};
use contract::tool::{Answered, Ask, Asking};
use contract::{ActionId, Envelope, RequestId, SessionId, TurnId};

use super::{AskSlot, Fitted, Release};
use crate::progress::{SharedWake, Stream};
use crate::{Loop, Model};
use fakes::Deadline;
use log::Log;

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

/// A wake that reports each wake on a channel.
struct Signal(Mutex<mpsc::Sender<()>>);

impl Wake for Signal {
    fn wake(&self) {
        let _sent = self.0.lock().unwrap().send(());
    }
}

fn signal() -> (Arc<Signal>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    (Arc::new(Signal(Mutex::new(tx))), rx)
}

/// Asks `times` times on `slot` from a helper thread, reporting each
/// answer.
fn ask_on(slot: &Arc<AskSlot>, wake: &Arc<Signal>, times: usize) -> mpsc::Receiver<Answered> {
    let (tx, rx) = mpsc::channel();
    let slot = Arc::clone(slot);
    let wake = Arc::clone(wake);
    thread::spawn(move || {
        for _ in 0..times {
            let _sent = tx.send(slot.ask(asking(), wake.as_ref()));
        }
    });
    rx
}

/// Asks through `stream` from a helper thread, reporting the answer.
fn ask_stream(stream: &Arc<Stream>) -> mpsc::Receiver<Answered> {
    let (tx, rx) = mpsc::channel();
    let stream = Arc::clone(stream);
    thread::spawn(move || {
        let _sent = tx.send(stream.ask(asking()));
        let _again = tx.send(stream.ask(asking()));
    });
    rx
}

fn confirmed() -> Answered {
    Answered::Reply(Answer::Confirmed { confirmed: true })
}

#[test]
fn a_raise_is_taken_once_and_wakes_the_loop_thread() {
    let slot = Arc::new(AskSlot::default());
    let (wake, woken) = signal();
    let _answers = ask_on(&slot, &wake, 1);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&woken)
        .expect("the raise wakes");
    let raised = slot.take_raised().expect("the raise is there");
    assert_eq!(raised.interaction, confirm());
    assert!(slot.take_raised().is_none(), "a raise is taken once");
    slot.close();
}

#[test]
fn resolve_releases_the_asker_with_that_answer() {
    let slot = Arc::new(AskSlot::default());
    let (wake, woken) = signal();
    let answers = ask_on(&slot, &wake, 1);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&woken)
        .expect("the raise wakes");
    let raised = slot.take_raised().unwrap();
    slot.pend(
        RequestId("r_1".into()),
        raised.interaction,
        None,
        None,
        false,
    );
    assert_eq!(slot.pending(), Some((RequestId("r_1".into()), None)));
    slot.resolve(confirmed());
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answers).unwrap(),
        confirmed()
    );
    assert_eq!(slot.pending(), None);
}

#[test]
fn the_release_frees_a_raised_and_a_pending_ask() {
    let wake = Arc::new(SharedWake::default());
    let raised = Arc::new(Stream::new(Arc::clone(&wake), ActionId("a_1".into()), true));
    let pending = Arc::new(Stream::new(Arc::clone(&wake), ActionId("a_2".into()), true));
    let mut release = Release::default();
    release.add(Arc::clone(&raised));
    release.add(Arc::clone(&pending));
    let raised_answers = ask_stream(&raised);
    let pending_answers = ask_stream(&pending);
    // Wait until both asks are raised, then take only the second. Each
    // raise bumps the wake, so the park never misses one.
    let clock = fakes::clock::FakeClock::new();
    let mut taken = None;
    let taken = loop {
        if taken.is_none() {
            taken = pending.asking().take_raised();
        }
        let first_raised = format!("{:?}", raised.asking()).contains("\"raised\"");
        if first_raised && let Some(asked) = taken.take() {
            break asked;
        }
        wake.park(clock.as_ref(), None);
    };
    pending.asking().pend(
        RequestId("r_2".into()),
        taken.interaction,
        None,
        None,
        false,
    );
    drop(release);
    for answers in [raised_answers, pending_answers] {
        let wait = Deadline::after(MUST_SUCCEED_WITHIN);
        assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
        // A second ask on a closed slot returns at once too.
        assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
    }
}

#[test]
fn after_the_release_a_first_ask_returns_no_answer_at_once() {
    let wake = Arc::new(SharedWake::default());
    let stream = Arc::new(Stream::new(Arc::clone(&wake), ActionId("a_1".into()), true));
    let mut release = Release::default();
    release.add(Arc::clone(&stream));
    drop(release);
    let answers = ask_stream(&stream);
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
    assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
    assert!(
        stream.asking().take_raised().is_none(),
        "nothing was raised"
    );
}

#[test]
fn a_second_ask_after_a_closed_release_returns_no_answer_at_once() {
    let slot = Arc::new(AskSlot::default());
    let (wake, woken) = signal();
    let answers = ask_on(&slot, &wake, 2);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&woken)
        .expect("the raise wakes");
    slot.close();
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
    assert_eq!(wait.recv(&answers).unwrap(), Answered::NoAnswer);
    assert!(
        woken.try_recv().is_err(),
        "an ask on a closed slot wakes nobody"
    );
}

#[test]
fn one_call_asks_twice_in_sequence() {
    let slot = Arc::new(AskSlot::default());
    let (wake, woken) = signal();
    let answers = ask_on(&slot, &wake, 2);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&woken)
        .expect("the first raise wakes");
    slot.take_raised().unwrap();
    slot.resolve(confirmed());
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answers).unwrap(),
        confirmed()
    );
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&woken)
        .expect("the second raise wakes");
    slot.take_raised().unwrap();
    slot.resolve(Answered::NoAnswer);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&answers).unwrap(),
        Answered::NoAnswer
    );
}

#[test]
fn a_reply_fits_only_its_pending_request_and_passes_the_check() {
    let slot = AskSlot::default();
    let yes = ReplyAnswer::Confirmed { confirmed: true };
    let request = RequestId("r_1".into());
    assert_eq!(slot.fit(&request, &yes), Fitted::NotThis, "nothing pending");
    let refuse: contract::tool::Check =
        Box::new(|answer| *answer != Answer::Confirmed { confirmed: true });
    slot.pend(request.clone(), confirm(), None, Some(refuse), false);
    assert_eq!(
        slot.fit(&RequestId("r_other".into()), &yes),
        Fitted::NotThis
    );
    assert_eq!(
        slot.fit(&request, &ReplyAnswer::Text { text: "x".into() }),
        Fitted::Unfit,
        "the kind does not fit"
    );
    assert_eq!(slot.fit(&request, &yes), Fitted::Unfit, "the check refuses");
    assert_eq!(
        slot.fit(&request, &ReplyAnswer::Confirmed { confirmed: false }),
        Fitted::Fits(Answer::Confirmed { confirmed: false })
    );
    let declined = ReplyAnswer::Declined { declined: True };
    let never: contract::tool::Check = Box::new(|_| false);
    slot.pend(request.clone(), confirm(), None, Some(never), false);
    assert_eq!(
        slot.fit(&request, &declined),
        Fitted::Fits(Answer::Declined { declined: True }),
        "a decline never reaches the check"
    );
}

#[test]
fn closing_keeps_an_answer_not_yet_taken_and_frees_a_pending_ask() {
    let slot = AskSlot::default();
    slot.pend(RequestId("r_1".into()), confirm(), None, None, false);
    slot.resolve(confirmed());
    slot.close();
    let shown = format!("{slot:?}");
    assert!(shown.contains("\"resolved\""), "{shown}");
    let pending = AskSlot::default();
    pending.pend(RequestId("r_2".into()), confirm(), None, None, false);
    pending.close();
    assert_eq!(pending.pending(), None, "a closed slot holds no request");
}

#[test]
fn a_pending_slot_reports_whether_its_ask_suspends() {
    let slot = AskSlot::default();
    assert!(!slot.pending_suspends(), "nothing pending");
    slot.pend(RequestId("r_1".into()), confirm(), None, None, true);
    assert!(slot.pending_suspends());
    slot.resolve(confirmed());
    assert!(!slot.pending_suspends(), "an answered ask is not pending");
    let plain = AskSlot::default();
    plain.pend(RequestId("r_2".into()), confirm(), None, None, false);
    assert!(!plain.pending_suspends());
}

#[test]
fn reraise_binds_the_next_raise_only() {
    let slot = AskSlot::default();
    assert_eq!(slot.take_reraise(), None, "nothing bound");
    slot.reraise(RequestId("r_7".into()));
    assert_eq!(slot.take_reraise(), Some(RequestId("r_7".into())));
    assert_eq!(slot.take_reraise(), None, "the raise after mints its own");
}

/// A session log in a scratch directory, removed on drop.
struct Logged {
    _root: fakes::TempDir,
    log: Log,
}

impl Logged {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-suspend-lookup");
        let log = Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        Self { _root: root, log }
    }

    fn append(&self, event: &Event, turn: &TurnId, action: Option<&ActionId>) -> Envelope {
        self.log
            .append(event, Some(turn.clone()), action.cloned())
            .unwrap()
    }
}

fn started() -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects: vec![Effect::Reads],
            reversible: true,
            paths: None,
            confined: false,
        },
        arguments: None,
        changed_by: None,
    })
}

/// A question the resume may raise again, naming `action`, under `id`.
fn resumable(id: &str, action: &ActionId) -> Event {
    Event::InteractionRequested(InteractionRequested {
        request_id: RequestId(id.into()),
        interaction: confirm(),
        action_ids: Some(vec![action.clone()]),
        extension: None,
        resumes: true,
    })
}

fn declined(id: &str) -> Event {
    Event::InteractionResolved(InteractionResolved {
        request_id: RequestId(id.into()),
        by: ResolvedBy::Fiber,
        answer: Answer::Declined { declined: True },
    })
}

/// The request [`crate::suspend::interaction_pending`] finds for `id`.
fn pending(lines: &[Envelope], id: &str) -> Option<(InteractionRequested, TurnId, ActionId)> {
    crate::suspend::interaction_pending(lines, &RequestId(id.into())).unwrap()
}

#[test]
fn a_request_never_written_is_not_pending_on_resume() {
    // A watcher saw the request, but it never reached the log: only
    // durable lines count, so the resume raises nothing again.
    let logged = Logged::new();
    let turn = TurnId("t_1".into());
    let action = ActionId("a_1".into());
    let first = logged.append(&started(), &turn, Some(&action));
    let mut aired = logged.append(&resumable("r_1", &action), &turn, None);
    aired.seq = None;
    assert!(pending(&[first, aired], "r_1").is_none());
}

#[test]
fn a_later_request_for_another_id_does_not_shadow_the_pending_one() {
    let logged = Logged::new();
    let turn = TurnId("t_1".into());
    let action = ActionId("a_1".into());
    let lines = vec![
        logged.append(&started(), &turn, Some(&action)),
        logged.append(&resumable("r_keep", &action), &turn, None),
        logged.append(&resumable("r_other", &action), &turn, None),
    ];
    let (asked, _, _) = pending(&lines, "r_keep").expect("the pending request stands");
    assert_eq!(asked.request_id, RequestId("r_keep".into()));
}

#[test]
fn an_older_resolved_request_for_another_id_does_not_clear_the_pending_one() {
    let logged = Logged::new();
    let turn = TurnId("t_1".into());
    let action = ActionId("a_1".into());
    let lines = vec![
        logged.append(&started(), &turn, Some(&action)),
        logged.append(&resumable("r_old", &action), &turn, None),
        logged.append(&declined("r_old"), &turn, None),
        logged.append(&resumable("r_keep", &action), &turn, None),
    ];
    let (asked, _, _) = pending(&lines, "r_keep").expect("the pending request stands");
    assert_eq!(asked.request_id, RequestId("r_keep".into()));
}

/// Standing rules that remember nothing.
struct Still;

impl contract::rules::Rules for Still {
    fn read(&self) -> Result<contract::rules::StandingRules, contract::rules::RulesError> {
        Ok(contract::rules::StandingRules::default())
    }

    fn remember(
        &self,
        _tool: &str,
        _prefix: &str,
        _session: &SessionId,
    ) -> Result<(), contract::rules::RulesError> {
        Ok(())
    }
}

/// A wake that drops every wake-up.
struct Awake;

impl Wake for Awake {
    fn wake(&self) {}
}

/// A loop with the fake clock, a 60 s idle delay and an open inbox: the
/// clock and the sender stay alive in the test.
fn suspendable_loop() -> (
    Loop,
    fakes::TempDir,
    mpsc::Sender<Delivery>,
    std::sync::Arc<fakes::clock::FakeClock>,
) {
    let home = fakes::TempDir::new("fiber-suspend-idle");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let log = Arc::new(
        Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
    );
    let (inbox, rx) = mpsc::channel::<Delivery>();
    let rules: Arc<dyn contract::rules::Rules> = Arc::new(Still);
    let looped = Loop::start(
        crate::Session {
            log,
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
                rules,
            },
        },
        None,
    )
    .unwrap()
    .idle_exit(Some(Duration::from_secs(60)))
    .inbox_wake(Arc::new(Awake));
    (looped, home, inbox, clock)
}

/// A suspending ask on `stream` under `request`.
fn suspend_on(stream: &Stream, request: &str) {
    stream
        .asking()
        .pend(RequestId(request.into()), confirm(), None, None, true);
}

#[test]
fn a_changed_suspendable_request_restarts_the_idle_deadline() {
    let (mut looped, _home, _held, clock) = suspendable_loop();
    let wake = Arc::new(SharedWake::default());
    let action = ActionId("a_1".into());
    let stream = Stream::new(Arc::clone(&wake), action.clone(), true);
    let calls = [(&action, Some(&stream))];
    let turn = TurnId("t_1".into());
    let mut suspend = crate::interactions::Suspend::default();
    // Each wait takes what is already queued and returns at once: the
    // inbox stays empty with its sender held, and `until` is now.
    let wait = |looped: &mut Loop, suspend: &mut crate::interactions::Suspend| {
        looped
            .wait_step(&wake, &calls, Some(clock.now()), suspend, &turn)
            .unwrap()
    };
    suspend_on(&stream, "r_old");
    assert!(!wait(&mut looped, &mut suspend), "the step waits");
    // The answer ends the old ask and the call asks again, with no pass
    // of the loop in between: the step suspends on the new request.
    // No code under test waits on the clock here: wait_step runs on the test thread with until = now, which records no park (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(50));
    stream
        .asking()
        .resolve(Answered::Reply(Answer::Confirmed { confirmed: true }));
    suspend_on(&stream, "r_new");
    assert!(!wait(&mut looped, &mut suspend), "the step waits");
    // Past the old request's deadline, still before the new one's: the
    // step waits on, rather than suspending with the stale deadline.
    // No code under test waits on the clock here: wait_step runs on the test thread with until = now, which records no park (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(20));
    assert!(
        !wait(&mut looped, &mut suspend),
        "the deadline counts from the new request"
    );
}

/// A tool whose worker reports that it runs, then waits for its
/// cancellation, recording whether it saw it before returning.
struct SeesCancel {
    started: mpsc::SyncSender<()>,
    saw: Arc<AtomicBool>,
}

struct CancelledWake(mpsc::SyncSender<()>);

impl Wake for CancelledWake {
    fn wake(&self) {
        let _sent = self.0.send(());
    }
}

impl contract::tool::Tool for SeesCancel {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "inner".into(),
            description: "An inner tool.".into(),
            input_schema: serde_json::json!({"type": "object"}),
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
                confined: false,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        cancel: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        // The watcher subscribes before the test can drop the release:
        // that drop only happens after the start signal sent below.
        let (woke_tx, woke) = mpsc::sync_channel::<()>(1);
        let wake: Arc<dyn Wake> = Arc::new(CancelledWake(woke_tx));
        cancel.subscribe(Arc::downgrade(&wake));
        self.started.send(()).unwrap();
        let saw = Deadline::after(MUST_SUCCEED_WITHIN).recv(&woke).is_ok();
        drop(wake);
        self.saw.store(saw, Ordering::SeqCst);
        contract::tool::Output::default()
    }
}

#[test]
fn dropping_the_release_cancels_each_inner_call_it_holds() {
    use contract::tool::InnerCalls as _;

    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let home = fakes::TempDir::new("fiber-release-cancels-inner");
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
    let turn = Arc::new(crate::TurnCancel::default());
    assert!(turn.arm());
    let (woke_tx, woke) = signal();
    let notice: Arc<dyn Wake> = woke_tx;
    let wake = Arc::new(SharedWake::default());
    wake.forward(Some(notice));
    let anchor_id = ActionId("a_outer".into());
    let slot = Arc::new(crate::inner_calls::InnerSlot::new(
        Arc::clone(&wake),
        anchor_id.clone(),
        "outer".into(),
        0,
    ));
    let anchor = Arc::new(
        Stream::new(Arc::clone(&wake), anchor_id.clone(), true).with_inner(Arc::clone(&slot)),
    );
    let (started_tx, started) = mpsc::sync_channel::<()>(1);
    let saw = Arc::new(AtomicBool::new(false));
    let tool = Arc::new(SeesCancel {
        started: started_tx,
        saw: Arc::clone(&saw),
    });
    let (_inbox, rx) = mpsc::channel();
    let mut looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new()))
                as Arc<dyn contract::provider::Provider>,
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
            tools: vec![("builtin".to_owned(), tool as Arc<dyn contract::tool::Tool>)],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(Still) as Arc<dyn contract::rules::Rules>,
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&turn));
    let raised = Arc::clone(&slot);
    let _answering = thread::spawn(move || raised.run("ext", "inner", serde_json::json!({"x": 1})));
    deadline.recv_or_fail(&woke, "raised inner call");
    let streams = [(&anchor_id, anchor.as_ref())];
    let turn_id = TurnId("t_test".into());
    thread::scope(|scope| {
        let mut inner = Vec::new();
        let mut release = Release::default();
        looped
            .take_inner(
                scope,
                &streams,
                &mut inner,
                &mut release,
                &wake,
                Some(&turn_id),
            )
            .unwrap();
        // The worker runs now; returning drops the release while it waits.
        deadline.recv(&started).expect("the inner worker runs");
    });
    slot.close();
    assert!(
        saw.load(Ordering::SeqCst),
        "the dropped release cancelled the inner worker"
    );
}
