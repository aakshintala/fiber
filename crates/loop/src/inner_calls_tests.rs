//! The inner-call seam, slots and cancellation.

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
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(2));
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
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(2));
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
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(2));
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
        fakes::Deadline::after(std::time::Duration::from_secs(2)).recv_or_fail(&rx, "cancel wake");
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
fn inner_depths_seven_and_eight_run_but_nine_is_refused() {
    use contract::tool::InnerCalls;
    for (requester, allowed) in [(6, true), (7, true), (8, false)] {
        let (slot, woke, _notice) = slot(requester);
        slot.close();
        let result = slot.run("ext", "other", serde_json::json!({}));
        if allowed {
            assert_eq!(result.unwrap().status, contract::events::CallStatus::Cancelled);
        } else {
            assert!(result.unwrap_err().contains("depth exceeds 8"));
        }
        assert!(slot.take_raised().is_empty());
        assert!(matches!(woke.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    }
}

#[test]
fn self_call_refusal_precedes_raising_even_when_the_slot_is_closed() {
    use contract::tool::InnerCalls;
    let (slot, woke, _notice) = slot(0);
    slot.close();
    assert!(slot.run("ext", "outer", serde_json::json!({})).unwrap_err().contains("cannot run itself"));
    assert!(slot.take_raised().is_empty());
    assert!(matches!(woke.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

