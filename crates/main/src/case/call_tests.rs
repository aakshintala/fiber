//! Provider call dispatch and case verdicts (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    reason = "the call dispatch assertion fails with its result"
)]

use std::sync::Arc;

use serde_json::{Value, json};

use super::{CallOutcome, compare_result, function, provider_extension};

#[test]
fn only_cost_is_a_supported_provider_call() {
    assert_eq!(function("cost"), Ok(()));
    let error = function("models").unwrap_err();
    assert!(error.contains("cost"), "{error}");
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
    let host = extensions::HostScript::new(Vec::new(), Vec::new());
    provider_extension(installed, provider, home, &clock, &host)
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
