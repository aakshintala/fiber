//! Reloading configuration without changing a running session's route
//! (`docs/configuration.md`, "When Fiber reads configuration").
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::{BlockLimits, Error, HandoffSettings, Loop, Retry, Reviewer};
use contract::diag::Observe;
use contract::events::{
    CacheLifetime, CommandInfo, LoadedExtension, Notice, ReloadFailure, ReloadedServers,
};
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

/// Restarts one MCP server, as a reload restarts its servers: adopted
/// when the session never started it, served in place otherwise. The
/// entry declares the tools itself, through the session's declarer, and
/// returns the registry's news with the server lines to write; the loop
/// merges the registry, writes the lines, and answers once they are
/// written.
pub type ReconnectServer = Arc<dyn Fn(&str) -> Result<Reconnected, Rejection> + Send + Sync>;

/// What a reconnect restarted: the loop registry's news and the log's.
pub struct Reconnected {
    /// The server's tools after the restart, with their owners, for the
    /// loop's registry.
    pub tools: Vec<(String, Arc<dyn Tool>)>,
    /// Qualified tool names the restart dropped, for the loop's
    /// registry.
    pub removed: Vec<String>,
    /// The `mcp_server_failed`/`mcp_server_ready` lines the restart
    /// observed, in order, written before the answer.
    pub records: Vec<contract::tool::ServerRecord>,
}

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

/// One reload's MCP server sync, in `reloaded` order: the four-quadrant
/// grouping, the server lines written before `reloaded`, the
/// non-required failures for its `failed` entry, and the carried
/// new-server prompt rows. Carried as one result from preparation
/// through installation to the log, so the fields describing one sync
/// never drift apart.
pub struct ServerSync {
    /// Kept, restarted, started and stopped, by name.
    pub servers: ReloadedServers,
    /// The `mcp_server_failed`/`mcp_server_ready` lines in sync order,
    /// written before `reloaded`.
    pub server_records: Vec<contract::tool::ServerRecord>,
    /// Each non-required server failure, for `reloaded`'s `failed` entry.
    pub server_failures: Vec<ReloadFailure>,
    /// The shadow-started servers' runnable prompt rows with the fetch
    /// answering them, for servers with no startup rows; `install`
    /// appends them, keeping every startup row. Rows and fetch travel
    /// as one value, so new rows without their fetch cannot be
    /// represented: `None` when the sync carried no new rows.
    pub new_prompts: Option<(Vec<CommandInfo>, crate::FetchPrompt)>,
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
    /// The reload's MCP server sync.
    pub server_sync: ServerSync,
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
/// extensions and notices, with the reload's server sync for `reloaded`.
struct Installed {
    extensions: Vec<LoadedExtension>,
    notices: Vec<Notice>,
    sync: ServerSync,
}

/// One server record as its log line: the reconnect driver and the reload
/// installer write the same lines, in the same order.
fn record_event(record: &contract::tool::ServerRecord) -> contract::events::Event {
    match record {
        contract::tool::ServerRecord::Failed(failed) => {
            contract::events::Event::McpServerFailed(failed.clone())
        }
        contract::tool::ServerRecord::Ready(ready) => {
            contract::events::Event::McpServerReady(ready.clone())
        }
    }
}

impl Loop {
    /// Prepares reloads with `prepare`; absent this, reload is unsupported.
    pub fn reloader(mut self, prepare: PrepareReload) -> Self {
        self.reloader = Some(prepare);
        self
    }

    /// Restarts failed servers with `reconnect`; absent this, reconnect
    /// is unsupported.
    pub fn reconnector(mut self, reconnect: ReconnectServer) -> Self {
        self.reconnector = Some(reconnect);
        self
    }

    pub(crate) fn take_reconnect(&mut self, server: String, ack: Ack) -> Result<(), Error> {
        use crate::inbox::{CLOSING, accept, reject};
        if self.closing {
            reject(ack, contract::ErrorCode::Closing, CLOSING);
            return Ok(());
        }
        let Some(reconnect) = self.reconnector.clone() else {
            reject(
                ack,
                contract::ErrorCode::InvalidArguments,
                "This session cannot reconnect servers.",
            );
            return Ok(());
        };
        match reconnect(&server) {
            Ok(reconnected) => {
                for name in &reconnected.removed {
                    self.tools.remove(name);
                }
                for (owner, tool) in &reconnected.tools {
                    let definition = tool.definition();
                    self.tools.insert(
                        definition.name.clone(),
                        (owner.clone(), Arc::clone(tool), definition),
                    );
                }
                for record in &reconnected.records {
                    self.write(&record_event(record), None, None)?;
                }
                accept(ack);
                Ok(())
            }
            Err(rejection) => {
                reject(ack, rejection.code, &rejection.message);
                Ok(())
            }
        }
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
        // New servers' prompts join the `/` list; every startup row stays.
        // Their sources join the fetch too: the carried tags route to the
        // carried fetch, so a new row answers through the same path as a
        // startup row, while every other tag keeps its own. A replaced
        // row keeps its metadata and is not appended again, but its tag
        // still routes to the replacement source, so a kept `/name` row
        // answers through the new slot.
        let mut sync = prepared.server_sync;
        if let Some((rows, extra)) = sync.new_prompts.take()
            && let Some(prompts) = self.server_prompts.as_mut()
        {
            let tags: HashSet<String> = rows.iter().map(|row| row.tag.clone()).collect();
            CommandInfo::merge_into(&mut prompts.rows, &rows);
            let old = Arc::clone(&prompts.fetch);
            prompts.fetch = Arc::new(move |server, prompt, text, cancel| {
                if tags.contains(server) {
                    extra(server, prompt, text, cancel)
                } else {
                    old(server, prompt, text, cancel)
                }
            });
        }
        (prepared.installed)();
        Installed {
            extensions: prepared.extensions,
            notices: prepared.notices,
            sync,
        }
    }

    fn apply_reload(&mut self, prepared: ReloadPrepared, ack: Ack) -> Result<(), Error> {
        use contract::events::{Event, ExtensionsLoaded, PreambleReason, Reloaded};
        let installed = self.install(prepared);
        #[cfg(test)]
        if tests::fail_reload_write() {
            return Err(crate::timing_tests::log_io_error());
        }
        // The sync's records first, in sync order, then `reloaded`:
        // program order on the loop thread is the edge for that line order.
        let Installed {
            extensions,
            notices,
            sync:
                ServerSync {
                    servers,
                    server_records,
                    server_failures,
                    new_prompts: _,
                },
        } = installed;
        for record in &server_records {
            self.write(&record_event(record), None, None)?;
        }
        let failed = (!server_failures.is_empty()).then_some(server_failures);
        self.write(
            &Event::Reloaded(Reloaded {
                servers,
                extensions: extensions
                    .iter()
                    .map(|extension| extension.name.clone())
                    .collect(),
                failed,
            }),
            None,
            None,
        )?;
        crate::inbox::accept(ack);
        self.write(
            &Event::ExtensionsLoaded(ExtensionsLoaded { extensions }),
            None,
            None,
        )?;
        for notice in notices {
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
