use super::*;

const DURABLE: &str = r#"{"kind":"turn_started","session_id":"s_4c1d","ts":1759150000000,"schema_version":1,"turn_id":"t_9a02","seq":7,"payload":{"input":[]}}"#;
const EPHEMERAL: &str = r#"{"kind":"assistant_message_delta","session_id":"s_4c1d","ts":1759150000123,"schema_version":1,"turn_id":"t_9a02","action_id":"a_03f7","payload":{"text":"Hel"}}"#;

fn parse(line: &str) -> Envelope {
    serde_json::from_str(line).unwrap()
}

#[test]
fn a_durable_line_round_trips_byte_for_byte() {
    let envelope = parse(DURABLE);
    assert_eq!(serde_json::to_string(&envelope).unwrap(), DURABLE);
    assert_eq!(envelope.seq, Some(Seq(7)));
    assert!(envelope.is_durable());
}

#[test]
fn an_ephemeral_line_round_trips_byte_for_byte() {
    let envelope = parse(EPHEMERAL);
    assert_eq!(serde_json::to_string(&envelope).unwrap(), EPHEMERAL);
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
fn a_payload_that_is_not_an_object_is_rejected() {
    let line = r#"{"kind":"x","session_id":"s","ts":1,"schema_version":1,"payload":[]}"#;
    assert!(serde_json::from_str::<Envelope>(line).is_err());
}
