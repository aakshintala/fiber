use super::*;
use crate::Envelope;

/// The example line under `docs/events.md`, "The envelope".
fn doc_example() -> &'static str {
    let doc = include_str!("../../../docs/events.md");
    let section = doc.split("## The envelope").nth(1).unwrap();
    let section = section.split("\n## ").next().unwrap();
    let block = section.split("```json\n").nth(2).unwrap();
    block.split("```").next().unwrap().trim_end()
}

#[test]
fn the_doc_example_round_trips_byte_for_byte() {
    let line = doc_example();
    let exit: PreSessionExit = serde_json::from_str(line).unwrap();
    assert_eq!(exit.payload.exit_code, 1);
    assert_eq!(serde_json::to_string(&exit).unwrap(), line);
}

#[test]
fn the_line_is_not_an_envelope() {
    assert!(serde_json::from_str::<Envelope>(doc_example()).is_err());
}

#[test]
fn an_envelope_without_a_session_id_is_rejected() {
    let line = r#"{"kind":"fiber_exited","ts":1,"schema_version":1,"payload":{}}"#;
    assert!(serde_json::from_str::<Envelope>(line).is_err());
}

#[test]
fn any_other_kind_is_rejected() {
    let line = doc_example().replace("fiber_exited", "turn_started");
    assert!(serde_json::from_str::<PreSessionExit>(&line).is_err());
}

#[test]
fn a_built_line_matches_the_doc_example() {
    let example: PreSessionExit = serde_json::from_str(doc_example()).unwrap();
    assert_eq!(
        PreSessionExit::new(1, example.payload.error.clone()),
        example
    );
}

#[test]
fn a_line_without_an_error_is_rejected() {
    let line = r#"{"kind":"fiber_exited","schema_version":1,"payload":{"exit_code":1}}"#;
    assert!(serde_json::from_str::<PreSessionExit>(line).is_err());
}
