//! The interval sits at the 100 ms floor up to 10 240 bytes and grows with
//! the byte rate past it; the cap keeps exactly the newest 64 KiB at a
//! character boundary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::time::Duration;

use super::{HELD_MAX, MIN_INTERVAL, cap_held, interval};

#[test]
fn the_interval_is_the_floor_below_the_rate_and_the_rate_above_it() {
    assert_eq!(interval(0), MIN_INTERVAL);
    assert_eq!(MIN_INTERVAL, Duration::from_millis(100));
    // 10 240 bytes cost exactly the floor: the edge between the two rules.
    assert_eq!(interval(10_240), Duration::from_millis(100));
    assert_eq!(interval(10_239), Duration::from_millis(100));
    // One byte past the edge costs more than the floor.
    assert!(interval(10_241) > Duration::from_millis(100));
    // 102 400 bytes cost one second.
    assert_eq!(interval(102_400), Duration::from_secs(1));
    assert_eq!(interval(204_800), Duration::from_secs(2));
    // A count too large to multiply does not overflow.
    assert!(interval(u64::MAX) >= Duration::from_secs(1));
}

#[test]
fn the_cap_keeps_exactly_the_newest_held_max_bytes() {
    let mut at_the_cap = "a".repeat(HELD_MAX);
    cap_held(&mut at_the_cap);
    assert_eq!(at_the_cap.len(), HELD_MAX);

    let mut under = "a".repeat(HELD_MAX - 1);
    cap_held(&mut under);
    assert_eq!(under.len(), HELD_MAX - 1);

    let mut over = format!("x{}", "a".repeat(HELD_MAX));
    cap_held(&mut over);
    assert_eq!(over, "a".repeat(HELD_MAX));

    let mut newest = format!("{}{}", "a".repeat(10), "b".repeat(HELD_MAX));
    cap_held(&mut newest);
    assert_eq!(newest, "b".repeat(HELD_MAX));
}

#[test]
fn the_cap_cuts_at_a_character_boundary() {
    // A 4-byte character straddling the cut: the cut falls inside it, so
    // the whole character goes and the text stays valid.
    let tail = "a".repeat(HELD_MAX - 2);
    let mut text = format!("😀{tail}");
    assert_eq!(text.len(), HELD_MAX + 2);
    cap_held(&mut text);
    assert_eq!(text, tail);

    // The cut lands exactly on the character: it is kept whole.
    let mut aligned = format!("x😀{}", "a".repeat(HELD_MAX - 4));
    assert_eq!(aligned.len(), HELD_MAX + 1);
    cap_held(&mut aligned);
    assert!(aligned.starts_with('😀'));
    assert_eq!(aligned.len(), HELD_MAX);
}
