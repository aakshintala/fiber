//! Finding the VM that runs one provider's `credential()` login
//! (`docs/model-routing.md`, "Logging in"): the extension that registered
//! the provider, set up as a session runs it, without a session.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;

use crate::oauth::Browser;
use crate::providers::Providers;
use crate::{Error, LuaExtension, LuaProvider};

/// The provider `name`'s VM, ready for [`LuaProvider::login`]: the
/// extension that registered it, with its manifest's secrets and memory
/// cap, and the browser the login opens. No installed extension registers
/// `name` is `ProviderMissing`.
pub fn login_provider(
    home: &Path,
    providers: &Providers,
    name: &str,
    browser: Arc<dyn Browser>,
    clock: Arc<dyn Clock>,
) -> Result<Arc<LuaProvider>, Error> {
    let extension_name = providers
        .extension_of(name)
        .ok_or_else(|| Error::ProviderMissing {
            provider: name.to_owned(),
        })?;
    let dir = home
        .join("extensions")
        .join(config::dir_name(extension_name));
    let manifest = config::read_manifest(&dir)?;
    let mut extension = LuaExtension::new(extension_name, dir, home, clock)
        .with_browser(browser)
        .with_secrets(manifest.secrets.clone());
    if let Some(cap) = manifest
        .memory_mib
        .and_then(|mib| usize::try_from(mib).ok())
        .and_then(|mib| mib.checked_mul(1 << 20))
        .and_then(NonZeroUsize::new)
    {
        extension = extension.with_memory_cap(cap);
    }
    Ok(LuaProvider::new(Arc::new(extension), name))
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
