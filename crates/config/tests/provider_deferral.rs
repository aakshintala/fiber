//! `docs/tools.md`, "Deferral is a property of the model": every model
//! whose provider data sets `deferred_tools` earned it in the probe. This
//! file compiles in the shipped provider files and `docs/tools.md`;
//! `provider_data.rs` reads synthetic documented data instead.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

/// Every `provider/id` whose probe passed, per provider. Task 5 fills this
/// from the runs; until Task 6 flips the data it is the expected set, so
/// the first two tests fail against unflipped data.
const DEFERRING: &[(&str, &[&str])] = &[
    (
        "anthropic",
        &[
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-haiku-5-5",
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5-5",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
        ],
    ),
    (
        "openai",
        &[
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.5",
            "gpt-5.6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-6-luna",
            "gpt-6-sol",
        ],
    ),
];

/// Every first-party `providers/<package>/providers/<name>.json`, compiled in.
const PROVIDER_FILES: &[(&str, &str)] = &[
    (
        "anthropic",
        include_str!("../../../providers/anthropic/providers/anthropic.json"),
    ),
    (
        "bedrock",
        include_str!("../../../providers/bedrock/providers/bedrock.json"),
    ),
    (
        "codex",
        include_str!("../../../providers/codex/providers/codex.json"),
    ),
    (
        "databricks",
        include_str!("../../../providers/databricks/providers/databricks.json"),
    ),
    (
        "gemini",
        include_str!("../../../providers/gemini/providers/gemini.json"),
    ),
    (
        "muse",
        include_str!("../../../providers/muse/providers/muse.json"),
    ),
    (
        "openai",
        include_str!("../../../providers/openai/providers/openai.json"),
    ),
    (
        "opencode-go",
        include_str!("../../../providers/opencode/providers/opencode-go.json"),
    ),
    (
        "opencode-zen",
        include_str!("../../../providers/opencode/providers/opencode-zen.json"),
    ),
    (
        "openrouter",
        include_str!("../../../providers/openrouter/providers/openrouter.json"),
    ),
    (
        "vertex",
        include_str!("../../../providers/vertex/providers/vertex.json"),
    ),
];

/// The models one provider file marks as deferring.
fn deferring_rows(text: &str) -> Vec<String> {
    let provider: config::ProviderData = serde_json::from_str(text).unwrap();
    provider
        .models
        .iter()
        .filter(|model| model.deferred_tools)
        .map(|model| format!("{}/{}", provider.name, model.id))
        .collect()
}

#[test]
fn only_probed_rows_set_deferred_tools() {
    let mut set: Vec<String> = PROVIDER_FILES
        .iter()
        .flat_map(|(_, text)| deferring_rows(text))
        .collect();
    set.sort();
    let mut probed: Vec<String> = DEFERRING
        .iter()
        .flat_map(|(provider, ids)| ids.iter().map(move |id| format!("{provider}/{id}")))
        .collect();
    probed.sort();
    assert_eq!(set, probed);
}

#[test]
fn every_deferring_row_has_a_pass_record() {
    let tools: &str = include_str!("../../../docs/tools.md");
    assert!(
        tools.contains("--example probe"),
        "docs/tools.md names the probe command"
    );
    for (provider, ids) in DEFERRING {
        for id in *ids {
            let record = format!("`{provider}/{id}` pass");
            assert!(tools.contains(&record), "docs/tools.md records {record}");
        }
    }
}

#[test]
fn every_deferring_row_speaks_a_deferring_protocol() {
    for (provider, ids) in DEFERRING {
        let (_, text) = PROVIDER_FILES
            .iter()
            .find(|(name, _)| name == provider)
            .unwrap();
        let data: config::ProviderData = serde_json::from_str(text).unwrap();
        for id in *ids {
            let model = data.models.iter().find(|model| model.id == *id).unwrap();
            assert!(
                matches!(
                    model.protocol,
                    config::Protocol::AnthropicMessages | config::Protocol::OpenaiResponses
                ),
                "{provider}/{id} speaks {:?}, which never defers",
                model.protocol
            );
        }
    }
}
