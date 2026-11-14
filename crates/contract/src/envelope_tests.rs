use super::*;
use crate::HubEventKind;

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
    assert_eq!(line.kind, EventKind::TurnStarted);
    line.kind = EventKind::TextCompleted;
    assert_eq!(line.kind(), EventKind::TextCompleted);
    assert_eq!(line.kind, EventKind::TextCompleted);
    line.kind = EventKind::Unknown("future_kind_é".into());
    assert_eq!(line.kind(), EventKind::Unknown("future_kind_é".into()));
    assert_eq!(line.kind, EventKind::Unknown("future_kind_é".into()));
}

#[test]
fn hub_kind_accessor_preserves_wire_and_reads_the_current_field() {
    use crate::HubEventKind;

    let cases = [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("session_deleted", HubEventKind::SessionDeleted),
        ("session_archived", HubEventKind::SessionArchived),
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
    assert_eq!(line.kind, HubEventKind::CommandAccepted);
    line.kind = HubEventKind::CommandRejected;
    assert_eq!(line.kind(), HubEventKind::CommandRejected);
    assert_eq!(line.kind, HubEventKind::CommandRejected);
    line.kind = HubEventKind::Unknown("future_kind_é".into());
    assert_eq!(line.kind(), HubEventKind::Unknown("future_kind_é".into()));
    assert_eq!(line.kind, HubEventKind::Unknown("future_kind_é".into()));
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

/// An unfamiliar `kind` keeps its exact text through both typed carriers:
/// deserialize, read the getter, serialize to the same bytes, and read
/// again. A non-string `kind` is rejected on both carriers.
#[test]
fn unknown_kind_round_trips_verbatim_through_typed_carriers() {
    use crate::HubEventKind;
    use crate::events::EventKind;

    let unknown_names = ["future_kind_é", "", " \t", "turn_started_next"];
    for name in unknown_names {
        let encoded = serde_json::to_string(name).unwrap();
        let wire = format!(
            r#"{{"kind":{encoded},"session_id":"s_x","ts":0,"schema_version":1,"payload":{{}}}}"#
        );
        let line: Envelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(line.kind(), EventKind::Unknown(name.to_owned()));
        assert_eq!(line.kind, EventKind::Unknown(name.to_owned()));
        assert_eq!(serde_json::to_string(&line).unwrap(), wire);
        let again: Envelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(again.kind(), EventKind::Unknown(name.to_owned()));
        assert_eq!(again.kind, EventKind::Unknown(name.to_owned()));
        assert_eq!(again.payload, line.payload);

        let hub_wire = format!(r#"{{"kind":{encoded},"ts":0,"schema_version":1,"payload":{{}}}}"#);
        let hub_line: HubLine = serde_json::from_str(&hub_wire).unwrap();
        assert_eq!(hub_line.kind(), HubEventKind::Unknown(name.to_owned()));
        assert_eq!(hub_line.kind, HubEventKind::Unknown(name.to_owned()));
        assert_eq!(serde_json::to_string(&hub_line).unwrap(), hub_wire);
        let hub_again: HubLine = serde_json::from_str(&hub_wire).unwrap();
        assert_eq!(hub_again.kind(), HubEventKind::Unknown(name.to_owned()));
        assert_eq!(hub_again.kind, HubEventKind::Unknown(name.to_owned()));
        assert_eq!(hub_again.payload, hub_line.payload);
    }
    let session_number_kind =
        r#"{"kind":42,"session_id":"s_x","ts":0,"schema_version":1,"payload":{}}"#;
    assert!(serde_json::from_str::<Envelope>(session_number_kind).is_err());
    let hub_number_kind = r#"{"kind":42,"ts":0,"schema_version":1,"payload":{}}"#;
    assert!(serde_json::from_str::<HubLine>(hub_number_kind).is_err());
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
    use crate::events::EventKind;

    let line =
        r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"future":true,"payload":{}}"#;
    let envelope = parse(line);
    assert_eq!(envelope.kind(), EventKind::Unknown("x".into()));
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
        kind: HubEventKind::HubHello,
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
    use crate::HubEventKind;

    let line = r#"{"kind":"hub_hello","ts":1759150000000,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#;
    let parsed: HubLine = serde_json::from_str(line).unwrap();
    assert_eq!(parsed.kind(), HubEventKind::HubHello);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
}

#[test]
fn a_payload_that_is_not_an_object_is_rejected() {
    let line = r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"payload":[]}"#;
    assert!(serde_json::from_str::<Envelope>(line).is_err());
}
