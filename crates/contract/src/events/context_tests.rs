#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use serde_json::{Value, json};

use super::{InstructionFile, InstructionReason, InstructionSent};

#[test]
fn stub_sent_round_trips_as_stub() {
    assert_eq!(
        serde_json::to_value(InstructionSent::Stub).expect("serializable"),
        Value::String("stub".to_owned())
    );
    assert_eq!(
        serde_json::from_value::<InstructionSent>(Value::String("stub".to_owned()))
            .expect("deserializable"),
        InstructionSent::Stub
    );
}

#[test]
fn stub_size_present_round_trips() {
    let file = InstructionFile {
        path: "/w/sub/AGENTS.md".to_owned(),
        reason: InstructionReason::Subdirectory,
        extension: None,
        content: Some("text".to_owned()),
        sent: InstructionSent::Stub,
        size: Some(40_000),
    };
    let value = serde_json::to_value(&file).expect("serializable");
    assert_eq!(value.get("size"), Some(&json!(40_000)));
    assert_eq!(
        serde_json::from_value::<InstructionFile>(value).expect("deserializable"),
        file
    );
}

#[test]
fn stub_size_absent_stays_absent() {
    let value = json!({"path": "/a", "reason": "subdirectory", "content": "c", "sent": "stub"});
    let file: InstructionFile = serde_json::from_value(value).expect("deserializable");
    assert_eq!(file.size, None);
    let round_tripped = serde_json::to_value(&file).expect("serializable");
    assert_eq!(round_tripped.get("size"), None);
}

#[test]
fn non_stub_lines_omit_size() {
    for sent in [
        InstructionSent::Full,
        InstructionSent::Diff,
        InstructionSent::None,
        InstructionSent::Deleted,
    ] {
        let file = InstructionFile {
            path: "/w/AGENTS.md".to_owned(),
            reason: InstructionReason::Changed,
            extension: None,
            content: Some("text".to_owned()),
            sent,
            size: None,
        };
        let value = serde_json::to_value(&file).expect("serializable");
        assert_eq!(value.get("size"), None, "{sent:?}");
        assert_eq!(
            serde_json::from_value::<InstructionFile>(value).expect("deserializable"),
            file
        );
    }
}
