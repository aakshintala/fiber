//! Unit tests for adding the built-in `scripted` provider to a registry
//! (`docs/model-routing.md`, "The scripted provider").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use config::{Config, CredentialSource, ModelData, ProjectKey, Protocol, ProviderData, Sources};
use contract::ErrorCode;
use fakes::clock::FakeClock;
use serde_json::{Map, json};

use super::{SCRIPTED, SCRIPTED_CONTEXT_WINDOW};
use crate::{Error, LuaExtension, LuaProvider, Providers};

/// The ids the `scripted` provider holds, in order; empty when it is absent.
fn scripted_ids(providers: &Providers) -> Vec<String> {
    providers
        .get("scripted")
        .map(|data| data.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default()
}

/// Writes the install record `extensions/<dir>/.fiber.json` holds, so the
/// directory is healthy: a directory with no record is damaged and its
/// providers are left out (`docs/extensions.md`, "Installing").
fn write_record(dir: &std::path::Path) {
    let text = std::fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0.0");
    std::fs::write(
        dir.join(".fiber.json"),
        serde_json::json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}}).to_string(),
    )
    .unwrap();
}

/// Writes an installed extension `name` serving one provider of the same
/// name with these model ids straight into `home`'s `extensions/`.
/// Writes an installed extension `ext` serving a data provider named
/// `scripted` with these model ids straight into `home`'s `extensions/`:
/// the name no installed package may claim (`docs/model-routing.md`, "The
/// scripted provider").
fn install_scripted_provider(home: &Path, ext: &str, ids: &[&str]) {
    let dir = home.join("extensions").join(ext);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": ext, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    let models: Vec<_> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    fs::write(
        dir.join("providers").join("scripted.json"),
        json!({ "name": "scripted", "models": models }).to_string(),
    )
    .unwrap();
    write_record(&dir);
}

fn install_provider(home: &Path, name: &str, ids: &[&str]) {
    let dir = home.join("extensions").join(name);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    write_record(&dir);
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
fn a_damaged_directory_s_providers_are_left_out() {
    let root = fakes::TempDir::new("fiber-damaged-providers");
    let home = root.path().join("home");
    install_provider(&home, "healthy", &["m"]);
    // Damaged: a manifest and provider data but no install record, so it
    // holds no installed extension (`docs/extensions.md`, "Installing").
    let dir = home.join("extensions").join("broken");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": "broken", "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join("broken.json"),
        json!({ "name": "broken", "models": [{ "id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }] }).to_string(),
    )
    .unwrap();
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().collect::<Vec<_>>(), ["healthy"]);
    assert!(providers.get("broken").is_none());
    assert!(providers.resolve("healthy/m").is_ok());
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
fn an_installed_data_provider_named_scripted_is_left_out_with_a_notice() {
    let root = fakes::TempDir::new("fiber-scripted-reserved-data");
    let home = root.path().join("home");
    install_scripted_provider(&home, "evil", &["s.json"]);
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(providers.get("scripted").is_none());
    assert!(providers.names().collect::<Vec<_>>().is_empty());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("evil"));
    assert!(
        notices[0].message.contains("`scripted`"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[0].message.contains("reserved"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_lua_provider_named_scripted_is_left_out_with_a_notice() {
    let root = fakes::TempDir::new("fiber-scripted-reserved-lua");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ext = root.path().join("ext");
    fs::create_dir_all(&ext).unwrap();
    fs::write(
        ext.join("init.lua"),
        "fiber.provider(\"scripted\", { \
         credential = { timeout = 1000, run = function() \
         return { token = \"test-token\", expires_at = 1893456000 } end }, \
         models = { timeout = 1000, run = function() return { { id = \"s.json\", \
         protocol = \"openai-responses\", base_url = \"http://127.0.0.1:1/v1\", \
         context_window = 1000 } } end } })\n",
    )
    .unwrap();
    let lua = LuaProvider::new(
        Arc::new(LuaExtension::new(
            "evil",
            ext,
            home.clone(),
            FakeClock::new(),
        )),
        "scripted",
    );
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: home,
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let mut providers = Providers::default();
    let notices = providers.add_lua("evil", &lua, &config);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("evil"));
    assert!(
        notices[0].message.contains("`scripted`"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[0].message.contains("reserved"),
        "{}",
        notices[0].message
    );
    assert!(providers.get("scripted").is_none());
    assert!(providers.lua("scripted").is_none());
}

#[test]
fn a_scripted_reference_reads_the_script_when_a_package_claims_the_name() {
    let root = fakes::TempDir::new("fiber-scripted-reserved-resolve");
    let home = root.path().join("home");
    install_scripted_provider(&home, "evil", &["s.json"]);
    let (mut providers, notices) = Providers::load(&home).unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    providers.add_scripted("scripted/s.json");
    let model = providers.resolve("scripted/s.json").unwrap();
    assert_eq!(model.provider.name, "scripted");
    assert_eq!(model.model.id, "s.json");
    assert_eq!(model.model.protocol, Protocol::Scripted);
    assert_eq!(model.provider.credential, None);
    assert_eq!(model.provider.credential_name, None);
}

#[test]
fn add_scripted_replaces_an_entry_it_did_not_build() {
    let mut providers = Providers::default();
    providers.by_name.insert(
        SCRIPTED.to_owned(),
        ProviderData {
            name: SCRIPTED.to_owned(),
            credential: Some(CredentialSource::Env("EVIL_KEY".to_owned())),
            credential_name: None,
            headers: BTreeMap::new(),
            placeholders: BTreeMap::new(),
            models: vec![ModelData {
                id: "s.json".to_owned(),
                protocol: Protocol::OpenaiResponses,
                base_url: "http://127.0.0.1:1/v1".to_owned(),
                compat: Map::new(),
                deferred_tools: false,
                extra_body: Map::new(),
                context_window: Some(1000),
                max_output_tokens: None,
                input: Vec::new(),
                cost: None,
                subscription: false,
                web_search: None,
                thinking_levels: Vec::new(),
                thinking_default: None,
                prompt_addendum: None,
            }],
            reviewer_model: None,
            login: None,
        },
    );
    providers.add_scripted("scripted/s.json");
    let model = providers.resolve("scripted/s.json").unwrap();
    assert_eq!(model.model.protocol, Protocol::Scripted);
    assert_eq!(model.provider.credential, None);
    assert!(model.provider.headers.is_empty());
    assert_eq!(providers.get("scripted").unwrap().models.len(), 1);
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
