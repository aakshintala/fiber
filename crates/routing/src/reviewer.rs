//! Who judges step 7's calls (`docs/permissions.md`, "How it runs"): the
//! reviewer's model reference, its resolution against the registry, and
//! its provider. A reviewer failure is not a startup error: the loop gets
//! it, and every reviewed call escalates it. There is no automatic
//! fallback to the session's own model: only an explicit pick may name it.

use std::sync::Arc;

use config::{Config, ProviderData};
use contract::diag::Observe;
use contract::provider::Provider;
use contract::shapes::Failure;
use extensions::Providers;

use crate::connect::{Diagnostics, Here, connect, connect_observed};
use crate::lua_providers::Access;
use crate::{failed, failure};

/// The reviewer model's reference: `reviewer.model` when set, else the
/// session provider's own `providers."<name>".reviewer_model`, else the
/// package's `reviewer_model`, else `None`. The provider lookup reads the
/// effective layers, so a project value wins over the global one and a
/// repository value is ignored; the stored ID is local and gains the
/// session provider's prefix once.
pub fn reference(config: &Config, session: &extensions::Model<'_>) -> Option<String> {
    if let Some(typed) = config
        .get("reviewer.model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
    {
        return Some(typed);
    }
    // Segments, never dotted text: a provider name holding a dot stays
    // one segment.
    let local = [
        "providers".to_owned(),
        session.provider.name.clone(),
        "reviewer_model".to_owned(),
    ];
    if let Some((value, _)) = config.get_segments(&local, None)
        && let Some(id) = value.as_str()
    {
        return Some(format!("{}/{}", session.provider.name, id));
    }
    session
        .provider
        .reviewer_model
        .as_ref()
        .map(|id| format!("{}/{}", session.provider.name, id))
}

/// The reviewer's model: [`reference`] resolved against `providers`, or
/// `None` when nothing names one. A reference that does not resolve is the
/// registry's own failure.
pub fn model<'a>(
    providers: &'a Providers,
    config: &Config,
    session: &extensions::Model<'_>,
) -> Result<Option<extensions::Model<'a>>, Failure> {
    reference(config, session)
        .map(|typed| providers.resolve(&typed).map_err(|e| failed(e.code(), e)))
        .transpose()
}

/// A chosen reviewer: its model and the provider built for it.
pub struct Chosen<'a> {
    /// The reviewer's model.
    pub model: extensions::Model<'a>,
    /// The provider the reviewer's calls go through.
    pub provider: Arc<dyn Provider>,
    /// The observed factory, when the reviewer is native and `diag` is
    /// at debug.
    pub observe: Option<Observe>,
    /// The model's context window, in tokens.
    pub context_window: u64,
}

/// The model's declared context window: a model that declares none, or a
/// zero window, is `no_model` before any credential is read or any
/// connection made.
fn context_window(model: &extensions::Model<'_>) -> Result<u64, Failure> {
    extensions::context_window(model.model, &model.reference())
        .map_err(|error| failure(error.code(), error))
}

/// Chooses the reviewer: `None` when nothing names one. The steps run in
/// order and each failure ends the choice: the model, its context window,
/// its credential, its connection. `credential` reads the startup map
/// first, then the provider's configured label, so a reviewer on the
/// session's own provider reuses the session's key and signer, and a
/// `command` credential runs once per process; a switch passes the access
/// its read returned (`docs/model-routing.md`, "Keys, tokens and OAuth").
/// It returns the label beside the access, so the reviewer's quota is
/// fetched for the account the lookup read (`docs/tools.md`, "Provider
/// quota"). The token is read once, so a failing `credential()` fails here.
pub fn choose<'a>(
    providers: &'a Providers,
    config: &Config,
    session: &extensions::Model<'_>,
    here: &Here,
    credential: &mut impl FnMut(&ProviderData) -> Result<(String, Access), Failure>,
    diag: Option<&Diagnostics>,
) -> Result<Option<Chosen<'a>>, Failure> {
    let Some(model) = model(providers, config, session)? else {
        return Ok(None);
    };
    let context_window = context_window(&model)?;
    // Another provider's reviewer goes through the same path, so a Lua
    // reviewer model works: the token when it registered `credential`,
    // else the key.
    let (label, access) = credential(model.provider)?;
    let (provider, observe) = match diag {
        Some(diag) => connect_observed(
            model,
            access.key,
            access.signer,
            access.lua.as_ref().map(|lua| (lua, label.as_str())),
            here,
            diag,
        )?,
        None => (
            connect(
                model,
                access.key,
                access.signer,
                access.lua.as_ref().map(|lua| (lua, label.as_str())),
                here,
            )?,
            None,
        ),
    };
    Ok(Some(Chosen {
        model,
        provider,
        observe,
        context_window,
    }))
}

#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod tests;
