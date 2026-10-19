//! `docs/model-routing.md`, "What a provider extension declares": a named
//! `reviewer_model` is one of its provider's own models and never a
//! contributor model. This file compiles in the shipped provider files;
//! `provider_data.rs` reads synthetic documented data instead.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::path::{Path, PathBuf};

fn package(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name)
}

#[test]
fn a_shipped_reviewer_model_is_in_its_providers_list_and_is_not_a_contributor_model() {
    // Every first-party `providers/<package>/providers/<name>.json`, compiled in:
    // listing them at run time would read the repository at test time
    // (`docs/ci.md`, "Selection").
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
    ];
    let mut checked = 0;
    for (stem, text) in PROVIDER_FILES {
        let provider: config::ProviderData = serde_json::from_str(text).unwrap();
        assert_eq!(provider.name, *stem, "{stem} names itself");
        let Some(model) = provider.reviewer_model.as_deref() else {
            continue;
        };
        checked += 1;
        assert!(
            !model.contains("contributor"),
            "{} names {model:?}, a contributor model",
            provider.name
        );
        assert!(
            provider.models.is_empty() || provider.models.iter().any(|m| m.id == model),
            "{} names {model:?} outside its models",
            provider.name
        );
    }
    assert!(checked > 0, "no shipped provider names a reviewer model");
}

#[test]
fn the_muse_1_3_entries_declare_the_thinking_levels_their_routes_accept() {
    use contract::ThinkingLevel::{High, Low, Max, Medium, Minimal, Xhigh};
    let levels = vec![Minimal, Low, Medium, High, Xhigh, Max];
    let muse = config::read_providers(&package("muse")).unwrap();
    let muse = muse.iter().find(|p| p.name == "muse").unwrap();
    for id in ["muse-spark-1.3", "muse-spark-1.3-contributor"] {
        let model = muse.models.iter().find(|m| m.id == id).unwrap();
        assert_eq!(model.thinking_levels, levels, "{id}");
        assert_eq!(model.thinking_default, None, "{id}");
    }
    let opencode = config::read_providers(&package("opencode")).unwrap();
    let go = opencode.iter().find(|p| p.name == "opencode-go").unwrap();
    let model = go
        .models
        .iter()
        .find(|m| m.id == "muse-spark-1.3-contributor")
        .unwrap();
    assert_eq!(model.thinking_levels, levels);
    assert_eq!(model.thinking_default, None);
    let old = go
        .models
        .iter()
        .find(|m| m.id == "muse-spark-1.2-contributor")
        .unwrap();
    // The Go route takes minimal through xhigh here
    // (research/opencode-thinking/raw/go-muse-spark-1.2-contributor.json).
    assert_eq!(old.thinking_levels, vec![Minimal, Low, Medium, High, Xhigh]);
    assert_eq!(old.thinking_default, None);
    let zen = opencode.iter().find(|p| p.name == "opencode-zen").unwrap();
    let model = zen
        .models
        .iter()
        .find(|m| m.id == "muse-spark-1.3")
        .unwrap();
    assert_eq!(model.thinking_levels, levels);
    assert_eq!(model.thinking_default, None);
}

#[test]
fn the_codex_entries_declare_the_thinking_levels_their_routes_accept() {
    use contract::ThinkingLevel::{High, Low, Max, Medium, Xhigh};
    let levels = vec![Low, Medium, High, Xhigh, Max];
    let codex = config::read_providers(&package("codex")).unwrap();
    let codex = codex.iter().find(|p| p.name == "codex").unwrap();
    for (id, default) in [
        ("gpt-6.1-sol", Low),
        ("gpt-6-sol", Medium),
        ("gpt-6-astra", Low),
        ("gpt-6-luna", Medium),
    ] {
        let model = codex.models.iter().find(|m| m.id == id).unwrap();
        assert_eq!(model.thinking_levels, levels, "{id}");
        assert_eq!(model.thinking_default, Some(default), "{id}");
    }
    let anthropic = config::read_providers(&package("anthropic")).unwrap();
    let anthropic = anthropic.iter().find(|p| p.name == "anthropic").unwrap();
    let model = anthropic
        .models
        .iter()
        .find(|m| m.id == "claude-opus-4-7")
        .unwrap();
    assert_eq!(
        model.thinking_levels,
        vec![contract::ThinkingLevel::Off, Low, Medium, High, Xhigh, Max]
    );
    assert_eq!(model.thinking_default, None);
    let gemini = config::read_providers(&package("gemini")).unwrap();
    let gemini = gemini.iter().find(|p| p.name == "gemini").unwrap();
    let model = gemini
        .models
        .iter()
        .find(|m| m.id == "gemini-3.8-flash")
        .unwrap();
    assert_eq!(model.thinking_levels, vec![Low, Medium, High]);
    assert_eq!(model.thinking_default, None);
    let openai = config::read_providers(&package("openai")).unwrap();
    let openai = openai.iter().find(|p| p.name == "openai").unwrap();
    let model = openai.models.iter().find(|m| m.id == "gpt-5.2").unwrap();
    assert_eq!(
        model.thinking_levels,
        vec![contract::ThinkingLevel::Off, Low, Medium, High, Xhigh]
    );
    assert_eq!(model.thinking_default, None);
    let plain = openai.models.iter().find(|m| m.id == "gpt-4o").unwrap();
    assert!(plain.thinking_levels.is_empty());
    assert_eq!(plain.thinking_default, None);
}
