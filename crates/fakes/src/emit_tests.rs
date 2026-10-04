//! [`super::Recorder`]: it records ephemeral events and ignores durable ones.

#![allow(clippy::unwrap_used, reason = "test code")]

use std::time::Duration;

use contract::emit::Emit;
use contract::events::{Event, Progress, TextDelta, ToolCallStarted};
use contract::shapes::DeclaredEffects;
use serde_json::json;

use super::Recorder;

#[test]
fn records_a_delta_and_ignores_a_durable_event() {
    let recorder = Recorder::default();
    recorder.emit(&Event::ToolCallDelta(Progress {
        text: Some("half".into()),
        details: None,
    }));
    recorder.emit(&Event::ToolCallStarted(ToolCallStarted {
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
        assert!(recorder.wait_for_text("hell", Duration::from_secs(10)));
    });
    assert_eq!(recorder.text(), "hello");
}

#[test]
fn wait_for_text_times_out_when_nothing_matches() {
    let recorder = Recorder::default();
    assert!(!recorder.wait_for_text("missing", Duration::from_millis(50)));
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
    assert!(!recorder.wait_for_text("not a delta", Duration::from_millis(20)));
    recorder.emit(&Event::ToolCallDelta(Progress {
        text: Some("yes".into()),
        details: Some(json!({"done": 1})),
    }));
    assert!(recorder.wait_for_text("yes", Duration::from_secs(10)));
}
