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

/// What a model's requests carry: the key, or nothing when a Lua
/// `credential()` supplies the token, which rides the signing seam instead.
pub(crate) struct Credential {
    /// The session's credential label, recorded as today.
    pub label: String,
    /// The key, sent as a bearer token; `None` when the token rides the
    /// signing seam.
    pub key: Option<String>,
    /// Signs each request; `Some` when the provider registered `credential`
    /// or `sign`.
    pub signer: Option<Arc<dyn Signer>>,
}

/// Merges every Lua provider's models into `providers`
/// (`docs/model-routing.md`, "Model discovery"): with no cached copy
/// `models()` runs synchronously once at startup, never on a request; with
/// one the cache serves and the refresh runs in the background at every
/// start. Background threads are detached; the cache write is atomic, so an
/// interrupted refresh leaves the old copy.
pub(crate) fn add_lua(extensions: &SessionExtensions, providers: &mut Providers) {
    for (extension, provider) in extensions.lua_providers() {
        // debt: notices from discovery are dropped, as `parts_with` drops
        // them; surfaced when #382 lands.
        let _notices = providers.add_lua(extension, provider);
        if provider
            .functions()
            .is_ok_and(|functions| functions.iter().any(|f| f == "models"))
        {
            // Detached: nothing joins it, and `fiber ask` does not wait on
            // it to exit.
            provider.refresh_models();
        }
    }
}

/// The session's credential for `provider`: the `credential()` token when
/// it registered `credential`, so no key file is needed, else the key
/// `key` reads. `label` is `config.credential_label` either way. The
/// token is read once, so a failing `credential()` fails here with its
/// code: before any session line for a session, and into the loop for a
/// reviewer (`docs/permissions.md`, "How it runs").
pub(crate) fn session_credential(
    providers: &Providers,
    provider: &ProviderData,
    label: String,
    key: impl FnOnce() -> Result<Secret, Failure>,
) -> Result<Credential, Failure> {
    let lua = providers.lua(&provider.name);
    let registered = match lua {
        Some(lua) => lua
            .registers("credential")
            .map_err(|e| failed(e.code(), e))?,
        None => false,
    };
    if registered {
        // `lua` is `Some`: it registered `credential`.
        if let Some(lua) = lua {
            lua.token().map(|_| ()).map_err(|e| failed(e.code(), e))?;
        }
        Ok(Credential {
            label,
            key: None,
            signer: signer(lua)?,
        })
    } else {
        let key = key()?;
        Ok(Credential {
            label,
            key: Some(key.expose().to_owned()),
            signer: signer(lua)?,
        })
    }
}

/// Signs `lua`'s requests; `Some` when it registered `credential` or `sign`.
pub(crate) fn signer(lua: Option<&Arc<LuaProvider>>) -> Result<Option<Arc<dyn Signer>>, Failure> {
    match lua {
        Some(lua) => lua.signer().map_err(|e| failed(e.code(), e)),
        None => Ok(None),
    }
}
