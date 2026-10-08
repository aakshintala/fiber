//! Starting one Lua extension's VM, at session start and when a `model`
//! switch needs a provider that is not loaded (`docs/model-routing.md`,
//! "Model discovery").

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Manifest};
use contract::clock::Clock;
use contract::events::Notice;
use contract::files::PathLock;

use super::SessionExtensions;
use crate::host::Session;
use crate::lua::LuaExtension;
use crate::{Error, LuaProvider};

/// The VM of the extension `name`, installed in `dir`, set up as a session
/// runs it: its settings checked, its session, secrets, memory cap and hook
/// timeout. Its entry script has not run yet. Also returns the notices for
/// the repository's settings keys its manifest does not let it read.
pub(super) fn start_vm(
    name: &str,
    dir: &Path,
    home: &Path,
    config: &Config,
    manifest: &Manifest,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
) -> Result<(LuaExtension, Vec<Notice>), Error> {
    let repo: Vec<&str> = manifest.repo_settings.iter().map(String::as_str).collect();
    let (_, ignored) = config.extension_settings(name, &repo)?;
    let mut extension = LuaExtension::new(name, dir, home, clock)
        .with_session(Session {
            config: config.clone(),
            repo_settings: manifest.repo_settings.clone(),
            locks,
        })
        .with_secrets(manifest.secrets.clone());
    if let Some(cap) = manifest
        .memory_mib
        .and_then(|mib| usize::try_from(mib).ok())
        .and_then(|mib| mib.checked_mul(1 << 20))
        .and_then(NonZeroUsize::new)
    {
        extension = extension.with_memory_cap(cap);
    }
    if let Some(ms) = config
        .get(&format!("extensions.\"{name}\".hook_timeout_ms"), None)
        .and_then(|(value, _)| value.as_u64())
        .filter(|ms| *ms > 0)
    {
        extension.override_hook_timeout(Duration::from_millis(ms));
    }
    Ok((extension, ignored))
}

impl SessionExtensions {
    /// The Lua provider `provider` of the loaded extension `extension`. An
    /// extension the session keeps for its hooks or commands lends its
    /// running VM; any other gets a fresh one, started as session start
    /// starts it, with no emit, driver or inbox, and its hooks and commands
    /// are not registered. A start that fails, or a VM that does not
    /// register `provider`, fails with `extension_failed`.
    pub fn start_provider(
        &self,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        extension: &str,
        provider: &str,
    ) -> Result<Arc<LuaProvider>, Error> {
        let lua = match self.lua.iter().find(|lua| lua.name() == extension) {
            Some(lua) => Arc::clone(lua),
            None => {
                let Some((_, dir)) = self.dirs.iter().find(|(name, _)| name == extension) else {
                    return Err(Error::Lua {
                        extension: extension.to_owned(),
                        message: "the extension is not loaded in this session".into(),
                    });
                };
                let manifest = config::read_manifest(dir)?;
                let (lua, _) =
                    start_vm(extension, dir, &self.home, config, &manifest, clock, locks)?;
                let lua = match &self.host_script {
                    Some(script) => lua.with_host_script(Arc::clone(script)),
                    None => lua,
                };
                Arc::new(lua)
            }
        };
        if !lua.provider_names()?.iter().any(|name| name == provider) {
            return Err(Error::Lua {
                extension: extension.to_owned(),
                message: format!("registers no provider `{provider}`"),
            });
        }
        Ok(LuaProvider::new(lua, provider))
    }
}
