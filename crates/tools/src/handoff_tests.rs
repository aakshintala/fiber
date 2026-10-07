use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::{EffectsError, Tool};
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::Handoff;

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn run(arguments: &Map<String, Value>) -> contract::tool::Output {
    Handoff.run(arguments, &CancelToken::new(), &Recorder::default())
}

#[test]
fn the_definition_names_the_note_and_the_cache_miss() {
    let definition = Handoff.definition();

    assert_eq!(definition.name, "handoff");
    assert!(!definition.deferred);
    assert_eq!(definition.input_schema["required"], json!(["note"]));
    assert_eq!(
        definition.input_schema["properties"]["note"]["type"],
        "string"
    );
    assert_eq!(definition.input_schema["additionalProperties"], false);
    assert!(
        definition
            .description
            .contains("The next request misses the prompt cache"),
        "{}",
        definition.description
    );
}

#[test]
fn it_declares_no_effects() {
    let effects = Handoff.effects(&args(json!({"note": "n"}))).unwrap();

    assert!(effects.declared.effects.is_empty());
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
}

#[test]
fn the_result_has_no_content_and_carries_the_note() {
    let output = run(&args(json!({"note": "Continue with the tests."})));

    assert!(output.content.is_empty());
    assert!(output.error.is_none());
    let control = output.control.unwrap();
    assert_eq!(control.handoff.as_deref(), Some("Continue with the tests."));
    assert_eq!(control.questions, None);
}

#[test]
fn a_missing_or_mistyped_note_is_an_invalid_argument() {
    for arguments in [json!({}), json!({"note": 3})] {
        let arguments = args(arguments);

        assert!(matches!(
            Handoff.effects(&arguments),
            Err(EffectsError::Arguments(_))
        ));
        let output = run(&arguments);
        assert!(output.control.is_none());
        assert_eq!(
            output.error.map(|error| error.code),
            Some(ErrorCode::InvalidArguments)
        );
        assert!(matches!(
            output.content.as_slice(),
            [ContentPart::Text { .. }]
        ));
    }
}

#[test]
fn its_guidelines_are_the_handoff_section() {
    let text = Handoff.guidelines().unwrap();

    assert!(text.contains("Call `handoff` with your note"), "{text}");
}
