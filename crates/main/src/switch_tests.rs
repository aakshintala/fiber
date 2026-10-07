//! `prepare` on a `Switching` built from fixture data
//! (`docs/model-routing.md`, "Naming a model" and "Thinking"): the naming
//! list is plain strings, so an unloaded Lua provider needs no VM.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use config::{Config, ProjectKey, Sources};
use contract::commands::ModelArgs;
use contract::{ErrorCode, ThinkingLevel};
use extensions::Providers;
use serde_json::json;

use super::{Credentials, Switching, prepare, sentence};

/// The fixture home, workspace and config: `fake` with `m`, `n` (low and
/// high, defaulting low, with an addendum) and `r`; `claude` with the
/// hosted-search model `w`; `other` with `m` behind a `command`
/// credential; `bed` with a bedrock model.
struct Fixture {
    _root: fakes::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
    marker: std::path::PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let root = fakes::TempDir::new(name);
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let marker = root.path().join("credential-ran");
    let fake = home.join("extensions/fake");
    std::fs::create_dir_all(fake.join("providers")).unwrap();
    std::fs::write(
        fake.join("extension.json"),
        json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        fake.join("providers/fake.json"),
        json!({
            "name": "fake",
            "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 100000},
                {"id": "n", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000,
                 "thinking_levels": ["low", "high"], "thinking_default": "low",
                 "prompt_addendum": "extra.md"},
                {"id": "r", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            ],
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(fake.join("extra.md"), "The extra paragraph.\n").unwrap();
    let claude = home.join("extensions/claude");
    std::fs::create_dir_all(claude.join("providers")).unwrap();
    std::fs::write(
        claude.join("extension.json"),
        json!({"name": "claude", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        claude.join("providers/claude.json"),
        json!({
            "name": "claude",
            "models": [
                {"id": "w", "protocol": "anthropic-messages",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 500,
                 "web_search": "web_search_20250305"},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let other = home.join("extensions/other");
    std::fs::create_dir_all(other.join("providers")).unwrap();
    std::fs::write(
        other.join("extension.json"),
        json!({"name": "other", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        other.join("providers/other.json"),
        json!({
            "name": "other",
            "credential": {"command": ["touch", marker.to_str().unwrap()]},
            "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let bed = home.join("extensions/bed");
    std::fs::create_dir_all(bed.join("providers")).unwrap();
    std::fs::write(
        bed.join("extension.json"),
        json!({"name": "bed", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        bed.join("providers/bed.json"),
        json!({
            "name": "bed",
            "models": [
                {"id": "bk", "protocol": "bedrock-converse",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            ],
        })
        .to_string(),
    )
    .unwrap();
    Fixture {
        _root: root,
        home,
        workspace,
        marker,
    }
}

fn config(fixture: &Fixture, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: fixture.home.clone(),
        workspace: fixture.workspace.clone(),
        project: ProjectKey::new("test").unwrap(),
        overrides: overrides.iter().map(|o| (*o).to_owned()).collect(),
    })
    .unwrap()
}

fn keyed(label: &str) -> (String, crate::lua_providers::KeyAndSigner) {
    (
        label.to_owned(),
        (Some(contract::Secret::new("k1".to_owned())), None),
    )
}

/// The `Switching` over the fixture with `overrides` and the credential
/// map holding `fake` and `claude` under `default`, through the same calls
/// `parts_in` makes: the load clone, the naming list from `add_lua`, and
/// `Switching::new`.
fn switching(fixture: &Fixture, overrides: &[&str]) -> Switching {
    let config = config(fixture, overrides);
    let (mut providers, _) = Providers::load(&fixture.home).unwrap();
    let snapshot = providers.clone();
    let extensions = extensions::SessionExtensions::load(
        &fixture.home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
    );
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    let credentials: Credentials = [("fake", keyed("default")), ("claude", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    Switching::new(snapshot, &[], naming, config, credentials).unwrap()
}

fn args(model: &str) -> ModelArgs {
    ModelArgs {
        model: model.into(),
        thinking: None,
    }
}

/// A prepared switch: `prepare` succeeds, and `Prepared` is no `Debug`, so
/// no `unwrap`.
fn prepared(
    switching: &Switching,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> r#loop::Prepared {
    match prepare(switching, args, chosen) {
        Ok(prepared) => prepared,
        Err(rejection) => panic!("the switch rejected: {}", rejection.message),
    }
}

/// A rejected switch: `prepare` fails, and `Prepared` is no `Debug`, so no
/// `unwrap_err`.
fn rejected(
    switching: &Switching,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> contract::inbox::Rejection {
    match prepare(switching, args, chosen) {
        Ok(_) => panic!("the switch prepared"),
        Err(rejection) => rejection,
    }
}

/// A failed reviewer: the loop gets it, and `Reviewer` is no `Debug`, so no
/// `expect_err`.
fn reviewer_failed(
    reviewer: Result<r#loop::Reviewer, contract::shapes::Failure>,
) -> contract::shapes::Failure {
    match reviewer {
        Ok(_) => panic!("the reviewer resolved"),
        Err(failure) => failure,
    }
}

fn args_thinking(model: &str, thinking: &str) -> ModelArgs {
    ModelArgs {
        model: model.into(),
        thinking: Some(thinking.into()),
    }
}

#[test]
fn the_session_and_reviewers_providers_resolve_with_the_maps_label() {
    let fixture = fixture("fiber-switch-session");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    assert_eq!(made.credential, Some("default".to_owned()));
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "fake/r");
}

#[test]
fn another_installed_provider_is_the_credential_sentence() {
    let fixture = fixture("fiber-switch-outside");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("other/m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, sentence("other"));
}

#[test]
fn unknown_models_are_invalid_arguments() {
    let fixture = fixture("fiber-switch-unknown");
    let switching = switching(&fixture, &[]);
    for typed in ["fake/x", "nope/x", "x", ""] {
        let rejection = rejected(&switching, &args(typed), None);
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{typed}");
    }
    let missing = rejected(&switching, &args("fake/x"), None);
    assert!(
        missing.message.contains("has no model"),
        "{}",
        missing.message
    );
}

#[test]
fn an_ambiguous_bare_id_lists_both_matches() {
    // `fake/m` and `other/m` share the id.
    let fixture = fixture("fiber-switch-ambiguous");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(
        rejection.message.contains("other/m"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_naming_list_counts_an_unloaded_lua_provider() {
    let fixture = fixture("fiber-switch-naming");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    // `lua` is unloaded: in the naming list only, sharing `m` with `fake`.
    let naming = vec![
        ("fake".to_owned(), "m".to_owned()),
        ("lua".to_owned(), "m".to_owned()),
    ];
    let switching = Switching::new(providers, &[], naming, config, credentials).unwrap();
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(rejection.message.contains("lua/m"), "{}", rejection.message);
    let limited = rejected(&switching, &args("lua/m"), None);
    assert_eq!(limited.code, ErrorCode::InvalidArguments);
    assert_eq!(limited.message, sentence("lua"));
    // An exact reference is never ambiguous.
    assert_eq!(
        prepared(&switching, &args("fake/m"), None).model.reference,
        "fake/m"
    );
    // The union is exact: no duplicate when the registry and the naming
    // list name the same reference. The fixture registry already holds
    // `other/m`, so all three list.
    assert_eq!(
        rejected(&switching, &args("m"), None).message,
        "The model `m` is offered by more than one provider: fake/m, lua/m, other/m. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_bare_id_only_an_unloaded_provider_has_is_the_credential_sentence() {
    let fixture = fixture("fiber-switch-bare-unloaded");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    // Only the unloaded `lua` names `rm`.
    let naming = vec![("lua".to_owned(), "rm".to_owned())];
    let switching = Switching::new(providers, &[], naming, config, credentials).unwrap();
    let rejection = rejected(&switching, &args("rm"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, sentence("lua"));
}

#[test]
fn a_naming_match_joins_a_resolved_match_in_ambiguity() {
    // The registry resolves bare `n` to `fake/n`; the naming list adds the
    // unloaded `lua/n`.
    let fixture = fixture("fiber-switch-resolved-naming");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n".to_owned()),
    ];
    let switching = Switching::new(providers, &[], naming, config, credentials).unwrap();
    let rejection = rejected(&switching, &args("n"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `n` is offered by more than one provider: fake/n, lua/n. \
         Name one as `provider/model`."
    );
}

#[test]
fn an_unconfigured_model_counts_toward_ambiguity() {
    let fixture = fixture("fiber-switch-unconfigured");
    let acme = fixture.home.join("extensions/acme");
    std::fs::create_dir_all(acme.join("providers")).unwrap();
    std::fs::write(
        acme.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        acme.join("providers/acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"workspace": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
    let config = config(&fixture, &[]);
    let (mut providers, _) = Providers::load(&fixture.home).unwrap();
    let snapshot = providers.clone();
    let extensions = extensions::SessionExtensions::load(
        &fixture.home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
    );
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    assert!(
        naming.contains(&("acme".to_owned(), "m".to_owned())),
        "the naming list keeps the unconfigured model: {naming:?}"
    );
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let switching = Switching::new(snapshot, &[], naming, config, credentials).unwrap();
    // Configured `fake/m` and unconfigured `acme/m` make the bare `m`
    // ambiguous.
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(
        rejection.message.contains("acme/m"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_suffix_beats_args_thinking_which_beats_chosen() {
    let fixture = fixture("fiber-switch-thinking");
    let switching = switching(&fixture, &[]);
    let made = prepared(
        &switching,
        &args_thinking("fake/n:high", "low"),
        Some(ThinkingLevel::Low),
    );
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
    assert_eq!(made.chosen, Some(ThinkingLevel::High));
    let made = prepared(
        &switching,
        &args_thinking("fake/n", "high"),
        Some(ThinkingLevel::Low),
    );
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
    assert_eq!(made.chosen, Some(ThinkingLevel::High));
    let made = prepared(&switching, &args("fake/n"), Some(ThinkingLevel::Low));
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    assert_eq!(made.chosen, Some(ThinkingLevel::Low));
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    assert_eq!(made.chosen, None);
}

#[test]
fn a_configured_unsupported_level_gives_the_default_and_a_notice() {
    let fixture = fixture("fiber-switch-configured-thinking");
    let switching = switching(&fixture, &[r#"models."fake/n".thinking=medium"#]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    let notice = made.notice.expect("the ignored key is noticed");
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
}

#[test]
fn a_chosen_unsupported_level_is_invalid_arguments() {
    let fixture = fixture("fiber-switch-chosen-thinking");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("fake/n"), Some(ThinkingLevel::Medium));
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
}

#[test]
fn an_unparseable_args_thinking_rejects_before_any_lookup() {
    let fixture = fixture("fiber-switch-bad-thinking");
    let switching = switching(&fixture, &[]);
    // `other/m` would be the credential sentence; the thinking level
    // rejects first.
    let rejection = rejected(&switching, &args_thinking("other/m", "sideways"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("sideways"),
        "{}",
        rejection.message
    );
}

#[test]
fn a_bedrock_model_is_rejected() {
    let fixture = fixture("fiber-switch-bedrock");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("bed", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![("bed".to_owned(), "bk".to_owned())];
    let switching = Switching::new(providers, &[], naming, config, credentials).unwrap();
    let rejection = rejected(&switching, &args("bed/bk"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("does not speak yet"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_reviewer_is_rechosen_for_the_new_provider() {
    let fixture = fixture("fiber-switch-reviewer");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    let made = prepared(&switching, &args("fake/n"), None);
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "fake/r");
    assert_eq!(reviewer.context_window, Some(1000));
}

#[test]
fn a_reviewer_outside_the_map_is_the_credential_sentence_and_the_switch_stands() {
    let fixture = fixture("fiber-switch-reviewer-outside");
    let switching = switching(&fixture, &["reviewer.model=other/m"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(failure.message, sentence("other"));
}

#[test]
fn a_reviewer_on_an_unloaded_lua_provider_is_the_credential_sentence() {
    let fixture = fixture("fiber-switch-reviewer-lua");
    let switching = switching(&fixture, &["reviewer.model=lua/rm"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(failure.message, sentence("lua"));
}

#[test]
fn without_a_reviewer_the_loop_gets_its_failure() {
    let fixture = fixture("fiber-switch-no-reviewer");
    let switching = switching(&fixture, &[]);
    let made = prepared(&switching, &args("fake/n"), None);
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
}

#[test]
fn the_prepared_fields_follow_the_new_model() {
    let fixture = fixture("fiber-switch-fields");
    let switching = switching(&fixture, &["cache.lifetime=5m", "handoff.enabled=false"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.context_window, Some(1000));
    assert_eq!(made.addendum, Some("The extra paragraph.\n".to_owned()));
    assert_eq!(
        made.cache_lifetime,
        contract::events::CacheLifetime::FiveMinutes
    );
    assert!(!made.handoff.enabled);
    let searched = prepared(&switching, &args("claude/w"), None);
    assert_eq!(searched.web_search, Some("web_search_20250305".to_owned()));
    assert_eq!(searched.context_window, Some(500));
}

#[test]
fn a_data_file_written_after_the_snapshot_does_not_resolve() {
    let fixture = fixture("fiber-switch-snapshot");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    std::fs::write(
        fixture.home.join("extensions/fake/providers/fake.json"),
        json!({
            "name": "fake",
            "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 100000},
                {"id": "n", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                {"id": "r", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                {"id": "late", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let rejection = rejected(&switching, &args("fake/late"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("has no model"),
        "{}",
        rejection.message
    );
}

#[test]
fn preparing_reads_no_credential_source() {
    let fixture = fixture("fiber-switch-no-read");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("other/m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        !fixture.marker.exists(),
        "the `command` credential never ran"
    );
}

/// A retained Lua provider with a model cache resolves its cached models
/// without any listing request; negating the cache guard would skip it.
#[test]
fn a_retained_lua_provider_with_a_cache_resolves_without_a_listing_request() {
    let server = fakes::ProviderServer::start([]).unwrap();
    let root = fakes::TempDir::new("fiber-switch-lua-cache");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(&home, "fixture.url", &config::Secret::new(server.url())).unwrap();
    config::store_secret(&home, "fixture.api_key", &config::Secret::new("k1".into())).unwrap();
    config::write_model_cache(
        &home,
        "fixture",
        &json!([{"id": "cached", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()), "context_window": 1000}]),
    )
    .unwrap();
    let config = Config::load(Sources {
        home: home.clone(),
        workspace,
        project: ProjectKey::new("test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let (mut providers, _) = Providers::load(&home).unwrap();
    let snapshot = providers.clone();
    let mut extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
    );
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    extensions.retain_lua_providers(&["fixture"]);
    let credentials: Credentials = [("fixture", ("default".to_owned(), (None, None)))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let switching = Switching::new(
        snapshot,
        extensions.lua_providers(),
        naming,
        config,
        credentials,
    )
    .unwrap();
    let made = prepared(&switching, &args("fixture/cached"), None);
    assert_eq!(made.model.reference, "fixture/cached");
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path != "/v1/models"),
        "no listing request ran: {:?}",
        server.requests()
    );
}

/// A retained Lua provider with no cache that registers `credential` does
/// not resolve, and the clone loads no listing; removing the guard would
/// retry discovery over a listing request. Discovery fails here (the server
/// answers 500 past its empty script), so no cache is written and the
/// naming list never holds the provider.
#[test]
fn a_retained_lua_provider_without_a_cache_does_not_resolve() {
    let server = fakes::ProviderServer::start([]).unwrap();
    let root = fakes::TempDir::new("fiber-switch-lua-live");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(&home, "fixture.url", &config::Secret::new(server.url())).unwrap();
    config::store_secret(&home, "fixture.api_key", &config::Secret::new("k1".into())).unwrap();
    let config = Config::load(Sources {
        home: home.clone(),
        workspace,
        project: ProjectKey::new("test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let (mut providers, _) = Providers::load(&home).unwrap();
    let snapshot = providers.clone();
    let mut extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
    );
    // Discovery runs here, for the naming list, and fails; the clone must
    // not run it again.
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    assert!(
        !naming.iter().any(|(provider, _)| provider == "fixture"),
        "failed discovery names nothing: {naming:?}"
    );
    let listed = server
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/models")
        .count();
    extensions.retain_lua_providers(&["fixture"]);
    let credentials: Credentials = [("fixture", ("default".to_owned(), (None, None)))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let switching = Switching::new(
        snapshot,
        extensions.lua_providers(),
        naming,
        config,
        credentials,
    )
    .unwrap();
    let rejection = rejected(&switching, &args("fixture/live"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/models")
            .count(),
        listed,
        "the clone loaded no listing"
    );
}
