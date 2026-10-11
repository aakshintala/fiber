//! Tests first for `delegate_models`: its definition, effects and run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;

use contract::tool::Tool as _;
use serde_json::json;

use super::DelegateModels;

fn tool() -> DelegateModels {
    DelegateModels {
        catalogue: Arc::from("Roles:\ndeep: fiber:fake/m\nReferences:\nfiber:fake/m"),
    }
}

#[test]
fn delegate_models_definition_takes_no_arguments_and_lists_nothing() {
    let definition = tool().definition();
    assert_eq!(definition.name, "delegate_models");
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    );
    assert_eq!(definition.deferred, false);
    assert_eq!(definition.hosted, None);
    let encoded = serde_json::to_value(&definition).unwrap().to_string();
    assert!(!encoded.contains("deep"), "{encoded}");
    assert!(!encoded.contains("fiber:fake/m"), "{encoded}");
}

#[test]
fn delegate_models_reads_only_and_is_never_always_reviewed() {
    let effects = tool()
        .effects(&serde_json::Map::new())
        .expect("no arguments classify");
    assert_eq!(effects.declared.effects, [contract::shapes::Effect::Reads]);
    assert_eq!(effects.declared.reversible, true);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.always_reviewed, false);
}

#[test]
fn delegate_models_returns_the_catalogue_text() {
    let output = tool().run(
        &serde_json::Map::new(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
        panic!("one text part: {:?}", output.content);
    };
    assert_eq!(text, "Roles:\ndeep: fiber:fake/m\nReferences:\nfiber:fake/m");
}
