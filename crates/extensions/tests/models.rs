//! `docs/model-routing.md`, "Naming a model" and "Choosing the model", and
//! `docs/extensions.md`, "A fresh install": an installed provider's models
//! are reached as `provider/model`, and a run with no model, no credential or
//! no provider fails with its code.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::{Setup, install, manifest, provider, write};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use extensions::{Error, Providers};
use serde_json::json;

fn installed(setup: &Setup, extensions: &[(&str, serde_json::Value)]) -> Providers {
    for (name, data) in extensions {
        let source = setup.source(name, &manifest(name), std::slice::from_ref(data));
        install(&setup.home(), &source, "0.1.0").unwrap();
    }
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
}

fn config(setup: &Setup, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

#[test]
fn an_exact_reference_resolves_even_when_its_id_holds_colons() {
    let setup = Setup::new();
    let providers = installed(
        &setup,
        &[(
            "openrouter",
            provider("openrouter", &["anthropic/claude:high", "anthropic/claude"]),
        )],
    );
    let exact = providers
        .resolve("openrouter/anthropic/claude:high")
        .unwrap();
    assert_eq!(exact.reference(), "openrouter/anthropic/claude:high");
    assert_eq!(exact.thinking, None);
    assert_eq!(exact.provider.name, "openrouter");
    assert_eq!(exact.model.id, "anthropic/claude:high");
}

#[test]
fn a_thinking_suffix_is_stripped_when_the_exact_string_matches_nothing() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let model = providers
            .resolve(&format!("openai/gpt-5.6:{level}"))
            .unwrap();
        assert_eq!(model.reference(), "openai/gpt-5.6");
        assert_eq!(model.thinking, Some(level));
    }
    let err = providers.resolve("openai/gpt-5.6:extreme").unwrap_err();
    assert!(matches!(err, Error::UnknownModel { .. }), "{err:?}");
}

#[test]
fn a_bare_id_resolves_when_exactly_one_provider_has_it() {
    let setup = Setup::new();
    let providers = installed(
        &setup,
        &[
            ("openai", provider("openai", &["gpt-5.6"])),
            ("opencode", provider("opencode", &["glm-5"])),
        ],
    );
    let model = providers.resolve("gpt-5.6").unwrap();
    assert_eq!(model.reference(), "openai/gpt-5.6");
    let model = providers.resolve("glm-5:low").unwrap();
    assert_eq!(model.reference(), "opencode/glm-5");
    assert_eq!(model.thinking, Some("low"));
}

#[test]
fn a_bare_id_two_providers_have_is_an_error_listing_both() {
    let setup = Setup::new();
    let providers = installed(
        &setup,
        &[
            ("databricks", provider("databricks", &["claude-opus-5"])),
            ("muse", provider("muse", &["claude-opus-5"])),
        ],
    );
    let err = providers.resolve("claude-opus-5").unwrap_err();
    let Error::Ambiguous { matches, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(matches, &["databricks/claude-opus-5", "muse/claude-opus-5"]);
    let message = err.to_string();
    assert!(message.contains("databricks/claude-opus-5") && message.contains("muse/claude-opus-5"));
    assert_eq!(err.code(), ErrorCode::ModelAmbiguous);
}

#[test]
fn a_provider_that_is_not_installed_is_extension_missing() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    for typed in ["openrouter/x", "openrouter/x:high", "nobody-has-this"] {
        let err = providers.resolve(typed).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionMissing, "{typed}: {err:?}");
    }
    let err = providers.resolve("openai/gpt-4").unwrap_err();
    assert!(matches!(err, Error::UnknownModel { .. }), "{err:?}");
    let (empty, _) = Providers::load(&setup.home().join("nowhere")).unwrap();
    let err = empty.resolve("openai/gpt-5.6").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionMissing);
}

#[test]
fn the_model_is_chosen_resumed_first_then_configuration() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["a", "b", "c"]))]);
    write(
        &setup.home().join("config.json"),
        r#"{"model": "openai/a"}"#,
    );
    let global = config(&setup, &[]);
    let flag = config(&setup, &["model=openai/b"]);
    assert_eq!(
        providers.choose(None, &global).unwrap().reference(),
        "openai/a"
    );
    assert_eq!(
        providers.choose(None, &flag).unwrap().reference(),
        "openai/b"
    );
    assert_eq!(
        providers
            .choose(Some("openai/c"), &flag)
            .unwrap()
            .reference(),
        "openai/c"
    );
}

#[test]
fn nothing_choosing_a_model_is_no_model() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["a"]))]);
    let err = providers.choose(None, &config(&setup, &[])).unwrap_err();
    assert!(matches!(err, Error::NoModel), "{err:?}");
    assert_eq!(err.code(), ErrorCode::NoModel);
}

#[test]
fn an_installed_provider_is_reached_with_its_credential() {
    let setup = Setup::new();
    let mut data = provider("acme", &["m1"]);
    data["credential"] = json!({ "command": ["printf", "key-from-command\n"] });
    data["headers"] = json!({ "x-client": "fiber" });
    let providers = installed(&setup, &[("acme", data)]);
    let config = config(&setup, &["model=acme/m1"]);
    let model = providers.choose(None, &config).unwrap();
    assert_eq!(model.provider.headers["x-client"], "fiber");
    assert_eq!(model.model.base_url, "http://127.0.0.1:1/v1");
    let key = config.credential(model.provider, "default").unwrap();
    assert_eq!(key.expose(), "key-from-command");
}

#[test]
fn a_run_whose_provider_has_no_credential_is_credential_missing() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("acme", provider("acme", &["m1"]))]);
    let config = config(&setup, &["model=acme/m1"]);
    let model = providers.choose(None, &config).unwrap();
    let err = config.credential(model.provider, "default").unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
}

#[test]
fn an_extension_for_another_api_is_left_out_with_a_notice() {
    let setup = Setup::new();
    installed(&setup, &[("openai", provider("openai", &["a"]))]);
    let mut other = manifest("acme");
    other["api"] = json!(2);
    let dir = setup.home().join("extensions/acme");
    write(&dir.join("extension.json"), &other.to_string());
    write(
        &dir.join("providers/acme.json"),
        &provider("acme", &["m1"]).to_string(),
    );
    write(
        &setup.home().join("extensions/.acme.1.new/extension.json"),
        "not json",
    );
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionIncompatible);
    assert_eq!(notices[0].extension.as_deref(), Some("acme"));
    assert!(
        notices[0].message.contains("API 2"),
        "{}",
        notices[0].message
    );
    assert!(providers.resolve("acme/m1").is_err());
    assert!(providers.resolve("openai/a").is_ok());
}

#[test]
fn an_extensions_directory_that_cannot_be_listed_is_io_failed() {
    let setup = Setup::new();
    write(&setup.home().join("extensions"), "not a directory");
    let err = Providers::load(&setup.home()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed);
}
