//! The inner-call seam, slots and cancellation.

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::Arc;

use contract::ActionId;
use contract::tool::Ask;

use crate::progress::{SharedWake, Stream};

#[test]
fn a_stream_without_an_inner_slot_has_no_runner() {
    let stream = Stream::new(
        Arc::new(SharedWake::default()),
        ActionId("a_outer".into()),
        true,
    );
    assert!(stream.inner_calls().is_none());
}

struct Notice(std::sync::mpsc::Sender<()>);
impl contract::clock::Wake for Notice {
    fn wake(&self) {
        self.0.send(()).unwrap();
    }
}

fn slot(
    depth: u8,
) -> (
    Arc<super::InnerSlot>,
    std::sync::mpsc::Receiver<()>,
    Arc<dyn contract::clock::Wake>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let notice: Arc<dyn contract::clock::Wake> = Arc::new(Notice(tx));
    let wake = Arc::new(SharedWake::default());
    wake.forward(Some(notice.clone()));
    (
        Arc::new(super::InnerSlot::new(
            wake,
            ActionId("a_outer".into()),
            "outer".into(),
            depth,
        )),
        rx,
        notice,
    )
}

fn raise(
    slot: Arc<super::InnerSlot>,
    name: &'static str,
) -> std::sync::mpsc::Receiver<Result<Box<contract::events::ToolCallCompleted>, String>> {
    use contract::tool::InnerCalls;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(slot.run("ext", name, serde_json::json!({"x": 1})))
            .unwrap();
    });
    rx
}

#[test]
fn a_raise_wakes_and_waits_for_its_written_answer() {
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    let (slot, woke, _notice) = slot(0);
    let result = raise(slot.clone(), "read");
    deadline.recv_or_fail(&woke, "raised inner call");
    let requests = slot.take_raised();
    assert_eq!(requests.len(), 1);
    assert!(slot.take_raised().is_empty());
    let request = requests.into_iter().next().unwrap();
    assert_eq!(request.extension, "ext");
    assert_eq!(request.name, "read");
    assert_eq!(request.arguments, serde_json::json!({"x": 1}));
    assert!(matches!(
        result.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let completed = crate::completion::completed("answer".into(), None);
    request.answer(Box::new(completed.clone()));
    assert_eq!(
        *deadline.recv_or_fail(&result, "written answer").unwrap(),
        completed
    );
}

#[test]
fn closing_releases_raised_taken_and_later_requests() {
    use contract::events::CallStatus;
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    for taken in [false, true] {
        let (slot, woke, _notice) = slot(0);
        let result = raise(slot.clone(), "read");
        deadline.recv_or_fail(&woke, "raised inner call");
        let held = taken.then(|| slot.take_raised());
        slot.close();
        assert_eq!(
            deadline
                .recv_or_fail(&result, "closed raised call")
                .unwrap()
                .status,
            CallStatus::Cancelled
        );
        assert!(slot.take_raised().is_empty());
        assert_eq!(
            deadline
                .recv_or_fail(&raise(slot.clone(), "read"), "closed later call")
                .unwrap()
                .status,
            CallStatus::Cancelled
        );
        drop(held);
    }
}

#[test]
fn close_before_any_raise_returns_cancelled_without_waking() {
    let (slot, woke, _notice) = slot(0);
    slot.close();
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        deadline
            .recv_or_fail(&raise(slot, "read"), "closed call")
            .unwrap()
            .status,
        contract::events::CallStatus::Cancelled
    );
    assert!(matches!(
        woke.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn own_and_turn_cancellation_wake_current_subscribers_not_later_ones() {
    use contract::tool::Cancel;
    for from_turn in [false, true] {
        let turn = Arc::new(crate::TurnCancel::default());
        assert!(turn.arm());
        let cancel = super::InnerCancel::new(turn.clone());
        assert!(!cancel.is_cancelled());
        let (tx, rx) = std::sync::mpsc::channel();
        let notice: Arc<dyn contract::clock::Wake> = Arc::new(Notice(tx));
        cancel.subscribe(Arc::downgrade(&notice));
        if from_turn {
            assert!(turn.cancel());
        } else {
            cancel.cancel();
        }
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "cancel wake");
        assert!(cancel.is_cancelled());
        cancel.subscribe(Arc::downgrade(&notice));
        cancel.cancel();
        assert!(turn.cancel());
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn a_cancel_created_after_turn_cancellation_is_already_cancelled() {
    use contract::tool::Cancel;
    let turn = Arc::new(crate::TurnCancel::default());
    assert!(turn.arm());
    assert!(turn.cancel());
    assert!(super::InnerCancel::new(turn).is_cancelled());
}

#[test]
fn following_an_ended_command_is_cancelled_at_once() {
    use contract::tool::Cancel;
    let source = fakes::CancelToken::new();
    source.cancel();
    let cancel = super::InnerCancel::new(Arc::new(crate::TurnCancel::default()));
    cancel.follow(&source);
    assert!(cancel.is_cancelled());
}

#[test]
fn following_a_live_command_keeps_turn_cancellation_and_wakes_subscribers() {
    use contract::tool::Cancel;
    for from_command in [false, true] {
        let turn = Arc::new(crate::TurnCancel::default());
        assert!(turn.arm());
        let source = fakes::CancelToken::new();
        let cancel = super::InnerCancel::new(turn.clone());
        cancel.follow(&source);
        assert!(!cancel.is_cancelled());
        let (tx, rx) = std::sync::mpsc::channel();
        let notice: Arc<dyn contract::clock::Wake> = Arc::new(Notice(tx));
        cancel.subscribe(Arc::downgrade(&notice));
        if from_command {
            source.cancel();
        } else {
            assert!(turn.cancel());
        }
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "followed cancellation");
        assert!(cancel.is_cancelled());
    }
}

#[test]
fn inner_depths_seven_and_eight_run_but_nine_is_refused() {
    use contract::tool::InnerCalls;
    for (requester, allowed) in [(6, true), (7, true), (8, false)] {
        let (slot, woke, _notice) = slot(requester);
        slot.close();
        let result = slot.run("ext", "other", serde_json::json!({}));
        if allowed {
            assert_eq!(
                result.unwrap().status,
                contract::events::CallStatus::Cancelled
            );
        } else {
            assert!(result.unwrap_err().contains("depth exceeds 8"));
        }
        assert!(slot.take_raised().is_empty());
        assert!(matches!(
            woke.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn self_call_refusal_precedes_raising_even_when_the_slot_is_closed() {
    use contract::tool::InnerCalls;
    let (slot, woke, _notice) = slot(0);
    slot.close();
    assert!(
        slot.run("ext", "outer", serde_json::json!({}))
            .unwrap_err()
            .contains("cannot run itself")
    );
    assert!(slot.take_raised().is_empty());
    assert!(matches!(
        woke.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

struct HeldWake {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl contract::clock::Wake for HeldWake {
    fn wake(&self) {
        self.entered.send(()).unwrap();
        let _released = self.release.lock().unwrap().recv();
    }
}

#[test]
fn a_subscription_after_turn_cancel_is_not_replayed_while_the_relay_is_delayed() {
    use contract::tool::Cancel;
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    let turn = Arc::new(crate::TurnCancel::default());
    assert!(turn.arm());
    let (entered, holding) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let first: Arc<dyn contract::clock::Wake> = Arc::new(HeldWake {
        entered,
        release: std::sync::Mutex::new(held),
    });
    turn.subscribe(Arc::downgrade(&first));
    let cancel = super::InnerCancel::new(turn.clone());
    let (ended, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        ended.send(turn.cancel()).unwrap();
    });
    deadline.recv_or_fail(&holding, "turn cancel before the inner relay");
    assert!(cancel.is_cancelled());
    let (tx, rx) = std::sync::mpsc::channel();
    let late: Arc<dyn contract::clock::Wake> = Arc::new(Notice(tx));
    cancel.subscribe(Arc::downgrade(&late));
    release.send(()).unwrap();
    assert!(deadline.recv_or_fail(&done, "turn cancel completes"));
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

/// Reads the rules once for each call that reaches the deny check. The
/// window runs inside that read, after `prepared` and before the second
/// cancel check in `take_inner`.
#[derive(Clone, Copy)]
enum Window {
    Quiet,
    CancelTurn,
    FinishAnchor,
}

struct Reads {
    window: Window,
    turn: Arc<crate::TurnCancel>,
    anchor: Arc<crate::progress::Stream>,
    count: std::sync::atomic::AtomicUsize,
}

impl contract::rules::Rules for Reads {
    fn read(&self) -> Result<contract::rules::StandingRules, contract::rules::RulesError> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.window {
            Window::Quiet => {}
            Window::CancelTurn => {
                let _cancelled = self.turn.cancel();
            }
            Window::FinishAnchor => self.anchor.finish(contract::tool::Output::default()),
        }
        Ok(contract::rules::StandingRules::default())
    }

    fn remember(
        &self,
        _: &str,
        _: &str,
        _: &contract::SessionId,
    ) -> Result<(), contract::rules::RulesError> {
        Ok(())
    }
}

/// The inner tool. It counts its runs and nothing else.
struct Inner {
    runs: std::sync::atomic::AtomicUsize,
}

impl contract::tool::Tool for Inner {
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
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        contract::tool::Output::default()
    }
}

struct Taken {
    status: Option<contract::events::CallStatus>,
    reads: usize,
    runs: usize,
}

/// Raises one inner call on an anchor and takes it through
/// `Loop::take_inner`. The anchor may be cancelled or finished before the
/// take, and `window` acts inside the deny check.
fn take(cancel_before: bool, finish_before: bool, window: Window) -> Taken {
    let deadline = fakes::Deadline::after(MUST_SUCCEED_WITHIN);
    let home = fakes::TempDir::new("fiber-inner-take");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log = Arc::new(
        log::Log::create(
            home.path(),
            contract::SessionId("s_test".into()),
            Arc::clone(&clock),
        )
        .unwrap(),
    );
    let turn = Arc::new(crate::TurnCancel::default());
    assert!(turn.arm());
    let (woke_tx, woke) = std::sync::mpsc::channel();
    let notice: Arc<dyn contract::clock::Wake> = Arc::new(Notice(woke_tx));
    let wake = Arc::new(SharedWake::default());
    wake.forward(Some(notice));
    let anchor_id = ActionId("a_outer".into());
    let slot = Arc::new(super::InnerSlot::new(
        Arc::clone(&wake),
        anchor_id.clone(),
        "outer".into(),
        0,
    ));
    let anchor = Arc::new(
        crate::progress::Stream::new(Arc::clone(&wake), anchor_id.clone(), true)
            .with_inner(Arc::clone(&slot)),
    );
    let answered = raise(Arc::clone(&slot), "inner");
    deadline.recv_or_fail(&woke, "raised inner call");
    if cancel_before {
        assert!(turn.cancel());
    }
    if finish_before {
        anchor.finish(contract::tool::Output::default());
    }
    let reads = Arc::new(Reads {
        window,
        turn: Arc::clone(&turn),
        anchor: Arc::clone(&anchor),
        count: std::sync::atomic::AtomicUsize::new(0),
    });
    let tool = Arc::new(Inner {
        runs: std::sync::atomic::AtomicUsize::new(0),
    });
    let (_inbox, rx) = std::sync::mpsc::channel();
    let mut looped = crate::Loop::start(
        crate::Session {
            log,
            provider: Arc::new(fakes::ScriptedProvider::new(Vec::new()))
                as Arc<dyn contract::provider::Provider>,
            model: crate::Model {
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
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![(
                "builtin".to_owned(),
                Arc::clone(&tool) as Arc<dyn contract::tool::Tool>,
            )],
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: reads.clone() as Arc<dyn contract::rules::Rules>,
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&turn));
    let streams = [(&anchor_id, anchor.as_ref())];
    let mut inner = Vec::new();
    let mut release = crate::asking::Release::default();
    let turn_id = contract::TurnId("t_test".into());
    std::thread::scope(|scope| {
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
    });
    let counted = Taken {
        status: None,
        reads: reads.count.load(std::sync::atomic::Ordering::SeqCst),
        runs: tool.runs.load(std::sync::atomic::Ordering::SeqCst),
    };
    // A request the loop kept instead of answering is released here.
    drop(inner);
    drop(release);
    let status = match deadline.recv(&answered) {
        Ok(Ok(done)) => Some(done.status),
        _ => None,
    };
    Taken { status, ..counted }
}

#[test]
fn a_request_taken_under_a_cancelled_turn_is_refused_before_the_rules_are_read() {
    let taken = take(true, false, Window::Quiet);
    assert_eq!(taken.status, Some(contract::events::CallStatus::Cancelled));
    assert_eq!(taken.reads, 0);
    assert_eq!(taken.runs, 0);
}

#[test]
fn a_request_taken_after_its_anchor_finished_is_refused_before_the_rules_are_read() {
    let taken = take(false, true, Window::Quiet);
    assert_eq!(taken.status, Some(contract::events::CallStatus::Cancelled));
    assert_eq!(taken.reads, 0);
    assert_eq!(taken.runs, 0);
}

#[test]
fn a_turn_cancelled_during_the_checks_never_runs_the_request() {
    let taken = take(false, false, Window::CancelTurn);
    assert_eq!(taken.status, Some(contract::events::CallStatus::Cancelled));
    assert_eq!(taken.runs, 0);
}

#[test]
fn an_anchor_finished_during_the_checks_never_runs_the_request() {
    let taken = take(false, false, Window::FinishAnchor);
    assert_eq!(taken.status, Some(contract::events::CallStatus::Cancelled));
    assert_eq!(taken.runs, 0);
}
