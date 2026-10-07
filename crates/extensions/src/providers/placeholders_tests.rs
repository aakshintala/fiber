//! Unit tests for filling a `base_url` template
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::cell::Cell;

use config::ConfigError;

use super::{Filled, Source, fill, host};

/// A lookup that serves `value` from the setting for `name` and nothing
/// for anything else.
fn lookup_of(
    name: &'static str,
    value: &'static str,
) -> impl Fn(&str) -> Result<Option<(String, Source)>, ConfigError> {
    move |at: &str| {
        if at == name {
            Ok(Some((value.to_owned(), Source::Setting)))
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
    let lookup = lookup_of("r", "us");
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
        let lookup = lookup_of(name, value);
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
    let lookup = lookup_of("a", "h");
    assert_eq!(
        fill("{{a}}", &lookup).unwrap(),
        Filled::Url("{h}".to_owned())
    );
}

#[test]
fn host_strips_one_scheme_and_slash_and_checks_the_grammar() {
    for (value, expected) in [
        ("adb-1.example", "adb-1.example"),
        ("https://adb-1.example/", "adb-1.example"),
        ("adb-1.example/", "adb-1.example"),
        ("https://adb-1.example", "adb-1.example"),
        ("a:0", "a:0"),
        ("adb-1.example:8443", "adb-1.example:8443"),
        ("a:65535", "a:65535"),
    ] {
        assert_eq!(host(value), Some(expected), "{value}");
    }
    for value in [
        "",
        "https://",
        "/",
        ":443",
        "a:",
        "a:+1",
        "a:65536",
        "a:1:2",
        "a_b",
        "a/b",
        "a b",
        "[::1]",
        "HTTPS://a",
        "https://https://a",
        "a//",
    ] {
        assert_eq!(host(value), None, "{value}");
    }
}

#[test]
fn a_valid_host_is_filled_stripped() {
    let lookup = lookup_of("a", "https://h.example/");
    assert_eq!(
        fill("{a}", &lookup).unwrap(),
        Filled::Url("h.example".to_owned())
    );
}

#[test]
fn a_value_that_is_not_a_host_is_reported_with_its_source() {
    let lookup = lookup_of("a", "x/y");
    assert_eq!(
        fill("{a}", &lookup).unwrap(),
        Filled::NotHost {
            name: "a".to_owned(),
            source: Source::Setting,
        }
    );
    let env = |at: &str| {
        if at == "a" {
            Ok(Some((
                "x/y".to_owned(),
                Source::Env("ACME_HOST".to_owned()),
            )))
        } else {
            Ok(None)
        }
    };
    assert_eq!(
        fill("{a}", &env).unwrap(),
        Filled::NotHost {
            name: "a".to_owned(),
            source: Source::Env("ACME_HOST".to_owned()),
        }
    );
}

#[test]
fn the_first_placeholder_with_no_value_is_reported_and_an_error_passes_on() {
    let none = |_name: &str| Ok(None);
    assert_eq!(
        fill("{a}{b}", &none).unwrap(),
        Filled::Missing("a".to_owned())
    );
    let lookup = lookup_of("a", "h");
    assert_eq!(
        fill("{a}{b}", &lookup).unwrap(),
        Filled::Missing("b".to_owned())
    );
    let err = fill("{a}", &|_name| {
        Err::<Option<(String, Source)>, ConfigError>(ConfigError::Override { arg: "bad".into() })
    })
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::Usage);
}
