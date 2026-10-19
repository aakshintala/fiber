//! Reloading configuration without changing a running session's route
//! (`docs/configuration.md`, "When Fiber reads configuration").
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::{BlockLimits, Error, HandoffSettings, Loop, Retry, Reviewer};
use contract::diag::Observe;
use contract::events::{CacheLifetime, LoadedExtension, Notice};
use contract::hook::Hooks;
use contract::inbox::{Ack, Rejection};
use contract::shapes::Failure;
use contract::tool::Tool;

/// The session's current route, which a reload must retain.
pub struct ReloadRoute {
    /// The current model reference.
    pub model: String,
    /// The current credential label.
    pub credential: Option<String>,
}

/// Prepares every fallible part of a reload before changing the session.
pub type PrepareReload =
    Arc<dyn Fn(&ReloadRoute) -> Result<ReloadPrepared, Rejection> + Send + Sync>;

/// Configuration-derived preamble inputs, excluding the session's route.
pub struct PromptPatch {
    /// Replacement system prompt text.
    pub system: Option<String>,
    /// Appended system prompt text.
    pub append: Option<String>,
    /// Extension prompt texts, with their names.
    pub extensions: Vec<(String, String)>,
    /// Extension package directories, with their names.
    pub extension_dirs: Vec<(String, PathBuf)>,
    /// Opening-message sections, with their names, files and token limits.
    pub extension_sections: Vec<(String, Vec<PathBuf>, Option<u64>)>,
    /// Disabled skill names.
    pub skills_disabled: Vec<String>,
    /// The prompt cache lifetime.
    pub cache_lifetime: CacheLifetime,
}

/// Configuration-derived loop settings replaced by a reload.
pub struct ReloadSettings {
    /// The person's reviewer context.
    pub reviewer_notes: String,
    /// Reviewer block limits.
    pub limits: BlockLimits,
    /// Model call retry settings.
    pub retry: Retry,
    /// Automatic handoff settings.
    pub handoff: HandoffSettings,
    /// The session's billed-spend limit.
    pub budget: Option<f64>,
    /// The idle exit timeout.
    pub idle_exit: Option<Duration>,
    /// The maximum number of lifetimes to warm an idle cache.
    pub warm_cap: Option<u32>,
}

/// A complete reload ready for an infallible field swap and installation.
pub struct ReloadPrepared {
    /// The non-route preamble inputs.
    pub prompt: PromptPatch,
    /// The complete tool set, in registration order, with each owner.
    pub tools: Vec<(String, Arc<dyn Tool>)>,
    /// The whole set of loaded extensions.
    pub extensions: Vec<LoadedExtension>,
    /// Tolerated startup failures and other load notices.
    pub notices: Vec<Notice>,
    /// Names of the running MCP servers.
    pub servers: Vec<String>,
    /// Hooks over the session's one extension state registry.
    pub hooks: Option<Arc<dyn Hooks>>,
    /// The reviewer for the session's current model.
    pub reviewer: Result<Reviewer, Failure>,
    /// Observation factory for reviewer calls.
    pub reviewer_observe: Option<Observe>,
    /// Credential source files that must join the deny.
    pub credential_files: Vec<PathBuf>,
    /// The loop's non-route settings.
    pub settings: ReloadSettings,
    /// Publishes the installed configuration, tools and extension handles.
    pub installed: Box<dyn FnOnce() + Send>,
}

/// What an installation leaves for its caller to write: the prepared
/// extensions, notices and servers.
struct Installed {
    extensions: Vec<LoadedExtension>,
    notices: Vec<Notice>,
    servers: Vec<String>,
}

impl Loop {
    /// Prepares reloads with `prepare`; absent this, reload is unsupported.
    pub fn reloader(mut self, prepare: PrepareReload) -> Self {
        self.reloader = Some(prepare);
        self
    }

    pub(crate) fn take_reload(&mut self, ack: Ack) -> Result<(), Error> {
        use crate::inbox::{CLOSING, reject};
        if self.closing {
            reject(ack, contract::ErrorCode::Closing, CLOSING);
            return Ok(());
        }
        let Some(prepare) = self.reloader.clone() else {
            reject(
                ack,
                contract::ErrorCode::InvalidArguments,
                "This session cannot reload.",
            );
            return Ok(());
        };
        let route = ReloadRoute {
            model: self.model.reference.clone(),
            credential: self.prompt.credential.clone(),
        };
        match prepare(&route) {
            Ok(prepared) => self.apply_reload(prepared, ack),
            Err(rejection) => {
                reject(ack, rejection.code, &rejection.message);
                Ok(())
            }
        }
    }

    /// Installs a preparation's field swaps and runs its `installed`
    /// hook: the step `apply_reload` and `apply_late` share. Each caller
    /// writes its own lines from what this returns.
    fn install(&mut self, prepared: ReloadPrepared) -> Installed {
        let patch = prepared.prompt;
        self.prompt.system = patch.system;
        self.prompt.append = patch.append;
        self.prompt.extensions = patch.extensions;
        self.prompt.extension_dirs = patch.extension_dirs;
        self.prompt.extension_sections = patch.extension_sections;
        self.prompt.skills_disabled = patch.skills_disabled;
        self.prompt.cache_lifetime = patch.cache_lifetime;
        self.skills.reloaded(self.prompt.clone());
        let settings = prepared.settings;
        self.reviewer_notes = settings.reviewer_notes;
        self.limits = settings.limits;
        self.retry = settings.retry;
        self.handoff.settings = settings.handoff;
        self.budget = settings.budget;
        self.idle_exit = settings.idle_exit;
        self.warming.set_cap(settings.warm_cap);
        self.reviewer = prepared.reviewer;
        self.reviewer_observe = prepared.reviewer_observe;
        self.reviewer_sent = None;
        crate::switch::deny_also(&mut self.credential_files, prepared.credential_files);
        (self.tools, self.replaced) = crate::calls::register(prepared.tools);
        self.hooks = prepared.hooks;
        (prepared.installed)();
        Installed {
            extensions: prepared.extensions,
            notices: prepared.notices,
            servers: prepared.servers,
        }
    }

    fn apply_reload(&mut self, prepared: ReloadPrepared, ack: Ack) -> Result<(), Error> {
        use contract::events::{
            Event, ExtensionsLoaded, PreambleReason, Reloaded, ReloadedServers,
        };
        let installed = self.install(prepared);
        #[cfg(test)]
        if tests::fail_reload_write() {
            return Err(crate::timing_tests::log_io_error());
        }
        self.write(
            &Event::Reloaded(Reloaded {
                servers: ReloadedServers {
                    kept: installed.servers,
                    restarted: Vec::new(),
                    started: Vec::new(),
                    stopped: Vec::new(),
                },
                extensions: installed
                    .extensions
                    .iter()
                    .map(|extension| extension.name.clone())
                    .collect(),
                failed: None,
            }),
            None,
            None,
        )?;
        crate::inbox::accept(ack);
        self.write(
            &Event::ExtensionsLoaded(ExtensionsLoaded {
                extensions: installed.extensions,
            }),
            None,
            None,
        )?;
        for notice in installed.notices {
            self.write(&Event::Notice(notice), None, None)?;
        }
        if let Some(hooks) = self.hooks.clone() {
            self.drain_state(&hooks, None)?;
        }
        self.preamble = None;
        self.preamble_reason = PreambleReason::Reload;
        self.warming.stop(self.log.clock().now());
        self.load_after_reload = true;
        self.build_preamble()?;
        if self.opened {
            let skills = self.skills.reload_check();
            for notice in skills.notices {
                self.write(&Event::Notice(notice), None, None)?;
            }
            if let Some(changed) = skills.changed {
                self.write(&Event::SkillsChanged(changed), None, None)?;
            }
        }
        Ok(())
    }

    /// Installs the late preparation before the first preamble: the
    /// prepared tools and prompt stand for the first request, and the
    /// prepared set is written as `extensions_loaded` with its notices.
    /// No `reloaded` is written and no ack answered: nobody delivered a
    /// reload. The caller drops the preamble the install replaced.
    pub(crate) fn apply_late(&mut self, prepared: ReloadPrepared) -> Result<(), Error> {
        use contract::events::{Event, ExtensionsLoaded};
        let installed = self.install(prepared);
        self.write(
            &Event::ExtensionsLoaded(ExtensionsLoaded {
                extensions: installed.extensions,
            }),
            None,
            None,
        )?;
        for notice in installed.notices {
            self.write(&Event::Notice(notice), None, None)?;
        }
        if let Some(hooks) = self.hooks.clone() {
            self.drain_state(&hooks, None)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod tests;
