//! Unit tests for the [`fixture_starts_with`](super::fixture_starts_with)
//! seam: each proves the exact-count ratchet on a synthetic one-row
//! table, without touching the checked-in [`SITES`](super::sites::SITES)
//! table.

use super::*;
use crate::rules::RustFile;

/// A synthetic workspace file holding `source`.
fn file(source: &str) -> RustFile {
    RustFile {
        krate: "synth".to_owned(),
        path: "crates/synth/src/case.rs".to_owned(),
        rel: "src/case.rs".to_owned(),
        source: source.to_owned(),
    }
}

/// A one-row table pinning `item` to `expected` direct calls.
fn row(item: &'static str, expected: usize) -> Site {
    Site {
        path: "crates/synth/src/case.rs",
        item,
        expected,
        kind: Kind::Later,
        marker: "",
        scope: Scope::BeforeUse,
    }
}

/// Three direct calls on two lines: two share one line.
const THREE_CALLS: &str = "fn three() {\n    let _ = fakes::mcp_fixture(); let _ = fakes::mcp_fixture();\n    let _ = fakes::mcp_fixture();\n}\n";

#[test]
fn three_direct_calls_pass_at_expected_three() {
    let failures = fixture_starts_with(&[file(THREE_CALLS)], &[row("three", 3)]);
    assert!(
        failures.is_empty(),
        "three calls pass at three: {failures:?}"
    );
}

#[test]
fn three_direct_calls_fail_at_expected_two() {
    let failures = fixture_starts_with(&[file(THREE_CALLS)], &[row("three", 2)]);
    assert_eq!(failures.len(), 1, "one count failure: {failures:?}");
    assert!(
        failures[0].contains("`three` makes 3 direct `mcp_fixture()` call(s), expected 2"),
        "the failure names the count: {failures:?}"
    );
}

#[test]
fn function_pointer_reference_fails() {
    let source = "fn pointer() {\n    let run = fakes::mcp_fixture;\n    let _ = run;\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("pointer", 0)]);
    assert_eq!(failures.len(), 1, "one reference failure: {failures:?}");
    assert!(
        failures[0].contains("`pointer` references `mcp_fixture` without calling it"),
        "the failure names the reference: {failures:?}"
    );
}

#[test]
fn imported_alias_fails() {
    let source = "use fakes::mcp_fixture as fixture;\nfn aliased() {\n    let _ = fixture();\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("aliased", 0)]);
    assert_eq!(failures.len(), 1, "one alias failure: {failures:?}");
    assert!(
        failures[0].contains("imported alias for `mcp_fixture`"),
        "the failure names the alias: {failures:?}"
    );
}

#[test]
fn nested_function_call_belongs_to_the_nested_function() {
    let source = "fn outer() {\n    fn inner() {\n        let _ = fakes::mcp_fixture();\n    }\n    inner();\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("inner", 1)]);
    assert!(failures.is_empty(), "the nested call passes: {failures:?}");
}

#[test]
fn nested_function_call_is_not_the_outer_function_call() {
    let source = "fn outer() {\n    fn inner() {\n        let _ = fakes::mcp_fixture();\n    }\n    inner();\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("outer", 1)]);
    assert_eq!(
        failures.len(),
        2,
        "count and unlisted failures: {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("unlisted fixture start in `inner`")),
        "the nested call is unlisted: {failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("`outer` makes 0 direct `mcp_fixture()` call(s)")),
        "the outer count is zero: {failures:?}"
    );
}

#[test]
fn comments_and_strings_never_count() {
    let source = "fn documented() {\n    // fakes::mcp_fixture()\n    let _ = \"fakes::mcp_fixture()\";\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("documented", 0)]);
    assert!(failures.is_empty(), "docs are not calls: {failures:?}");
}

#[test]
fn call_with_arguments_is_a_local_helper_not_the_accessor() {
    let source = "fn helper() {\n    let _ = mcp_fixture(&setup);\n}\n";
    let failures = fixture_starts_with(&[file(source)], &[row("helper", 0)]);
    assert!(
        failures.is_empty(),
        "a helper call is not a fixture call: {failures:?}"
    );
}
