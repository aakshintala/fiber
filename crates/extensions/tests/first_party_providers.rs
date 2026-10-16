//! `docs/extensions.md`, "A fresh install": `extension_missing` names the
//! first-party extension that serves the provider, or gives a generic hint
//! when no first-party package serves it.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test helpers; a failure is the test's")]

use extensions::Error;

/// Every first-party `providers/<package>/providers/<name>.json`, compiled
/// in: reading them at run time would read the repository at test time
/// (`docs/ci.md`, "Selection").
const PROVIDER_FILES: &[(&str, &str, &str)] = &[
    (
        "anthropic",
        "anthropic",
        include_str!("../../../providers/anthropic/providers/anthropic.json"),
    ),
    (
        "codex",
        "codex",
        include_str!("../../../providers/codex/providers/codex.json"),
    ),
    (
        "gemini",
        "gemini",
        include_str!("../../../providers/gemini/providers/gemini.json"),
    ),
    (
        "muse",
        "muse",
        include_str!("../../../providers/muse/providers/muse.json"),
    ),
    (
        "openai",
        "openai",
        include_str!("../../../providers/openai/providers/openai.json"),
    ),
    (
        "opencode",
        "opencode-go",
        include_str!("../../../providers/opencode/providers/opencode-go.json"),
    ),
    (
        "opencode",
        "opencode-zen",
        include_str!("../../../providers/opencode/providers/opencode-zen.json"),
    ),
    (
        "openrouter",
        "openrouter",
        include_str!("../../../providers/openrouter/providers/openrouter.json"),
    ),
];

fn declared_providers() -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (package, stem, text) in PROVIDER_FILES.iter().copied() {
        let data: serde_json::Value = serde_json::from_str(text).expect("provider file parses");
        let name = data
            .get("name")
            .and_then(|name| name.as_str())
            .expect("provider file names itself");
        assert_eq!(name, stem, "{package}/{stem} names itself");
        pairs.push((name.to_owned(), package.to_owned()));
    }
    pairs
}

#[test]
fn every_declared_provider_names_its_package() {
    let pairs = declared_providers();
    assert!(
        pairs.contains(&("opencode-go".to_owned(), "opencode".to_owned())),
        "{pairs:?}"
    );
    assert!(
        pairs.contains(&("opencode-zen".to_owned(), "opencode".to_owned())),
        "{pairs:?}"
    );
    for (name, package) in &pairs {
        assert_eq!(
            Error::ProviderMissing {
                provider: name.clone(),
            }
            .to_string(),
            format!(
                "The provider `{name}` is not installed. Run `fiber extension install {package}`."
            )
        );
        assert_eq!(
            config::full_name(package),
            format!("github.com/aakshintala/fiber/providers/{package}")
        );
    }
}

#[test]
fn a_provider_no_first_party_package_serves_gets_the_generic_hint() {
    for provider in ["nobody-serves", "opencode"] {
        assert_eq!(
            Error::ProviderMissing {
                provider: provider.to_owned(),
            }
            .to_string(),
            format!(
                "The provider `{provider}` is not installed. Install the extension that provides it with `fiber extension install <name or URL>`."
            )
        );
    }
}

/// The first-party models whose provider hosts a search (`docs/tools.md`,
/// "Hosted by the provider"): every Anthropic model, every muse model on
/// `/v1/responses`, every Gemini model, and OpenCode's OpenAI pass-through.
#[test]
fn first_party_hosted_searches_are_the_marked_models() {
    let mut pairs = Vec::new();
    for (package, _, text) in PROVIDER_FILES.iter().copied() {
        let data: config::ProviderData = serde_json::from_str(text).expect("provider data parses");
        let mut models = data.models.clone();
        let notices = extensions::leave_out_invalid(&data.name, package, &mut models);
        assert!(notices.is_empty(), "{package}: {notices:?}");
        for model in &models {
            if let Some(kind) = &model.web_search {
                pairs.push((format!("{}/{}", data.name, model.id), kind.clone()));
            }
            if let Some(include) = model.extra_body.get("include") {
                assert_eq!(
                    include.as_array().and_then(|items| items.first()),
                    Some(&serde_json::Value::String(
                        "reasoning.encrypted_content".into()
                    )),
                    "{}/{} keeps reasoning replay first",
                    data.name,
                    model.id
                );
            }
        }
    }
    pairs.sort();
    let mut expected: Vec<(String, String)> = [
        ("anthropic/claude-fable-5", "web_search_20250305"),
        ("anthropic/claude-fable-5-1", "web_search_20250305"),
        ("anthropic/claude-haiku-4-5", "web_search_20250305"),
        ("anthropic/claude-haiku-4-5-20251001", "web_search_20250305"),
        ("anthropic/claude-haiku-5-5", "web_search_20250305"),
        ("anthropic/claude-opus-4-5", "web_search_20250305"),
        ("anthropic/claude-opus-4-5-20251101", "web_search_20250305"),
        ("anthropic/claude-opus-4-6", "web_search_20250305"),
        ("anthropic/claude-opus-4-7", "web_search_20250305"),
        ("anthropic/claude-opus-4-8", "web_search_20250305"),
        ("anthropic/claude-opus-5", "web_search_20250305"),
        ("anthropic/claude-opus-5-5", "web_search_20250305"),
        ("anthropic/claude-sonnet-4-5", "web_search_20250305"),
        (
            "anthropic/claude-sonnet-4-5-20250929",
            "web_search_20250305",
        ),
        ("anthropic/claude-sonnet-4-6", "web_search_20250305"),
        ("anthropic/claude-sonnet-5", "web_search_20250305"),
        ("anthropic/claude-sonnet-5-5", "web_search_20250305"),
        ("muse/muse-spark-1.1", "web_search"),
        ("muse/muse-spark-1.2", "web_search"),
        ("muse/muse-spark-1.2-contributor", "web_search"),
        ("muse/muse-spark-1.3", "web_search"),
        ("muse/muse-spark-1.3-contributor", "web_search"),
        ("gemini/gemini-3.1-flash-lite", "google_search"),
        ("gemini/gemini-3.1-flash-lite-preview", "google_search"),
        ("gemini/gemini-3.1-pro-preview", "google_search"),
        ("gemini/gemini-3.1-pro-preview-customtools", "google_search"),
        ("gemini/gemini-3.5-flash", "google_search"),
        ("gemini/gemini-3.5-flash-lite", "google_search"),
        ("gemini/gemini-3.6-flash", "google_search"),
        ("gemini/gemini-3.7-flash", "google_search"),
        ("gemini/gemini-3.8-flash", "google_search"),
        ("gemini/gemini-3-flash-preview", "google_search"),
        ("gemini/gemini-flash-latest", "google_search"),
        ("gemini/gemini-flash-lite-latest", "google_search"),
        ("gemini/gemma-4-26b-a4b-it", "google_search"),
        ("gemini/gemma-4-31b-it", "google_search"),
        ("openai/gpt-4.1", "web_search"),
        ("openai/gpt-4.1-mini", "web_search"),
        ("openai/gpt-4o", "web_search"),
        ("openai/gpt-4o-2024-05-13", "web_search"),
        ("openai/gpt-4o-2024-08-06", "web_search"),
        ("openai/gpt-4o-2024-11-20", "web_search"),
        ("openai/gpt-4o-mini", "web_search"),
        ("openai/gpt-5", "web_search"),
        ("openai/gpt-5-mini", "web_search"),
        ("openai/gpt-5-nano", "web_search"),
        ("openai/gpt-5-pro", "web_search"),
        ("openai/gpt-5.1", "web_search"),
        ("openai/gpt-5.2", "web_search"),
        ("openai/gpt-5.2-pro", "web_search"),
        ("openai/gpt-5.3-codex", "web_search"),
        ("openai/gpt-5.4", "web_search"),
        ("openai/gpt-5.4-mini", "web_search"),
        ("openai/gpt-5.4-nano", "web_search"),
        ("openai/gpt-5.4-pro", "web_search"),
        ("openai/gpt-5.5", "web_search"),
        ("openai/gpt-5.5-pro", "web_search"),
        ("openai/gpt-5.6-luna", "web_search"),
        ("openai/gpt-5.6-sol", "web_search"),
        ("openai/gpt-5.6-terra", "web_search"),
        ("openai/gpt-6-astra", "web_search"),
        ("openai/gpt-6-luna", "web_search"),
        ("openai/gpt-6-sol", "web_search"),
        ("openai/gpt-6.1-sol", "web_search"),
        ("openai/o3", "web_search"),
        ("openai/o4-mini", "web_search"),
        ("opencode-zen/gpt-6.1-sol", "web_search"),
    ]
    .into_iter()
    .map(|(model, kind)| (model.to_owned(), kind.to_owned()))
    .collect();
    expected.sort();
    assert_eq!(pairs, expected);
}
