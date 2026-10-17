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
        quota: Arc::new(|| String::from("Quota: none")),
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
    assert!(!definition.deferred);
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
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert!(!effects.always_reviewed);
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
    assert_eq!(
        text,
        "Roles:\ndeep: fiber:fake/m\nReferences:\nfiber:fake/m\nQuota: none"
    );
}

#[test]
fn delegate_models_appends_the_quota_text() {
    let loading = DelegateModels {
        catalogue: Arc::from("Roles:\ndeep: fiber:fake/m\nReferences:\nfiber:fake/m"),
        quota: Arc::new(|| String::from("Quota: loading")),
    };
    let output = loading.run(
        &serde_json::Map::new(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
        panic!("one text part: {:?}", output.content);
    };
    assert_eq!(
        text,
        "Roles:\ndeep: fiber:fake/m\nReferences:\nfiber:fake/m\nQuota: loading"
    );
}

#[test]
fn the_quota_is_read_on_every_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let tool = DelegateModels {
        catalogue: Arc::from("Roles: none\nReferences: none"),
        quota: Arc::new(move || {
            let call = counted.fetch_add(1, Ordering::SeqCst);
            format!("Quota: {call}")
        }),
    };
    let run = || {
        let output = tool.run(
            &serde_json::Map::new(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        );
        let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
            panic!("one text part: {:?}", output.content);
        };
        text.clone()
    };
    assert_eq!(run(), "Roles: none\nReferences: none\nQuota: 0");
    assert_eq!(run(), "Roles: none\nReferences: none\nQuota: 1");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
