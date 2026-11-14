//! Unit tests for the shared suggestion helpers: the edit count, the gated
//! candidate pool, ranking and roles line both delegate callers use.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::collections::BTreeMap;

use config::{
    Config, CredentialFrom, ModelData, ProjectKey, Protocol, ProviderData, Secret, Sources,
};
use contract::ThinkingLevel;
use serde_json::Map;

use super::{closest_refs, distance, roles_line};
use crate::Providers;

/// A plain model `id` on the test protocol.
fn model(id: &str) -> ModelData {
    ModelData {
        is_new: false,
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
        omit_reason: None,
    }
}

/// A model `id` taking `levels`.
fn leveled(id: &str, levels: Vec<ThinkingLevel>) -> ModelData {
    let mut made = model(id);
    made.thinking_default = levels.first().copied();
    made.thinking_levels = levels;
    made
}

/// A registry holding `models` under each provider in `entries`, every
/// provider reading its credential from storage.
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

/// A configuration over a fresh home, switching on each of `holders`
/// through a stored credential, with `roles` written as the `roles` object.
/// The temporary directory lives as long as the configuration, so the
/// stored files stay readable.
fn configured(holders: &[&str], roles: serde_json::Value) -> (fakes::TempDir, Config) {
    let root = fakes::TempDir::new("fiber-suggest-credentials");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for holder in holders {
        config::store_credential(&home, holder, "default", &Secret::new("key".into())).unwrap();
    }
    std::fs::write(
        home.join("config.json"),
        serde_json::json!({"roles": roles}).to_string(),
    )
    .unwrap();
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
fn distance_pins_known_edit_counts() {
    assert_eq!(distance("", "ab"), 2);
    assert_eq!(distance("abc", ""), 3);
    assert_eq!(distance("kitten", "sitting"), 3);
    assert_eq!(distance("ab", "ba"), 2);
}

#[test]
fn distance_ranks_a_shared_prefix_closer_than_a_single_letter() {
    assert_eq!(distance("abc", "ab"), 1);
    assert_eq!(distance("abc", "a"), 2);
    assert!(distance("abc", "ab") < distance("abc", "a"));
}

/// The count runs over characters, not bytes: a one-letter accented
/// substitution and a one-letter kana substitution each cost 1, where a
/// byte-wise count would charge their full UTF-8 widths.
#[test]
fn distance_counts_characters_not_bytes() {
    assert_eq!(distance("héllo", "hello"), 1);
    assert_eq!(distance("あ", "い"), 1);
}

/// The ruled shape: full strings compare whole, so the suffixed typo is 1
/// edit away while the two bare forms tie at 7 and sort lexically; the
/// 4th, farther reference truncates away.
#[test]
fn closest_refs_ranks_full_strings_with_sorted_ties_and_truncates_at_three() {
    let providers = registry(&[(
        "openai",
        vec![
            leveled("gpt-5.6", vec![ThinkingLevel::Xhigh]),
            model("gpt-5.4"),
            model("other"),
        ],
    )]);
    let (_root, config) = configured(&["openai"], serde_json::json!({}));
    assert_eq!(
        closest_refs(&providers, &config, "fiber:openai/gpt-5.5:xhigh"),
        [
            "fiber:openai/gpt-5.6:xhigh",
            "fiber:openai/gpt-5.4",
            "fiber:openai/gpt-5.6",
        ]
    );
}

/// Presence gates the pool: the logged-out provider's models never rank,
/// however near they read, so the typed bare reference suggests only the
/// switched-on provider's reference.
#[test]
fn closest_refs_skips_logged_out_providers() {
    let providers = registry(&[("off", vec![model("aaa")]), ("on", vec![model("zzz")])]);
    let (_root, config) = configured(&["on"], serde_json::json!({}));
    assert_eq!(
        closest_refs(&providers, &config, "off/aae"),
        ["fiber:on/zzz"]
    );
}

/// No switched-on provider means no candidates, so the refusal renders
/// the `none` line.
#[test]
fn closest_refs_with_no_candidates_is_empty() {
    let providers = registry(&[("off", vec![model("aaa")])]);
    let (_root, config) = configured(&[], serde_json::json!({}));
    assert!(closest_refs(&providers, &config, "off/aaa").is_empty());
}

/// Every configured role lists by name, sorted: the object form drops its
/// credential label, and an unresolvable target still counts by name.
#[test]
fn roles_line_lists_configured_role_names_sorted() {
    let (_root, config) = configured(
        &[],
        serde_json::json!({
            "zeta": "fiber:fake/z",
            "alpha": {"model": "fiber:fake/a", "credential": "work"},
            "mid": "fiber:gone/m",
        }),
    );
    assert_eq!(roles_line(&config), "Roles: alpha, mid, zeta");
}

/// No configured roles renders the `none` line.
#[test]
fn roles_line_with_no_roles_reads_none() {
    let (_root, config) = configured(&[], serde_json::json!({}));
    assert_eq!(roles_line(&config), "Roles: none");
}
