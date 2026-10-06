//! A Lua provider's models and credential in a session
//! (`docs/model-routing.md`, "Model discovery", "Signing a request" and
//! "Keys, tokens and OAuth"): merging each registered provider's `models()`
//! into the installed providers, and sending `credential()`'s token on every
//! request through the signing seam.

use std::sync::Arc;

use config::{ProviderData, Secret};
use contract::shapes::Failure;
use contract::signing::Signer;
use extensions::{LuaProvider, Providers, SessionExtensions};

use crate::failed;

/// The key a model's requests carry, or nothing when a Lua `credential()`
/// supplies the token, which rides the signing seam instead; and what signs
/// each request, `Some` when the provider registered `credential` or `sign`.
pub(crate) type KeyAndSigner = (Option<String>, Option<Arc<dyn Signer>>);

/// Merges every Lua provider's models into `providers`
/// (`docs/model-routing.md`, "Model discovery"): with no cached copy
/// `models()` runs synchronously once at startup, never on a request; with
/// one the cache serves and the refresh runs in the background at every
/// start. Background threads are detached; the cache write is atomic, so an
/// interrupted refresh leaves the old copy.
pub(crate) fn add_lua(extensions: &SessionExtensions, providers: &mut Providers) {
    for (extension, provider) in extensions.lua_providers() {
        // debt: notices from discovery and the `registers` error are
        // dropped, as `parts_with` drops them; surfaced when #382 lands.
        // The cache is read before the add: with none `models()` runs
        // synchronously inside it, and `discover` writes the cache, so a
        // read after would see its own write.
        let cached = provider.has_model_cache();
        let _notices = providers.add_lua(extension, provider);
        if cached
            && provider
                .registers("models")
                .is_ok_and(|registers| registers)
        {
            // Detached: nothing joins it, and `fiber ask` does not wait on
            // it to exit.
            provider.refresh_models();
        }
    }
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
            lua.token().map(|_| ()).map_err(|e| failed(e.code(), e))?;
            None
        }
        _ => Some(key()?.expose().to_owned()),
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
