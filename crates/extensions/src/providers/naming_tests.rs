//! Unit tests for model-name resolution (`docs/model-routing.md`, "Naming a
//! model"): every branch of `resolve_named` and `delegate_model` against a
//! registry built by hand, plus the `naming()` set across the placeholder
//! fill.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::collections::BTreeMap;

use config::{
    Config, CredentialFrom, ModelData, Placeholder, ProjectKey, Protocol, ProviderData, Sources,
};
use contract::ThinkingLevel;
use serde_json::Map;

use crate::{Providers, START_ONLY};

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

/// The naming list sorted, for set comparison.
fn sorted(naming: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut naming = naming;
    naming.sort();
    naming
}

#[test]
fn an_exact_provider_model_resolves() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m2")])]);
    let made = providers.resolve_named(&[], "fake/m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_scripted_model_missing_from_the_registry_gives_start_only() {
    let providers = registry(&[("fake", vec![model("m")])]);
    assert_eq!(
        providers.resolve_named(&[], "scripted/new.json"),
        Err(START_ONLY.to_owned())
    );
}

#[test]
fn an_unknown_reference_gives_the_registry_message() {
    let providers = registry(&[("fake", vec![model("m")])]);
    assert_eq!(
        providers.resolve_named(&[], "fake/nope"),
        Err(
            "The provider `fake` has no model `nope`. Run `fiber models` and name one it lists."
                .to_owned()
        )
    );
}

#[test]
fn a_bare_id_one_provider_names_resolves() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m2")])]);
    let made = providers.resolve_named(&[], "m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_bare_id_two_providers_share_is_ambiguous() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m")])]);
    assert_eq!(
        providers.resolve_named(&[], "m"),
        Err(
            "The model `m` is offered by more than one provider: fake/m, other/m. \
             Name one as `provider/model`."
                .to_owned()
        )
    );
}

#[test]
fn a_bare_id_only_the_naming_list_names_is_the_registry_error() {
    let providers = registry(&[("fake", vec![model("m")])]);
    let naming = vec![("lua".to_owned(), "rm".to_owned())];
    // One naming-list match is not ambiguity: the message is the
    // registry's unknown-model error.
    assert_eq!(
        providers.resolve_named(&naming, "rm"),
        Err("No installed model matches `rm`. Run `fiber models` to list them.".to_owned())
    );
}

#[test]
fn a_duplicate_naming_entry_does_not_make_an_exact_match_ambiguous() {
    let providers = registry(&[("fake", vec![model("m")])]);
    // The registry and the naming list name the same reference.
    let naming = vec![("fake".to_owned(), "m".to_owned())];
    let made = providers.resolve_named(&naming, "m").unwrap();
    assert_eq!(made.reference(), "fake/m");
}

#[test]
fn a_naming_match_joins_a_resolved_match_in_ambiguity() {
    let providers = registry(&[("fake", vec![model("n")])]);
    // The registry resolves bare `n` to `fake/n`; the naming list adds the
    // unloaded `lua/n`.
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n".to_owned()),
    ];
    assert_eq!(
        providers.resolve_named(&naming, "n"),
        Err(
            "The model `n` is offered by more than one provider: fake/n, lua/n. \
             Name one as `provider/model`."
                .to_owned()
        )
    );
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
    let made = providers.resolve_named(&naming, "n:high").unwrap();
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
    let made = providers.resolve_named(&naming, "n:high").unwrap();
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
    assert_eq!(
        providers.resolve_named(&naming, "n:high"),
        Err(message.to_owned())
    );
}

#[test]
fn two_naming_matches_for_a_literal_are_ambiguous_on_the_full_id() {
    let providers = registry(&[("lit", vec![model("n:high")])]);
    // The unloaded `lua` names the same literal `n:high`.
    let naming = vec![
        ("lit".to_owned(), "n:high".to_owned()),
        ("lua".to_owned(), "n:high".to_owned()),
    ];
    assert_eq!(
        providers.resolve_named(&naming, "n:high"),
        Err(
            "The model `n:high` is offered by more than one provider: lit/n:high, lua/n:high. \
             Name one as `provider/model`."
                .to_owned()
        )
    );
}

#[test]
fn two_registry_literals_sharing_one_id_are_ambiguous_on_the_full_id() {
    let providers = registry(&[
        ("lit1", vec![model("n:high")]),
        ("lit2", vec![model("n:high")]),
    ]);
    // The naming list holds only one of the two literals.
    let naming = vec![("lit1".to_owned(), "n:high".to_owned())];
    assert_eq!(
        providers.resolve_named(&naming, "n:high"),
        Err(
            "The model `n:high` is offered by more than one provider: lit1/n:high, lit2/n:high. \
             Name one as `provider/model`."
                .to_owned()
        )
    );
}

#[test]
fn a_suffixed_id_without_a_literal_reports_the_stripped_ambiguity() {
    let providers = registry(&[("fake", vec![model("m")]), ("other", vec![model("m")])]);
    assert_eq!(
        providers.resolve_named(&[], "m:high"),
        Err(
            "The model `m` is offered by more than one provider: fake/m, other/m. \
             Name one as `provider/model`."
                .to_owned()
        )
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
