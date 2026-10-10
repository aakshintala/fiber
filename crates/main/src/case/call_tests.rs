//! Provider call dispatch and case verdicts (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    reason = "the call dispatch assertion fails with its result"
)]

use std::sync::Arc;

use serde_json::{Value, json};

use super::{CallOutcome, compare_credential, compare_result, provider_extension};

#[test]
fn cost_and_models_dispatch_and_anything_else_names_both() {
    let cost = super::Call {
        provider: "p".to_owned(),
        function: "cost".to_owned(),
        arg: json!({"generation_id": "gen-abc", "base_url": "https://example.test"}),
    };
    assert!(matches!(super::ready(&cost), Ok(super::Ready::Cost(_))));

    let models = super::Call {
        provider: "p".to_owned(),
        function: "models".to_owned(),
        arg: json!({}),
    };
    assert!(matches!(super::ready(&models), Ok(super::Ready::Models)));

    let quota = super::Call {
        provider: "p".to_owned(),
        function: "quota".to_owned(),
        arg: json!({}),
    };
    let error = super::ready(&quota).unwrap_err();
    assert!(
        error.contains("cost") && error.contains("models"),
        "{error}"
    );
}

#[test]
fn models_takes_exactly_an_empty_object() {
    assert!(super::models_args(&json!({})).is_ok());
    for arg in [
        json!({"generation_id": "gen-abc"}),
        json!([]),
        json!(null),
        json!("x"),
    ] {
        let error = super::models_args(&arg).unwrap_err();
        assert!(error.contains("call.arg"), "{arg}: {error}");
    }
}

#[test]
fn credential_functions_validate_their_arguments() {
    for (function, arg) in [
        ("credential", json!({})),
        ("credential", json!({"label": "work"})),
        ("login", json!({"method": "browser"})),
        ("login", json!({"method": "device", "label": "work"})),
        (
            "sign",
            json!({"method": "POST", "url": "https://example.test", "headers": {}}),
        ),
        (
            "sign",
            json!({"method": "GET", "url": "https://example.test", "headers": {"x-test": "value"}}),
        ),
    ] {
        let call = super::Call {
            provider: "p".to_owned(),
            function: function.to_owned(),
            arg,
        };
        assert!(super::ready(&call).is_ok(), "{function}: {}", call.arg);
    }
    for (function, arg) in [
        ("credential", json!(null)),
        ("credential", json!({"label": 1})),
        ("credential", json!({"other": true})),
        ("login", json!({})),
        ("login", json!({"method": "other"})),
        ("login", json!({"method": "browser", "other": true})),
        (
            "sign",
            json!({"method": "POST", "url": "https://example.test"}),
        ),
        (
            "sign",
            json!({"method": "POST", "url": "https://example.test", "headers": {"x": 1}}),
        ),
        (
            "sign",
            json!({"method": 1, "url": "https://example.test", "headers": {}}),
        ),
        ("sign", json!({"method": "POST", "url": 1, "headers": {}})),
        (
            "sign",
            json!({"method": "POST", "url": "https://example.test", "headers": {}, "label": "work"}),
        ),
    ] {
        let call = super::Call {
            provider: "p".to_owned(),
            function: function.to_owned(),
            arg,
        };
        assert!(super::ready(&call).is_err(), "{function}: {}", call.arg);
    }
}

fn returns(value: Value) -> CallOutcome {
    CallOutcome::Returns(value)
}

fn error(value: Value) -> CallOutcome {
    CallOutcome::Error(value)
}

#[test]
fn returns_and_error_use_the_case_json_subset_matcher() {
    assert!(compare_result(&returns(json!(0.5)), Ok(json!(0.5))).is_empty());

    let mismatch = compare_result(&returns(json!(0.4)), Ok(json!(0.5))).join("\n");
    assert!(mismatch.contains("returns"), "{mismatch}");

    assert!(
        compare_result(
            &error(json!({"code": "extension_failed"})),
            Err(json!({"code": "extension_failed", "message": "lookup failed"}))
        )
        .is_empty()
    );

    let mismatch = compare_result(
        &error(json!({"code": "io_failed"})),
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(mismatch.contains("error.code"), "{mismatch}");
}

#[test]
fn exact_credential_expectations_compare_the_whole_nested_value() {
    let expected = json!({"token": "t", "metadata": {"account_id": "a"}});
    assert!(compare_credential(&expected, &expected, true).is_ok());
    let wrong_value = json!({"token": "wrong", "metadata": {"account_id": "a"}});
    assert!(compare_credential(&expected, &wrong_value, true).is_err());
    assert!(compare_credential(&expected, &wrong_value, false).is_err());

    let extra = json!({"token": "t", "metadata": {"account_id": "a"}, "email": "a@example.test"});
    assert!(compare_credential(&expected, &extra, true).is_err());
    assert!(compare_credential(&expected, &extra, false).is_ok());

    let missing = json!({"metadata": {"account_id": "a"}});
    assert!(compare_credential(&expected, &missing, true).is_err());
    assert!(compare_credential(&expected, &missing, false).is_err());

    let nested_extra =
        json!({"token": "t", "metadata": {"account_id": "a", "email": "a@example.test"}});
    assert!(compare_credential(&expected, &nested_extra, true).is_err());

    let null_field = json!({"token": "t", "email": null});
    let missing_null = json!({"token": "t"});
    assert!(compare_credential(&null_field, &missing_null, true).is_err());
    assert!(compare_credential(&null_field, &missing_null, false).is_ok());
}

#[test]
fn exact_credential_arrays_match_when_every_element_matches() {
    let expected = json!([{"id": 1, "meta": {"account": "a"}}, {"id": 2}]);
    let same = json!([{"id": 1, "meta": {"account": "a"}}, {"id": 2}]);
    assert!(compare_credential(&expected, &same, true).is_ok());
}

#[test]
fn exact_credential_arrays_of_different_length_do_not_match() {
    let expected = json!([{"id": 1}, {"id": 2}]);
    let longer = json!([{"id": 1}, {"id": 2}, {"id": 3}]);
    let shorter = json!([{"id": 1}]);
    for actual in [&longer, &shorter] {
        assert!(
            compare_credential(&expected, actual, true).is_err(),
            "{actual}"
        );
    }
}

#[test]
fn exact_credential_arrays_need_every_element_to_match_in_shape() {
    let expected = json!([{"id": 1}, {"id": 2, "meta": {"account": "a"}}]);

    let differing_value = json!([{"id": 1}, {"id": 3, "meta": {"account": "a"}}]);
    assert!(compare_credential(&expected, &differing_value, true).is_err());
    assert!(compare_credential(&expected, &differing_value, false).is_err());

    let extra_element_field =
        json!([{"id": 1}, {"id": 2, "meta": {"account": "a"}, "email": "a@example.test"}]);
    assert!(compare_credential(&expected, &extra_element_field, true).is_err());
    assert!(compare_credential(&expected, &extra_element_field, false).is_ok());

    let nested_extra_in_element =
        json!([{"id": 1}, {"id": 2, "meta": {"account": "a", "email": "a@example.test"}}]);
    assert!(compare_credential(&expected, &nested_extra_in_element, true).is_err());
    assert!(compare_credential(&expected, &nested_extra_in_element, false).is_ok());
}

#[test]
fn exact_credential_same_length_arrays_pass_and_an_array_is_not_a_scalar_or_object() {
    let expected = json!([1, "two", null]);
    let same = json!([1, "two", null]);
    assert!(compare_credential(&expected, &same, true).is_ok());

    let object = json!({"0": 1});
    assert!(compare_credential(&json!([1]), &object, true).is_err());
    assert!(compare_credential(&json!([1]), &json!(1), true).is_err());
}

#[test]
fn an_error_when_a_return_was_expected_names_returns() {
    let error = compare_result(
        &returns(json!(0.5)),
        Err(json!({"code": "extension_failed", "message": "lookup failed"})),
    )
    .join("\n");
    assert!(error.contains("returns"), "{error}");
}

/// The first-party package whose short name is the provider, and a
/// competitor that also registers it under a differing name.
const OPENROUTER: &str = "github.com/aakshintala/fiber/providers/openrouter";
const COMPETITOR: &str = "fiber.test/openrouter-competitor";

fn fixture_home(name: &str) -> (fakes::TempDir, std::path::PathBuf) {
    let root = fakes::TempDir::new(name);
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join("extensions")).unwrap();
    (root, home)
}

/// Writes the package's directory with a manifest and an entry script
/// that registers `provider`, or nothing when `provider` is `None`.
fn install_package(home: &std::path::Path, package: &str, provider: Option<&str>) {
    let dir = home.join("extensions").join(config::dir_name(package));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        json!({"name": package, "version": "0.1.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    let lua = provider.map_or_else(String::new, |name| {
        format!(
            "fiber.provider({name:?}, {{ cost = {{ timeout = 5000, \
             run = function() return 0.5 end }} }})"
        )
    });
    std::fs::write(dir.join("init.lua"), lua).unwrap();
}

fn installed(package: &str) -> extensions::Installed {
    extensions::Installed {
        name: package.to_owned(),
        version: "0.1.0".to_owned(),
        provenance: extensions::Provenance::Path(std::path::PathBuf::from("test")),
        requested: true,
        depends: std::collections::BTreeMap::new(),
    }
}

/// The name of the package `provider_extension` picks, if any.
fn resolved(
    installed: &[extensions::Installed],
    provider: &str,
    home: &std::path::Path,
) -> Option<String> {
    let clock: Arc<dyn contract::clock::Clock> = crate::case::clock::CaseClock::new();
    let host = extensions::HostScript::new(Vec::new(), Vec::new(), Vec::new());
    provider_extension(installed, provider, home, &clock, &host, true)
        .map(|extension| extension.name().to_owned())
}

#[test]
fn the_package_whose_short_name_is_the_provider_is_chosen() {
    let (_root, home) = fixture_home("fiber-call-same-name");
    install_package(&home, OPENROUTER, Some("openrouter"));
    install_package(&home, COMPETITOR, Some("openrouter"));
    let installed = [installed(OPENROUTER), installed(COMPETITOR)];
    assert_eq!(
        resolved(&installed, "openrouter", &home).as_deref(),
        Some(OPENROUTER)
    );
}

#[test]
fn the_registering_package_is_chosen_when_no_short_name_matches() {
    let (_root, home) = fixture_home("fiber-call-registered");
    install_package(&home, "quiet", None);
    install_package(&home, "casefixture", Some("acme"));
    // The decoy sorts first: `!=` would pick it over the registrar.
    let installed = [installed("quiet"), installed("casefixture")];
    assert_eq!(
        resolved(&installed, "acme", &home).as_deref(),
        Some("casefixture")
    );
}

#[test]
fn a_package_that_registers_nothing_is_not_chosen() {
    let (_root, home) = fixture_home("fiber-call-quiet-decoy");
    install_package(&home, OPENROUTER, Some("openrouter"));
    install_package(&home, "quiet", None);
    let installed = [installed(OPENROUTER), installed("quiet")];
    assert_eq!(
        resolved(&installed, "openrouter", &home).as_deref(),
        Some(OPENROUTER)
    );
}

#[test]
fn a_package_registering_a_different_provider_is_not_chosen() {
    let (_root, home) = fixture_home("fiber-call-other-provider-decoy");
    install_package(&home, OPENROUTER, Some("openrouter"));
    install_package(&home, "third", Some("something-else"));
    let installed = [installed(OPENROUTER), installed("third")];
    assert_eq!(
        resolved(&installed, "openrouter", &home).as_deref(),
        Some(OPENROUTER)
    );
}
