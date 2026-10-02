use std::time::{Duration, UNIX_EPOCH};

use contract::clock::Clock;

use super::System;

/// 2020-01-01T00:00:00Z. `wall()` is after this; the test asserts no duration.
const YEAR_2020: Duration = Duration::from_secs(1_577_836_800);

#[test]
fn wall_is_after_the_2020_epoch() {
    assert!(System.wall() > UNIX_EPOCH + YEAR_2020);
}

#[test]
fn now_does_not_go_backwards() {
    let first = System.now();
    let second = System.now();
    assert!(second >= first);
}

#[test]
fn wait_until_at_or_before_now_hands_the_closure_zero() {
    let now = System.now();
    let mut seen = None;
    System.wait_until(Some(now), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
    let earlier = now.checked_sub(Duration::from_secs(1)).unwrap();
    System.wait_until(Some(earlier), &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(Some(Duration::ZERO)));
    System.wait_until(None, &mut |bound| seen = Some(bound));
    assert_eq!(seen, Some(None));
}
