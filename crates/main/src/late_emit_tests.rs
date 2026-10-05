use std::sync::Arc;

use contract::emit::Emit;
use contract::events::{Event, Progress};

use super::LateEmit;

fn delta(text: &str) -> Event {
    Event::ToolCallDelta(Progress {
        text: Some(text.to_owned()),
        details: None,
    })
}

#[test]
fn events_before_the_target_is_set_are_dropped_and_later_ones_forwarded() {
    let late = LateEmit::default();
    late.emit(&delta("early"));
    let recorder = Arc::new(fakes::Recorder::default());
    late.set(Arc::clone(&recorder) as _);
    late.emit(&delta("late"));
    assert_eq!(recorder.events(), vec![delta("late")]);
}

#[test]
fn a_second_target_is_ignored() {
    let late = LateEmit::default();
    let first = Arc::new(fakes::Recorder::default());
    let second = Arc::new(fakes::Recorder::default());
    late.set(Arc::clone(&first) as _);
    late.set(Arc::clone(&second) as _);
    late.emit(&delta("x"));
    assert_eq!(first.events(), vec![delta("x")]);
    assert!(second.events().is_empty());
}
