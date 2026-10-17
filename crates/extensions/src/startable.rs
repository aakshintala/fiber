//! Whether a model can start, shared by the session and `fiber doctor`:
//! one message constructor each, so neither copy drifts. The session
//! turns them into failures; doctor prints them as blockers.

use config::{ModelData, Protocol};
use contract::ThinkingLevel;

/// The model's declared context window, in tokens: a model that declares
/// none — or a zero window — cannot start.
pub fn context_window(model: &ModelData, reference: &str) -> Result<u64, String> {
    match model.context_window {
        Some(window) if window > 0 => Ok(window),
        _ => Err(format!(
            "The model `{reference}` declares no `context_window`."
        )),
    }
}

/// The refusal when `level` is not one the model takes, else `None`: a
/// level the model does not take never starts.
pub fn thinking_refusal(
    level: ThinkingLevel,
    model: &ModelData,
    reference: &str,
) -> Option<String> {
    if model.thinking_levels.contains(&level) {
        None
    } else {
        Some(format!(
            "The thinking level `{level}` is not one model `{reference}` takes: it {}.",
            takes(model),
        ))
    }
}

/// The refusal when this Fiber speaks no `protocol`, else `None`: only
/// `bedrock-converse` is refused, before anything is read for the model.
pub fn protocol_refusal(protocol: Protocol, reference: &str) -> Option<String> {
    match protocol {
        Protocol::BedrockConverse => Some(format!(
            "The model `{reference}` speaks a protocol this Fiber does not speak yet; pick another model."
        )),
        Protocol::OpenaiResponses
        | Protocol::OpenaiCompletions
        | Protocol::AnthropicMessages
        | Protocol::GoogleGenerativeAi
        | Protocol::Scripted => None,
    }
}

/// What a model takes, for a message: "takes `low`, `high`" or "takes no
/// thinking level".
pub fn takes(model: &ModelData) -> String {
    let levels = model
        .thinking_levels
        .iter()
        .map(|level| format!("`{level}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if levels.is_empty() {
        "takes no thinking level".to_owned()
    } else {
        format!("takes {levels}")
    }
}
