use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::{EffectsError, Tool};
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::Compact;

fn run(arguments: &Map<String, Value>) -> contract::tool::Output {
    Compact.run(arguments, &CancelToken::new(), &Recorder::default())
}

#[test]
fn the_description_names_carried_context_and_the_cache_miss() {
    let definition = Compact.definition();

    for detail in [
        "Kept messages with their replies",
        "running jobs",
        "loaded skills",
        "come along with the note",
        "The next request misses the prompt cache",
    ] {
        assert!(
            definition.description.contains(detail),
            "missing {detail:?}: {}",
            definition.description
        );
    }
}

#[test]
fn it_declares_no_effects() {
    let effects = Compact
        .effects(&fakes::tool_args(json!({"note": "n"})))
        .unwrap();

    assert!(effects.declared.effects.is_empty());
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
}

#[test]
fn the_result_has_no_content_and_carries_the_note() {
    let output = run(&fakes::tool_args(
        json!({"note": "Continue with the tests."}),
    ));

    assert!(output.content.is_empty());
    assert!(output.error.is_none());
    let control = output.control.unwrap();
    assert_eq!(control.compact.as_deref(), Some("Continue with the tests."));
    assert_eq!(control.questions, None);
}

#[test]
fn a_missing_or_mistyped_note_is_an_invalid_argument() {
    let arguments = fakes::tool_args(json!({"note": 3}));

    assert!(matches!(
        Compact.effects(&arguments),
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
