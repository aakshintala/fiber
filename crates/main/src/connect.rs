//! The provider a model reaches (`docs/model-routing.md`, "Protocols and
//! providers").

use std::sync::Arc;

use config::Protocol;
use contract::ErrorCode;
use contract::provider::Provider;
use contract::shapes::Failure;
use doors::failure;
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;
use provider::{Compat, Endpoint};
use serde_json::Value;

/// The provider a model reaches: the endpoint and protocol construction the
/// session's model and the reviewer's share. A reviewer failure never falls
/// back to the session's model (`docs/permissions.md`, "How it runs").
pub(crate) fn connect(
    model: extensions::Model<'_>,
    key: String,
) -> Result<Arc<dyn Provider>, Failure> {
    let endpoint = Endpoint {
        provider: model.provider.name.clone(),
        model: model.model.id.clone(),
        base_url: model.model.base_url.clone(),
        key: Some(key),
        headers: model
            .provider
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        compat: Compat::from_data(&model.model.compat),
        max_output_tokens: model.model.max_output_tokens,
        extra_body: model.model.extra_body.clone(),
        text_only: !model.model.input.iter().any(|kind| kind == "image"),
        direct: false,
    };
    Ok(match model.model.protocol {
        Protocol::OpenaiResponses => {
            let responses = Responses::new(endpoint);
            Arc::new(
                match model
                    .model
                    .compat
                    .get("cache_key_header")
                    .and_then(Value::as_str)
                {
                    Some(name) => responses.cache_key_header(name),
                    None => responses,
                },
            )
        }
        Protocol::OpenaiCompletions => Arc::new(Completions::new(endpoint)),
        Protocol::AnthropicMessages => Arc::new(Messages::new(endpoint)),
        Protocol::GoogleGenerativeAi => {
            let gemini = Gemini::new(endpoint);
            Arc::new(
                match model
                    .model
                    .compat
                    .get("cache_key_header")
                    .and_then(Value::as_str)
                {
                    Some(name) => gemini.cache_key_header(name),
                    None => gemini,
                },
            )
        }
        Protocol::BedrockConverse => {
            return Err(failure(
                ErrorCode::ProtocolUnsupported,
                format!(
                    "The model `{}` speaks a protocol this Fiber does not speak yet.",
                    model.reference()
                ),
            ));
        }
    })
}
