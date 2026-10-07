//! A Lua provider's models and credential in a session
//! (`docs/model-routing.md`, "Model discovery", "Signing a request" and
//! "Keys, tokens and OAuth"): merging each registered provider's `models()`
//! into the installed providers, and sending `credential()`'s token on every
//! request through the signing seam.

use std::sync::Arc;

use config::{Config, ProviderData, Secret};
use contract::shapes::Failure;
use contract::signing::Signer;
use extensions::{LuaProvider, Providers, SessionExtensions};
use provider::redact::Secrets;

use crate::failed;

/// The key a model's requests carry, or nothing when a Lua `credential()`
/// supplies the token, which rides the signing seam instead; and what signs
/// each request, `Some` when the provider registered `credential` or `sign`.
pub(crate) type KeyAndSigner = (Option<Secret>, Option<Arc<dyn Signer>>);

/// Merges every Lua provider's models into `providers`
/// (`docs/model-routing.md`, "Model discovery"): with no cached copy
/// `models()` runs synchronously once at startup for a provider with a
/// credential, never on a request; a stale cached list refreshes in the
/// background, once across the processes sharing a Fiber home. Background
/// threads are detached; the cache write is atomic, so an interrupted
/// refresh leaves the old copy. A refresh never touches `providers`, so a
/// running session's tool definitions never change.
pub(crate) fn add_lua(extensions: &SessionExtensions, providers: &mut Providers, config: &Config) {
    for (extension, provider) in extensions.lua_providers() {
        // debt: notices from discovery are dropped, as `parts_with` drops
        // them; surfaced when #382 lands.
        let _notices = providers.add_lua(extension, provider, config);
    }
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
}

/// The session's key and signer for `provider`: no key when it registered
/// `credential`, so no key file is needed, else the key `key` reads. The
/// token is read once, so a failing `credential()` fails here with its
/// code: before any session line for a session, and into the loop for a
/// reviewer (`docs/permissions.md`, "How it runs").
pub(crate) fn session_credential(
    providers: &Providers,
    provider: &ProviderData,
    key: impl FnOnce() -> Result<Secret, Failure>,
) -> Result<KeyAndSigner, Failure> {
    let lua = providers.lua(&provider.name);
    let key = match lua {
        Some(lua)
            if lua
                .registers("credential")
                .map_err(|e| failed(e.code(), e))? =>
        {
            lua.credential_token().map(|_| ()).map_err(|e| {
                provider::Error::Sign(e).failure(&provider.name, &Secrets::default())
            })?;
            None
        }
        _ => Some(key()?),
    };
    Ok((key, signer(lua)?))
}

/// Signs `lua`'s requests; `Some` when it registered `credential` or `sign`.
pub(crate) fn signer(lua: Option<&Arc<LuaProvider>>) -> Result<Option<Arc<dyn Signer>>, Failure> {
    match lua {
        Some(lua) => lua.signer().map_err(|e| failed(e.code(), e)),
        None => Ok(None),
    }
}
