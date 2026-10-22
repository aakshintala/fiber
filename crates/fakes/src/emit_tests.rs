//! [`super::Recorder`]: it records ephemeral events and ignores durable ones.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{Event, Progress, TextDelta, ToolCallStarted};
use contract::shapes::DeclaredEffects;
use serde_json::json;

use super::{EventLog, Recorder};

#[test]
fn records_a_delta_and_ignores_a_durable_event() {
    let recorder = Recorder::default();
    recorder.emit(&Event::ToolCallDelta(Progress {
        text: Some("half".into()),
        details: None,
    }));
    recorder.emit(&Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects: vec![],
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    }));
    let events = recorder.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Event::ToolCallDelta(_)));
}

#[test]
fn wait_for_text_sees_a_delta_emitted_after_the_wait_started() {
    let recorder = Recorder::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            recorder.emit(&Event::ToolCallDelta(Progress {
                text: Some("hello".into()),
                details: None,
            }));
        });
        assert!(recorder.wait_for_text("hell", crate::Deadline::after(Duration::from_secs(10))));
    });
    assert_eq!(recorder.text(), "hello");
}

#[test]
fn wait_for_text_times_out_when_nothing_matches() {
    let recorder = Recorder::default();
    assert!(!recorder.wait_for_text("missing", crate::Deadline::after(Duration::from_millis(50))));
}

#[test]
fn ignores_another_ephemeral_kind_and_keeps_recording_deltas() {
    let recorder = Recorder::default();
    recorder.emit(&Event::AssistantMessageDelta(TextDelta {
        text: "not a delta".into(),
    }));
    // Recorded (it is ephemeral), but it carries no delta text.
    assert_eq!(recorder.events().len(), 1);
    assert_eq!(recorder.text(), String::new());
    assert!(!recorder.wait_for_text(
        "not a delta",
        crate::Deadline::after(Duration::from_millis(20))
    ));
    recorder.emit(&Event::ToolCallDelta(Progress {
        text: Some("yes".into()),
        details: Some(json!({"done": 1})),
    }));
    assert!(recorder.wait_for_text("yes", crate::Deadline::after(Duration::from_secs(10))));
}

#[test]
fn event_log_records_durable_and_ephemeral_events_in_order() {
    let log = EventLog::default();
    let started = Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects: vec![],
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    });
    let delta = Event::ToolCallDelta(Progress {
        text: Some("half".into()),
        details: None,
    });
    let message = Event::AssistantMessageDelta(TextDelta {
        text: "hello".into(),
    });
    for event in [&started, &delta, &message] {
        log.emit(event);
    }
    let events = log.events();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Event::ToolCallStarted(_)));
    assert!(matches!(events[1], Event::ToolCallDelta(_)));
    assert!(matches!(events[2], Event::AssistantMessageDelta(_)));
    // events() is a snapshot: later emits leave the earlier vec unchanged.
    log.emit(&Event::ToolCallDelta(Progress {
        text: Some("later".into()),
        details: None,
    }));
    assert_eq!(events.len(), 3, "the earlier vec is unchanged");
    assert_eq!(log.events().len(), 4);
}

#[test]
fn channel_hands_each_durable_and_ephemeral_event_over_and_keeps_it_logged() {
    let (log, rx) = EventLog::channel();
    let started = Event::ToolCallStarted(ToolCallStarted {
        always_reviewed: false,
        declared: DeclaredEffects {
            effects: vec![],
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    });
    let delta = Event::ToolCallDelta(Progress {
        text: Some("half".into()),
        details: None,
    });
    log.emit(&started);
    log.emit(&delta);
    // try_recv: each event is already queued, so the hand-off needs no wait.
    assert!(matches!(rx.try_recv(), Ok(Event::ToolCallStarted(_))));
    assert!(matches!(rx.try_recv(), Ok(Event::ToolCallDelta(_))));
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(log.events().len(), 2);
}

#[test]
fn channel_emit_survives_a_dropped_receiver() {
    let (log, rx) = EventLog::channel();
    drop(rx);
    log.emit(&Event::ToolCallDelta(Progress {
        text: Some("late".into()),
        details: None,
    }));
    assert_eq!(log.events().len(), 1, "the event is still logged");
}
