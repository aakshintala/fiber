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

use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Wake;
use contract::commands::ReplyAnswer;
use contract::events::{Answer, Interaction};
use contract::shapes::True;
use contract::tool::{Answered, Ask, Asking};
use contract::{ActionId, RequestId};

use super::{AskSlot, Fitted, Release};
use crate::progress::{SharedWake, Stream};

/// Wall-clock bound on every receive.
const DEADLINE: Duration = Duration::from_secs(5);

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
    woken.recv_timeout(DEADLINE).expect("the raise wakes");
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
    woken.recv_timeout(DEADLINE).expect("the raise wakes");
    let raised = slot.take_raised().unwrap();
    slot.pend(RequestId("r_1".into()), raised.interaction, None, None);
    assert_eq!(slot.pending(), Some((RequestId("r_1".into()), None)));
    slot.resolve(confirmed());
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), confirmed());
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
    pending
        .asking()
        .pend(RequestId("r_2".into()), taken.interaction, None, None);
    drop(release);
    for answers in [raised_answers, pending_answers] {
        assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
        // A second ask on a closed slot returns at once too.
        assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
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
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
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
    woken.recv_timeout(DEADLINE).expect("the raise wakes");
    slot.close();
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
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
    woken.recv_timeout(DEADLINE).expect("the first raise wakes");
    slot.take_raised().unwrap();
    slot.resolve(confirmed());
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), confirmed());
    woken
        .recv_timeout(DEADLINE)
        .expect("the second raise wakes");
    slot.take_raised().unwrap();
    slot.resolve(Answered::NoAnswer);
    assert_eq!(answers.recv_timeout(DEADLINE).unwrap(), Answered::NoAnswer);
}

#[test]
fn a_reply_fits_only_its_pending_request_and_passes_the_check() {
    let slot = AskSlot::default();
    let yes = ReplyAnswer::Confirmed { confirmed: true };
    let request = RequestId("r_1".into());
    assert_eq!(slot.fit(&request, &yes), Fitted::NotThis, "nothing pending");
    let refuse: contract::tool::Check =
        Box::new(|answer| *answer != Answer::Confirmed { confirmed: true });
    slot.pend(request.clone(), confirm(), None, Some(refuse));
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
    slot.pend(request.clone(), confirm(), None, Some(never));
    assert_eq!(
        slot.fit(&request, &declined),
        Fitted::Fits(Answer::Declined { declined: True }),
        "a decline never reaches the check"
    );
}

#[test]
fn closing_keeps_an_answer_not_yet_taken_and_frees_a_pending_ask() {
    let slot = AskSlot::default();
    slot.pend(RequestId("r_1".into()), confirm(), None, None);
    slot.resolve(confirmed());
    slot.close();
    let shown = format!("{slot:?}");
    assert!(shown.contains("\"resolved\""), "{shown}");
    let pending = AskSlot::default();
    pending.pend(RequestId("r_2".into()), confirm(), None, None);
    pending.close();
    assert_eq!(pending.pending(), None, "a closed slot holds no request");
}
