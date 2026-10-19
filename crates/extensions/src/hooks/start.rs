//! Starting one Lua extension's VM, at session start and when a `model`
//! switch needs a provider that is not loaded (`docs/model-routing.md`,
//! "Model discovery").

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, PoisonError};
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
#[allow(
    clippy::too_many_arguments,
    reason = "VM startup receives the session's existing services and shared state"
)]
pub(super) fn start_vm(
    name: &str,
    dir: &Path,
    home: &Path,
    config: &Config,
    manifest: &Manifest,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    state: Arc<crate::host::state::StateRegistry>,
) -> Result<(LuaExtension, Vec<Notice>), Error> {
    let repo: Vec<&str> = manifest.repo_settings.iter().map(String::as_str).collect();
    let (_, ignored) = config.extensions().merged(name, &repo)?;
    let mut extension = LuaExtension::new(name, dir, home, clock)
        .with_state(state)
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

pub(super) fn provider_handles(
    session: &mut SessionExtensions,
    entry: &super::Started,
    config: &Config,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
) -> Result<(), Error> {
    if entry.providers.is_empty() {
        return Ok(());
    }
    let lua = match &entry.extension {
        Some(lua) => Arc::clone(lua),
        None => {
            let (lua, mut notices) = start_vm(
                &entry.name,
                &entry.dir,
                &session.home,
                config,
                &entry.manifest,
                clock,
                locks,
                session.state.clone(),
            )?;
            session.notices.append(&mut notices);
            let lua = lua.with_browser(session.browser.clone());
            Arc::new(match &session.host_script {
                Some(script) => lua.with_host_script(Arc::clone(script)),
                None => lua,
            })
        }
    };
    session
        .provider_vms
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(entry.name.clone(), Arc::downgrade(&lua));
    for name in &entry.providers {
        session
            .lua_providers
            .push((entry.name.clone(), LuaProvider::new(Arc::clone(&lua), name)));
    }
    Ok(())
}

impl SessionExtensions {
    /// The Lua provider `provider` of the loaded extension `extension`. An
    /// extension the session keeps for its hooks or commands lends its
    /// running VM; any other gets a fresh one, started as session start
    /// starts it, with the session's browser and any installed emitter, but
    /// no driver or inbox. Its hooks and commands are not registered. A start that fails, or a VM that does not
    /// register `provider`, fails with `extension_failed`.
    pub fn start_provider(
        &self,
        config: &Config,
        clock: Arc<dyn Clock>,
        locks: Arc<dyn PathLock>,
        extension: &str,
        provider: &str,
    ) -> Result<Arc<LuaProvider>, Error> {
        let Some((_, dir)) = self.dirs.iter().find(|(name, _)| name == extension) else {
            return Err(Error::Lua {
                extension: extension.to_owned(),
                message: "the extension is not loaded in this session".into(),
            });
        };
        let manifest = config::read_manifest(dir)?;
        let scripts = crate::providers::provider_scripts(dir)?;
        if let Some(unclaimed) = crate::providers::first_undeclared(
            &manifest.providers,
            scripts.iter().map(String::as_str),
        ) {
            return Err(Error::UndeclaredProvider {
                extension: extension.to_owned(),
                provider: unclaimed.to_owned(),
            });
        }
        if !scripts.iter().any(|name| name == provider) {
            return Err(Error::Lua {
                extension: extension.to_owned(),
                message: format!("registers no provider `{provider}`"),
            });
        }
        let mut vms = self
            .provider_vms
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let lua = match vms.get(extension).and_then(std::sync::Weak::upgrade) {
            Some(lua) => lua,
            None => {
                let (lua, _) = start_vm(
                    extension,
                    dir,
                    &self.home,
                    config,
                    &manifest,
                    clock,
                    locks,
                    self.state.clone(),
                )?;
                let lua = lua.with_browser(self.browser.clone());
                let lua = match &self.host_script {
                    Some(script) => lua.with_host_script(Arc::clone(script)),
                    None => lua,
                };
                // The retained session sink reaches a late fresh VM
                // before its handle escapes; a reused VM already has it.
                let lua = match self.diagnostics() {
                    Some(debug) => lua.with_diagnostics(debug),
                    None => lua,
                };
                let lua = Arc::new(lua);
                // Lock order: provider_vms, then emitter, then inbox. emit_to,
                // seal and deliver_to drop their sink before collecting VMs,
                // so none misses this VM.
                let emit = self
                    .emitter
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                // The retained session inbox reaches a late fresh VM before
                // its handle escapes; a reused VM already has it, and none
                // was ever delivered when the retained sender is `None`.
                let inbox = self
                    .inbox
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                #[cfg(test)]
                self.at_late(super::LatePoint::Read);
                if let Some(emit) = emit {
                    lua.set_emit(emit);
                }
                if let Some(inbox) = inbox {
                    lua.deliver_to(inbox);
                }
                vms.insert(extension.to_owned(), Arc::downgrade(&lua));
                lua
            }
        };
        Ok(LuaProvider::new(lua, provider))
    }
}
