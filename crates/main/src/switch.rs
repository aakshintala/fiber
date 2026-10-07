//! A second model for the `model` driver command
//! (`docs/model-routing.md`, "Naming a model" and "Thinking"): the
//! stateless preparation `main` composes at startup and hands to
//! `Loop::switcher`, built from the same calls `parts_in` makes for the
//! startup model. Preparation reads no credential source, environment
//! variable or file, and runs no subprocess, Lua function or network
//! request: a provider whose credential was not read at startup rejects
//! with the credential sentence, and the switch is rejected.

use std::collections::BTreeMap;
use std::sync::Arc;

use config::{Config, ProviderData};
use contract::commands::ModelArgs;
use contract::inbox::Rejection;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use extensions::{LuaProvider, Providers};

use crate::lua_providers::KeyAndSigner;

/// One read credential, by provider name: its label, and its key and
/// signer.
/// debt: no credential source is read and no command runs for a switch;
/// only a credential read at startup switches (#1094). #1094 lifts it
/// after #649 merges.
pub(crate) type Credentials = BTreeMap<String, (String, KeyAndSigner)>;

/// What preparing a switch reads: the registry as the session's providers
/// plus every retained Lua provider's cached models, every model every
/// provider names before placeholders are filled, the configuration, and
/// the credentials read at startup. Immutable after `new`: the closure
/// holds it behind an `Arc` and no mutable state.
pub(crate) struct Switching {
    registry: Providers,
    naming: Vec<(String, String)>,
    config: Config,
    credentials: Credentials,
}

impl Switching {
    /// The registry `clone` is the `Providers::load` clone `parts_in`
    /// built before any `add_lua`: every retained Lua provider with a
    /// model cache adds its cached models, then placeholders fill as at
    /// startup, with the same environment reader. No refresh thread
    /// starts for the clone.
    /// debt: a Lua provider the session unloaded is not loaded again for
    /// a switch; only a cache written by startup discovery switches back
    /// (#1094). #1094 lifts it after #649 merges.
    pub(crate) fn new(
        mut registry: Providers,
        retained: &[(String, Arc<LuaProvider>)],
        naming: Vec<(String, String)>,
        config: Config,
        credentials: Credentials,
    ) -> Result<Self, Failure> {
        for (extension, provider) in retained {
            if provider.has_model_cache() {
                let _notices = registry.add_lua(extension, provider, &config);
            }
        }
        registry
            .fill_placeholders(&config, &|name| std::env::var(name).ok())
            .map_err(|error| crate::failed(error.code(), error))?;
        Ok(Self {
            registry,
            naming,
            config,
            credentials,
        })
    }

    /// The `Loop::switcher` closure: `prepare` over the shared `Switching`.
    pub(crate) fn closure(self) -> r#loop::Prepare {
        let shared = Arc::new(self);
        Arc::new(move |args, chosen| prepare(&shared, args, chosen))
    }
}

/// Prepares a switch without changing any state, from the same calls
/// `parts_in` makes for the startup model: resolving, the credential the
/// map holds, thinking, connecting, the re-chosen reviewer, the cache
/// lifetime, the handoff settings and the addendum. A rejection changes
/// nothing. The reviewer's failure is not a rejection: the loop gets it,
/// and every reviewed call escalates it (`docs/permissions.md`, "How it
/// runs").
pub(crate) fn prepare(
    switching: &Switching,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> Result<r#loop::Prepared, Rejection> {
    // An unparseable thinking level rejects before any lookup.
    let asked = match &args.thinking {
        Some(level) => Some(
            level
                .parse::<ThinkingLevel>()
                .map_err(|_| invalid(format!("unknown thinking level `{level}`")))?,
        ),
        None => None,
    };
    let in_map = |name: &str| switching.credentials.contains_key(name);
    let resolved = resolve(&switching.registry, &switching.naming, &in_map, &args.model)?;
    let reference = resolved.reference();
    let Some((label, (key, signer))) = switching
        .credentials
        .get(resolved.provider.name.as_str())
        .cloned()
    else {
        // `resolve` already refused a provider outside the map; this is
        // unreachable, and refuses the same way.
        return Err(limited(resolved.provider.name.as_str()));
    };
    let mut notices = Vec::new();
    let thinking = crate::settings::thinking(
        resolved.thinking,
        asked.or(chosen),
        &switching.config,
        resolved.model,
        &reference,
        &mut notices,
    )
    .map_err(|failure| Rejection {
        code: failure.code,
        message: failure.message,
    })?;
    let provider = crate::connect(resolved, key, signer).map_err(|failure| Rejection {
        code: ErrorCode::InvalidArguments,
        message: failure.message,
    })?;
    Ok(r#loop::Prepared {
        provider,
        model: r#loop::Model {
            reference: reference.clone(),
            cost: resolved.model.cost.clone().map(crate::cost::declared),
            subscription: resolved.model.subscription,
        },
        thinking,
        // The suffix first, then what was asked for now, then the
        // session's choice.
        chosen: resolved.thinking.or(asked).or(chosen),
        credential: Some(label),
        cache_lifetime: crate::settings::cache_lifetime(&switching.config, &reference),
        context_window: resolved.model.context_window,
        addendum: switching.registry.addendum(&resolved).map(str::to_owned),
        handoff: crate::handoff::handoff_settings(&switching.config, &reference),
        reviewer: reviewer_for(switching, &resolved),
        web_search: resolved.model.web_search.clone(),
        // At most one notice: a configured level the model lacks.
        notice: notices.into_iter().next(),
    })
}

/// Who judges step 7's calls under the new `session` model: the
/// reference `main` would choose at startup, resolved against the switch
/// registry with the map-only lookup. A `provider/model` reference whose
/// provider the map does not hold fails before any registry resolution,
/// so an unloaded Lua-only reviewer is the credential sentence; a bare
/// id goes through resolution, and its failure stands.
fn reviewer_for(
    switching: &Switching,
    session: &extensions::Model<'_>,
) -> Result<r#loop::Reviewer, Failure> {
    let reference = crate::reviewer_reference(&switching.config, session)?;
    if let Some((provider, _)) = reference.split_once('/')
        && !switching.credentials.contains_key(provider)
    {
        return Err(limit_failure(provider));
    }
    let mut lookup = |provider: &ProviderData| -> Result<(String, KeyAndSigner), Failure> {
        switching
            .credentials
            .get(&provider.name)
            .cloned()
            .ok_or_else(|| limit_failure(&provider.name))
    };
    crate::choose_reviewer(&switching.registry, &switching.config, session, &mut lookup)
}

/// The typed model against the switch registry and the naming list: an
/// exact `provider/model` resolves as at startup, while a bare id counts
/// every provider that names it, configured or not, loaded or not. A bare
/// id tries the full typed id first, so a literal id ending in a thinking
/// level matches before the suffix strips (`docs/model-routing.md`,
/// "Naming a model": the exact match comes first, since OpenRouter ids
/// contain colons); only when nothing names the full id does the stripped
/// id count. Every failure but the credential sentence is the resolution's
/// own message as `invalid_arguments`.
/// debt: only the session's and the reviewer's providers switch; any
/// other installed provider is the credential sentence (#1094). #1094
/// lifts it after #649 merges.
fn resolve<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    in_map: &dyn Fn(&str) -> bool,
    typed: &str,
) -> Result<extensions::Model<'a>, Rejection> {
    let (rest, _) = Providers::split_thinking(typed);
    if let Some((provider, _)) = rest.split_once('/') {
        return match registry.resolve(typed) {
            Ok(model) if in_map(model.provider.name.as_str()) => Ok(model),
            Ok(model) => Err(limited(model.provider.name.as_str())),
            Err(_) if !in_map(provider) => Err(limited(provider)),
            Err(error) => Err(invalid(error.to_string())),
        };
    }
    if typed != rest
        && let Some(prepared) = resolve_literal(registry, naming, in_map, typed)
    {
        return prepared;
    }
    let mut matches = named(naming, rest);
    match registry.resolve(typed) {
        Ok(model) => {
            let reference = model.reference();
            if !matches.contains(&reference) {
                matches.push(reference);
            }
            if matches.len() > 1 {
                return Err(ambiguous(rest, &mut matches));
            }
            if in_map(model.provider.name.as_str()) {
                Ok(model)
            } else {
                Err(limited(model.provider.name.as_str()))
            }
        }
        Err(error) => {
            if let extensions::Error::Ambiguous {
                matches: theirs, ..
            } = &error
            {
                for reference in theirs {
                    if !matches.contains(reference) {
                        matches.push(reference.clone());
                    }
                }
            }
            if matches.len() > 1 {
                return Err(ambiguous(rest, &mut matches));
            }
            if matches.len() == 1
                && let Some(first) = matches.first()
                && let Some((provider, _)) = first.split_once('/')
                && !in_map(provider)
            {
                return Err(limited(provider));
            }
            Err(invalid(error.to_string()))
        }
    }
}

/// The `provider/model` references the naming list holds for the bare id
/// `text`.
fn named(naming: &[(String, String)], text: &str) -> Vec<String> {
    naming
        .iter()
        .filter(|(_, id)| id == text)
        .map(|(provider, id)| format!("{provider}/{id}"))
        .collect()
}

/// A bare id that is also a literal model id ending in a thinking level:
/// the full typed id against the registry and the naming list, before the
/// suffix strips. `Some` when the full id names anything: the match, its
/// ambiguity, or its credential sentence. `None` when nothing names it,
/// and the stripped id counts instead; a single naming-only match for a
/// provider in the map falls through too, since the registry past
/// placeholders names no such literal.
fn resolve_literal<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    in_map: &dyn Fn(&str) -> bool,
    typed: &str,
) -> Option<Result<extensions::Model<'a>, Rejection>> {
    let mut matches = named(naming, typed);
    match registry.resolve(typed) {
        // The registry's own exact-first order tries the full id before
        // the suffix strips, so a model whose id is the full text is a
        // full-id match; anything else resolved through the suffix.
        Ok(model) if model.model.id == typed => {
            let reference = model.reference();
            if !matches.contains(&reference) {
                matches.push(reference);
            }
            if matches.len() > 1 {
                return Some(Err(ambiguous(typed, &mut matches)));
            }
            if in_map(model.provider.name.as_str()) {
                return Some(Ok(model));
            }
            return Some(Err(limited(model.provider.name.as_str())));
        }
        Err(extensions::Error::Ambiguous {
            id,
            matches: theirs,
        }) if id == typed => {
            for reference in theirs {
                if !matches.contains(&reference) {
                    matches.push(reference.clone());
                }
            }
            return Some(Err(ambiguous(typed, &mut matches)));
        }
        Ok(_) | Err(_) => {}
    }
    if matches.len() > 1 {
        return Some(Err(ambiguous(typed, &mut matches)));
    }
    if let Some(first) = matches.first()
        && let Some((provider, _)) = first.split_once('/')
        && !in_map(provider)
    {
        return Some(Err(limited(provider)));
    }
    None
}

/// The credential sentence: the credential for `provider` was not read
/// when this session started.
fn sentence(provider: &str) -> String {
    format!(
        "The credential for `{provider}` was not read when this session started; \
         start a session with `--model <ref>`."
    )
}

/// A rejected switch: the message with code `invalid_arguments`.
fn invalid(message: String) -> Rejection {
    Rejection {
        code: ErrorCode::InvalidArguments,
        message,
    }
}

/// A switch refused for its credential: the sentence as a rejection.
fn limited(provider: &str) -> Rejection {
    invalid(sentence(provider))
}

/// A reviewer refused for its credential: the sentence as a failure the
/// loop escalates.
fn limit_failure(provider: &str) -> Failure {
    Failure {
        code: ErrorCode::InvalidArguments,
        message: sentence(provider),
        retry_after_ms: None,
        provider: None,
    }
}

/// A bare id more than one provider names, in the shape `resolve` names
/// it.
fn ambiguous(id: &str, matches: &mut Vec<String>) -> Rejection {
    matches.sort();
    matches.dedup();
    invalid(format!(
        "The model `{id}` is offered by more than one provider: {}. \
         Name one as `provider/model`.",
        matches.join(", ")
    ))
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
