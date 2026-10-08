//! The `scripted` provider in a session (`docs/model-routing.md`, "The
//! scripted provider"): which references reach the registry, and that a
//! scripted model reads no credential and never warms.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::Path;
use std::sync::Arc;

use config::{Config, ProjectKey, Protocol, ProviderData, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use extensions::Providers;
use serde_json::json;

use super::{access, is_scripted, prepare, warm};
use crate::lua_providers::Access;

fn config(home: &Path, workspace: &Path, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: ProjectKey::new("test").unwrap(),
        overrides: overrides.iter().map(|o| (*o).to_owned()).collect(),
    })
    .unwrap()
}

/// The ids the `scripted` provider holds, in order.
fn scripted_ids(providers: &Providers) -> Vec<String> {
    providers
        .get("scripted")
        .map(|data| data.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default()
}

/// A provider named `name` with one model on `protocol`.
fn provider(name: &str, protocol: &str) -> ProviderData {
    serde_json::from_value(json!({
        "name": name,
        "models": [{ "id": "m", "protocol": protocol, "base_url": "u" }],
    }))
    .unwrap()
}

/// The scripted provider as the registry holds it.
fn scripted_provider() -> ProviderData {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json");
    providers.get("scripted").unwrap().clone()
}

#[test]
fn prepare_adds_the_configured_recorded_and_reviewer_references() {
    let root = fakes::TempDir::new("fiber-scripted-prepare");
    let config = config(
        root.path(),
        root.path(),
        &["model=scripted/a.json", "reviewer.model=scripted/r.json"],
    );
    let mut providers = Providers::default();
    prepare(&mut providers, &config, Some("scripted/old.json"));
    assert_eq!(scripted_ids(&providers), ["a.json", "r.json", "old.json"]);
}

#[test]
fn prepare_adds_nothing_for_other_references() {
    let root = fakes::TempDir::new("fiber-scripted-prepare");
    let config = config(
        root.path(),
        root.path(),
        &["model=openrouter/a.json", "reviewer.model=openrouter/r"],
    );
    let mut providers = Providers::default();
    prepare(&mut providers, &config, Some("openrouter/old"));
    prepare(&mut providers, &config, None);
    assert!(providers.get("scripted").is_none());
}

#[test]
fn a_scripted_provider_reads_no_credential() {
    let scripted = scripted_provider();
    assert!(is_scripted(&scripted));
    let got = access(&scripted, || {
        panic!("the reader ran for a scripted provider")
    })
    .unwrap();
    assert!(got.key.is_none());
    assert!(got.signer.is_none());
    assert!(got.lua.is_none());
}

#[test]
fn any_other_provider_defers_to_the_reader() {
    let other = provider("openrouter", "openai-responses");
    assert!(!is_scripted(&other));
    let got = access(&other, || {
        Ok(Access {
            key: Some(contract::Secret::new("k".to_owned())),
            signer: None,
            lua: None,
        })
    })
    .unwrap();
    assert!(got.key.is_some());
    let failed = access(&other, || {
        Err(doors::failure(ErrorCode::CredentialMissing, "none"))
    });
    assert!(failed.is_err());
}

#[test]
fn a_scripted_model_never_warms() {
    let scripted = scripted_provider();
    assert_eq!(scripted.models[0].protocol, Protocol::Scripted);
    assert_eq!(warm(&scripted.models[0], Some(2)), None);
    let other = provider("openrouter", "openai-responses");
    assert_eq!(warm(&other.models[0], Some(2)), Some(2));
    assert_eq!(warm(&other.models[0], None), None);
}

/// `parts_in` over an empty home and a workspace holding `files`, with
/// `config.json` holding `global`.
fn parts(
    global: &serde_json::Value,
    files: &[(&str, &str)],
    recorded: Option<&str>,
) -> Result<crate::Parts, contract::shapes::Failure> {
    let root = fakes::TempDir::new("fiber-scripted-parts");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(home.join("config.json"), global.to_string()).unwrap();
    for (name, text) in files {
        std::fs::write(workspace.join(name), text).unwrap();
    }
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    crate::parts_in(
        home, workspace, None, recorded, None, None, clock, None, None,
    )
}

const ONE_STEP: &str = r#"{"steps": [{"text": "Hi."}]}"#;

#[test]
fn a_scripted_session_starts_with_no_credential_and_no_warming() {
    let parts = parts(
        &json!({"model": "scripted/s.json", "cache": {"warm_idle": true}}),
        &[("s.json", ONE_STEP)],
        None,
    )
    .unwrap();
    assert_eq!(parts.model.reference, "scripted/s.json");
    assert!(parts.model.cost.is_none());
    assert!(!parts.model.subscription);
    assert_eq!(parts.warm, None);
    // No `reviewer.model`, and the scripted provider names no reviewer.
    match &parts.reviewer {
        Err(failure) => assert_eq!(failure.code, ErrorCode::NoModel),
        Ok(reviewer) => panic!("a reviewer resolved: {}", reviewer.model.reference),
    }
}

#[test]
fn a_resumed_scripted_session_starts_from_its_recorded_model() {
    let parts = parts(
        &json!({"model": "scripted/other.json"}),
        &[("s.json", ONE_STEP)],
        Some("scripted/s.json"),
    )
    .unwrap();
    assert_eq!(parts.model.reference, "scripted/s.json");
}

#[test]
fn a_scripted_reviewer_resolves_to_its_own_script() {
    let parts = parts(
        &json!({"model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"}}),
        &[("s.json", ONE_STEP), ("r.json", ONE_STEP)],
        None,
    )
    .unwrap();
    match &parts.reviewer {
        Ok(reviewer) => assert_eq!(reviewer.model.reference, "scripted/r.json"),
        Err(failure) => panic!("no reviewer: {}", failure.message),
    }
}

#[test]
fn a_missing_script_fails_io_failed_and_a_malformed_one_config_invalid() {
    let missing = parts(&json!({"model": "scripted/s.json"}), &[], None)
        .err()
        .unwrap();
    assert_eq!(missing.code, ErrorCode::IoFailed);
    assert!(missing.message.contains("s.json"), "{}", missing.message);
    let malformed = parts(
        &json!({"model": "scripted/s.json"}),
        &[("s.json", r#"{"steps": [{"text": "a"}, {"colour": 1}]}"#)],
        None,
    )
    .err()
    .unwrap();
    assert_eq!(malformed.code, ErrorCode::ConfigInvalid);
    assert!(
        malformed.message.contains("s.json"),
        "{}",
        malformed.message
    );
    assert!(
        malformed.message.contains("step 2"),
        "{}",
        malformed.message
    );
}
