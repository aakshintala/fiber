//! Tests for the shared MCP response slots and wait conditions.

use contract::clock::Clock;
use serde_json::json;

#[test]
fn shared_slots_deliver_remove_and_mark_gone() {
    use super::{Outcome, Shared};
    let shared = Shared::default();
    assert_eq!(shared.next_id(), 1);
    assert_eq!(shared.next_id(), 2);
    shared.insert(7);
    shared.deliver(7, Outcome::Result(json!({})));
    let cancel = fakes::CancelToken::new();
    let seen = shared.view(7, &cancel);
    assert_eq!(seen.response, Some(Outcome::Result(json!({}))));
    assert!(!seen.gone);
    // A late response to a removed id is discarded, never misrouted.
    shared.remove(7);
    shared.deliver(7, Outcome::Result(json!({"late": true})));
    let missing = shared.view(7, &cancel);
    assert_eq!(missing.response, None);
    shared.gone();
    assert!(shared.view(9, &cancel).gone);
}

#[test]
fn the_wait_ends_on_a_new_response_or_cancel() {
    // All four combinations: `||` into `&&` misses the two mixed rows,
    // and `!=` into `==` flips the two uncancelled rows.
    assert!(!super::should_stop(7, 7, false));
    assert!(super::should_stop(8, 7, false));
    assert!(super::should_stop(7, 7, true));
    assert!(super::should_stop(8, 7, true));
}

#[test]
fn no_cancel_never_cancels() {
    use contract::tool::Cancel;
    assert!(!super::NoCancel.is_cancelled());
}

#[test]
fn deadline_is_now_plus_timeout_and_saturates() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    assert_eq!(
        super::deadline(clock.as_ref(), std::time::Duration::from_secs(5)),
        now.checked_add(std::time::Duration::from_secs(5))
            .expect("deadline"),
    );
    assert_eq!(
        super::deadline(clock.as_ref(), std::time::Duration::MAX),
        now,
        "an overflowing timeout saturates to now",
    );
}

#[test]
fn awaiting_holds_only_for_a_pending_slot() {
    use super::Shared;
    let shared = Shared::default();
    assert!(!shared.awaiting(7), "no slot waits");
    shared.insert(7);
    assert!(shared.awaiting(7), "an empty slot waits");
    shared.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(!shared.awaiting(7), "an answered slot waits no longer");
    shared.remove(7);
    assert!(!shared.awaiting(7), "a removed slot waits no longer");
}

#[test]
fn gone_if_awaiting_marks_only_a_pending_slot() {
    use super::Shared;
    // The forced interleaving at the decision point: insert, remove, then
    // the mark returns false and leaves the server live.
    let shared = Shared::default();
    shared.insert(7);
    assert!(shared.gone_if_awaiting(7), "a pending slot marks");
    let cancel = fakes::CancelToken::new();
    assert!(shared.view(7, &cancel).gone, "the mark wakes the waiter");
    shared.remove(7);
    let live = Shared::default();
    live.insert(7);
    live.remove(7);
    assert!(!live.gone_if_awaiting(7), "a removed slot marks nothing");
    assert!(
        !crate::registry::lock(&live.inner).gone,
        "the server stays live"
    );
    let answered = Shared::default();
    answered.insert(7);
    answered.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(
        !answered.gone_if_awaiting(7),
        "an answered slot marks nothing"
    );
    assert!(
        !crate::registry::lock(&answered.inner).gone,
        "the server stays live"
    );
}

#[test]
fn expire_if_awaiting_sets_expired_and_gone_and_wakes() {
    use super::Shared;
    let shared = Shared::default();
    shared.insert(7);
    assert!(shared.expire_if_awaiting(7), "a pending slot expires");
    let cancel = fakes::CancelToken::new();
    let seen = shared.view(7, &cancel);
    assert!(seen.expired, "the waiter sees the expiry");
    assert!(seen.gone, "an expiry is gone too");
}

#[test]
fn expire_if_awaiting_marks_nothing_once_gone_or_unawaited() {
    use super::Shared;
    // After `gone` the expiry leaves `expired` false: a death stays a death.
    let dead = Shared::default();
    dead.insert(7);
    assert!(dead.gone_if_awaiting(7), "the death marks");
    assert!(
        !dead.expire_if_awaiting(7),
        "an expiry past a death marks nothing"
    );
    assert!(
        !crate::registry::lock(&dead.inner).expired,
        "expired stays false past a death"
    );
    let removed = Shared::default();
    removed.insert(7);
    removed.remove(7);
    assert!(
        !removed.expire_if_awaiting(7),
        "a removed slot expires nothing"
    );
    assert!(
        !crate::registry::lock(&removed.inner).gone,
        "the server stays live"
    );
    let answered = Shared::default();
    answered.insert(7);
    answered.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(
        !answered.expire_if_awaiting(7),
        "an answered slot expires nothing"
    );
}
