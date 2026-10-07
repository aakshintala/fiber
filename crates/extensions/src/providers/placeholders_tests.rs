//! Unit tests for filling a `base_url` template
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::cell::Cell;

use config::ConfigError;
use serde_json::{Value, json};

use super::{Filled, fill};

/// A lookup that serves `value` for `name` and nothing for anything else.
fn lookup_of(
    name: &'static str,
    value: Value,
) -> impl Fn(&str) -> Result<Option<Value>, ConfigError> {
    move |at: &str| {
        if at == name {
            Ok(Some(value.clone()))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn a_template_with_no_placeholder_is_unchanged_and_looks_nothing_up() {
    let calls = Cell::new(0);
    let filled = fill("http://127.0.0.1:1/v1", &|_name| {
        calls.set(calls.get() + 1);
        Ok(None)
    })
    .unwrap();
    assert_eq!(filled, Filled::Url("http://127.0.0.1:1/v1".to_owned()));
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_repeated_name_gets_the_same_value_every_time() {
    let lookup = lookup_of("r", json!("us"));
    assert_eq!(
        fill("https://{r}-x/{r}/y", &lookup).unwrap(),
        Filled::Url("https://us-x/us/y".to_owned())
    );
}

#[test]
fn underscores_dashes_digits_and_capitals_are_name_characters() {
    for (template, name, value) in [
        ("{a_b}", "a_b", "one"),
        ("{a-b}", "a-b", "two"),
        ("{A9}", "A9", "three"),
    ] {
        let lookup = lookup_of(name, json!(value));
        assert_eq!(
            fill(template, &lookup).unwrap(),
            Filled::Url(value.to_owned()),
            "{template}"
        );
    }
}

#[test]
fn other_braces_stay_as_written_and_are_not_looked_up() {
    let calls = Cell::new(0);
    let lookup = |_name: &str| {
        calls.set(calls.get() + 1);
        Ok(None)
    };
    for template in ["{a.b}", "{a b}", "{}", "x}y", "{a"] {
        assert_eq!(
            fill(template, &lookup).unwrap(),
            Filled::Url(template.to_owned()),
            "{template}"
        );
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_lone_brace_before_a_placeholder_stays_as_written() {
    let lookup = lookup_of("a", json!("h"));
    assert_eq!(
        fill("{{a}}", &lookup).unwrap(),
        Filled::Url("{h}".to_owned())
    );
}

#[test]
fn only_a_non_empty_string_fills() {
    let lookup = lookup_of("a", json!("h"));
    assert_eq!(fill("{a}", &lookup).unwrap(), Filled::Url("h".to_owned()));
    for value in [json!(""), json!(5), json!(true), json!(null)] {
        let lookup = lookup_of("a", value.clone());
        assert_eq!(
            fill("{a}", &lookup).unwrap(),
            Filled::Missing("a".to_owned()),
            "{value}"
        );
    }
    let none = |_name: &str| Ok(None);
    assert_eq!(fill("{a}", &none).unwrap(), Filled::Missing("a".to_owned()));
}

#[test]
fn the_first_placeholder_with_no_value_is_reported_and_an_error_passes_on() {
    let none = |_name: &str| Ok(None);
    assert_eq!(
        fill("{a}{b}", &none).unwrap(),
        Filled::Missing("a".to_owned())
    );
    let lookup = lookup_of("a", json!("h"));
    assert_eq!(
        fill("{a}{b}", &lookup).unwrap(),
        Filled::Missing("b".to_owned())
    );
    let err = fill("{a}", &|_name| {
        Err::<Option<Value>, ConfigError>(ConfigError::Override { arg: "bad".into() })
    })
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::Usage);
}
