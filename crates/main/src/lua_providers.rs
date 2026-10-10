//! A Lua provider's models and credential in a session
//! (`docs/model-routing.md`, "Model discovery", "Signing a request" and
//! "Keys, tokens and OAuth"): merging each registered provider's `models()`
//! into the installed providers, and sending `credential()`'s token on every
//! request through the signing seam.

use std::sync::Arc;

use config::{Config, ProviderData, Secret};
use contract::events::Notice;
use contract::shapes::Failure;
use contract::signing::Signer;
use extensions::{CredentialPair, LuaProvider, Providers, SessionExtensions};
use provider::redact::Secrets;

use crate::failed;

/// The key a model's requests carry, or nothing when a Lua `credential()`
/// supplies the token, which rides the signing seam instead; and what signs
/// each request, `Some` when the provider registered `credential` or `sign`.
pub(crate) type KeyAndSigner = (Option<Secret>, Option<Arc<dyn Signer>>);

/// What a session reaches one provider with: the key or signer
/// [`session_credential`] gave, and the Lua provider that signs its
/// requests and looks up its costs, if one registered it.
#[derive(Clone)]
pub(crate) struct Access {
    pub(crate) key: Option<Secret>,
    pub(crate) signer: Option<Arc<dyn Signer>>,
    pub(crate) lua: Option<Arc<LuaProvider>>,
}

impl Access {
    /// The access a read `credential` and the provider's `lua` give.
    pub(crate) fn new(lua: Option<&Arc<LuaProvider>>, (key, signer): KeyAndSigner) -> Self {
        Self {
            key,
            signer,
            lua: lua.cloned(),
        }
    }
}

/// What `add_lua` returns: every model as `(provider, id)` pairs, and
/// the discovery and placeholder notices for the session's startup lines.
pub(crate) type AddedLua = (Vec<(String, String)>, Vec<Notice>);

/// Merges every Lua provider's models into `providers`
/// (`docs/model-routing.md`, "Model discovery"): with no cached copy
/// `models()` runs synchronously once at startup for a provider with a
/// credential, never on a request; a stale cached list refreshes in the
/// background, once across the processes sharing a Fiber home. Background
/// threads are detached; the cache write is atomic, so an interrupted
/// refresh leaves the old copy. A refresh never touches `providers`, so a
/// running session's tool definitions never change. Then fills per-account
/// host placeholders, so a session chooses from filled URLs.
///
/// Returns every model of every provider as `(provider, id)` pairs, taken
/// after `add_lua` and before placeholders are filled, so a model left out
/// with `model_unconfigured` still counts toward ambiguity, alongside the
/// discovery and placeholder notices for the session's startup lines.
pub(crate) fn add_lua(
    extensions: &SessionExtensions,
    providers: &mut Providers,
    config: &Config,
) -> Result<AddedLua, Failure> {
    let mut notices = Vec::new();
    for (extension, provider) in extensions.lua_providers() {
        notices.extend(providers.add_lua(extension, provider, config));
    }
    let naming: Vec<(String, String)> = providers
        .names()
        .flat_map(|name| {
            providers
                .get(name)
                .map(|data| {
                    data.models
                        .iter()
                        .map(|model| (name.to_owned(), model.id.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect();
    notices.extend(
        providers
            .fill_placeholders(config, &|name| std::env::var(name).ok())
            .map_err(|e| failed(e.code(), e))?,
    );
    // Detached: nothing joins them, and `fiber ask` does not wait on them
    // to exit.
    let started = extensions::refresh_lists(
        &extensions
            .lua_providers()
            .iter()
            .map(|(_, provider)| Arc::clone(provider))
            .collect::<Vec<_>>(),
        providers,
        config,
        Some(config::refresh_after(config)),
    );
    let _detached = started;
    Ok((naming, notices))
}

/// The session's key and signer for `provider`, whose Lua provider is
/// `lua`: no key when it registered `credential`, so no key file is
/// needed, else the key `key` reads. The
/// token is read once, so a failing `credential()` fails here with its
/// code: before any session line for a session, and into the loop for a
/// reviewer (`docs/permissions.md`, "How it runs").
pub(crate) fn session_credential(
    lua: Option<&Arc<LuaProvider>>,
    provider: &ProviderData,
    label: &str,
    key: impl FnOnce() -> Result<Secret, Failure>,
) -> Result<KeyAndSigner, Failure> {
    let pair = CredentialPair::for_provider(provider, label);
    let key = match lua {
        Some(lua)
            if lua
                .registers("credential")
                .map_err(|e| failed(e.code(), e))? =>
        {
            lua.credential_token(&pair).map(|_| ()).map_err(|e| {
                provider::Error::Sign(e).failure(&provider.name, &Secrets::default())
            })?;
            None
        }
        _ => Some(key()?),
    };
    Ok((key, signer(lua, pair)?))
}

/// Signs `lua`'s requests; `Some` when it registered `credential` or `sign`.
pub(crate) fn signer(
    lua: Option<&Arc<LuaProvider>>,
    pair: CredentialPair,
) -> Result<Option<Arc<dyn Signer>>, Failure> {
    match lua {
        Some(lua) => lua.signer(pair).map_err(|e| failed(e.code(), e)),
        None => Ok(None),
    }
}

#[cfg(test)]
#[path = "lua_providers_tests.rs"]
mod tests;
