//! Unit tests for adding the built-in `scripted` provider to a registry
//! (`docs/model-routing.md`, "The scripted provider").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::fs;
use std::path::Path;

use config::Protocol;
use serde_json::json;

use super::SCRIPTED_CONTEXT_WINDOW;
use crate::{Error, Providers};

/// The ids the `scripted` provider holds, in order; empty when it is absent.
fn scripted_ids(providers: &Providers) -> Vec<String> {
    providers
        .get("scripted")
        .map(|data| data.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default()
}

/// Writes an installed extension `name` serving one provider of the same
/// name with these model ids straight into `home`'s `extensions/`.
fn install_provider(home: &Path, name: &str, ids: &[&str]) {
    let dir = home.join("extensions").join(name);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    let models: Vec<_> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    fs::write(
        dir.join("providers").join(format!("{name}.json")),
        json!({ "name": name, "models": models }).to_string(),
    )
    .unwrap();
}

#[test]
fn a_scripted_reference_resolves_to_a_model_whose_id_is_the_path() {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json");
    let model = providers.resolve("scripted/a.json").unwrap();
    assert_eq!(model.provider.name, "scripted");
    assert_eq!(model.model.id, "a.json");
    assert_eq!(model.model.protocol, Protocol::Scripted);
    assert_eq!(model.model.context_window, Some(1_000_000));
    assert_eq!(SCRIPTED_CONTEXT_WINDOW, 1_000_000);
    assert_eq!(model.model.cost, None);
    assert!(!model.model.subscription);
    assert_eq!(model.provider.credential, None);
    assert_eq!(model.provider.credential_name, None);
    assert_eq!(model.provider.reviewer_model, None);
    assert!(model.provider.headers.is_empty());
}

#[test]
fn a_second_reference_adds_a_model_and_keeps_the_first() {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json");
    providers.add_scripted("scripted/b/c.json");
    assert_eq!(scripted_ids(&providers), ["a.json", "b/c.json"]);
    assert_eq!(
        providers.resolve("scripted/b/c.json").unwrap().model.id,
        "b/c.json"
    );
}

#[test]
fn the_same_reference_twice_adds_one_model() {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json");
    providers.add_scripted("scripted/a.json");
    assert_eq!(scripted_ids(&providers), ["a.json"]);
}

#[test]
fn a_thinking_suffix_is_not_part_of_the_path() {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json:high");
    assert_eq!(scripted_ids(&providers), ["a.json"]);
}

#[test]
fn a_reference_that_names_no_script_adds_nothing() {
    for typed in [
        "openrouter/x",
        "scripted",
        "scripted/",
        "scripted/:high",
        "a.json",
    ] {
        let mut providers = Providers::default();
        providers.add_scripted(typed);
        assert!(providers.get("scripted").is_none(), "{typed}");
    }
}

#[test]
fn a_loaded_registry_never_holds_the_scripted_provider() {
    let root = fakes::TempDir::new("fiber-scripted-registry");
    let home = root.path().join("home");
    install_provider(&home, "openrouter", &["a.json"]);
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().collect::<Vec<_>>(), ["openrouter"]);
    assert!(providers.get("scripted").is_none());
}

#[test]
fn a_scripted_model_is_chosen_only_when_named_in_full() {
    let mut providers = Providers::default();
    providers.add_scripted("scripted/a.json");
    let err = providers.resolve("a.json").unwrap_err();
    assert!(matches!(err, Error::ModelMissing { .. }), "{err:?}");
    assert_eq!(
        providers.resolve("scripted/a.json").unwrap().reference(),
        "scripted/a.json"
    );
}

#[test]
fn a_bare_id_an_installed_provider_shares_resolves_to_that_provider() {
    let root = fakes::TempDir::new("fiber-scripted-registry");
    let home = root.path().join("home");
    install_provider(&home, "openrouter", &["a.json"]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    providers.add_scripted("scripted/a.json");
    assert_eq!(
        providers.resolve("a.json").unwrap().reference(),
        "openrouter/a.json"
    );
    assert_eq!(
        providers.resolve("scripted/a.json").unwrap().reference(),
        "scripted/a.json"
    );
}
