//! Per-account host placeholders (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use std::collections::HashMap;

use common::{Setup, install, manifest, write};
use config::{Config, ProjectKey, Sources, write_model_cache};
use contract::ErrorCode;
use contract::events::Notice;
use extensions::Providers;
use serde_json::{Value, json};

fn config(setup: &Setup, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

fn provider_with(name: &str, models: Value, placeholders: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), Value::String(name.into()));
    map.insert("models".into(), models);
    map.insert("placeholders".into(), placeholders);
    Value::Object(map)
}

fn model(id: &str, base_url: &str) -> Value {
    json!({"id": id, "protocol": "openai-responses", "base_url": base_url})
}

/// Installs extension `acme` registering provider `acme` with model `m` at
/// `https://{workspace}/v1` and no environment fallback.
fn install_template(setup: &Setup) {
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn global_setting_fills_the_url() {
    let setup = Setup::new();
    install_template(&setup);
    write(
        &setup.home().join("config/acme.json"),
        r#"{"workspace":"adb-1.example"}"#,
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let found = providers.resolve("acme/m").unwrap();
    assert_eq!(found.model.base_url, "https://adb-1.example/v1");
}

#[test]
fn per_project_setting_fills_the_url() {
    let setup = Setup::new();
    install_template(&setup);
    write(
        &setup.home().join("projects/p/config/acme.json"),
        r#"{"workspace":"adb-1.example"}"#,
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example/v1"
    );
}

#[test]
fn a_model_with_no_value_is_left_out_with_a_notice() {
    let setup = Setup::new();
    install_template(&setup);
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
    assert_eq!(notices[0].extension.as_deref(), Some("acme"));
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, which has no value."
    );
}

#[test]
fn the_named_environment_variable_fills_the_url() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {"env": "ACME_HOST"}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[("ACME_HOST", "adb-2.example")]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-2.example/v1"
    );
}

#[test]
fn the_setting_beats_the_environment() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {"env": "ACME_HOST"}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.home().join("config/acme.json"),
        r#"{"workspace":"adb-1.example"}"#,
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[("ACME_HOST", "adb-2.example")]);
    providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example/v1"
    );
}

#[test]
fn a_missing_environment_value_is_a_notice_naming_it() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {"env": "ACME_HOST"}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value, and `ACME_HOST` has none either."
    );
}

#[test]
fn the_environment_is_read_only_for_a_declared_name() {
    let setup = Setup::new();
    install_template(&setup);
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let notices = providers
        .fill_placeholders(&cfg, &|_name| Some("adb-3.example".to_owned()))
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, which has no value."
    );
}

#[test]
fn the_repository_never_supplies_the_host() {
    let setup = Setup::new();
    let mut manifest_value = manifest("acme");
    manifest_value["repo_settings"] = json!(["workspace"]);
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {}}),
    );
    let source = setup.source("acme", &manifest_value, &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.workspace().join(".fiber/config/acme.json"),
        r#"{"workspace":"evil.example"}"#,
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
}

#[test]
fn a_value_that_cannot_be_used_counts_as_no_value() {
    // Each case leaves the model out, with the environment named when one
    // is declared.
    let cases: Vec<(Value, Value, &str)> = vec![
        (
            json!(""),
            json!({"workspace": {"env": "ACME_HOST"}}),
            "The model `acme/m` needs the setting `workspace` for its base URL, \
          which has no value, and `ACME_HOST` has none either.",
        ),
        (
            json!(5),
            json!({"workspace": {"env": "ACME_HOST"}}),
            "The model `acme/m` needs the setting `workspace` for its base URL, \
          whose value is not a host.",
        ),
        (
            json!(true),
            json!({"workspace": {"env": "ACME_HOST"}}),
            "The model `acme/m` needs the setting `workspace` for its base URL, \
          whose value is not a host.",
        ),
    ];
    for (setting, placeholders, message) in cases {
        let setup = Setup::new();
        let mut settings = serde_json::Map::new();
        settings.insert("workspace".into(), setting);
        let data = provider_with(
            "acme",
            json!([model("m", "https://{workspace}/v1")]),
            placeholders,
        );
        let source = setup.source("acme", &manifest("acme"), &[data]);
        install(&setup.home(), &source, "0.1.0").unwrap();
        write(
            &setup.home().join("config/acme.json"),
            &Value::Object(settings).to_string(),
        );
        let cfg = config(&setup, &[]);
        let (mut providers, _) = Providers::load(&setup.home()).unwrap();
        let env = env_of(&[]);
        let notices = providers
            .fill_placeholders(&cfg, &|name| env.get(name).cloned())
            .unwrap();
        assert!(providers.resolve("acme/m").is_err());
        assert_eq!(notices[0].message, message);
    }
    // An empty environment value counts as no value too, with no setting.
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {"env": "ACME_HOST"}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[("ACME_HOST", "")]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert!(providers.resolve("acme/m").is_err());
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value, and `ACME_HOST` has none either."
    );
    // An empty setting counts as unset, so the environment fills it.
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {"env": "ACME_HOST"}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.home().join("config/acme.json"),
        r#"{"workspace":""}"#,
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[("ACME_HOST", "adb-4.example")]);
    providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-4.example/v1"
    );
}

#[test]
fn a_non_string_setting_is_not_replaced_by_the_environment() {
    // A present setting that is not a string leaves the model out with the
    // setting-source notice, even when the environment holds a valid host.
    for setting in [json!(true), json!({"zone": "a"})] {
        let setup = Setup::new();
        let data = provider_with(
            "acme",
            json!([model("m", "https://{workspace}/v1")]),
            json!({"workspace": {"env": "ACME_HOST"}}),
        );
        let source = setup.source("acme", &manifest("acme"), &[data]);
        install(&setup.home(), &source, "0.1.0").unwrap();
        let mut settings = serde_json::Map::new();
        settings.insert("workspace".into(), setting.clone());
        write(
            &setup.home().join("config/acme.json"),
            &Value::Object(settings).to_string(),
        );
        let cfg = config(&setup, &[]);
        let (mut providers, _) = Providers::load(&setup.home()).unwrap();
        let env = env_of(&[("ACME_HOST", "adb-2.example")]);
        let notices = providers
            .fill_placeholders(&cfg, &|name| env.get(name).cloned())
            .unwrap();
        assert_eq!(notices.len(), 1, "{setting}");
        assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
        assert_eq!(
            notices[0].message,
            "The model `acme/m` needs the setting `workspace` for its base URL, \
             whose value is not a host.",
            "{setting}"
        );
        assert!(
            !notices[0].message.contains("adb-2.example"),
            "{setting}: {}",
            notices[0].message
        );
        assert!(
            !notices[0].message.contains(&setting.to_string()),
            "{setting}: {}",
            notices[0].message
        );
        let err = providers.resolve("acme/m").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ModelUnconfigured, "{setting}");
    }
}

#[test]
fn braces_that_are_not_placeholders_stay_as_written() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{work.space}/v1/{}")]),
        json!({"workspace": {}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(&setup.home().join("config/acme.json"), "not json");
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let calls = std::cell::Cell::new(0);
    let notices = providers
        .fill_placeholders(&cfg, &|_name| {
            calls.set(calls.get() + 1);
            None
        })
        .unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(calls.get(), 0);
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://{work.space}/v1/{}"
    );
}

#[test]
fn a_settings_file_that_cannot_be_read_fails_the_fill() {
    let setup = Setup::new();
    install_template(&setup);
    write(&setup.home().join("config/acme.json"), "not json");
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let err = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn the_cache_keeps_the_template() {
    let setup = Setup::new();
    install_template(&setup);
    write(
        &setup.home().join("config/acme.json"),
        r#"{"workspace":"adb-1.example"}"#,
    );
    write_model_cache(
        &setup.home(),
        "acme",
        &json!([model("m", "https://{workspace}/v1")]),
    )
    .unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example/v1"
    );
    let cached: Vec<config::ModelData> = config::read_model_cache(&setup.home(), "acme")
        .unwrap()
        .unwrap();
    assert_eq!(cached[0].base_url, "https://{workspace}/v1");
}

fn lua_named(
    setup: &Setup,
    dir: &str,
    provider: &str,
    models_run: &str,
) -> std::sync::Arc<extensions::LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("init.lua"),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"test-token\", expires_at = 1893456000 }} end }}, \
             models = {{ timeout = 1000, run = function() return {models_run} end }} }})\n"
        ),
    );
    let extension = std::sync::Arc::new(extensions::LuaExtension::new(
        "acme-ext",
        ext,
        setup.home(),
        fakes::clock::FakeClock::new(),
    ));
    extensions::LuaProvider::new(extension, provider)
}

#[test]
fn lua_models_are_filled_and_the_cache_keeps_the_template() {
    let setup = Setup::new();
    let mut providers = Providers::default();
    let lua = lua_named(
        &setup,
        "ext",
        "acme",
        "{ { id = \"m\", protocol = \"openai-responses\", \
         base_url = \"https://{workspace}/v1\" } }",
    );
    let cfg = config(
        &setup,
        &["extensions.\"acme-ext\".settings.workspace=h.example"],
    );
    providers.add_lua("acme-ext", &lua, &cfg);
    let env = env_of(&[]);
    providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://h.example/v1"
    );
    let cached: Vec<config::ModelData> = config::read_model_cache(&setup.home(), "acme")
        .unwrap()
        .unwrap();
    assert_eq!(cached[0].base_url, "https://{workspace}/v1");
}

#[test]
fn kept_models_stay_and_notices_come_in_order() {
    let setup = Setup::new();
    for name in ["a", "b"] {
        let data = provider_with(
            name,
            json!([
                model("m", "https://{workspace}/v1"),
                model("plain", "http://127.0.0.1:1/v1"),
            ]),
            json!({"workspace": {}}),
        );
        let source = setup.source(name, &manifest(name), &[data]);
        install(&setup.home(), &source, "0.1.0").unwrap();
    }
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    for name in ["a", "b"] {
        assert_eq!(
            providers
                .resolve(&format!("{name}/plain"))
                .unwrap()
                .model
                .base_url,
            "http://127.0.0.1:1/v1"
        );
        assert!(providers.resolve(&format!("{name}/m")).is_err());
    }
    assert_eq!(notices.len(), 2);
    assert!(
        notices[0].message.contains("`a/m`"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[1].message.contains("`b/m`"),
        "{}",
        notices[1].message
    );
}

/// Installs `acme` with model `m` at `base_url` and fills it: `setting` is
/// the `workspace` setting when present, `env` the process environment.
fn fill_with(
    setting: Option<&str>,
    env: &[(&str, &str)],
    placeholders: Value,
    base_url: &str,
) -> (Providers, Vec<Notice>) {
    let setup = Setup::new();
    let data = provider_with("acme", json!([model("m", base_url)]), placeholders);
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    if let Some(value) = setting {
        write(
            &setup.home().join("config/acme.json"),
            &json!({"workspace": value}).to_string(),
        );
    }
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let map = env_of(env);
    let notices = providers
        .fill_placeholders(&cfg, &|name| map.get(name).cloned())
        .unwrap();
    (providers, notices)
}

/// `value` as the `workspace` setting leaves the model out with the
/// setting-source "not a host" notice, and resolving names it the same way.
fn assert_not_a_host(value: &str) {
    let setup = Setup::new();
    install_template(&setup);
    write(
        &setup.home().join("config/acme.json"),
        &json!({"workspace": value}).to_string(),
    );
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 1, "{value:?}");
    let notice = notices.first().unwrap();
    assert_eq!(notice.code, ErrorCode::ModelUnconfigured);
    assert_eq!(notice.extension.as_deref(), Some("acme"));
    assert_eq!(
        notice.message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         whose value is not a host."
    );
    assert!(!notice.message.contains(value), "{value:?}");
    let err = providers.resolve("acme/m").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert_eq!(err.to_string(), notice.message);
}

#[test]
fn an_https_host_with_a_trailing_slash_fills_the_url() {
    let (providers, notices) = fill_with(
        None,
        &[("DATABRICKS_HOST", "https://adb-1.azuredatabricks.net/")],
        json!({"workspace": {"env": "DATABRICKS_HOST"}}),
        "https://{workspace}/ai-gateway/anthropic",
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.azuredatabricks.net/ai-gateway/anthropic"
    );
    let (providers, notices) = fill_with(
        Some("adb-1.example:8443"),
        &[],
        json!({"workspace": {}}),
        "https://{workspace}/v1",
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example:8443/v1"
    );
}

#[test]
fn a_value_with_a_path_is_not_a_host() {
    for value in [
        "adb-1.example/x",
        "https://adb-1.example/v1/",
        "adb-1.example//",
    ] {
        assert_not_a_host(value);
    }
}

#[test]
fn a_value_with_an_at_sign_is_not_a_host() {
    for value in ["user@adb-1.example", "https://user:pw@adb-1.example"] {
        assert_not_a_host(value);
    }
}

#[test]
fn a_value_with_a_question_mark_is_not_a_host() {
    assert_not_a_host("adb-1.example?x=1");
}

#[test]
fn a_value_with_a_hash_is_not_a_host() {
    assert_not_a_host("adb-1.example#x");
}

#[test]
fn a_value_with_whitespace_is_not_a_host() {
    for value in [
        "adb-1.example ",
        " adb-1.example",
        "adb-1\t.example",
        "adb-1.example\n",
    ] {
        assert_not_a_host(value);
    }
}

#[test]
fn a_value_with_another_scheme_is_not_a_host() {
    for value in [
        "http://adb-1.example",
        "HTTPS://adb-1.example",
        "ftp://adb-1.example",
        "https://https://adb-1.example",
    ] {
        assert_not_a_host(value);
    }
}

#[test]
fn an_environment_value_that_is_not_a_host_names_the_variable() {
    let (providers, notices) = fill_with(
        None,
        &[("ACME_HOST", "evil@x")],
        json!({"workspace": {"env": "ACME_HOST"}}),
        "https://{workspace}/v1",
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value, and the value of `ACME_HOST` is not a host."
    );
    assert!(!notices[0].message.contains("evil@x"));
    let err = providers.resolve("acme/m").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert_eq!(err.to_string(), notices[0].message);
}

#[test]
fn a_setting_that_is_not_a_host_is_not_replaced_by_the_environment() {
    let (providers, notices) = fill_with(
        Some("a/b"),
        &[("ACME_HOST", "adb-2.example")],
        json!({"workspace": {"env": "ACME_HOST"}}),
        "https://{workspace}/v1",
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         whose value is not a host."
    );
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );
}

#[test]
fn choosing_an_unconfigured_model_is_model_unconfigured() {
    let setup = Setup::new();
    install_template(&setup);
    let flag = config(&setup, &["model=acme/m"]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&flag, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 1);
    for typed in ["acme/m", "acme/m:high", "m"] {
        let err = providers.resolve(typed).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ModelUnconfigured, "{typed}");
        assert_eq!(err.to_string(), notices[0].message, "{typed}");
    }
    let err = providers.choose(None, &flag).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert_eq!(err.to_string(), notices[0].message);
}

#[test]
fn a_bare_id_in_a_configured_and_an_unconfigured_provider_is_ambiguous() {
    let setup = Setup::new();
    let data = provider_with("a", json!([model("m", "http://127.0.0.1:1/v1")]), json!({}));
    let source = setup.source("a", &manifest("a"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let data = provider_with(
        "b",
        json!([model("m", "https://{workspace}/v1")]),
        json!({"workspace": {}}),
    );
    let source = setup.source("b", &manifest("b"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    let err = providers.resolve("m").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelAmbiguous);
    let text = err.to_string();
    assert!(text.contains("a/m"), "{text}");
    assert!(text.contains("b/m"), "{text}");
    assert!(
        text.find("a/m").unwrap() < text.find("b/m").unwrap(),
        "{text}"
    );
    assert!(providers.resolve("a/m").is_ok());
    assert_eq!(
        providers.resolve("b/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );
}

#[test]
fn each_unconfigured_model_keeps_its_own_message() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([
            model("m", "https://{workspace}/v1"),
            model("n", "https://{region}/v1"),
        ]),
        json!({"workspace": {}, "region": {}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 2);
    let err = providers.resolve("acme/n").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert!(err.to_string().contains("`acme/n`"), "{err}");
    assert!(err.to_string().contains("`region`"), "{err}");
    let err = providers.resolve("n").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert!(err.to_string().contains("`acme/n`"), "{err}");
    let err = providers.resolve("acme/m").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ModelUnconfigured);
    assert!(err.to_string().contains("`workspace`"), "{err}");
}

#[test]
fn the_first_bad_placeholder_in_template_order_is_reported() {
    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{a}.{b}/v1")]),
        json!({"a": {}, "b": {}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(&setup.home().join("config/acme.json"), r#"{"a":"x/y"}"#);
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].message.contains("`a`"), "{}", notices[0].message);
    assert!(
        notices[0].message.contains("is not a host"),
        "{}",
        notices[0].message
    );

    let setup = Setup::new();
    let data = provider_with(
        "acme",
        json!([model("m", "https://{a}.{b}/v1")]),
        json!({"a": {}, "b": {}}),
    );
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(&setup.home().join("config/acme.json"), r#"{"b":"x/y"}"#);
    let cfg = config(&setup, &[]);
    let (mut providers, _) = Providers::load(&setup.home()).unwrap();
    let env = env_of(&[]);
    let notices = providers
        .fill_placeholders(&cfg, &|name| env.get(name).cloned())
        .unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].message.contains("`a`"), "{}", notices[0].message);
    assert!(
        notices[0].message.contains("has no value"),
        "{}",
        notices[0].message
    );
}
