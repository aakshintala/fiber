use std::time::Duration;

use contract::clock::Clock;

use super::System;

/// The one test that sleeps on the wall clock (`docs/testing.md`, "Values
/// that change every run"). The OS guarantees the lower bound.
#[test]
fn sleep_waits_at_least_the_duration_asked() {
    let asked = Duration::from_millis(1);
    let before = System.now();
    System.sleep(asked);
    assert!(System.now().duration_since(before) >= asked);
}
