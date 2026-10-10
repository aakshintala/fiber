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
        now.checked_add(std::time::Duration::from_secs(5)).expect("deadline"),
    );
    assert_eq!(
        super::deadline(clock.as_ref(), std::time::Duration::MAX),
        now,
        "an overflowing timeout saturates to now",
    );
}
