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
    // The registering extension's own directory: the pinned copy for a
    // repository entry, the personal install otherwise. A provider whose
    // extension never loaded as a candidate falls back to the home layout.
    let dir = providers
        .provider_source_dir(name)
        .unwrap_or_else(|| config::extensions_dir(home).join(config::dir_name(extension_name)));
    let manifest = config::read_manifest(&dir)?;
    // The dependency layout follows the resolved directory: a pinned
    // copy serves its own .fiber-depends, a personal install serves home —
    // the same rule start_vm uses.
    let is_repository_copy = crate::hooks::is_repository_copy(&dir, home);
    let deps = crate::dep_dirs(home, &dir, &manifest.depends, is_repository_copy);
    let mut extension = LuaExtension::new(extension_name, dir, home, clock)
        .with_dependencies(deps)
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
    let extension = Arc::new(extension);
    if !manifest.providers.contains_key(name) {
        return Err(Error::UndeclaredProvider {
            extension: extension_name.to_owned(),
            provider: name.to_owned(),
        });
    }
    Ok(LuaProvider::new(extension, name))
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
