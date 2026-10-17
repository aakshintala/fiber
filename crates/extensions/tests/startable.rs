//! Whether a model can start, through the shared message constructors:
//! the window a model declares, the thinking suffix it takes, and the
//! protocols this Fiber speaks.

use config::{ModelData, Protocol};
use contract::ThinkingLevel;
use extensions::{context_window, protocol_refusal, takes, thinking_refusal};

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
        let refusal = context_window(&model(window, &[], Protocol::OpenaiResponses), "fake/m");
        assert_eq!(
            refusal,
            Err("The model `fake/m` declares no `context_window`.".to_owned())
        );
    }
    assert_eq!(
        context_window(&model(Some(1000), &[], Protocol::OpenaiResponses), "fake/m"),
        Ok(1000)
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

#[test]
fn only_bedrock_converse_is_refused_and_scripted_passes() {
    for protocol in [
        Protocol::OpenaiResponses,
        Protocol::OpenaiCompletions,
        Protocol::AnthropicMessages,
        Protocol::GoogleGenerativeAi,
        Protocol::Scripted,
    ] {
        assert_eq!(protocol_refusal(protocol, "fake/m"), None);
    }
    assert_eq!(
        protocol_refusal(Protocol::BedrockConverse, "fake/m"),
        Some(
            "The model `fake/m` speaks a protocol this Fiber does not speak yet; pick another model."
                .to_owned()
        )
    );
}
