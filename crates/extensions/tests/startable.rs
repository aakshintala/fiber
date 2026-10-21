//! Whether a model can start, through the shared message constructors:
//! the window a model declares and the thinking suffix it takes.

use config::{ModelData, Protocol};
use contract::ThinkingLevel;
use extensions::{context_window, takes, thinking_refusal};

/// A model with these window, levels and protocol, and default fields
/// otherwise.
fn model(window: Option<u64>, levels: &[ThinkingLevel], protocol: Protocol) -> ModelData {
    ModelData {
        id: "m".to_owned(),
        name: None,
        protocol,
        base_url: "http://127.0.0.1:1/v1".to_owned(),
        compat: serde_json::Map::new(),
        deferred_tools: false,
        extra_body: serde_json::Map::new(),
        context_window: window,
        max_output_tokens: None,
        input: Vec::new(),
        cost: None,
        subscription: false,
        web_search: None,
        thinking_levels: levels.to_vec(),
        thinking_default: None,
        prompt_addendum: None,
    }
}

#[test]
fn a_missing_or_zero_window_is_refused_and_a_positive_one_passes() {
    for window in [None, Some(0)] {
        let Err(refusal) = context_window(&model(window, &[], Protocol::OpenaiResponses), "fake/m")
        else {
            panic!("a missing window resolves");
        };
        assert_eq!(refusal.code(), contract::ErrorCode::NoModel);
        assert_eq!(
            refusal.to_string(),
            "The model `fake/m` declares no `context_window`."
        );
    }
    assert_eq!(
        context_window(&model(Some(1000), &[], Protocol::OpenaiResponses), "fake/m").unwrap(),
        1000
    );
}

#[test]
fn a_taken_suffix_passes_and_an_untaken_one_is_refused() {
    let taken = model(
        Some(1000),
        &[ThinkingLevel::Low, ThinkingLevel::High],
        Protocol::OpenaiResponses,
    );
    assert_eq!(thinking_refusal(ThinkingLevel::Low, &taken, "fake/m"), None);
    assert_eq!(
        thinking_refusal(ThinkingLevel::Medium, &taken, "fake/m"),
        Some(
            "The thinking level `medium` is not one model `fake/m` takes: it takes `low`, `high`."
                .to_owned()
        )
    );
}

#[test]
fn a_model_with_no_levels_refuses_every_suffix() {
    let none = model(Some(1000), &[], Protocol::OpenaiResponses);
    assert_eq!(
        thinking_refusal(ThinkingLevel::Low, &none, "fake/m"),
        Some(
            "The thinking level `low` is not one model `fake/m` takes: it takes no thinking level."
                .to_owned()
        )
    );
    assert_eq!(takes(&none), "takes no thinking level");
    let taken = model(
        Some(1000),
        &[ThinkingLevel::Low, ThinkingLevel::High],
        Protocol::OpenaiResponses,
    );
    assert_eq!(takes(&taken), "takes `low`, `high`");
}
