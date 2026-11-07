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
fn delegate_models_definition_takes_an_optional_filter_and_lists_nothing() {
    let definition = tool().definition();
    assert_eq!(definition.name, "delegate_models");
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Keep only references containing this string."
                }
            },
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

/// A grouped catalogue: the session line, one role, and two providers
/// with a leveled reference carrying an annotation.
fn grouped_tool() -> DelegateModels {
    DelegateModels {
        catalogue: Arc::from(
            "Session model: fiber:pa/opus-4\nRoles:\ndeep: fiber:pb/m\nReferences:\npa (2):\nfiber:pa/opus-4 (thinking: low, high)\nfiber:pa/sonnet\npb (1):\nfiber:pb/m",
        ),
        quota: Arc::new(|| String::from("Quota: none")),
    }
}

fn run_with(tool: &DelegateModels, filter: serde_json::Value) -> String {
    let mut arguments = serde_json::Map::new();
    arguments.insert("filter".to_owned(), filter);
    let output = tool.run(
        &arguments,
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
        panic!("one text part: {:?}", output.content);
    };
    text.clone()
}

#[test]
fn delegate_models_filter_keeps_only_matching_references_with_post_filter_counts() {
    // A dropped filter would return the whole catalogue, so the pin
    // names the one kept entry and its updated group count; an
    // inverted match would keep the wrong entries.
    assert_eq!(
        run_with(&grouped_tool(), json!("opus")),
        "Session model: fiber:pa/opus-4\nRoles:\ndeep: fiber:pb/m\nReferences:\npa (1):\nfiber:pa/opus-4 (thinking: low, high)\nQuota: none"
    );
}

#[test]
fn delegate_models_filter_matching_nothing_renders_references_none() {
    assert_eq!(
        run_with(&grouped_tool(), json!("opus-5")),
        "Session model: fiber:pa/opus-4\nRoles:\ndeep: fiber:pb/m\nReferences: none\nQuota: none"
    );
}

#[test]
fn delegate_models_filter_matches_the_reference_token_case_sensitively() {
    // `high` names only the thinking annotation, and `OPUS` only the
    // case: neither is a substring of a reference token, so both read
    // as no match. An annotation match would keep `fiber:pa/opus-4`.
    for filter in ["high", "OPUS"] {
        assert_eq!(
            run_with(&grouped_tool(), json!(filter)),
            "Session model: fiber:pa/opus-4\nRoles:\ndeep: fiber:pb/m\nReferences: none\nQuota: none",
            "{filter}"
        );
    }
}

#[test]
fn delegate_models_absent_null_and_empty_filter_read_the_whole_catalogue() {
    let tool = grouped_tool();
    let expected = "Session model: fiber:pa/opus-4\nRoles:\ndeep: fiber:pb/m\nReferences:\npa (2):\nfiber:pa/opus-4 (thinking: low, high)\nfiber:pa/sonnet\npb (1):\nfiber:pb/m\nQuota: none";
    let output = tool.run(
        &serde_json::Map::new(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    let [contract::shapes::ContentPart::Text { text }] = &output.content[..] else {
        panic!("one text part: {:?}", output.content);
    };
    assert_eq!(text, expected);
    // Null behaves as absent; the empty string is a substring of every
    // reference, so it matches all with no special case.
    for filter in [json!(null), json!("")] {
        assert_eq!(run_with(&tool, filter.clone()), expected, "{filter}");
    }
}

#[test]
fn delegate_models_non_string_filter_fails_invalid_arguments() {
    for filter in [json!(1), json!(true), json!([])] {
        let mut arguments = serde_json::Map::new();
        arguments.insert("filter".to_owned(), filter.clone());
        let output = grouped_tool().run(
            &arguments,
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        );
        // A wrong code would pass the message assert, so the code is
        // pinned too.
        let error = output.error.expect("a non-string filter fails");
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
        assert_eq!(error.message, "`filter` must be a string.");
        assert!(output.content.is_empty(), "{filter}");
        assert!(output.jobs.is_empty(), "{filter}");
    }
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
