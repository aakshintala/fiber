use contract::ErrorCode;
use contract::events::Control;
use contract::shapes::ContentPart;
use contract::tool::{Bound, EffectsError, Tool};
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::NameSession;

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn run(arguments: &Map<String, Value>) -> contract::tool::Output {
    NameSession.run(arguments, &CancelToken::new(), &Recorder::default())
}

#[test]
fn the_definition_names_the_tool_and_its_schema() {
    let definition = NameSession.definition();

    assert_eq!(definition.name, "name_session");
    assert!(!definition.deferred);
    assert_eq!(definition.hosted, None);
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 60,
                    "description": "The session's name, 1 to 60 characters: a few words saying what the work is about."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    );
}

#[test]
fn the_description_carries_the_naming_guidance() {
    let definition = NameSession.definition();

    assert!(
        definition.description.contains("after the first prompt"),
        "{}",
        definition.description
    );
    assert!(
        definition
            .description
            .contains("when the work's topic changes, a handoff included"),
        "{}",
        definition.description
    );
}

#[test]
fn it_declares_no_effects() {
    let effects = NameSession.effects(&args(json!({"name": "n"}))).unwrap();

    assert!(effects.declared.effects.is_empty());
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
}

#[test]
fn the_result_has_no_content_and_carries_the_name_unchanged() {
    let output = run(&args(json!({"name": "  Fix the parser "})));

    assert!(output.content.is_empty());
    assert!(output.error.is_none());
    assert_eq!(
        output.control,
        Some(Control {
            handoff: None,
            questions: None,
            skill: None,
            name: Some("  Fix the parser ".into()),
        })
    );
}

#[test]
fn length_or_characters_never_change_the_result() {
    for name in ["a".repeat(61), String::new(), "a\tb\u{1b}[31m".into()] {
        let output = run(&args(json!({"name": name.clone()})));

        assert!(output.content.is_empty(), "{name:?}");
        assert!(output.error.is_none(), "{name:?}");
        assert_eq!(
            output.control,
            Some(Control {
                handoff: None,
                questions: None,
                skill: None,
                name: Some(name),
            }),
            "{output:?}"
        );
    }
}

#[test]
fn a_missing_or_mistyped_name_is_an_invalid_argument() {
    for value in [json!({}), json!({"name": 3})] {
        let arguments = args(value);

        assert!(matches!(
            NameSession.effects(&arguments),
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
fn it_has_no_guidelines_and_the_default_bound() {
    assert_eq!(NameSession.guidelines(), None);
    assert_eq!(NameSession.bound(), Bound::DEFAULT);
}
