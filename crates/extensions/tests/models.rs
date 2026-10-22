//! `docs/model-routing.md`, "Naming a model" and "Choosing the model", and
//! `docs/extensions.md`, "A fresh install": an installed provider's models
//! are reached as `provider/model`, and a run with no model, no credential or
//! no provider fails with its code.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use common::write_record;
use common::{Setup, config, install, lua_named, manifest, provider, write};
use config::{ModelData, Secret, write_model_cache};
use contract::ErrorCode;
use contract::clock::Clock;
use extensions::{Error, ModelsArg, Providers, leave_out_invalid};
use fakes::{Deadline, NoLock};
use serde_json::json;

#[test]
fn enabled_loading_filters_packages_not_provider_ids_and_inventory_stays_complete() {
    fakes::within(
        "Providers::load_enabled",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let setup = Setup::new();
            let full = "github.com/aakshintala/fiber/providers/opencode";
            let source = setup.source(
                "opencode",
                &manifest(full),
                &[
                    provider("opencode-go", &["m"]),
                    provider("opencode-zen", &["m"]),
                ],
            );
            install(&setup.home(), &source, "0.1.0").unwrap();
            let other = setup.source("other", &manifest("other"), &[provider("other", &["m"])]);
            install(&setup.home(), &other, "0.1.0").unwrap();
            for name in ["opencode", full] {
                for enabled in [None, Some(true), Some(false)] {
                    let overrides =
                        enabled.map(|enabled| format!("extensions.\"{name}\".enabled={enabled}"));
                    let refs: Vec<&str> = overrides.iter().map(String::as_str).collect();
                    let configuration = config(&setup, &refs);
                    let (providers, notices) =
                        Providers::load_enabled(&setup.home(), &configuration).unwrap();
                    assert!(notices.is_empty(), "{notices:?}");
                    let expected = if enabled == Some(false) {
                        vec!["other"]
                    } else {
                        vec!["opencode-go", "opencode-zen", "other"]
                    };
                    assert_eq!(providers.names().collect::<Vec<_>>(), expected);
                    assert_eq!(providers.resolve("other/m").unwrap().reference(), "other/m");
                    let (inventory, notices) = Providers::load(&setup.home()).unwrap();
                    assert!(notices.is_empty());
                    assert_eq!(
                        inventory.names().collect::<Vec<_>>(),
                        ["opencode-go", "opencode-zen", "other"]
                    );
                }
            }
            write(
                &setup.home().join("config.json"),
                r#"{"extensions":{"opencode":{"enabled":false}}}"#,
            );
            write(
                &setup.home().join("projects/p/config.json"),
                r#"{"extensions":{"opencode":{"enabled":true}}}"#,
            );
            let configuration = config(&setup, &[]);
            let (providers, _) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
            assert!(providers.resolve("opencode-go/m").is_ok());
            let other_config = config::Config::load(config::Sources {
                home: setup.home(),
                workspace: setup.workspace(),
                project: config::ProjectKey::new("elsewhere").unwrap(),
                overrides: vec![],
            })
            .unwrap();
            let (providers, _) = Providers::load_enabled(&setup.home(), &other_config).unwrap();
            assert_eq!(providers.names().collect::<Vec<_>>(), ["other"]);
        },
    );
}

#[test]
fn disabled_packages_are_skipped_before_provider_files_secrets_and_cached_models() {
    fakes::within(
        "Providers::load_enabled",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        || {
            let setup = Setup::new();
            let mut m = manifest("acme");
            m["secrets"] = json!(["acme-token"]);
            let source = setup.source("acme", &m, &[provider("p", &["m"])]);
            install(&setup.home(), &source, "0.1.0").unwrap();
            write_model_cache(&setup.home(), "p", &provider("p", &["cached"])["models"]).unwrap();
            let configuration = config(&setup, &["extensions.acme.enabled=false"]);
            let (providers, notices) =
                Providers::load_enabled(&setup.home(), &configuration).unwrap();
            assert!(notices.is_empty());
            assert_eq!(providers.names().collect::<Vec<_>>(), Vec::<&str>::new());
            assert_eq!(providers.secrets().collect::<Vec<_>>(), Vec::<&str>::new());
            write(
                &setup.home().join("extensions/acme/providers/p.json"),
                "not JSON",
            );
            let (providers, notices) =
                Providers::load_enabled(&setup.home(), &configuration).unwrap();
            assert!(notices.is_empty());
            assert_eq!(providers.names().collect::<Vec<_>>(), Vec::<&str>::new());
            assert!(Providers::load(&setup.home()).is_err());
        },
    );
}

#[test]
fn fresh_cache_is_used_before_loading_or_refreshing_provider_lua() {
    use std::sync::Arc;
    use std::time::Duration;
    let setup = Setup::new();
    let marker = setup.workspace().join("provider-loaded");
    let manifest = declared_manifest("fiber.test/lazy", json!({"p":["http://127.0.0.1:1/v1"]}));
    let source = setup.source("lazy", &manifest, &[provider("p", &["cached"])]);
    write(
        &source.join("providers/p.lua"),
        &format!(
            "host.fs.write({:?}, 'x')\n fiber.provider('p', {{ models={{timeout=100, run=function() return {{}} end}}, credential={{timeout=100, run=function() return {{token='t', expires_at=1893456000}} end}} }})",
            marker.display().to_string(),
        ),
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty());
    write_model_cache(&setup.home(), "p", &provider("p", &["cached"])["models"]).unwrap();
    let configuration = config(&setup, &[]);
    let session = extensions::SessionExtensions::load(
        &setup.home(),
        &configuration,
        fakes::clock::FakeClock::new(),
        Arc::new(NoLock),
        None,
    );
    let lua = Arc::clone(&session.lua_providers()[0].1);
    let (tx, rx) = std::sync::mpsc::channel();
    let cache_marker = marker.clone();
    let home = setup.home();
    std::thread::spawn(move || {
        assert!(
            providers
                .add_session(
                    &session,
                    &configuration,
                    &|_, _| unreachable!("no credential source runs"),
                    &|_| None
                )
                .is_empty()
        );
        assert!(
            !cache_marker.exists(),
            "cached models must not inspect callbacks"
        );
        assert!(
            extensions::refresh_lists(
                &[Arc::clone(&lua)],
                &providers,
                &configuration,
                Some(Duration::from_secs(60)),
                &|_, _| unreachable!("no credential source runs"),
                &|_| None
            )
            .is_empty()
        );
        assert!(
            lua.refresh(Some(Duration::from_secs(60)), ModelsArg::default())
                .is_none()
        );
        tx.send(providers.resolve("p/cached").unwrap().reference())
            .unwrap();
        drop(home);
    });
    assert_eq!(
        Deadline::after(Duration::from_secs(5)).recv_or_fail(&rx, "fresh model cache"),
        "p/cached"
    );
    assert!(!marker.exists());
}

/// Installs the data files under the manifest name `ext`: the data
/// provider keeps its own name, while admission checks the extension's.
fn installed_ext(setup: &Setup, dir: &str, ext: &str, datas: &[serde_json::Value]) -> Providers {
    let source = setup.source(dir, &manifest(ext), datas);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
}

fn installed(setup: &Setup, extensions: &[(&str, serde_json::Value)]) -> Providers {
    for (name, data) in extensions {
        let source = setup.source(name, &manifest(name), std::slice::from_ref(data));
        install(&setup.home(), &source, "0.1.0").unwrap();
    }
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
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
    for level in contract::ThinkingLevel::ALL.map(contract::ThinkingLevel::as_str) {
        let model = providers
            .resolve(&format!("openai/gpt-5.6:{level}"))
            .unwrap();
        assert_eq!(model.reference(), "openai/gpt-5.6");
        assert_eq!(model.thinking.map(|l| l.as_str()), Some(level));
    }
    let err = providers.resolve("openai/gpt-5.6:extreme").unwrap_err();
    assert!(matches!(err, Error::UnknownModel { .. }), "{err:?}");
}

#[test]
fn an_unknown_model_message_says_to_run_fiber_models() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    let err = providers.resolve("openai/nope").unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("openai") && message.contains("nope"),
        "{message}"
    );
    assert!(message.contains("fiber models"), "{message}");
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
    assert_eq!(model.thinking, Some(contract::ThinkingLevel::Low));
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
    for typed in ["openrouter/x", "openrouter/x:high"] {
        let err = providers.resolve(typed).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionMissing, "{typed}: {err:?}");
    }
    let err = providers.resolve("nobody-has-this").unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoModel, "{err:?}");
    assert_eq!(
        err.to_string(),
        "No installed model matches `nobody-has-this`. Run `fiber models` to list them."
    );
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
    let key = config
        .credentials()
        .credential_with(model.provider, "default", &fakes::children::grouped_output)
        .map(|read| read.secret)
        .unwrap();
    assert_eq!(key.expose(), "key-from-command");
}

#[test]
fn a_run_whose_provider_has_no_credential_is_credential_missing() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("acme", provider("acme", &["m1"]))]);
    let config = config(&setup, &["model=acme/m1"]);
    let model = providers.choose(None, &config).unwrap();
    let err = config
        .credentials()
        .credential_with(model.provider, "default", &fakes::children::grouped_output)
        .map(|read| read.secret)
        .unwrap_err();
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
    write_record(&dir);
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
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "web_search": "web_search_20250305"},
            {"id": "plain", "protocol": "anthropic-messages",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000},
            {"id": "bad", "protocol": "anthropic-messages",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "web_search": "web_search_20260209"},
            {"id": "wrong", "protocol": "openai-responses",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "web_search": "web_search_20250305"},
            {"id": "hosted", "protocol": "openai-responses",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "web_search": "web_search"},
        ]
    });
    let source = setup.source("acme", &manifest("acme"), std::slice::from_ref(&data));
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/good").is_ok());
    assert!(providers.resolve("acme/plain").is_ok());
    assert!(providers.resolve("acme/hosted").is_ok());
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
            "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
            "extra_body": { (*field): 1 }
        }));
    }
    models.push(json!({
        "id": "ok", "protocol": protocol,
        "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
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
fn extra_body_matching_is_exact_and_top_level_only() {
    let setup = Setup::new();
    let data = json!({
        "name": "acme",
        "models": [{
            "id": "ok", "protocol": "google-generative-ai",
            "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
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
            "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
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
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}]),
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
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
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
         "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "extra_body": { "tools": 1 }},
        {"id": "ok", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "extra_body": { "max_tokens": 1 }}
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

fn lua_acme(setup: &Setup, dir: &str, models_run: &str) -> std::sync::Arc<extensions::LuaProvider> {
    lua_named(setup, dir, "acme", models_run)
}

/// A Lua provider with no `credential` function and no other credential:
/// the credential gate never lets its `models()` run.
fn lua_bare(
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

/// A Lua provider with a `credential` function but no `models` function:
/// the refresh gate never starts it, so no lock, thread or call follows.
fn lua_no_models(
    setup: &Setup,
    dir: &str,
    provider: &str,
) -> std::sync::Arc<extensions::LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("init.lua"),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"test-token\", expires_at = 1893456000 }} end }} }})\n"
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

fn lua_list(ids: &[&str]) -> String {
    let models: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                "{{ id = \"{id}\", protocol = \"openai-responses\", \
                 base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 }}"
            )
        })
        .collect();
    format!("{{ {} }}", models.join(", "))
}

#[test]
fn add_lua_without_a_data_file_creates_the_provider() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let lua = lua_acme(&setup, "ext", &lua_list(&["m"]));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/m").is_ok());
    let data = providers.get("acme").unwrap();
    assert_eq!(data.name, "acme");
    assert!(data.credential_from.credential().is_none());
    assert!(data.credential_name.is_none());
    assert!(data.headers.is_empty());
    assert!(data.reviewer_model.is_none());
}

#[test]
fn add_lua_with_a_data_file_replaces_only_models() {
    let setup = Setup::new();
    let mut data = provider("acme", &["old"]);
    data["headers"] = json!({ "x-client": "fiber" });
    data["reviewer_model"] = json!("tiny");
    let mut providers = installed_ext(&setup, "acme", "acme-ext", &[data]);
    let lua = lua_acme(&setup, "ext", &lua_list(&["new"]));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/new").is_ok());
    assert!(matches!(
        providers.resolve("acme/old").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    let data = providers.get("acme").unwrap();
    assert_eq!(
        data.headers.get("x-client").map(String::as_str),
        Some("fiber")
    );
    assert_eq!(data.reviewer_model.as_deref(), Some("tiny"));
}

#[test]
fn add_lua_leaves_out_an_invalid_model_with_a_notice() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let lua = lua_acme(
        &setup,
        "ext",
        &[
            "{ { id = \"ok\", protocol = \"openai-responses\", \
             base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 }, ",
            "{ id = \"bad\", protocol = \"openai-responses\", \
             base_url = \"http://127.0.0.1:1/v1\", context_window = 1000, extra_body = { tools = 1 } } }",
        ]
        .concat(),
    );
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.resolve("acme/ok").is_ok());
    assert!(matches!(
        providers.resolve("acme/bad").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ModelInvalid);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
    assert_eq!(
        notices[0].message,
        "The model `acme/bad` names `tools` in its `extra_body`, a field Fiber builds itself."
    );
}

#[test]
fn add_lua_filters_a_cached_list_without_running_lua() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    write_model_cache(
        &setup.home(),
        "acme",
        &json!([
            {"id": "bad", "protocol": "openai-responses",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000, "extra_body": { "tools": 1 }},
            {"id": "ok", "protocol": "openai-responses",
             "base_url": "http://127.0.0.1:1/v1", "context_window": 1000},
        ]),
    )
    .unwrap();
    let lua = lua_acme(&setup, "ext", "error(\"must not run\")");
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.resolve("acme/ok").is_ok());
    assert!(matches!(
        providers.resolve("acme/bad").unwrap_err(),
        Error::UnknownModel { .. }
    ));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ModelInvalid);
}

#[test]
fn a_failing_models_leaves_no_models_with_the_errors_code() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let lua = lua_acme(&setup, "ext", "error(\"boom\")");
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert_eq!(providers.names().count(), 0);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
    assert!(
        notices[0].message.contains("boom"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_failing_models_keeps_the_data_files_models() {
    let setup = Setup::new();
    let mut providers = installed(&setup, &[("acme", provider("acme", &["a"]))]);
    let lua = lua_acme(&setup, "ext", "error(\"boom\")");
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.resolve("acme/a").is_ok());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
}

#[test]
fn providers_debug_names_the_installed_providers() {
    let setup = Setup::new();
    let providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    let text = format!("{providers:?}");
    assert!(text.contains("openai"), "{text:?}");
}

#[test]
fn add_lua_keeps_the_provider_for_its_signer_even_when_models_fails() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let lua = lua_acme(&setup, "ext", &lua_list(&["m"]));
    providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.lua("acme").is_some());
    assert!(providers.lua("nobody").is_none());

    let setup2 = Setup::new();
    let mut failing = lua_only(
        &setup2,
        "srclua",
        "broke-ext",
        "broke",
        &["http://127.0.0.1:1/v1"],
    );
    let bad = lua_named(&setup2, "bad", "broke", "error(\"boom\")");
    assert_eq!(
        failing
            .add_lua(
                "broke-ext",
                &bad,
                &config(&setup2, &[]),
                &fakes::children::grouped_output,
                &|_| None
            )
            .len(),
        1
    );
    assert!(failing.lua("broke").is_some());
}

/// A provider with one model carrying `prompt_addendum`.
fn provider_with_addendum(name: &str, id: &str, addendum: &str) -> serde_json::Value {
    json!({
        "name": name,
        "credential": { "env": "FIBER_TEST_UNSET_KEY" },
        "models": [{"id": id, "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
                    "prompt_addendum": addendum}]
    })
}

/// A Lua `models()` run returning one model with `prompt_addendum`.
fn lua_addendum_run(id: &str, addendum: &str) -> String {
    format!(
        "{{ {{ id = \"{id}\", protocol = \"openai-responses\", \
         base_url = \"http://127.0.0.1:1/v1\", context_window = 1000, prompt_addendum = \"{addendum}\" }} }}"
    )
}

#[test]
fn an_addendum_file_is_read_for_static_data() {
    let setup = Setup::new();
    let mut with = provider_with_addendum("acme", "with", "prompts/with.md");
    with["models"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "without", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}));
    let source = setup.source("acme", &manifest("acme"), &[with]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.home().join("extensions/acme/prompts/with.md"),
        "Answer as a pirate.\n",
    );
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let model = providers.resolve("acme/with").unwrap();
    assert_eq!(providers.addendum(&model), Some("Answer as a pirate.\n"));
    // A model naming no addendum has none.
    let plain = providers.resolve("acme/without").unwrap();
    assert_eq!(providers.addendum(&plain), None);
}

#[test]
fn a_missing_addendum_leaves_out_the_whole_extension_and_keeps_the_other() {
    let setup = Setup::new();
    let good = provider_with_addendum("a", "m", "prompts/m.md");
    let bad = provider_with_addendum("b", "m", "prompts/gone.md");
    let source = setup.source("acme", &manifest("acme"), &[good, bad]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.home().join("extensions/acme/prompts/m.md"),
        "Present.\n",
    );
    let other = setup.source("other", &manifest("other"), &[provider("zed", &["z"])]);
    install(&setup.home(), &other, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    // The bad second file leaves out the good first file's provider too.
    assert!(providers.resolve("a/m").is_err());
    assert!(providers.resolve("b/m").is_err());
    assert!(providers.resolve("zed/z").is_ok());
    assert_eq!(
        providers.addendum(&providers.resolve("zed/z").unwrap()),
        None
    );
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme"));
    assert!(
        notices[0].message.contains("prompt_addendum") && notices[0].message.contains("gone.md"),
        "{}",
        notices[0].message
    );
}

#[test]
fn the_later_extension_wins_the_provider_and_its_addendum() {
    let setup = Setup::new();
    for (dir, text) in [("first", "From one.\n"), ("second", "From two.\n")] {
        let data = provider_with_addendum("acme", "m", "prompts/m.md");
        let source = setup.source(dir, &manifest(dir), &[data]);
        install(&setup.home(), &source, "0.1.0").unwrap();
        write(
            &setup.home().join(format!("extensions/{dir}/prompts/m.md")),
            text,
        );
    }
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let model = providers.resolve("acme/m").unwrap();
    assert_eq!(providers.addendum(&model), Some("From two.\n"));
}

#[test]
fn add_lua_reads_the_addendum_against_the_lua_extensions_directory() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    write(&setup.home().join("ext/prompts/m.md"), "Lua says hi.\n");
    let lua = lua_acme(&setup, "ext", &lua_addendum_run("m", "prompts/m.md"));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    let model = providers.resolve("acme/m").unwrap();
    assert_eq!(providers.addendum(&model), Some("Lua says hi.\n"));
}

#[test]
fn add_lua_replaces_the_data_files_models_and_addenda_together() {
    let setup = Setup::new();
    let data = provider_with_addendum("acme", "old", "prompts/old.md");
    let source = setup.source("acme", &manifest("acme-ext"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    write(
        &setup.home().join("extensions/acme-ext/prompts/old.md"),
        "Stale.\n",
    );
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers.addendum(&providers.resolve("acme/old").unwrap()),
        Some("Stale.\n")
    );
    write(&setup.home().join("ext/prompts/new.md"), "Fresh.\n");
    let lua = lua_acme(&setup, "ext", &lua_addendum_run("new", "prompts/new.md"));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/old").is_err());
    let model = providers.resolve("acme/new").unwrap();
    assert_eq!(providers.addendum(&model), Some("Fresh.\n"));
}

#[test]
fn add_lua_with_a_missing_addendum_leaves_no_models_but_keeps_the_provider() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let lua = lua_acme(&setup, "ext", &lua_addendum_run("m", "prompts/gone.md"));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert_eq!(providers.names().count(), 0);
    assert!(providers.lua("acme").is_some());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
    assert!(
        notices[0].message.contains("prompt_addendum") && notices[0].message.contains("gone.md"),
        "{}",
        notices[0].message
    );
}

#[test]
fn add_lua_with_a_missing_addendum_keeps_the_data_files_models() {
    let setup = Setup::new();
    let mut providers = installed_ext(&setup, "acme", "acme-ext", &[provider("acme", &["a"])]);
    let lua = lua_acme(&setup, "ext", &lua_addendum_run("m", "prompts/gone.md"));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.resolve("acme/a").is_ok());
    assert_eq!(
        providers.addendum(&providers.resolve("acme/a").unwrap()),
        None
    );
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
}

/// Sets the cached list's mtime, so its age reads against the fake clock.
fn set_cache_mtime(home: &std::path::Path, provider: &str, mtime: std::time::SystemTime) {
    let file = home.join("cache/models").join(format!("{provider}.json"));
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

fn wrote(home: &std::path::Path, provider: &str, ids: &[&str]) {
    let models: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            json!({"id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000})
        })
        .collect();
    write_model_cache(home, provider, &serde_json::Value::Array(models)).unwrap();
}

fn cached_ids(home: &std::path::Path, provider: &str) -> Vec<String> {
    config::read_model_cache(home, provider)
        .unwrap()
        .unwrap()
        .iter()
        .map(|m| m.id.clone())
        .collect()
}

const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[test]
fn a_fresh_list_is_not_refreshed_and_lua_never_runs() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(&home, "acme", clock.wall());
    let lua = lua_acme(&setup, "ext", "error(\"must not run\")");
    assert!(lua.refresh(Some(DAY), ModelsArg::default()).is_none());
    assert_eq!(cached_ids(&home, "acme"), ["old"]);
}

#[test]
fn a_provider_with_no_cached_list_is_not_refreshed_at_start() {
    let setup = Setup::new();
    let home = setup.home();
    let lua = lua_acme(&setup, "ext", "error(\"must not run\")");
    assert!(lua.refresh(Some(DAY), ModelsArg::default()).is_none());
    assert!(config::read_model_cache(&home, "acme").unwrap().is_none());
}

#[test]
fn a_list_exactly_as_old_as_the_maximum_is_not_stale() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(&home, "acme", clock.wall() - DAY);
    let lua = lua_acme(&setup, "ext", "error(\"must not run\")");
    assert!(lua.refresh(Some(DAY), ModelsArg::default()).is_none());
    assert_eq!(cached_ids(&home, "acme"), ["old"]);
}

#[test]
fn a_stale_list_refreshes_once_and_the_cache_is_replaced() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(
        &home,
        "acme",
        clock.wall() - DAY - std::time::Duration::from_secs(1),
    );
    let lua = lua_acme(&setup, "ext", &lua_list(&["new"]));
    let models = fakes::within(
        "lua.refresh",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            lua.refresh(Some(DAY), ModelsArg::default())
                .unwrap()
                .join()
                .unwrap()
        },
    )
    .unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["new"]
    );
    assert_eq!(cached_ids(&home, "acme"), ["new"]);
}

#[test]
fn without_a_maximum_even_a_fresh_list_refreshes() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(&home, "acme", clock.wall());
    let lua = lua_acme(&setup, "ext", &lua_list(&["new"]));
    fakes::within(
        "lua.refresh",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            lua.refresh(None, ModelsArg::default())
                .unwrap()
                .join()
                .unwrap()
        },
    )
    .unwrap();
    assert_eq!(cached_ids(&home, "acme"), ["new"]);
}

#[test]
fn two_providers_over_one_stale_list_refresh_it_once() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(
        &home,
        "acme",
        clock.wall() - DAY - std::time::Duration::from_secs(1),
    );
    // Two extensions, two VMs, one home: the second sees the first's lock.
    let first = lua_named(&setup, "one", "acme", &lua_list(&["new"]));
    let second = lua_named(&setup, "two", "acme", &lua_list(&["new"]));
    let a = first.refresh(Some(DAY), ModelsArg::default());
    let b = second.refresh(Some(DAY), ModelsArg::default());
    assert!(a.is_some(), "the first refresh starts");
    assert!(b.is_none(), "the second sees the first's lock");
    fakes::within(
        "refresh.join",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || a.unwrap().join().unwrap(),
    )
    .unwrap();
    assert_eq!(cached_ids(&home, "acme"), ["new"]);
}

#[test]
fn a_lock_held_elsewhere_is_not_started_again() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "acme", &["old"]);
    set_cache_mtime(
        &home,
        "acme",
        clock.wall() - DAY - std::time::Duration::from_secs(1),
    );
    let path = config::model_cache_lock_file(&home, "acme").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let held = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    held.try_lock().unwrap();
    let lua = lua_acme(&setup, "ext", "error(\"must not run\")");
    assert!(lua.refresh(Some(DAY), ModelsArg::default()).is_none());
    assert!(lua.refresh(None, ModelsArg::default()).is_none());
    drop(held);
    assert_eq!(cached_ids(&home, "acme"), ["old"]);
}

/// Writes the cached list of `provider` and backdates it past `DAY`, so it
/// is stale against the fake clock.
fn stale(home: &std::path::Path, provider: &str, ids: &[&str]) {
    wrote(home, provider, ids);
    let wall = fakes::clock::FakeClock::new().wall();
    set_cache_mtime(
        home,
        provider,
        wall - DAY - std::time::Duration::from_secs(1),
    );
}

#[test]
fn refresh_lists_starts_only_the_stale_provider_with_a_credential() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    wrote(&home, "fresh", &["old-fresh"]);
    set_cache_mtime(&home, "fresh", clock.wall());
    stale(&home, "stale", &["old-stale"]);
    stale(&home, "nocred", &["old-nocred"]);
    let fresh = lua_named(&setup, "one", "fresh", &lua_list(&["new-fresh"]));
    let stale = lua_named(&setup, "two", "stale", &lua_list(&["new-stale"]));
    let nocred = lua_bare(&setup, "three", "nocred", "error(\"must not run\")");
    let lua = [
        ("one".to_owned(), fresh),
        ("two".to_owned(), stale),
        ("three".to_owned(), nocred),
    ];
    let config = config(&setup, &[]);
    let providers = Providers::default();
    let lua: Vec<std::sync::Arc<extensions::LuaProvider>> = lua
        .iter()
        .map(|(_, provider)| std::sync::Arc::clone(provider))
        .collect();
    let started = extensions::refresh_lists(
        &lua,
        &providers,
        &config,
        Some(DAY),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert_eq!(started.len(), 1);
    let names: Vec<&str> = started.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["stale"]);
    for (_, handle) in started {
        fakes::within(
            "handle.join",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || handle.join().unwrap(),
        )
        .unwrap();
    }
    // Only the stale provider with a credential ran: the fresh list and
    // the credential-less one are untouched.
    assert_eq!(cached_ids(&home, "fresh"), ["old-fresh"]);
    assert_eq!(cached_ids(&home, "stale"), ["new-stale"]);
    assert_eq!(cached_ids(&home, "nocred"), ["old-nocred"]);
}

#[test]
fn refresh_skips_a_provider_that_did_not_register_models() {
    let setup = Setup::new();
    let home = setup.home();
    stale(&home, "nomodels", &["old"]);
    let lua = lua_no_models(&setup, "ext", "nomodels");
    // A credential is there, and the list is stale, yet nothing starts:
    // no lock, no thread, no `models()` call.
    assert!(lua.refresh(None, ModelsArg::default()).is_none());
    assert!(lua.refresh(Some(DAY), ModelsArg::default()).is_none());
    let config = config(&setup, &[]);
    let started = extensions::refresh_lists(
        &[lua],
        &Providers::default(),
        &config,
        None,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(started.is_empty());
    assert_eq!(cached_ids(&home, "nomodels"), ["old"]);
}

#[test]
fn add_lua_without_a_cache_or_a_credential_runs_no_models() {
    let setup = Setup::new();
    let home = setup.home();
    let mut providers = installed(&setup, &[("acme", provider("acme", &["a"]))]);
    // No cached copy, and the data file's credential names an unset
    // variable: `models()` would fail the test if it ran.
    let lua = lua_bare(&setup, "ext", "acme", "error(\"must not run\")");
    let config = config(&setup, &[]);
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/a").is_ok());
    assert!(
        config::read_model_cache(&home, "acme").unwrap().is_none(),
        "no discovery ran, so no copy was written"
    );
}

#[test]
fn leave_out_invalid_drops_a_model_whose_default_is_not_among_its_levels() {
    let list = json!([
        {"id": "bad", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
         "thinking_levels": ["low"], "thinking_default": "high"},
        {"id": "ok", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
         "thinking_levels": ["low", "high"], "thinking_default": "high"}
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
    assert!(
        notices[0].message.contains("acme/bad") && notices[0].message.contains("thinking_default"),
        "{}",
        notices[0].message
    );
}

#[test]
fn leave_out_invalid_drops_a_model_that_declares_no_context_window() {
    let list = json!([
        {"id": "bare", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1"},
        {"id": "zero", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "context_window": 0},
        {"id": "ok", "protocol": "anthropic-messages",
         "base_url": "http://127.0.0.1:1/v1", "context_window": 200000}
    ]);
    let mut models: Vec<ModelData> = serde_json::from_value(list).unwrap();
    let notices = leave_out_invalid("acme", "acme", &mut models);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["ok"]
    );
    assert_eq!(notices.len(), 2);
    for (notice, id) in notices.iter().zip(["bare", "zero"]) {
        assert_eq!(notice.code, ErrorCode::ModelInvalid);
        assert_eq!(notice.extension.as_deref(), Some("acme"));
        assert!(
            notice.message.contains(&format!("acme/{id}"))
                && notice.message.contains("context_window"),
            "{}",
            notice.message
        );
    }
}

#[test]
fn add_lua_for_a_provider_with_only_cost_keeps_its_data_models_and_its_handle() {
    let setup = Setup::new();
    let home = setup.home();
    let mut data = provider("acme", &["a"]);
    data["credential"] = json!({ "command": ["printf", "key-from-command\n"] });
    let mut providers = installed(&setup, &[("acme", data)]);
    let ext = home.join("ext");
    write(
        &{
            std::fs::create_dir_all((ext.join("providers/acme.lua")).parent().unwrap()).unwrap();
            ext.join("providers/acme.lua")
        },
        "fiber.provider(\"acme\", { cost = { timeout = 1000, run = function() return 0 end } })\n",
    );
    let lua = extensions::LuaProvider::new(
        std::sync::Arc::new(extensions::LuaExtension::new(
            "acme-ext",
            ext,
            setup.home(),
            fakes::clock::FakeClock::new(),
        )),
        "acme",
    );
    let config = config(&setup, &[]);
    // A credential is there and no cache is: only the missing `models`
    // keeps `add_lua` from calling it.
    assert!(lua.has_credential(
        &config,
        providers.get("acme").unwrap(),
        &fakes::children::grouped_output
    ));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/a").is_ok());
    assert!(
        config::read_model_cache(&home, "acme").unwrap().is_none(),
        "no discovery ran, so no copy was written"
    );
    assert!(std::sync::Arc::ptr_eq(providers.lua("acme").unwrap(), &lua));
}

#[test]
fn forget_lua_drops_every_handle_and_keeps_the_models() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    write(&setup.home().join("ext/prompts/m.md"), "Lua says hi.\n");
    let lua = lua_acme(&setup, "ext", &lua_addendum_run("m", "prompts/m.md"));
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    let weak = std::sync::Arc::downgrade(&lua);
    drop(lua);
    providers.forget_lua();
    assert!(providers.lua("acme").is_none());
    assert!(
        weak.upgrade().is_none(),
        "the registry held the last handle"
    );
    assert_eq!(providers.names().collect::<Vec<_>>(), ["acme"]);
    assert_eq!(providers.get("acme").unwrap().name, "acme");
    let model = providers.resolve("acme/m").unwrap();
    assert_eq!(providers.addendum(&model), Some("Lua says hi.\n"));
}

/// One cached model with `id`, on `openai-responses`.
fn cached(id: &str) -> config::ModelData {
    serde_json::from_value(json!({
        "id": id,
        "protocol": "openai-responses",
        "base_url": "http://127.0.0.1:1/v1",
        "context_window": 1000,
    }))
    .unwrap()
}

#[test]
fn add_cached_inserts_an_unheld_provider_with_its_models() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    assert!(providers.add_cached("acme", vec![cached("m1")]).is_some());
    assert_eq!(providers.names().collect::<Vec<_>>(), ["acme"]);
    assert_eq!(
        providers
            .get("acme")
            .unwrap()
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["m1"]
    );
    assert!(providers.resolve("acme/m1").is_ok());
}

#[test]
fn add_cached_leaves_a_held_provider_alone() {
    let setup = Setup::new();
    let mut providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    assert!(
        providers
            .add_cached("openai", vec![cached("other")])
            .is_none()
    );
    assert_eq!(
        providers
            .get("openai")
            .unwrap()
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5.6"]
    );
}

#[test]
fn add_cached_refuses_scripted() {
    let mut providers = Providers::default();
    assert!(
        providers
            .add_cached("scripted", vec![cached("m1")])
            .is_none()
    );
    assert!(providers.get("scripted").is_none());
}

#[test]
fn set_models_replaces_a_held_providers_models() {
    let setup = Setup::new();
    let mut providers = installed(&setup, &[("openai", provider("openai", &["gpt-5.6"]))]);
    let notices = providers.set_models("openai", vec![cached("gpt-7")]);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        providers
            .get("openai")
            .unwrap()
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-7"]
    );
}

#[test]
fn set_models_inserts_an_unheld_provider_and_refuses_scripted() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let notices = providers.set_models("acme", vec![cached("m1")]);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/m1").is_ok());
    assert!(
        providers
            .set_models("scripted", vec![cached("m1")])
            .is_empty()
    );
    assert!(providers.get("scripted").is_none());
}

fn declared_manifest(name: &str, providers: serde_json::Value) -> serde_json::Value {
    let mut m = manifest(name);
    if let Some(into) = m.as_object_mut() {
        into.insert("providers".to_owned(), providers);
    }
    m
}

/// A registry whose only extension is the Lua-only `ext` declaring
/// `provider` with `urls`: what `add_lua` tests admit against.
fn lua_only(setup: &Setup, src: &str, ext: &str, provider: &str, urls: &[&str]) -> Providers {
    let mut map = serde_json::Map::new();
    map.insert(provider.to_owned(), serde_json::json!(urls));
    let manifest = declared_manifest(ext, serde_json::Value::Object(map));
    let source = setup.source(src, &manifest, &[]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
}

fn model_at(id: &str, base: &str) -> serde_json::Value {
    serde_json::json!({"id": id, "protocol": "openai-responses",
        "base_url": base, "context_window": 1000})
}

fn data_with(name: &str, models: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"name": name,
        "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models})
}

#[test]
fn an_extension_whose_data_names_an_undeclared_provider_loads_none_of_its_providers() {
    let setup = Setup::new();
    let a_manifest = declared_manifest(
        "a-ext",
        serde_json::json!({"a1": ["http://127.0.0.1:1/v1"]}),
    );
    let source = setup.source(
        "a",
        &a_manifest,
        &[
            data_with("a1", vec![model_at("m", "http://127.0.0.1:1/v1")]),
            data_with("a2", vec![model_at("m", "http://127.0.0.1:1/v1")]),
        ],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let b_source = setup.source(
        "b",
        &declared_manifest("b-ext", serde_json::json!({"b": ["http://127.0.0.1:1/v1"]})),
        &[data_with("b", vec![model_at("m", "http://127.0.0.1:1/v1")])],
    );
    install(&setup.home(), &b_source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert_eq!(providers.names().collect::<Vec<_>>(), ["b"]);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("a-ext"));
    assert!(notices[0].message.contains("a2"), "{}", notices[0].message);
}

#[test]
fn a_model_whose_base_url_its_entry_does_not_list_is_left_out_with_extension_failed() {
    let setup = Setup::new();
    let source = setup.source(
        "a",
        &declared_manifest(
            "acme-ext",
            serde_json::json!({"acme": ["http://127.0.0.1:1/v1"]}),
        ),
        &[data_with(
            "acme",
            vec![
                model_at("m1", "http://127.0.0.1:1/v1"),
                model_at("m2", "http://127.0.0.1:1/v1/"),
            ],
        )],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/m1").is_ok());
    assert!(providers.resolve("acme/m2").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
    assert!(
        notices[0].message.contains("acme/m2"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_listed_pattern_admits_its_template_and_the_filled_url_is_not_compared() {
    let setup = Setup::new();
    let source = setup.source(
        "a",
        &declared_manifest(
            "tmpl-ext",
            serde_json::json!({"acme": ["https://{workspace}/v1"]}),
        ),
        &[data_with(
            "acme",
            vec![model_at("m", "https://{workspace}/v1")],
        )],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/m").is_ok());
    assert!(notices.is_empty(), "{notices:?}");

    let setup2 = Setup::new();
    let source2 = setup2.source(
        "a",
        &declared_manifest(
            "filled-ext",
            serde_json::json!({"acme": ["https://adb.example/v1"]}),
        ),
        &[data_with(
            "acme",
            vec![model_at("m", "https://{workspace}/v1")],
        )],
    );
    install(&setup2.home(), &source2, "0.1.0").unwrap();
    let (providers2, notices2) = Providers::load(&setup2.home()).unwrap();
    assert!(providers2.resolve("acme/m").is_err());
    assert_eq!(notices2.len(), 1);
}

#[test]
fn an_empty_entry_admits_no_model() {
    let setup = Setup::new();
    let source = setup.source(
        "a",
        &declared_manifest("e-ext", serde_json::json!({"acme": []})),
        &[data_with(
            "acme",
            vec![model_at("m", "http://127.0.0.1:1/v1")],
        )],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/m").is_err());
    assert_eq!(notices.len(), 1);
}

#[test]
fn a_cached_list_that_replaces_the_data_is_checked_too() {
    let setup = Setup::new();
    let source = setup.source(
        "a",
        &declared_manifest(
            "acme-ext",
            serde_json::json!({"acme": ["http://127.0.0.1:1/v1"]}),
        ),
        &[data_with(
            "acme",
            vec![model_at("a", "http://127.0.0.1:1/v1")],
        )],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "b", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:2/v1", "context_window": 1000}]),
    )
    .unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(providers.resolve("acme/b").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
}

#[test]
fn add_lua_leaves_out_a_models_result_at_an_unlisted_url() {
    let setup = Setup::new();
    let source = setup.source(
        "a",
        &declared_manifest(
            "acme-ext",
            serde_json::json!({"acme": ["http://127.0.0.1:1/v1"]}),
        ),
        &[],
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let lua = lua_named(
        &setup,
        "ext",
        "acme",
        "{ { id = \"listed\", protocol = \"openai-responses\", \
           base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 }, \
           { id = \"rogue\", protocol = \"openai-responses\", \
           base_url = \"http://127.0.0.1:2/v1\", context_window = 1000 } }",
    );
    let notices = providers.add_lua(
        "acme-ext",
        &lua,
        &config(&setup, &[]),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(providers.resolve("acme/listed").is_ok());
    assert!(providers.resolve("acme/rogue").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("acme-ext"));
}

#[test]
fn add_cached_admits_only_listed_urls_and_adds_nothing_no_extension_declares() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    assert!(providers.add_cached("ghost", vec![cached("m")]).is_none());
    assert!(providers.get("ghost").is_none());
    let notices = providers
        .add_cached(
            "acme",
            vec![
                serde_json::from_value(json!({"id": "m1", "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}))
                .unwrap(),
                serde_json::from_value(json!({"id": "m2", "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:2/v1", "context_window": 1000}))
                .unwrap(),
            ],
        )
        .unwrap();
    assert!(providers.resolve("acme/m1").is_ok());
    assert!(providers.resolve("acme/m2").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(providers.extension_of("acme"), Some("acme-ext"));
}

#[test]
fn set_models_leaves_out_unlisted_urls_and_records_the_owner() {
    let setup = Setup::new();
    let mut providers = lua_only(
        &setup,
        "srclua",
        "acme-ext",
        "acme",
        &["http://127.0.0.1:1/v1"],
    );
    let notices = providers.set_models(
        "acme",
        vec![
            serde_json::from_value(json!({"id": "m1", "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}))
            .unwrap(),
            serde_json::from_value(json!({"id": "m2", "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:2/v1", "context_window": 1000}))
            .unwrap(),
        ],
    );
    assert!(providers.resolve("acme/m1").is_ok());
    assert!(providers.resolve("acme/m2").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(providers.extension_of("acme"), Some("acme-ext"));
    // No installed extension declares `ghost`: every model is left out,
    // with `ghost` standing in as the extension, and no owner is recorded.
    let notices = providers.set_models("ghost", vec![cached("m")]);
    assert!(providers.resolve("ghost/m").is_err());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("ghost"));
    assert_eq!(providers.extension_of("ghost"), None);
}

#[test]
fn a_cached_lua_only_template_fills_from_its_extensions_setting() {
    let setup = Setup::new();
    let mut map = serde_json::Map::new();
    map.insert(
        "acme".to_owned(),
        serde_json::json!(["https://{workspace}/v1"]),
    );
    let manifest = declared_manifest("acme-ext", serde_json::Value::Object(map));
    let source = setup.source("srclua", &manifest, &[]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let template: config::ModelData = serde_json::from_value(json!({
        "id": "m", "protocol": "openai-responses",
        "base_url": "https://{workspace}/v1", "context_window": 1000,
    }))
    .unwrap();
    let notices = providers.add_cached("acme", vec![template]).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let config = config(
        &setup,
        &["extensions.\"acme-ext\".settings.workspace=adb.example"],
    );
    let notices = providers.fill_placeholders(&config, &|_| None).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let found = providers.resolve("acme/m").unwrap();
    assert_eq!(found.model.base_url, "https://adb.example/v1");
}

#[test]
fn add_session_drops_the_data_of_an_extension_rejected_for_its_lua() {
    use extensions::SessionExtensions;
    use std::sync::Arc;
    let setup = Setup::new();
    let mut cmap = serde_json::Map::new();
    cmap.insert("c".to_owned(), serde_json::json!(["http://127.0.0.1:1/v1"]));
    let c_manifest = declared_manifest("c-ext", serde_json::Value::Object(cmap));
    let c_source = setup.source(
        "c",
        &c_manifest,
        &[data_with("c", vec![model_at("m", "http://127.0.0.1:1/v1")])],
    );
    install(&setup.home(), &c_source, "0.1.0").unwrap();
    let mut dmap = serde_json::Map::new();
    dmap.insert("d".to_owned(), serde_json::json!(["http://127.0.0.1:1/v1"]));
    let d_manifest = declared_manifest("d-ext", serde_json::Value::Object(dmap));
    let d_source = setup.source(
        "d",
        &d_manifest,
        &[data_with("d", vec![model_at("m", "http://127.0.0.1:1/v1")])],
    );
    install(&setup.home(), &d_source, "0.1.0").unwrap();
    // The rejected extension's script registers the undeclared `rogue`
    // beside its hooks; its data was admitted, since loading runs no Lua.
    let c_dir = setup
        .home()
        .join("extensions")
        .join(config::dir_name("c-ext"));
    std::fs::write(
        {
            std::fs::create_dir_all((c_dir.join("providers/rogue.lua")).parent().unwrap()).unwrap();
            c_dir.join("providers/rogue.lua")
        },
        "fiber.provider(\"rogue\", { credential = { timeout = 1000, \
         run = function() return { token = \"t\", expires_at = 1893456000 } end } })\n",
    )
    .unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.get("c").is_some());
    let config = config(&setup, &[]);
    let clock = fakes::clock::FakeClock::new();
    let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
    let extensions = SessionExtensions::load(&setup.home(), &config, clock, locks, None);
    assert_eq!(extensions.rejected(), ["c-ext"]);
    let notices = providers.add_session(
        &extensions,
        &config,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.get("c").is_none());
    assert!(providers.resolve("d/m").is_ok());
}

/// Installs extension `ext` with provider `provider`: the data file holds
/// `models` and `placeholders`, and the manifest declares `templates` for
/// the provider, which discovered models must match byte for byte.
fn installed_placeholders(
    setup: &Setup,
    ext: &str,
    provider: &str,
    templates: &[&str],
    placeholders: &serde_json::Value,
    models: &serde_json::Value,
) -> Providers {
    let mut declared = manifest(ext);
    let mut entries = serde_json::Map::new();
    entries.insert(provider.to_owned(), json!(templates));
    declared
        .as_object_mut()
        .unwrap()
        .insert("providers".to_owned(), serde_json::Value::Object(entries));
    let data = json!({
        "name": provider,
        "credential": { "env": "FIBER_TEST_UNSET_KEY" },
        "models": models,
        "placeholders": placeholders,
    });
    let source = setup.source("src", &declared, &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
}

/// A `models()` returning one discovered model whose id joins `left` and
/// `right` around `@`, at `base_url`: what `models()` receives is read as
/// `arg`.
fn lua_join(left: &str, right: &str, base_url: &str) -> String {
    format!(
        "{{ {{ id = {left} .. \"@\" .. {right}, protocol = \"openai-responses\", \
         base_url = \"{base_url}\", context_window = 1000 }} }}"
    )
}

/// A Lua provider with no `credential` function whose `models()` is
/// `models_run`, which reads its argument as `arg`.
fn lua_bare_arg(
    setup: &Setup,
    dir: &str,
    provider: &str,
    models_run: &str,
) -> std::sync::Arc<extensions::LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("providers").join(format!("{provider}.lua")),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             models = {{ timeout = 1000, run = function(arg) return {models_run} end }} }})\n"
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

/// A Lua provider with a `credential` function whose `models()` is
/// `models_run`, which reads its argument as `arg`.
fn lua_named_arg(
    setup: &Setup,
    dir: &str,
    provider: &str,
    models_run: &str,
) -> std::sync::Arc<extensions::LuaProvider> {
    let ext = setup.home().join(dir);
    write(
        &ext.join("providers").join(format!("{provider}.lua")),
        &format!(
            "fiber.provider(\"{provider}\", {{ \
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"test-token\", expires_at = 1893456000 }} end }}, \
             models = {{ timeout = 1000, run = function(arg) return {models_run} end }} }})\n"
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

/// No environment variable: every lookup is empty.
fn env_empty(_name: &str) -> Option<String> {
    None
}

/// The workspace variable serves a host with a scheme and slash to strip.
fn env_workspace(name: &str) -> Option<String> {
    (name == "FIBER_TEST_WORKSPACE").then(|| "https://e.example/".to_owned())
}

#[test]
fn models_receives_the_session_key_beside_the_workspace_host() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &json!([]),
    );
    config::store_credential(&setup.home(), "acme", "default", &Secret::new("k1".into())).unwrap();
    let config = config(
        &setup,
        &["extensions.\"acme-ext\".settings.workspace=h.example"],
    );
    let lua = lua_bare_arg(
        &setup,
        "ext",
        "acme",
        &lua_join(
            "arg.key",
            "arg.placeholders.workspace",
            "https://{workspace}/v1",
        ),
    );
    let home = setup.home();
    let (notices, cached) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            );
            let cached = cached_ids(&home, "acme");
            (notices, cached)
        },
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(cached, ["k1@h.example"]);
}

#[test]
fn the_workspace_host_falls_back_to_the_environment() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &json!([]),
    );
    config::store_credential(&setup.home(), "acme", "default", &Secret::new("k1".into())).unwrap();
    let config = config(&setup, &[]);
    let lua = lua_bare_arg(
        &setup,
        "ext",
        "acme",
        &lua_join(
            "arg.key",
            "arg.placeholders.workspace",
            "https://{workspace}/v1",
        ),
    );
    let home = setup.home();
    let (notices, cached) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_workspace,
            );
            let cached = cached_ids(&home, "acme");
            (notices, cached)
        },
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(cached, ["k1@e.example"]);
}

#[test]
fn models_receives_no_key_for_a_provider_that_registers_credential() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://h.example/v1"],
        &json!({}),
        &json!([]),
    );
    // A stored key must not reach `models()`: the provider registers
    // `credential()`, so its argument carries no key.
    config::store_credential(&setup.home(), "acme", "default", &Secret::new("k1".into())).unwrap();
    let lua = lua_named_arg(
        &setup,
        "ext",
        "acme",
        &lua_join("tostring(arg.key)", "\"static\"", "https://h.example/v1"),
    );
    let config = config(&setup, &[]);
    let home = setup.home();
    let (notices, cached) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            );
            let cached = cached_ids(&home, "acme");
            (notices, cached)
        },
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(cached, ["nil@static"]);
}

#[test]
fn models_is_not_called_while_the_workspace_has_no_value() {
    let setup = Setup::new();
    let models = json!([{ "id": "m", "protocol": "openai-responses",
        "base_url": "https://{workspace}/v1", "context_window": 1000 }]);
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &models,
    );
    let lua = lua_bare(&setup, "ext", "acme", "error(\"must not run\")");
    let home = setup.home();
    let (mut providers, notices, config) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            let config = config(&setup, &[]);
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            );
            (providers, notices, config)
        },
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(
        config::read_model_cache(&home, "acme").unwrap().is_none(),
        "no discovery ran, so no copy was written"
    );
    let notices = providers.fill_placeholders(&config, &env_empty).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
    assert!(providers.resolve("acme/m").is_err());
}

#[test]
fn models_is_not_called_while_the_workspace_is_not_a_host() {
    let setup = Setup::new();
    let models = json!([{ "id": "m", "protocol": "openai-responses",
        "base_url": "https://{workspace}/v1", "context_window": 1000 }]);
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &models,
    );
    let lua = lua_bare(&setup, "ext", "acme", "error(\"must not run\")");
    let home = setup.home();
    let (mut providers, notices, config) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            let config = config(&setup, &["extensions.\"acme-ext\".settings.workspace=a/b"]);
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            );
            (providers, notices, config)
        },
    );
    assert!(notices.is_empty(), "{notices:?}");
    assert!(
        config::read_model_cache(&home, "acme").unwrap().is_none(),
        "no discovery ran, so no copy was written"
    );
    let notices = providers.fill_placeholders(&config, &env_empty).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ModelUnconfigured);
    assert!(
        !notices[0].message.contains("a/b"),
        "the notice never repeats the value: {}",
        notices[0].message
    );
    assert!(providers.resolve("acme/m").is_err());
}

#[test]
fn refresh_lists_passes_the_key_and_host_to_the_thread() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &json!([]),
    );
    stale(&setup.home(), "acme", &["old"]);
    config::store_credential(&setup.home(), "acme", "default", &Secret::new("k1".into())).unwrap();
    let config = config(
        &setup,
        &["extensions.\"acme-ext\".settings.workspace=h.example"],
    );
    let lua = lua_bare_arg(
        &setup,
        "ext",
        "acme",
        &lua_join(
            "arg.key",
            "arg.placeholders.workspace",
            "https://{workspace}/v1",
        ),
    );
    let started = fakes::within(
        "refresh_lists",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            extensions::refresh_lists(
                &[lua],
                &providers,
                &config,
                Some(DAY),
                &fakes::children::grouped_output,
                &env_empty,
            )
        },
    );
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, "acme");
    for (_, handle) in started {
        fakes::within(
            "handle.join",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || handle.join().unwrap(),
        )
        .unwrap();
    }
    assert_eq!(cached_ids(&setup.home(), "acme"), ["k1@h.example"]);
}

#[test]
fn refresh_lists_starts_nothing_while_the_workspace_has_no_value() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://{workspace}/v1"],
        &json!({"workspace": {"env": "FIBER_TEST_WORKSPACE"}}),
        &json!([]),
    );
    stale(&setup.home(), "acme", &["old"]);
    config::store_credential(&setup.home(), "acme", "default", &Secret::new("k1".into())).unwrap();
    let config = config(&setup, &[]);
    let lua = lua_bare_arg(
        &setup,
        "ext",
        "acme",
        &lua_join(
            "arg.key",
            "arg.placeholders.workspace",
            "https://{workspace}/v1",
        ),
    );
    let started = fakes::within(
        "refresh_lists",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            extensions::refresh_lists(
                &[lua],
                &providers,
                &config,
                Some(DAY),
                &fakes::children::grouped_output,
                &env_empty,
            )
        },
    );
    assert!(started.is_empty());
    assert_eq!(cached_ids(&setup.home(), "acme"), ["old"]);
}

#[test]
fn models_arg_debug_never_prints_the_key() {
    let arg = ModelsArg {
        key: Some(Secret::new("k1".into())),
        placeholders: [("workspace".to_owned(), "h.example".to_owned())]
            .into_iter()
            .collect(),
    };
    let printed = format!("{arg:?}");
    assert!(!printed.contains("k1"), "{printed}");
}

#[test]
fn a_models_failure_redacts_the_key() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://h.example/v1"],
        &json!({}),
        &json!([]),
    );
    config::store_credential(
        &setup.home(),
        "acme",
        "default",
        &Secret::new("dapi-secret".into()),
    )
    .unwrap();
    let lua = lua_bare_arg(&setup, "ext", "acme", "error(\"saw \" .. arg.key)");
    let arg = ModelsArg {
        key: Some(Secret::new("dapi-secret".into())),
        placeholders: std::collections::BTreeMap::new(),
    };
    let err = fakes::within(
        "lua.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || lua.list_models(&arg),
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("[redacted]"), "{text}");
    assert!(!text.contains("dapi-secret"), "{text}");
    let lua = lua_bare_arg(&setup, "ext", "acme", "error(\"saw \" .. arg.key)");
    let config = config(&setup, &[]);
    let notices = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            )
        },
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert!(
        notices[0].message.contains("[redacted]"),
        "{}",
        notices[0].message
    );
    assert!(
        !notices[0].message.contains("dapi-secret"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_models_non_list_return_redacts_the_key() {
    let setup = Setup::new();
    let providers = installed_placeholders(
        &setup,
        "acme-ext",
        "acme",
        &["https://h.example/v1"],
        &json!({}),
        &json!([]),
    );
    config::store_credential(
        &setup.home(),
        "acme",
        "default",
        &Secret::new("dapi-secret".into()),
    )
    .unwrap();
    let lua = lua_bare_arg(&setup, "ext", "acme", "arg.key");
    let arg = ModelsArg {
        key: Some(Secret::new("dapi-secret".into())),
        placeholders: std::collections::BTreeMap::new(),
    };
    let err = fakes::within(
        "lua.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || lua.list_models(&arg),
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("[redacted]"), "{text}");
    assert!(!text.contains("dapi-secret"), "{text}");
    let lua = lua_bare_arg(&setup, "ext", "acme", "arg.key");
    let config = config(&setup, &[]);
    let notices = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut providers = providers;
            providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &env_empty,
            )
        },
    );
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert!(
        notices[0].message.contains("[redacted]"),
        "{}",
        notices[0].message
    );
    assert!(
        !notices[0].message.contains("dapi-secret"),
        "{}",
        notices[0].message
    );
}
