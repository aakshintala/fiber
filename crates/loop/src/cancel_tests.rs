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
    assert!(cancel.arm());
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
    assert!(cancel.arm());
    let counter = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.cancel());
    assert_eq!(counter.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn disarm_reports_whether_a_cancel_landed() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert!(!cancel.disarm());
    assert!(cancel.arm());
    assert!(cancel.cancel());
    assert!(cancel.disarm());
}

#[test]
fn disarm_makes_a_later_cancel_return_false() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert!(cancel.cancel());
    assert!(cancel.disarm());
    assert!(!cancel.is_cancelled());
    assert!(!cancel.cancel());
}

#[test]
fn arm_clears_the_cancelled_flag_and_the_subscribers() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    let stale = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.arm());
    assert!(!cancel.is_cancelled());
    let fresh = subscribed(&cancel);
    assert!(cancel.cancel());
    assert_eq!(stale.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(fresh.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_late_wake_after_disarm_reaches_nothing_new() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    let counter = subscribed(&cancel);
    assert!(cancel.cancel());
    assert!(cancel.disarm());
    // `arm` starts an empty list, so no wake from the landed cancel is
    // still pending: the counter woke exactly once.
    assert_eq!(counter.wakes.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_cancels_an_armed_turn_and_wakes_subscribers() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    let counter = subscribed(&cancel);
    cancel.shutdown(143);
    assert!(cancel.is_cancelled());
    assert_eq!(counter.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(cancel.shutdown_code(), Some(143));
    // The turn was armed, so `disarm` reports the cancel.
    assert!(cancel.disarm());
}

#[test]
fn shutdown_survives_disarm_and_refuses_arm() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    cancel.shutdown(130);
    cancel.disarm();
    assert!(!cancel.arm());
    assert!(cancel.is_cancelled());
    assert_eq!(cancel.shutdown_code(), Some(130));
}

#[test]
fn shutdown_while_disarmed_still_refuses_the_next_arm() {
    let cancel = TurnCancel::default();
    cancel.shutdown(129);
    assert!(cancel.is_cancelled());
    assert!(!cancel.arm());
    // No turn was armed, so nothing landed on one.
    assert!(!cancel.disarm());
}

#[test]
fn the_first_shutdown_code_stays() {
    let cancel = TurnCancel::default();
    cancel.shutdown(143);
    cancel.shutdown(130);
    assert_eq!(cancel.shutdown_code(), Some(143));
}

#[test]
fn a_fresh_signal_arms_and_has_no_shutdown_code() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert_eq!(cancel.shutdown_code(), None);
}

#[test]
fn cancel_alone_never_sets_the_shutdown_code() {
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert!(cancel.cancel());
    assert_eq!(cancel.shutdown_code(), None);
    cancel.disarm();
    assert!(cancel.arm());
    assert!(!cancel.is_cancelled());
}

#[test]
fn commit_arm_runs_the_write_until_a_shutdown() {
    use super::Commit;
    let cancel = TurnCancel::default();
    assert_eq!(cancel.commit(Commit::Arm, || 7), Some(7));
    assert!(cancel.cancel(), "the commit armed the turn");
    cancel.disarm();
    cancel.shutdown(143);
    assert_eq!(cancel.commit(Commit::Arm, || 7), None);
}

#[test]
fn commit_step_is_refused_by_a_cancel_and_by_a_shutdown() {
    use super::Commit;
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert_eq!(cancel.commit(Commit::Step, || 1), Some(1));
    assert!(cancel.cancel());
    assert_eq!(cancel.commit(Commit::Step, || 1), None);
    cancel.disarm();
    assert!(cancel.arm());
    assert_eq!(cancel.commit(Commit::Step, || 1), Some(1));
    cancel.shutdown(130);
    assert_eq!(cancel.commit(Commit::Step, || 1), None);
}

#[test]
fn state_is_live_on_a_fresh_armed_signal() {
    use super::SignalState;
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert_eq!(cancel.state(), SignalState::Live);
}

#[test]
fn state_is_cancelled_after_a_cancel_and_live_again_after_the_next_arm() {
    use super::SignalState;
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert!(cancel.cancel());
    assert_eq!(cancel.state(), SignalState::Cancelled);
    cancel.disarm();
    assert!(cancel.arm());
    assert_eq!(cancel.state(), SignalState::Live);
}

#[test]
fn state_is_shutdown_after_a_shutdown_armed_or_not() {
    use super::SignalState;
    let armed = TurnCancel::default();
    assert!(armed.arm());
    armed.shutdown(143);
    assert_eq!(armed.state(), SignalState::Shutdown);
    let idle = TurnCancel::default();
    idle.shutdown(130);
    assert_eq!(idle.state(), SignalState::Shutdown);
}

#[test]
fn state_is_shutdown_when_a_cancel_also_landed() {
    use super::SignalState;
    let cancel = TurnCancel::default();
    assert!(cancel.arm());
    assert!(cancel.cancel());
    cancel.shutdown(129);
    assert_eq!(cancel.state(), SignalState::Shutdown);
}
