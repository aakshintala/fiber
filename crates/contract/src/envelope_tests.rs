use super::*;

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
