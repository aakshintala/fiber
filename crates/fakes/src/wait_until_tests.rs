use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::deadline::Deadline;
use crate::within::MUST_SUCCEED_WITHIN;

fn message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "<not a string>".to_owned()
    }
}

#[test]
fn returns_once_another_thread_flips_the_predicate() {
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    let setter = std::sync::Arc::clone(&flag);
    // The delay is a bounded wait on a channel, never a sleep: it times
    // out, then flips the flag the predicate reads.
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let _delayed = Deadline::after(Duration::from_millis(50)).recv(&rx);
        setter.store(true, Ordering::SeqCst);
    });
    wait_until(
        "the flag to flip",
        Deadline::after(Duration::from_secs(10)),
        move || flag.load(Ordering::SeqCst),
    );
    drop(tx);
}

#[test]
#[should_panic(expected = "the thing")]
fn a_predicate_that_never_holds_fails_naming_what() {
    wait_until(
        "the thing to happen",
        Deadline::after(Duration::from_millis(100)),
        || false,
    );
}

#[test]
fn the_poll_thread_ends_after_the_deadline() {
    let (tx, rx) = mpsc::channel::<()>();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        wait_until(
            "the thing to happen",
            Deadline::after(Duration::from_millis(100)),
            move || {
                let _ = &tx;
                false
            },
        );
    }));
    result.expect_err("an unmet predicate fails");
    // The poll thread drops the predicate within one pause of the expiry,
    // which drops the sender it owns: the receive reports the hang-up,
    // and would time out instead if the thread were still polling.
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&rx) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        other => panic!("the poll thread ended and dropped its predicate: {other:?}"),
    }
}

#[test]
fn a_panicking_predicate_fails_naming_what() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        wait_until(
            "the thing to happen",
            Deadline::after(Duration::from_secs(10)),
            || {
                panic!("boom");
            },
        )
    }));
    let text = message(result.expect_err("a panicking predicate fails"));
    assert!(
        text.contains("the wait for the thing to happen panicked"),
        "names the wait: {text}"
    );
    assert!(text.contains("boom"), "reports the payload: {text}");
}
