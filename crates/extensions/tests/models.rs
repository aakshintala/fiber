//! `docs/model-routing.md`, "Naming a model" and "Choosing the model", and
//! `docs/extensions.md`, "A fresh install": an installed provider's models
//! are reached as `provider/model`, and a run with no model, no credential or
//! no provider fails with its code.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::{Setup, install, manifest, provider, write};
use config::{Config, ModelData, ProjectKey, Sources, write_model_cache};
use contract::ErrorCode;
use extensions::{Error, Providers, leave_out_invalid};
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

#[test]
fn the_installed_providers_are_listed_by_name_and_looked_up() {
    let setup = Setup::new();
    let providers = installed(
        &setup,
        &[
            ("zed", provider("zed", &["a"])),
            ("alpha", provider("alpha", &["b"])),
        ],
    );
    assert_eq!(providers.names().collect::<Vec<_>>(), ["alpha", "zed"]);
    assert_eq!(providers.get("zed").unwrap().name, "zed");
    assert!(providers.get("nobody").is_none());
    assert_eq!(Providers::default().names().count(), 0);
}

#[test]
fn a_model_whose_web_search_its_protocol_does_not_read_is_left_out() {
    let setup = Setup::new();
    let data = serde_json::json!({
        "name": "acme",
        "models": [
            {"id": "good", "protocol": "anthropic-messages",
             "base_url": "http://127.0.0.1:1/v1", "web_search": "web_search_20250305"},
            {"id": "plain", "protocol": "anthropic-messages",
             "base_url": "http://127.0.0.1:1/v1"},
            {"id": "bad", "protocol": "anthropic-messages",
             "base_url": "http://127.0.0.1:1/v1", "web_search": "web_search_20260209"},
            {"id": "wrong", "protocol": "openai-responses",
             "base_url": "http://127.0.0.1:1/v1", "web_search": "web_search_20250305"},
        ]
    });
    let source = setup.source("acme", &manifest("acme"), std::slice::from_ref(&data));
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/good").is_ok());
    assert!(providers.resolve("acme/plain").is_ok());
    assert!(matches!(
        providers.resolve("acme/bad").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert!(matches!(
        providers.resolve("acme/wrong").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert_eq!(notices.len(), 2);
    for notice in &notices {
        assert_eq!(notice.code, ErrorCode::ModelInvalid);
        assert_eq!(notice.extension.as_deref(), Some("acme"));
    }
    let messages: Vec<&str> = notices.iter().map(|n| n.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("acme/bad") && m.contains("web_search_20260209")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("acme/wrong") && m.contains("web_search_20250305")),
        "{messages:?}"
    );
}

fn reserved_case(protocol: &str, reserved: &[&str]) {
    let setup = Setup::new();
    let mut models = Vec::new();
    for field in reserved {
        models.push(json!({
            "id": format!("bad-{field}"),
            "protocol": protocol,
            "base_url": "http://127.0.0.1:1/v1",
            "extra_body": { (*field): 1 }
        }));
    }
    models.push(json!({
        "id": "ok", "protocol": protocol,
        "base_url": "http://127.0.0.1:1/v1",
        "extra_body": { "max_tokens": 1 }
    }));
    let data = json!({ "name": "acme", "models": models });
    let source = setup.source("acme", &manifest("acme"), std::slice::from_ref(&data));
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/ok").is_ok());
    for field in reserved {
        let id = format!("acme/bad-{field}");
        assert!(
            matches!(
                providers.resolve(&id).unwrap_err(),
                Error::UnknownModel { .. }
            ),
            "{id}"
        );
    }
    assert_eq!(notices.len(), reserved.len(), "{notices:?}");
    for (notice, field) in notices.iter().zip(reserved.iter()) {
        assert_eq!(notice.code, ErrorCode::ModelInvalid);
        assert_eq!(notice.extension.as_deref(), Some("acme"));
        assert_eq!(
            notice.message,
            format!(
                "The model `acme/bad-{field}` names `{field}` in its `extra_body`, \
                 a field Fiber builds itself."
            )
        );
    }
}

#[test]
fn anthropic_reserved_extra_body_fields_leave_the_model_out() {
    reserved_case(
        "anthropic-messages",
        &[
            "model",
            "system",
            "messages",
            "tools",
            "tool_choice",
            "stream",
        ],
    );
}

#[test]
fn completions_reserved_extra_body_fields_leave_the_model_out() {
    reserved_case(
        "openai-completions",
        &["model", "messages", "tools", "tool_choice", "stream"],
    );
}

#[test]
fn responses_reserved_extra_body_fields_leave_the_model_out() {
    reserved_case(
        "openai-responses",
        &[
            "model",
            "instructions",
            "input",
            "tools",
            "tool_choice",
            "stream",
        ],
    );
}

#[test]
fn google_reserved_extra_body_fields_leave_the_model_out() {
    reserved_case(
        "google-generative-ai",
        &["systemInstruction", "contents", "tools", "toolConfig"],
    );
}

#[test]
fn bedrock_reserved_extra_body_fields_leave_the_model_out() {
    reserved_case("bedrock-converse", &["system", "messages", "toolConfig"]);
}

#[test]
fn extra_body_matching_is_exact_and_top_level_only() {
    let setup = Setup::new();
    let data = json!({
        "name": "acme",
        "models": [{
            "id": "ok", "protocol": "google-generative-ai",
            "base_url": "http://127.0.0.1:1/v1",
            "extra_body": {
                "Tools": [],
                "generationConfig": { "tools": 1 }
            }
        }]
    });
    let source = setup.source("acme", &manifest("acme"), std::slice::from_ref(&data));
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/ok").is_ok());
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn a_model_with_two_reserved_fields_and_an_unread_search_gets_two_notices() {
    let setup = Setup::new();
    let data = json!({
        "name": "acme",
        "models": [{
            "id": "bad", "protocol": "anthropic-messages",
            "base_url": "http://127.0.0.1:1/v1",
            "web_search": "web_search_20260209",
            "extra_body": { "tools": 1, "system": 1 }
        }]
    });
    let source = setup.source("acme", &manifest("acme"), std::slice::from_ref(&data));
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(matches!(
        providers.resolve("acme/bad").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert_eq!(notices.len(), 2, "{notices:?}");
    for notice in &notices {
        assert_eq!(notice.code, ErrorCode::ModelInvalid);
        assert_eq!(notice.extension.as_deref(), Some("acme"));
    }
    let messages: Vec<&str> = notices.iter().map(|n| n.message.as_str()).collect();
    assert!(
        messages.contains(
            &"The model `acme/bad` names `system`, `tools` in its `extra_body`, \
               fields Fiber builds itself."
        ),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("acme/bad") && m.contains("web_search_20260209")),
        "{messages:?}"
    );
}

#[test]
fn a_cached_list_replaces_the_data_files_models() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("acme", provider("acme", &["a"]))]);
    assert!(providers.resolve("acme/a").is_ok());
    write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "b", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
    )
    .unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/b").is_ok());
    assert!(matches!(
        providers.resolve("acme/a").unwrap_err(),
        Error::UnknownModel { .. }
    ));
}

#[test]
fn without_a_cache_the_data_files_models_stand() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("acme", provider("acme", &["a"]))]);
    assert!(providers.resolve("acme/a").is_ok());
    assert!(matches!(
        providers.resolve("acme/b").unwrap_err(),
        Error::UnknownModel { .. }
    ));
}

#[test]
fn a_cache_that_is_not_a_list_leaves_the_data_files_models() {
    let setup = Setup::new();
    installed(&setup, &[("acme", provider("acme", &["a"]))]);
    write_model_cache(&setup.home(), "acme", &json!({})).unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/a").is_ok());
}

#[test]
fn a_cached_invalid_model_is_left_out_with_a_notice() {
    let setup = Setup::new();
    installed(&setup, &[("acme", provider("acme", &["a"]))]);
    write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "bad", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1",
                 "extra_body": { "tools": 1 }}]),
    )
    .unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(matches!(
        providers.resolve("acme/bad").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ModelInvalid);
    assert_eq!(notices[0].extension.as_deref(), Some("acme"));
}

#[test]
fn leave_out_invalid_filters_a_model_list_like_models_returns() {
    let list = json!([
        {"id": "bad", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "extra_body": { "tools": 1 }},
        {"id": "ok", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "extra_body": { "max_tokens": 1 }}
    ]);
    let mut models: Vec<ModelData> = serde_json::from_value(list).unwrap();
    let notices = leave_out_invalid("acme", "acme", &mut models);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["ok"]
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelInvalid);
    assert_eq!(notices[0].extension.as_deref(), Some("acme"));
    assert_eq!(
        notices[0].message,
        "The model `acme/bad` names `tools` in its `extra_body`, a field Fiber builds itself."
    );
}
