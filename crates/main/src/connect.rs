//! The provider a model reaches (`docs/model-routing.md`, "Protocols and
//! providers").

use std::path::PathBuf;
use std::sync::Arc;

use config::Protocol;
use contract::ErrorCode;
use contract::Secret;
use contract::clock::Clock;
use contract::provider::Provider;
use contract::shapes::Failure;
use doors::failure;
use extensions::LuaProvider;
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;
use provider::scripted::{Script, ScriptError, Scripted};
use provider::{Compat, Endpoint};
use serde_json::Value;

/// Whether this Fiber speaks `protocol`, checked before anything is read
/// for the model `reference`: `bedrock-converse` fails as [`connect`] does.
pub(crate) fn speaks(protocol: Protocol, reference: &str) -> Result<(), Failure> {
    match protocol {
        Protocol::BedrockConverse => Err(unspoken(reference)),
        Protocol::OpenaiResponses
        | Protocol::OpenaiCompletions
        | Protocol::AnthropicMessages
        | Protocol::GoogleGenerativeAi
        | Protocol::Scripted => Ok(()),
    }
}

/// The model `reference` speaks a protocol this Fiber does not speak.
fn unspoken(reference: &str) -> Failure {
    failure(
        ErrorCode::ProtocolUnsupported,
        format!(
            "The model `{reference}` speaks a protocol this Fiber does not speak yet; pick another model."
        ),
    )
}

/// Where a session runs: the workspace a `scripted` model's path resolves
/// against, and the clock its pauses wait on (`docs/model-routing.md`, "The
/// scripted provider").
pub(crate) struct Here {
    /// The session's workspace.
    pub(crate) workspace: PathBuf,
    /// The session's clock.
    pub(crate) clock: Arc<dyn Clock>,
}

/// The `scripted` protocol for the script at `id`, resolved against the
/// workspace (an absolute path stands) and read whole, once: a file that
/// cannot be read is `io_failed`, a malformed one `config_invalid`.
fn scripted(id: &str, here: &Here) -> Result<Arc<dyn Provider>, Failure> {
    let path = here.workspace.join(id);
    let script = Script::read(&path).map_err(|e| {
        let code = match &e {
            ScriptError::Unreadable { .. } => ErrorCode::IoFailed,
            ScriptError::Malformed { .. } => ErrorCode::ConfigInvalid,
        };
        failure(code, e.to_string())
    })?;
    Ok(Arc::new(Scripted::new(
        path,
        script,
        Arc::clone(&here.clock),
    )))
}

/// The provider a model reaches: the endpoint and protocol construction the
/// session's model and the reviewer's share. A reviewer failure never falls
/// back to the session's model (`docs/permissions.md`, "How it runs").
/// `key` is `None` when a Lua `credential()` supplies the token, which
/// rides the signing seam instead (`docs/model-routing.md`, "Keys, tokens
/// and OAuth"). `lua` is the model's provider's Lua, when it has one: a
/// provider whose package declares `cost()` carries it as its lookup, bound
/// to this model's base URL and `key` (`docs/model-routing.md`, "Cost"). A
/// package that fails to start gets no lookup; the calls' costs stay as
/// first recorded. `here` is where a `scripted` model's script is read.
pub(crate) fn connect(
    model: extensions::Model<'_>,
    key: Option<Secret>,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    lua: Option<&Arc<LuaProvider>>,
    here: &Here,
) -> Result<Arc<dyn Provider>, Failure> {
    let lookup_key = key.clone();
    let endpoint = Endpoint {
        provider: model.provider.name.clone(),
        model: model.model.id.clone(),
        base_url: model.model.base_url.clone(),
        key,
        headers: model
            .provider
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        signer,
        compat: Compat::from_data(&model.model.compat),
        max_output_tokens: model.model.max_output_tokens,
        extra_body: model.model.extra_body.clone(),
        text_only: !model.model.input.iter().any(|kind| kind == "image"),
        direct: false,
    };
    let cache_key_header = model
        .model
        .compat
        .get("cache_key_header")
        .and_then(Value::as_str);
    let provider: Arc<dyn Provider> = match model.model.protocol {
        Protocol::OpenaiResponses => {
            let responses = Responses::new(endpoint);
            Arc::new(match cache_key_header {
                Some(name) => responses.cache_key_header(name),
                None => responses,
            })
        }
        Protocol::OpenaiCompletions => {
            let completions = Completions::new(endpoint);
            Arc::new(match cache_key_header {
                Some(name) => completions.cache_key_header(name),
                None => completions,
            })
        }
        Protocol::AnthropicMessages => {
            let messages = Messages::new(endpoint);
            Arc::new(match cache_key_header {
                Some(name) => messages.cache_key_header(name),
                None => messages,
            })
        }
        Protocol::GoogleGenerativeAi => {
            let gemini = Gemini::new(endpoint);
            Arc::new(match cache_key_header {
                Some(name) => gemini.cache_key_header(name),
                None => gemini,
            })
        }
        Protocol::BedrockConverse => return Err(unspoken(&model.reference())),
        Protocol::Scripted => scripted(&model.model.id, here)?,
    };
    Ok(match lua {
        Some(lua) => lua
            .costed(Arc::clone(&provider), &model.model.base_url, lookup_key)
            .unwrap_or(provider),
        None => provider,
    })
}
