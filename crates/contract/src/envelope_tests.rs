use super::*;

#[test]
fn session_kind_accessor_preserves_wire_and_reads_the_current_field() {
    use crate::events::EventKind;

    let cases = [
        ("turn_started", EventKind::TurnStarted),
        ("text_completed", EventKind::TextCompleted),
        ("", EventKind::Unknown("".into())),
        (" \t", EventKind::Unknown(" \t".into())),
        ("future_kind_é", EventKind::Unknown("future_kind_é".into())),
    ];
    for (name, expected) in cases {
        let wire = format!(
            r#"{{"kind":{},"session_id":"s_x","ts":0,"schema_version":1,"payload":{{}}}}"#,
            serde_json::to_string(name).unwrap()
        );
        let line: Envelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(line.kind(), expected);
        assert_eq!(serde_json::to_string(&line).unwrap(), wire);
    }
    let mut line = parse(
        r#"{"kind":"turn_started","session_id":"s_x","ts":0,"schema_version":1,"payload":{}}"#,
    );
    assert_eq!(line.kind(), EventKind::TurnStarted);
    line.kind = "text_completed".into();
    assert_eq!(line.kind(), EventKind::TextCompleted);
    line.kind = "future_kind_é".into();
    assert_eq!(line.kind(), EventKind::Unknown("future_kind_é".into()));
}

#[test]
fn hub_kind_accessor_preserves_wire_and_reads_the_current_field() {
    use crate::HubEventKind;

    let cases = [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("stream_closed", HubEventKind::StreamClosed),
        ("stream_reopened", HubEventKind::StreamReopened),
        ("command_accepted", HubEventKind::CommandAccepted),
        ("command_rejected", HubEventKind::CommandRejected),
        ("", HubEventKind::Unknown("".into())),
        (" \t", HubEventKind::Unknown(" \t".into())),
        (
            "future_kind_é",
            HubEventKind::Unknown("future_kind_é".into()),
        ),
    ];
    for (name, expected) in cases {
        let wire = format!(
            r#"{{"kind":{},"ts":0,"schema_version":1,"payload":{{}}}}"#,
            serde_json::to_string(name).unwrap()
        );
        let line: HubLine = serde_json::from_str(&wire).unwrap();
        assert_eq!(line.kind(), expected);
        assert_eq!(serde_json::to_string(&line).unwrap(), wire);
    }
    let mut line: HubLine = serde_json::from_str(
        r#"{"kind":"command_accepted","ts":0,"schema_version":1,"payload":{}}"#,
    )
    .unwrap();
    assert_eq!(line.kind(), HubEventKind::CommandAccepted);
    line.kind = "command_rejected".into();
    assert_eq!(line.kind(), HubEventKind::CommandRejected);
    line.kind = "future_kind_é".into();
    assert_eq!(line.kind(), HubEventKind::Unknown("future_kind_é".into()));
}

/// The example lines under `docs/events.md`, "The envelope".
fn doc_examples() -> Vec<&'static str> {
    let doc = include_str!("../../../docs/events.md");
    let section = doc.split("## The envelope").nth(1).unwrap();
    let block = section.split("```json\n").nth(1).unwrap();
    block.split("```").next().unwrap().lines().collect()
}

fn parse(line: &str) -> Envelope {
    serde_json::from_str(line).unwrap()
}

/// A session line without `session_id` fails to deserialize: every
/// session line carries one (`docs/events.md`, "The envelope"). The
/// sessionless process line is a different type with no `session_id`
/// key at all.
#[test]
fn a_session_line_without_session_id_fails_to_deserialize() {
    let line = r#"{"kind":"x","ts":1,"schema_version":1,"payload":{}}"#;
    let error = serde_json::from_str::<Envelope>(line).expect_err("no session_id");
    assert!(error.to_string().contains("session_id"), "{error}");
}

#[test]
fn the_doc_examples_round_trip_byte_for_byte() {
    let lines = doc_examples();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert_eq!(serde_json::to_string(&parse(line)).unwrap(), *line);
    }
}

#[test]
fn the_durable_example_carries_seq() {
    let envelope = parse(doc_examples()[0]);
    assert_eq!(envelope.seq, Some(Seq(7)));
    assert!(envelope.is_durable());
}

#[test]
fn the_ephemeral_example_carries_no_seq() {
    let envelope = parse(doc_examples()[1]);
    assert_eq!(envelope.action_id, Some(ActionId("a_03f7".into())));
    assert!(!envelope.is_durable());
}

#[test]
fn unknown_fields_are_ignored() {
    let line =
        r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"future":true,"payload":{}}"#;
    let envelope = parse(line);
    assert_eq!(envelope.kind, "x");
    assert_eq!(
        serde_json::to_string(&envelope).unwrap(),
        r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"payload":{}}"#
    );
}

#[test]
fn a_line_without_a_required_field_is_rejected() {
    let line = r#"{"kind":"x","session_id":"s","ts":1,"payload":{}}"#;
    assert!(serde_json::from_str::<Envelope>(line).is_err());
}

#[test]
fn hub_hello_serializes_kind_ts_schema_version_payload_in_order() {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "fiber_version".to_owned(),
        serde_json::Value::String("0.0.0".to_owned()),
    );
    let line = HubLine {
        kind: "hub_hello".to_owned(),
        ts: 1_759_150_000_000,
        schema_version: SCHEMA_VERSION,
        payload,
    };
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"kind":"hub_hello","ts":1759150000000,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#
    );
}

#[test]
fn a_hub_line_carries_no_session_id() {
    let line = r#"{"kind":"hub_hello","ts":1759150000000,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#;
    let parsed: HubLine = serde_json::from_str(line).unwrap();
    assert_eq!(parsed.kind, "hub_hello");
    assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
}

#[test]
fn a_payload_that_is_not_an_object_is_rejected() {
    let line = r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"payload":[]}"#;
    assert!(serde_json::from_str::<Envelope>(line).is_err());
}
