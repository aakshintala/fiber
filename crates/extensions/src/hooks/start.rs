//! Starting one Lua extension's VM, at session start and when a `model`
//! switch needs a provider that is not loaded (`docs/model-routing.md`,
//! "Model discovery").

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use support::lock;

use config::{Config, Manifest};
use contract::clock::Clock;
use contract::events::Notice;
use contract::files::PathLock;

use super::SessionExtensions;
use crate::host::Session;
use crate::lua::LuaExtension;
use crate::{Error, LuaProvider};

/// Whether `dir` is a pinned repository copy: it lives under the pinned
/// directory, where repository copies live, so a pinned copy never reads
/// a personal install of the same dependency.
pub(crate) fn is_repository_copy(dir: &Path, home: &Path) -> bool {
    dir.starts_with(config::pinned_dir(home))
}

/// Where each manifest `depends` entry lives, by its name: the directory
/// a qualified `require` loads from. Every key is mapped, including one
/// holding a colon: matching, not the map, decides, so the longest key
/// prefix wins. A repository copy reads its own `.fiber-depends`, never
/// Fiber home, so a missing entry still names the dependency there
/// instead of leaking across the boundary.
pub fn dep_dirs(
    home: &Path,
    dir: &Path,
    depends: &BTreeMap<String, String>,
    is_repository_copy: bool,
) -> BTreeMap<String, PathBuf> {
    let root = if is_repository_copy {
        config::extensions_dir(&dir.join(".fiber-depends"))
    } else {
        config::extensions_dir(home)
    };
    let mut deps = BTreeMap::new();
    for name in depends.keys() {
        if let Ok(slug) = crate::install::slug(name) {
            deps.insert(name.clone(), root.join(slug));
        }
    }
    deps
}

/// The VM of the extension `name`, installed in `dir`, set up as a session
/// runs it: its settings checked, its session, secrets, memory cap and hook
/// timeout. Its entry script has not run yet. Also returns the notices for
/// the repository's settings keys its manifest does not let it read. The
/// session's diagnostic sink, when debugging, installs before any Lua runs,
/// so an entry script's `host.http` still records; `None` stays silent.
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
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
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
    if let Some(debug) = debug {
        extension = extension.with_diagnostics(debug);
    }
    // A repository copy reads its own `.fiber-depends`: the predicate
    // below is where those copies live, so a pinned copy never reads a
    // personal install of the same dependency.
    let is_repository_copy = is_repository_copy(dir, home);
    extension =
        extension.with_dependencies(dep_dirs(home, dir, &manifest.depends, is_repository_copy));
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

/// Registers the Lua providers of one started entry on the session: the
/// caller reads each provider's declared names from its `ProviderData`,
/// and the session keeps one `LuaProvider` per script under its weak VMs.
/// Reads no key and runs no Lua. A provider-only entry reuses the VM the
/// session started for it, whose entry script never runs; an entry with no
/// VM yet starts one as session start starts it.
pub(super) fn provider_handles(
    session: &mut SessionExtensions,
    entry: &super::Started,
    config: &Config,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
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
                debug,
            )?;
            session.notices.append(&mut notices);
            let lua = lua.with_browser(session.browser.clone());
            Arc::new(match &session.host_script {
                Some(script) => lua.with_host_script(Arc::clone(script)),
                None => lua,
            })
        }
    };
    lock(&session.provider_vms).insert(entry.name.clone(), Arc::downgrade(&lua));
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
    /// no driver or inbox. A late fresh VM for a disabled provider-only
    /// extension skips its entry script, as the session's one does; only
    /// providers may run there. Its hooks and commands are not registered. A start that fails, or a VM that does not
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
        if manifest.process.is_some() {
            return self
                .process_providers
                .get(&(extension.to_owned(), provider.to_owned()))
                .cloned()
                .ok_or_else(|| Error::Lua {
                    extension: extension.to_owned(),
                    message: format!("registers no provider `{provider}`"),
                });
        }
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
        let mut vms = lock(&self.provider_vms);
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
                    None,
                )?;
                // A disabled provider-only extension's late VM skips its
                // entry script, as the session's one does: only providers
                // may run.
                let lua = if !crate::is_enabled(config, extension) && !manifest.providers.is_empty()
                {
                    lua.without_entry()
                } else {
                    lua
                };
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
                let emit = lock(&self.emitter).clone();
                // The retained session inbox reaches a late fresh VM before
                // its handle escapes; a reused VM already has it, and none
                // was ever delivered when the retained sender is `None`.
                let inbox = lock(&self.inbox).clone();
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

    use super::*;

    const DEP: &str = "example.com/acme/strings";

    /// The manifest `fiber.test/root` with these `depends`, read back the
    /// way the session reads it.
    fn manifest_with_depends(dir: &Path, depends: &str) -> Manifest {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("extension.json"),
            format!(
                "{{\"name\":\"fiber.test/root\",\"version\":\"v1.0.0\",\"fiber\":\"0.1.0\",\"api\":1,\"depends\":{depends}}}"
            ),
        )
        .unwrap();
        config::read_manifest(dir).unwrap()
    }

    #[test]
    fn a_personal_install_maps_each_dependency_into_fiber_home() {
        let home = fakes::TempDir::new("fiber-dep-dirs-personal");
        let dir = home.path().join("extensions").join("fiber.test-root");
        let manifest = manifest_with_depends(&dir, r#"{"example.com/acme/strings":"1.0"}"#);
        let deps = dep_dirs(home.path(), &dir, &manifest.depends, false);
        assert_eq!(
            deps.get(DEP),
            Some(
                &home
                    .path()
                    .join("extensions")
                    .join("example.com-acme-strings")
            )
        );
    }

    #[test]
    fn a_repository_copy_maps_each_dependency_into_its_own_depends() {
        let home = fakes::TempDir::new("fiber-dep-dirs-copy");
        let dir = home.path().join("pinned").join("abc123");
        let manifest = manifest_with_depends(&dir, r#"{"example.com/acme/strings":"1.0"}"#);
        let deps = dep_dirs(home.path(), &dir, &manifest.depends, true);
        assert_eq!(
            deps.get(DEP),
            Some(
                &dir.join(".fiber-depends")
                    .join("extensions")
                    .join("example.com-acme-strings")
            )
        );
    }

    #[test]
    fn a_missing_copy_local_dependency_stays_copy_local_with_no_home_fallback() {
        let home = fakes::TempDir::new("fiber-dep-dirs-missing");
        let dir = home.path().join("pinned").join("abc123");
        let manifest = manifest_with_depends(&dir, r#"{"example.com/acme/strings":"1.0"}"#);
        // Nothing is installed under the copy yet: the map still points
        // there, so a require names the dependency instead of reading
        // a personal install of the same name.
        assert!(!dir.join(".fiber-depends").exists());
        let deps = dep_dirs(home.path(), &dir, &manifest.depends, true);
        let mapped = deps.get(DEP).unwrap();
        assert!(mapped.starts_with(dir.join(".fiber-depends")));
        assert!(!mapped.starts_with(config::extensions_dir(home.path())));
    }

    #[test]
    fn a_depends_key_holding_a_colon_is_mapped_for_longest_prefix_matching() {
        let home = fakes::TempDir::new("fiber-dep-dirs-colon");
        let dir = home.path().join("extensions").join("fiber.test-root");
        let manifest = manifest_with_depends(&dir, r#"{"github.com/acme/lib:aux":"1.0"}"#);
        // Matching, not the map, decides: the colon-holding key stays in
        // the map so the longest prefix wins at require time.
        let deps = dep_dirs(home.path(), &dir, &manifest.depends, false);
        assert_eq!(
            deps.get("github.com/acme/lib:aux"),
            Some(
                &home
                    .path()
                    .join("extensions")
                    .join(config::dir_name("github.com/acme/lib:aux"))
            )
        );
    }
}
