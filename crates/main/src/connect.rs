//! The provider a model reaches (`docs/model-routing.md`, "Protocols and
//! providers").

use std::path::PathBuf;
use std::sync::Arc;

use config::Protocol;
use contract::ErrorCode;
use contract::Secret;
use contract::clock::Clock;
use contract::diag::{DebugLog, Observe};
use contract::provider::{ModelCall, ModelRequest, Provider};
use contract::shapes::Failure;
use doors::failure;
use extensions::{CredentialPair, LuaProvider};
use provider::anthropic_messages::Messages;
use provider::google_generative_ai::Gemini;
use provider::openai_completions::Completions;
use provider::openai_responses::Responses;
use provider::scripted::{Script, ScriptError, Scripted};
use provider::{Compat, Endpoint, RequestObservation};
use serde_json::Value;

/// Whether this Fiber speaks `protocol`, checked before anything is read
/// for the model `reference`: `bedrock-converse` fails as [`connect`] does.
pub(crate) fn speaks(protocol: Protocol, reference: &str) -> Result<(), Failure> {
    match extensions::protocol_refusal(protocol, reference) {
        Some(message) => Err(failure(ErrorCode::ProtocolUnsupported, message)),
        None => Ok(()),
    }
}

/// The model `reference` speaks a protocol this Fiber does not speak.
fn unspoken(reference: &str) -> Failure {
    match extensions::protocol_refusal(Protocol::BedrockConverse, reference) {
        Some(message) => failure(ErrorCode::ProtocolUnsupported, message),
        // Unreachable: the shared check refuses `bedrock-converse`.
        None => failure(
            ErrorCode::ProtocolUnsupported,
            format!("The model `{reference}` speaks bedrock-converse."),
        ),
    }
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
/// and OAuth"). `lua` is the model's provider's Lua and the session's
/// credential label, when it has one: a provider whose package declares
/// `cost()` carries it as its lookup, bound to this model's base URL and
/// `key` (`docs/model-routing.md`, "Cost"), and one whose package declares
/// `quota()` fetches that label's quota off the request path on every call,
/// with the label's key (`docs/tools.md`, "Provider quota"). A package that fails to start gets
/// neither; the calls' costs stay as first recorded. `here` is where a
/// `scripted` model's script is read.
pub(crate) fn connect(
    model: extensions::Model<'_>,
    key: Option<Secret>,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    lua: Option<(&Arc<LuaProvider>, &str)>,
    here: &Here,
) -> Result<Arc<dyn Provider>, Failure> {
    Ok(build(model, key, signer, lua, here, None)?.provider)
}

/// As [`connect`], also building the session's observed factory over the
/// exact configured native protocol instance: `diag`'s writer and the
/// session's clock ride each attempt. At `info` the factory is `None` and
/// the provider is built exactly as [`connect`] builds it. A `scripted`
/// model produces no factory.
#[allow(
    clippy::too_many_arguments,
    reason = "one construction with its observation writer beside the existing inputs"
)]
pub(crate) fn connect_observed(
    model: extensions::Model<'_>,
    key: Option<Secret>,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    lua: Option<(&Arc<LuaProvider>, &str)>,
    here: &Here,
    diag: &Arc<log::diag::Diag>,
) -> Result<(Arc<dyn Provider>, Option<Observe>), Failure> {
    let built = build(model, key, signer, lua, here, Some(diag))?;
    Ok((built.provider, built.observe))
}

/// What one construction builds: the provider and its observed factory.
struct Built {
    provider: Arc<dyn Provider>,
    observe: Option<Observe>,
}

/// Builds one factory over the configured `protocol` instance: its own
/// `call_observed` with the writer and clock bound, so every attempt
/// records that route. `None` at `info`: the call then runs exactly as an
/// unobserved one does, with no model cloned and no clock read.
fn observed<P>(
    diag: &Arc<log::diag::Diag>,
    clock: &Arc<dyn Clock>,
    protocol: P,
    call: fn(&P, &ModelRequest, RequestObservation) -> Box<dyn ModelCall>,
) -> Option<Observe>
where
    P: Clone + Send + Sync + 'static,
{
    if !diag.debugging() {
        return None;
    }
    let log = Arc::clone(diag) as Arc<dyn DebugLog>;
    let clock = Arc::clone(clock);
    // The connector timing rides each attempt beside the writer and the
    // clock: one `net.connect` scope per actual attempt group, obtained
    // only at debug before the writer is erased into the observation.
    let timings = diag.timings();
    Some(Arc::new(
        move |request: &ModelRequest, attempt: std::num::NonZeroU32| {
            call(
                &protocol,
                request,
                RequestObservation::new(Arc::clone(&log), Arc::clone(&clock), attempt)
                    .with_timing(Arc::clone(&timings)),
            )
        },
    ))
}
/// One construction through one implementation: the endpoint and protocol
/// the session's model and the reviewer's share, with the observed factory
/// over the exact configured native instance. `None` builds exactly as
/// [`connect`] does, with no factory and no sink.
#[allow(
    clippy::too_many_arguments,
    reason = "one construction with its optional observation writer beside the existing inputs"
)]
fn build(
    model: extensions::Model<'_>,
    key: Option<Secret>,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    lua: Option<(&Arc<LuaProvider>, &str)>,
    here: &Here,
    diag: Option<&Arc<log::diag::Diag>>,
) -> Result<Built, Failure> {
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
        limits: net::LIMITS,
    };
    let cache_key_header = model
        .model
        .compat
        .get("cache_key_header")
        .and_then(Value::as_str);
    // One arm per protocol: the erased provider with its factory over the
    // same configured instance, after the cache-key-header configuration.
    // A scripted model produces no factory.
    let (provider, observe): (Arc<dyn Provider>, Option<Observe>) = match model.model.protocol {
        Protocol::OpenaiResponses => {
            let responses = match cache_key_header {
                Some(name) => Responses::new(endpoint).cache_key_header(name),
                None => Responses::new(endpoint),
            };
            let observe = diag.and_then(|diag| {
                observed(
                    diag,
                    &here.clock,
                    responses.clone(),
                    Responses::call_observed,
                )
            });
            (Arc::new(responses), observe)
        }
        Protocol::OpenaiCompletions => {
            let completions = match cache_key_header {
                Some(name) => Completions::new(endpoint).cache_key_header(name),
                None => Completions::new(endpoint),
            };
            let observe = diag.and_then(|diag| {
                observed(
                    diag,
                    &here.clock,
                    completions.clone(),
                    Completions::call_observed,
                )
            });
            (Arc::new(completions), observe)
        }
        Protocol::AnthropicMessages => {
            let messages = match cache_key_header {
                Some(name) => Messages::new(endpoint).cache_key_header(name),
                None => Messages::new(endpoint),
            };
            let observe = diag.and_then(|diag| {
                observed(diag, &here.clock, messages.clone(), Messages::call_observed)
            });
            (Arc::new(messages), observe)
        }
        Protocol::GoogleGenerativeAi => {
            let gemini = match cache_key_header {
                Some(name) => Gemini::new(endpoint).cache_key_header(name),
                None => Gemini::new(endpoint),
            };
            let observe = diag.and_then(|diag| {
                observed(diag, &here.clock, gemini.clone(), Gemini::call_observed)
            });
            (Arc::new(gemini), observe)
        }
        Protocol::BedrockConverse => return Err(unspoken(&model.reference())),
        Protocol::Scripted => (scripted(&model.model.id, here)?, None),
    };
    // The quota tail mirrors [`connect`]: the costed provider carries the
    // lookup, and a package that fails to start leaves the native provider
    // with no factory, as the ordinary fallback leaves it.
    let (provider, observe) = match lua {
        Some((lua, label)) => {
            let costed = lua
                .costed(
                    Arc::clone(&provider),
                    &model.model.base_url,
                    lookup_key.clone(),
                )
                .unwrap_or(Arc::clone(&provider));
            let pair = CredentialPair::for_provider(model.provider, label);
            match observe {
                Some(observe) => {
                    match lua.with_quota_observed(costed, Some(observe), pair, lookup_key) {
                        Ok(built) => built,
                        Err(_) => (provider, None),
                    }
                }
                None => (
                    lua.with_quota(costed, pair, lookup_key).unwrap_or(provider),
                    None,
                ),
            }
        }
        None => (provider, observe),
    };
    Ok(Built { provider, observe })
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
