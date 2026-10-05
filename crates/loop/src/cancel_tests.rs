//! Tests for [`TurnCancel`](super::TurnCancel), through the signal only:
//! no contract trait is involved.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contract::clock::Wake;
use contract::tool::Cancel as _;

use super::TurnCancel;

#[derive(Default)]
struct Counter {
    wakes: AtomicUsize,
}

impl Wake for Counter {
    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}

fn subscribed(cancel: &TurnCancel) -> Arc<Counter> {
    let counter: Arc<Counter> = Arc::default();
    let shared: Arc<dyn Wake> = counter.clone();
    cancel.subscribe(Arc::downgrade(&shared));
    counter
}

#[test]
fn cancel_before_arm_returns_false() {
    let cancel = TurnCancel::default();
    assert!(!cancel.cancel());
    assert!(!cancel.is_cancelled());
}

#[test]
fn cancel_after_arm_returns_true_and_wakes_subscribers() {
    let cancel = TurnCancel::default();
    cancel.arm();
    let first = subscribed(&cancel);
    let second = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.is_cancelled());
    assert_eq!(first.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(second.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_second_cancel_returns_true_and_wakes_nobody_twice() {
    let cancel = TurnCancel::default();
    cancel.arm();
    let counter = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.cancel());
    assert_eq!(counter.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn disarm_reports_whether_a_cancel_landed() {
    let cancel = TurnCancel::default();
    cancel.arm();
    assert!(!cancel.disarm());
    cancel.arm();
    assert!(cancel.cancel());
    assert!(cancel.disarm());
}

#[test]
fn disarm_makes_a_later_cancel_return_false() {
    let cancel = TurnCancel::default();
    cancel.arm();
    assert!(cancel.cancel());
    assert!(cancel.disarm());
    assert!(!cancel.is_cancelled());
    assert!(!cancel.cancel());
}

#[test]
fn arm_clears_the_cancelled_flag_and_the_subscribers() {
    let cancel = TurnCancel::default();
    cancel.arm();
    let stale = subscribed(&cancel);
    assert!(cancel.cancel());
    cancel.arm();
    assert!(!cancel.is_cancelled());
    let fresh = subscribed(&cancel);
    assert!(cancel.cancel());
    assert_eq!(stale.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(fresh.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_late_wake_after_disarm_reaches_nothing_new() {
    let cancel = TurnCancel::default();
    cancel.arm();
    let counter = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.disarm());
    // `arm` starts an empty list, so no wake from the landed cancel is
    // still pending: the counter woke exactly once.
    assert_eq!(counter.wakes.load(Ordering::SeqCst), 1);
}
