//! The pacer (`docs/tools.md`, "Progress"): what one running call holds,
//! when it is due, and what a write costs. `Instant`s come from the caller;
//! the pacer never reads a clock.

use std::time::Duration;

use contract::events::Progress;
use fakes::clock::FakeClock;
use serde_json::json;

use super::Pacer;

/// One origin; every instant below is built from it, so no clock is read.
fn origin() -> std::time::Instant {
    FakeClock::new().origin()
}

fn text(value: &str) -> Progress {
    Progress {
        text: Some(value.into()),
        details: None,
    }
}

#[test]
fn the_first_change_after_idle_is_due_at_once() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("half"));
    assert_eq!(pacer.take_due(at), Some(text("half")));
}

#[test]
fn a_second_change_inside_100_ms_is_held_until_written_at_plus_100_ms() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(1, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_due(at + Duration::from_millis(99)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(text("second"))
    );
}

#[test]
fn three_held_changes_collapse_to_concatenated_text_and_the_latest_details() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("a"));
    assert_eq!(pacer.take_due(at), Some(text("a")));
    pacer.wrote(1, at);
    pacer.hold(&Progress {
        text: Some("b".into()),
        details: Some(json!({"done": 1})),
    });
    pacer.hold(&Progress {
        text: Some("c".into()),
        details: None,
    });
    pacer.hold(&Progress {
        text: Some("d".into()),
        details: Some(json!({"done": 2})),
    });
    // Nothing is due yet: the collapsed delta waits for the interval.
    assert_eq!(pacer.take_due(at + Duration::from_millis(50)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(Progress {
            text: Some("bcd".into()),
            details: Some(json!({"done": 2})),
        })
    );
}

#[test]
fn a_later_delta_without_details_keeps_the_earlier_held_details() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&Progress {
        text: Some("a".into()),
        details: Some(json!({"done": 1})),
    });
    assert_eq!(
        pacer.take_due(at),
        Some(Progress {
            text: Some("a".into()),
            details: Some(json!({"done": 1})),
        })
    );
    pacer.wrote(1, at);
    pacer.hold(&Progress {
        text: Some("b".into()),
        details: Some(json!({"done": 1})),
    });
    pacer.hold(&text("c"));
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(Progress {
            text: Some("bc".into()),
            details: Some(json!({"done": 1})),
        })
    );
}

#[test]
fn a_20_kib_write_pushes_the_next_due_to_200_ms() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_due(at + Duration::from_millis(199)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(200)),
        Some(text("second"))
    );
}

#[test]
fn a_1_byte_write_keeps_the_next_due_at_100_ms() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(1, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.deadline(), Some(at + Duration::from_millis(100)));
}

#[test]
fn the_deadline_is_none_when_idle_or_when_nothing_is_held() {
    let at = origin();
    let mut pacer = Pacer::new();
    assert_eq!(pacer.deadline(), None);
    pacer.hold(&text("first"));
    // Idle: the held change is due at once, so there is nothing to wait for.
    assert_eq!(pacer.deadline(), None);
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.deadline(), Some(at + Duration::from_millis(200)));
}

#[test]
fn the_final_take_returns_the_held_change_before_the_interval() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_final(), Some(text("second")));
    assert_eq!(pacer.take_final(), None);
}

#[test]
fn an_empty_delta_is_ignored() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&Progress {
        text: None,
        details: None,
    });
    assert_eq!(pacer.take_due(at), None);
    assert_eq!(pacer.take_final(), None);
    assert_eq!(pacer.deadline(), None);
}

#[test]
fn a_details_only_delta_is_held_and_taken_without_text() {
    let at = origin();
    let mut pacer = Pacer::new();
    pacer.hold(&Progress {
        text: None,
        details: Some(json!({"done": 3})),
    });
    assert_eq!(
        pacer.take_due(at),
        Some(Progress {
            text: None,
            details: Some(json!({"done": 3})),
        })
    );
}
