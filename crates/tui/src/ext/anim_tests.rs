//! Tests for the shared tick floor: below clamps, at and above pass
//! through.

use std::time::Duration;

use super::effective_interval;
use crate::tick::TICK;

#[test]
fn below_the_floor_clamps_to_the_tick() {
    assert_eq!(
        effective_interval(TICK.checked_sub(Duration::from_millis(1)).unwrap_or(TICK)),
        TICK
    );
    assert_eq!(effective_interval(Duration::from_millis(1)), TICK);
}

#[test]
fn at_the_floor_passes_through() {
    assert_eq!(effective_interval(TICK), TICK);
}

#[test]
fn above_the_floor_passes_through() {
    assert_eq!(
        effective_interval(TICK.checked_add(Duration::from_millis(1)).unwrap_or(TICK)),
        TICK.checked_add(Duration::from_millis(1)).unwrap_or(TICK)
    );
    assert_eq!(
        effective_interval(Duration::from_millis(500)),
        Duration::from_millis(500)
    );
}
