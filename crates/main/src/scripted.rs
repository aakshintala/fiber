//! The built-in `scripted` provider in a session (`docs/model-routing.md`,
//! "The scripted provider"): the session's registry gains a scripted model
//! only for a reference that names it, and a scripted model reads no
//! credential and never warms, since it has no credential, quota, cost or
//! prompt cache.

use config::{Config, ModelData, Protocol, ProviderData};
use contract::shapes::Failure;
use extensions::Providers;
use serde_json::Value;

use crate::lua_providers::Access;

#[cfg(test)]
#[path = "scripted_tests.rs"]
mod tests;

/// Why a switch to a scripted model the session did not start with is
/// rejected: the registry a session switches within is fixed at start.
pub(crate) const START_ONLY: &str =
    "Choose a scripted model when the session starts: `fiber ask --model scripted/<path>`.";

/// Adds to `providers` each scripted model the session may name: the
/// configured `model` (which `--model` sets), the resumed session's
/// `recorded` model and `reviewer.model`. Any other reference adds nothing.
pub(crate) fn prepare(providers: &mut Providers, config: &Config, recorded: Option<&str>) {
    for key in ["model", "reviewer.model"] {
        if let Some((Value::String(typed), _)) = config.get(key, None) {
            providers.add_scripted(&typed);
        }
    }
    if let Some(typed) = recorded {
        providers.add_scripted(typed);
    }
}

/// Whether `provider` serves scripted models.
pub(crate) fn is_scripted(provider: &ProviderData) -> bool {
    provider
        .models
        .iter()
        .any(|model| model.protocol == Protocol::Scripted)
}

/// `provider`'s access: none for a scripted provider, which reads no
/// credential and runs no Lua, else what `read` returns.
pub(crate) fn access(
    provider: &ProviderData,
    read: impl FnOnce() -> Result<Access, Failure>,
) -> Result<Access, Failure> {
    if is_scripted(provider) {
        return Ok(Access {
            key: None,
            signer: None,
            lua: None,
        });
    }
    read()
}

/// The configured warming for `model`: none for a scripted model, which has
/// no prompt cache to warm, and whose warm call would take a step.
pub(crate) fn warm(model: &ModelData, warm: Option<u32>) -> Option<u32> {
    warm.filter(|_| model.protocol != Protocol::Scripted)
}
