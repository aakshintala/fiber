//! Unit tests for model-name resolution (`docs/model-routing.md`, "Naming a
//! model"): every branch of `resolve_named` and `delegate_model` against a
//! registry built by hand, plus the `naming()` set across the placeholder
//! fill.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::collections::BTreeMap;

use config::{
    Config, CredentialFrom, ModelData, Placeholder, ProjectKey, Protocol, ProviderData, Secret,
    Sources,
};
use contract::ThinkingLevel;
use serde_json::Map;

use crate::{Error, Providers, START_ONLY};

/// A plain model `id` on the test protocol.
fn model(id: &str) -> ModelData {
    ModelData {
        id: id.to_owned(),
        name: None,
        protocol: Protocol::OpenaiResponses,
        base_url: "http://127.0.0.1:9/v1".to_owned(),
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
    }
}

/// A model `id` taking the low and high thinking levels, defaulting low.
fn thinking(id: &str) -> ModelData {
    let mut made = model(id);
    made.thinking_levels = vec![ThinkingLevel::Low, ThinkingLevel::High];
    made.thinking_default = Some(ThinkingLevel::Low);
    made
}

/// A registry holding `models` under each provider in `entries`.
fn registry(entries: &[(&str, Vec<ModelData>)]) -> Providers {
    let mut providers = Providers::default();
    for (name, models) in entries {
        providers.by_name.insert(
            (*name).to_owned(),
            ProviderData {
                name: (*name).to_owned(),
                credential_from: CredentialFrom::Stored {
                    credential: None,
                    login: None,
                },
                credential_name: None,
                headers: BTreeMap::new(),
                placeholders: BTreeMap::new(),
                models: models.clone(),
                reviewer_model: None,
            },
        );
    }
    providers
}

/// A registry like `registry`, with every provider reading its credential
/// from `from`: a `Function` source names `credential()` providers that read
/// nothing Fiber stores.
fn registry_from(entries: &[(&str, Vec<ModelData>)], from: CredentialFrom) -> Providers {
    let mut providers = Providers::default();
    for (name, models) in entries {
        providers.by_name.insert(
            (*name).to_owned(),
            ProviderData {
                name: (*name).to_owned(),
                credential_from: from.clone(),
                credential_name: None,
                headers: BTreeMap::new(),
                placeholders: BTreeMap::new(),
                models: models.clone(),
                reviewer_model: None,
            },
        );
    }
    providers
}

/// The naming list sorted, for set comparison.
fn sorted(naming: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut naming = naming;
    naming.sort();
    naming
}

/// A configuration over a fresh home, switching on each of `holders`
/// through a stored credential: presence comes from `has_credential`'s
/// own origins, never a bypass seam. The temporary directory lives as
/// long as the configuration, so the stored files stay readable.
fn credentialed(holders: &[&str]) -> (fakes::TempDir, Config) {
    let root = fakes::TempDir::new("fiber-naming-credentials");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for holder in holders {
        config::store_credential(&home, holder, "default", &Secret::new("key".into())).unwrap();
    }
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: home,
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    (root, config)
}

#[test]
fn an_exact_provider_model_resolves() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m2")])]);
    let (_root, config) = credentialed(&[]);
    let made = providers.resolve_named(&[], &config, "fake/m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_scripted_model_missing_from_the_registry_gives_start_only() {
    let providers = registry(&[("fake", vec![model("m")])]);
    let (_root, config) = credentialed(&[]);
    let error = providers
        .resolve_named(&[], &config, "scripted/new.json")
        .unwrap_err();
    assert!(matches!(error, Error::ScriptedStartOnly));
    assert_eq!(error.to_string(), START_ONLY);
}

#[test]
fn an_unknown_reference_gives_the_registry_message() {
    let providers = registry(&[("fake", vec![model("m")])]);
    let (_root, config) = credentialed(&[]);
    let error = providers
        .resolve_named(&[], &config, "fake/nope")
        .unwrap_err();
    assert!(matches!(error, Error::UnknownModel { .. }));
    assert_eq!(
        error.to_string(),
        "The provider `fake` has no model `nope`. Run `fiber models` and name one it lists."
    );
}

#[test]
fn a_bare_id_one_provider_names_resolves() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m2")])]);
    let (_root, config) = credentialed(&["fake"]);
    let made = providers.resolve_named(&[], &config, "m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_bare_id_two_providers_share_is_ambiguous() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m")])]);
    let (_root, config) = credentialed(&["fake", "other"]);
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(error, Error::Ambiguous { .. }));
    assert_eq!(
        error.to_string(),
        "The model `m` is offered by more than one provider: fake/m, other/m. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_bare_id_only_the_naming_list_names_is_the_registry_error() {
    let providers = registry(&[("fake", vec![model("m")])]);
    let naming = vec![("lua".to_owned(), "rm".to_owned())];
    // One naming-list match is not ambiguity: the message is the
    // registry's unknown-model error.
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&naming, &config, "rm").unwrap_err();
    assert!(matches!(error, Error::ModelMissing { .. }));
    assert_eq!(
        error.to_string(),
        "No installed model matches `rm`. Run `fiber models` to list them."
    );
}

#[test]
fn an_unknown_bare_id_is_the_registry_error_not_an_empty_ambiguity() {
    let providers = registry(&[("fake", vec![model("m")])]);
    // Nothing names `nope`: no match at all is not ambiguity.
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&[], &config, "nope").unwrap_err();
    assert!(matches!(error, Error::ModelMissing { .. }));
}

#[test]
fn a_naming_match_joins_the_registry_ambiguity() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m")])]);
    // The registry already finds `m` ambiguous; the naming list adds the
    // unloaded `lua/m`, which the filter drops: it has no owning provider
    // in the switched-on set, so only installed switched-on holders list.
    let naming = vec![("lua".to_owned(), "m".to_owned())];
    let (_root, config) = credentialed(&["fake", "other"]);
    let error = providers.resolve_named(&naming, &config, "m").unwrap_err();
    assert!(matches!(error, Error::Ambiguous { .. }));
    assert_eq!(
        error.to_string(),
        "The model `m` is offered by more than one provider: fake/m, other/m. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_duplicate_naming_entry_does_not_make_an_exact_match_ambiguous() {
    let providers = registry(&[("fake", vec![model("m")])]);
    // The registry and the naming list name the same reference.
    let naming = vec![("fake".to_owned(), "m".to_owned())];
    let (_root, config) = credentialed(&["fake"]);
    let made = providers.resolve_named(&naming, &config, "m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_naming_match_joins_a_resolved_match_in_ambiguity() {
    let providers = registry(&[("fake", vec![model("n")])]);
    // The registry resolves bare `n` to `fake/n`; the naming list adds the
    // unloaded `lua/n`, which the filter drops: one switched-on holder
    // resolves.
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n".to_owned()),
    ];
    let (_root, config) = credentialed(&["fake"]);
    let made = providers.resolve_named(&naming, &config, "n").unwrap();
    assert_eq!(made.reference(), "fake/n");
}

#[test]
fn a_naming_only_literal_falls_back_to_the_stripped_id() {
    let providers = registry(&[("fake", vec![thinking("n")])]);
    // Only `lua`, which the registry does not hold, names the literal
    // `n:high`, so `fake/n` takes the `high` suffix.
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n:high".to_owned()),
    ];
    let (_root, config) = credentialed(&["fake"]);
    let made = providers.resolve_named(&naming, &config, "n:high").unwrap();
    assert_eq!(made.reference(), "fake/n");
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
}

#[test]
fn a_registry_literal_beats_the_suffix() {
    let providers = registry(&[("lit", vec![thinking("n"), model("n:high")])]);
    let naming = vec![
        ("lit".to_owned(), "n".to_owned()),
        ("lit".to_owned(), "n:high".to_owned()),
    ];
    // `n:high` names the literal model, with no thinking level: it is
    // neither `n` with `high` nor ambiguous with it.
    let (_root, config) = credentialed(&["lit"]);
    let made = providers.resolve_named(&naming, &config, "n:high").unwrap();
    assert_eq!(made.model.id, "n:high");
    assert_eq!(made.thinking, None);
}

#[test]
fn an_unconfigured_literal_reports_its_message_first() {
    let message = "The model `lit/n:high` needs the setting `workspace` for its base URL, \
         which has no value. Run `fiber login lit`, \
         or set `workspace` in `config/lit.json` in Fiber home.";
    let mut providers = registry(&[("fake", vec![thinking("n")]), ("lit", vec![thinking("n")])]);
    providers.unconfigured.insert(
        "lit".to_owned(),
        vec![("n:high".to_owned(), message.to_owned())],
    );
    // The stripped id is ambiguous on its own; the unconfigured literal
    // still wins on the full id.
    let naming = vec![("lit".to_owned(), "n:high".to_owned())];
    let (_root, config) = credentialed(&["lit"]);
    let error = providers
        .resolve_named(&naming, &config, "n:high")
        .unwrap_err();
    assert!(matches!(error, Error::Unconfigured { .. }));
    assert_eq!(error.to_string(), message);
}

#[test]
fn two_naming_matches_for_a_literal_are_ambiguous_on_the_full_id() {
    let providers = registry(&[("lit", vec![model("n:high")])]);
    // The unloaded `lua` names the same literal `n:high`, which the filter
    // drops: one switched-on holder of the full id resolves it.
    let naming = vec![
        ("lit".to_owned(), "n:high".to_owned()),
        ("lua".to_owned(), "n:high".to_owned()),
    ];
    let (_root, config) = credentialed(&["lit"]);
    let made = providers.resolve_named(&naming, &config, "n:high").unwrap();
    assert_eq!(made.model.id, "n:high");
    assert_eq!(made.thinking, None);
}

#[test]
fn two_registry_literals_sharing_one_id_are_ambiguous_on_the_full_id() {
    let providers = registry(&[
        ("lit1", vec![model("n:high")]),
        ("lit2", vec![model("n:high")]),
    ]);
    // The naming list holds only one of the two literals.
    let naming = vec![("lit1".to_owned(), "n:high".to_owned())];
    let (_root, config) = credentialed(&["lit1", "lit2"]);
    let error = providers
        .resolve_named(&naming, &config, "n:high")
        .unwrap_err();
    assert!(matches!(error, Error::Ambiguous { .. }));
    assert_eq!(
        error.to_string(),
        "The model `n:high` is offered by more than one provider: lit1/n:high, lit2/n:high. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_suffixed_id_without_a_literal_reports_the_stripped_ambiguity() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m")])]);
    let (_root, config) = credentialed(&["fake", "other"]);
    let error = providers.resolve_named(&[], &config, "m:high").unwrap_err();
    assert!(matches!(error, Error::Ambiguous { .. }));
    assert_eq!(
        error.to_string(),
        "The model `m` is offered by more than one provider: fake/m, other/m. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_bare_id_with_one_switched_on_holder_resolves_to_it() {
    // `pa` is switched on and `pb` is logged out, both having `m`: the
    // logged-out holder counts for nothing, so bare `m` is `pa/m`.
    let providers = registry(&[("pa", vec![model("m")]), ("pb", vec![model("m")])]);
    let (_root, config) = credentialed(&["pa"]);
    let made = providers.resolve_named(&[], &config, "m").unwrap();
    assert_eq!(made.reference(), "pa/m");
}

#[test]
fn a_bare_id_only_a_logged_out_holder_names_is_credential_missing() {
    // Only the logged-out `pb` has `m`: bare `m` names its holder as not
    // logged in, with the login command.
    let providers = registry(&[("pb", vec![model("m")])]);
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `pb`: `pb` has `m` but is not logged in. Run `fiber login pb`."
    );
}

#[test]
fn a_bare_id_with_no_switched_on_holder_names_every_logged_out_holder() {
    // Neither holder is switched on: one error names both, sorted, with
    // the single login command the missing-credential display appends.
    let providers = registry(&[("pb", vec![model("m")]), ("pa", vec![model("m")])]);
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `pa`: `pa`, `pb` have `m` but are not logged in. \
         Run `fiber login pa`."
    );
}

#[test]
fn a_bare_id_with_a_switched_off_unconfigured_holder_resolves() {
    // `fake` is switched on with the configured `m`; `acme` is logged out
    // with `m` left out with `model_unconfigured`: the logged-out entry
    // counts for nothing, so bare `m` is `fake/m`.
    let mut providers = registry(&[("fake", vec![model("m")]), ("acme", Vec::new())]);
    providers.unconfigured.insert(
        "acme".to_owned(),
        vec![(
            "m".to_owned(),
            "The model `acme/m` needs the setting `workspace`.".to_owned(),
        )],
    );
    let (_root, config) = credentialed(&["fake"]);
    let made = providers.resolve_named(&[], &config, "m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_literal_id_only_a_logged_out_provider_names_is_credential_missing() {
    // Only the logged-out `pb` holds the literal `n:high`: the full-id
    // stage reports its missing credential before any suffix strips.
    let providers = registry(&[("pb", vec![model("n:high")])]);
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&[], &config, "n:high").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `pb`: `pb` has `n:high` but is not logged in. \
         Run `fiber login pb`."
    );
}

#[test]
fn a_bare_id_with_a_thinking_suffix_keeps_the_level_on_the_switched_on_singleton() {
    // `pa` is switched on and `pb` is logged out, both holding thinking
    // model `n`: bare `n:high` strips to the switched-on singleton and
    // keeps the `high` level.
    let providers = registry(&[("pa", vec![thinking("n")]), ("pb", vec![thinking("n")])]);
    let (_root, config) = credentialed(&["pa"]);
    let made = providers.resolve_named(&[], &config, "n:high").unwrap();
    assert_eq!(made.reference(), "pa/n");
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
}

#[test]
fn a_bare_id_only_a_switched_off_unconfigured_holder_names_is_credential_missing() {
    // `acme` is logged out with `m` left out with `model_unconfigured`:
    // an unconfigured entry counts like a configured one, so bare `m`
    // names its logged-out holder instead of reporting `model_unconfigured`.
    let mut providers = registry(&[("acme", Vec::new())]);
    providers.unconfigured.insert(
        "acme".to_owned(),
        vec![(
            "m".to_owned(),
            "The model `acme/m` needs the setting `workspace`.".to_owned(),
        )],
    );
    let naming = vec![("acme".to_owned(), "m".to_owned())];
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&naming, &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `acme`: `acme` has `m` but is not logged in. \
         Run `fiber login acme`."
    );
}

#[test]
fn a_literal_id_only_a_switched_off_unconfigured_holder_names_is_credential_missing() {
    // The full-id stage counts an unconfigured entry like a configured
    // one: the logged-out holder's missing credential reports before any
    // suffix strips.
    let mut providers = registry(&[("acme", Vec::new())]);
    providers.unconfigured.insert(
        "acme".to_owned(),
        vec![(
            "n:high".to_owned(),
            "The model `acme/n:high` needs the setting `workspace`.".to_owned(),
        )],
    );
    let naming = vec![("acme".to_owned(), "n:high".to_owned())];
    let (_root, config) = credentialed(&[]);
    let error = providers
        .resolve_named(&naming, &config, "n:high")
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `acme`: `acme` has `n:high` but is not logged in. \
         Run `fiber login acme`."
    );
}

#[test]
fn naming_returns_the_same_set_before_and_after_the_placeholder_fill() {
    let root = fakes::TempDir::new("fiber-naming-fill");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    // `acme/m` fills its host from the `workspace` setting, which has no
    // value, so the fill leaves it out with `model_unconfigured`.
    let mut placeholders = BTreeMap::new();
    placeholders.insert("workspace".to_owned(), Placeholder { env: None });
    let mut filled = model("m");
    filled.base_url = "https://{workspace}/v1".to_owned();
    let mut providers = Providers::default();
    providers.by_name.insert(
        "acme".to_owned(),
        ProviderData {
            name: "acme".to_owned(),
            credential_from: CredentialFrom::Stored {
                credential: None,
                login: None,
            },
            credential_name: None,
            headers: BTreeMap::new(),
            placeholders,
            models: vec![filled],
            reviewer_model: None,
        },
    );
    providers
        .extension_of
        .insert("acme".to_owned(), "acme".to_owned());
    let before = sorted(providers.naming());
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: home.clone(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    providers.fill_placeholders(&config, &|_| None).unwrap();
    let after = sorted(providers.naming());
    assert!(after.contains(&("acme".to_owned(), "m".to_owned())));
    assert_eq!(before, after);
}

#[test]
fn delegate_model_takes_a_fiber_reference() {
    let providers = registry(&[("openai", vec![model("x")])]);
    let made = providers.delegate_model("fiber:openai/x").unwrap();
    assert_eq!(made.reference(), "openai/x");
}

#[test]
fn delegate_model_refuses_a_reference_without_the_prefix() {
    let providers = registry(&[("openai", vec![model("x")])]);
    assert_eq!(providers.delegate_model("openai/x"), None);
}

#[test]
fn delegate_model_refuses_a_bare_id_without_a_provider() {
    let providers = registry(&[("openai", vec![model("x")])]);
    assert_eq!(providers.delegate_model("fiber:x"), None);
}

#[test]
fn delegate_model_refuses_an_unknown_model() {
    let providers = registry(&[("openai", vec![model("x")])]);
    assert_eq!(providers.delegate_model("fiber:nope/x"), None);
}

#[test]
fn a_holder_flips_between_counted_and_logged_out_without_a_config_change() {
    // Presence is live: under one unchanged configuration, storing `pa`'s
    // credential flips bare `m` from naming its logged-out holder to
    // resolving it, and deleting the credential flips it back.
    let providers = registry(&[("pa", vec![model("m")])]);
    let root = fakes::TempDir::new("fiber-naming-presence-flip");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = Config::load(Sources {
        home: home.clone(),
        workspace: home.clone(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    config::store_credential(&home, "pa", "default", &Secret::new("key".into())).unwrap();
    let made = providers.resolve_named(&[], &config, "m").unwrap();
    assert_eq!(made.reference(), "pa/m");
    assert!(config::delete_credential(&home, "pa", "default").unwrap());
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    assert_eq!(
        error.to_string(),
        "No credential for `pa`: `pa` has `m` but is not logged in. Run `fiber login pa`."
    );
}

#[test]
fn a_logged_out_function_holder_renders_no_login_advice() {
    // Only the logged-out `bedrock` holder has `m`, and its `credential()`
    // reads nothing Fiber stores: bare `m` names it without the login
    // command.
    let providers = registry_from(
        &[("bedrock", vec![model("m")])],
        CredentialFrom::Function(config::CredentialLabelSources {
            names: "profile".into(),
            default: "the environment".into(),
            credential_env: Vec::new(),
            credential_paths: Vec::new(),
        }),
    );
    let (_root, config) = credentialed(&[]);
    let error = providers.resolve_named(&[], &config, "m").unwrap_err();
    assert!(matches!(
        error,
        Error::Config(config::ConfigError::CredentialMissing { .. })
    ));
    let message = error.to_string();
    assert!(message.contains("No credential for `bedrock`"), "{message}");
    assert!(!message.contains("fiber login"), "{message}");
}
