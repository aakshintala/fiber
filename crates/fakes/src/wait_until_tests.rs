use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::deadline::Deadline;
use crate::within::MUST_SUCCEED_WITHIN;

#[test]
fn returns_once_the_predicate_holds_after_false_probes() {
    let probes = std::sync::Arc::new(AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&probes);
    wait_until(
        "the predicate to hold",
        Deadline::after(Duration::from_secs(10)),
        move || counting.fetch_add(1, Ordering::SeqCst) >= 2,
    );
    assert!(
        probes.load(Ordering::SeqCst) >= 3,
        "waited through the false probes instead of returning at once"
    );
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
