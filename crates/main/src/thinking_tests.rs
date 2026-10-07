//! The thinking resolution's precedence (`docs/model-routing.md`,
//! "Thinking"): each source beats every later one, and a level the model
//! does not take fails before any request.

use super::thinking as resolve;
use config::ModelData;
use contract::events::Notice;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use serde_json::json;

fn model() -> ModelData {
    serde_json::from_value(json!({
        "id": "gpt-5.6",
        "protocol": "openai-responses",
        "base_url": "https://x/v1", "context_window": 1000,
        "thinking_levels": ["low", "high"],
        "thinking_default": "low",
    }))
    .unwrap()
}

fn model_without_levels() -> ModelData {
    serde_json::from_value(json!({
        "id": "mini",
        "protocol": "openai-responses",
        "base_url": "https://x/v1", "context_window": 1000,
    }))
    .unwrap()
}

fn config(overrides: Vec<String>) -> config::Config {
    config_with_global(None, overrides)
}

fn config_with_global(global: Option<serde_json::Value>, overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-thinking");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    if let Some(global) = global {
        std::fs::write(home.join("config.json"), global.to_string()).unwrap();
    }
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

fn thinking(
    suffix: Option<ThinkingLevel>,
    session: Option<ThinkingLevel>,
    config: &config::Config,
    model: &ModelData,
    reference: &str,
) -> Result<Option<ThinkingLevel>, Failure> {
    resolve(suffix, session, config, model, reference, &mut Vec::new())
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
        thinking(
            None,
            None,
            &config(vec!["thinking=high".into()]),
            &model,
            reference
        )
        .unwrap(),
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

fn resolved(
    config: &config::Config,
    model: &ModelData,
    reference: &str,
) -> (Option<ThinkingLevel>, Vec<Notice>) {
    let mut notices = Vec::new();
    let level = resolve(None, None, config, model, reference, &mut notices).unwrap();
    (level, notices)
}

#[test]
fn a_top_level_level_the_model_does_not_declare_is_ignored_with_a_notice() {
    let (level, notices) = resolved(
        &config(vec!["thinking=high".into()]),
        &model_without_levels(),
        "openai/mini",
    );
    assert_eq!(level, None);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ConfigKeyIgnored);
    for part in ["`thinking`", "`high`", "`openai/mini`"] {
        assert!(notices[0].message.contains(part), "{}", notices[0].message);
    }
}

#[test]
fn a_per_model_level_the_model_does_not_declare_falls_back_to_its_default() {
    let (level, notices) = resolved(
        &config(vec!["models.\"openai/gpt-5.6\".thinking=max".into()]),
        &model(),
        "openai/gpt-5.6",
    );
    assert_eq!(level, Some(ThinkingLevel::Low));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ConfigKeyIgnored);
    for part in [
        "models.\"openai/gpt-5.6\".thinking",
        "`max`",
        "`openai/gpt-5.6`",
    ] {
        assert!(notices[0].message.contains(part), "{}", notices[0].message);
    }
}

#[test]
fn the_notice_names_the_key_of_the_winning_layer() {
    let (level, notices) = resolved(
        &config_with_global(
            Some(json!({"models": {"openai/gpt-5.6": {"thinking": "low"}}})),
            vec!["thinking=max".into()],
        ),
        &model(),
        "openai/gpt-5.6",
    );
    assert_eq!(level, Some(ThinkingLevel::Low));
    assert_eq!(notices.len(), 1, "{notices:?}");
    let message = &notices[0].message;
    assert!(message.contains("`thinking` setting `max`"), "{message}");
    assert!(!message.contains("models."), "{message}");
}

#[test]
fn a_declared_configured_level_logs_no_notice() {
    let (level, notices) = resolved(
        &config(vec!["thinking=high".into()]),
        &model(),
        "openai/gpt-5.6",
    );
    assert_eq!(level, Some(ThinkingLevel::High));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn an_asked_for_level_beside_an_undeclared_configured_one_logs_no_notice() {
    let mut notices = Vec::new();
    let level = resolve(
        Some(ThinkingLevel::High),
        None,
        &config(vec!["thinking=max".into()]),
        &model(),
        "openai/gpt-5.6",
        &mut notices,
    )
    .unwrap();
    assert_eq!(level, Some(ThinkingLevel::High));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn a_session_choice_the_model_does_not_declare_is_invalid_arguments() {
    let err = thinking(
        None,
        Some(ThinkingLevel::Max),
        &config(vec!["thinking=high".into()]),
        &model(),
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
