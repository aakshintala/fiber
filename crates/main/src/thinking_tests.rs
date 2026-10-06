//! The thinking resolution's precedence (`docs/model-routing.md`,
//! "Thinking"): each source beats every later one, and a level the model
//! does not take fails before any request.

use super::thinking;
use config::ModelData;
use contract::{ErrorCode, ThinkingLevel};
use serde_json::json;

fn model() -> ModelData {
    serde_json::from_value(json!({
        "id": "gpt-5.6",
        "protocol": "openai-responses",
        "base_url": "https://x/v1",
        "thinking_levels": ["low", "high"],
        "thinking_default": "low",
    }))
    .unwrap()
}

fn model_without_levels() -> ModelData {
    serde_json::from_value(json!({
        "id": "mini",
        "protocol": "openai-responses",
        "base_url": "https://x/v1",
    }))
    .unwrap()
}

fn config(overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-thinking");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let project = config::ProjectKey::new("test").unwrap();
    config::Config::load(config::Sources {
        home,
        workspace,
        project,
        overrides,
    })
    .unwrap()
}

fn empty() -> config::Config {
    config(Vec::new())
}

#[test]
fn each_source_beats_every_later_one() {
    use ThinkingLevel::{High, Low};
    let model = model();
    let reference = "openai/gpt-5.6";
    // Suffix beats all.
    assert_eq!(
        thinking(
            Some(High),
            Some(Low),
            &config(vec![
                "thinking=low".into(),
                "models.\"openai/gpt-5.6\".thinking=low".into()
            ]),
            &model,
            reference,
        )
        .unwrap(),
        Some(High)
    );
    // Session beats config and the default.
    assert_eq!(
        thinking(Some(High), None, &empty(), &model, reference).unwrap(),
        Some(High)
    );
    // Per-model beats the top level.
    assert_eq!(
        thinking(
            None,
            None,
            &config(vec![
                "thinking=low".into(),
                "models.\"openai/gpt-5.6\".thinking=high".into()
            ]),
            &model,
            reference,
        )
        .unwrap(),
        Some(High)
    );
    // Top level beats the model default.
    assert_eq!(
        thinking(None, None, &config(vec!["thinking=high".into()]), &model, reference).unwrap(),
        Some(High)
    );
    // Nothing configured falls back to the model default.
    assert_eq!(
        thinking(None, None, &empty(), &model, reference).unwrap(),
        Some(Low)
    );
}

#[test]
fn per_model_thinking_applies_only_to_that_model() {
    let model = model();
    let config = config(vec!["models.\"other/model\".thinking=high".into()]);
    assert_eq!(
        thinking(None, None, &config, &model, "openai/gpt-5.6").unwrap(),
        Some(ThinkingLevel::Low)
    );
}

#[test]
fn a_level_the_model_does_not_take_is_invalid_arguments() {
    let model = model();
    let err = thinking(
        Some(ThinkingLevel::Max),
        None,
        &empty(),
        &model,
        "openai/gpt-5.6",
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArguments);
    assert!(
        err.message.contains("max")
            && err.message.contains("openai/gpt-5.6")
            && err.message.contains("low")
            && err.message.contains("high"),
        "{}",
        err.message
    );
}

#[test]
fn a_configured_level_the_model_does_not_take_is_invalid_arguments() {
    let model = model();
    let err = thinking(
        None,
        None,
        &config(vec!["thinking=off".into()]),
        &model,
        "openai/gpt-5.6",
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArguments);
}

#[test]
fn a_model_with_no_levels_resolves_to_none_but_rejects_any_level() {
    let model = model_without_levels();
    assert_eq!(
        thinking(None, None, &empty(), &model, "openai/mini").unwrap(),
        None
    );
    let err = thinking(
        Some(ThinkingLevel::Low),
        None,
        &empty(),
        &model,
        "openai/mini",
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArguments);
}
