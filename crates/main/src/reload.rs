//! Preparation and publication of configuration reloads (`docs/configuration.md`).
use contract::diag::DebugLog as _;
use contract::events::ToolSource;
use contract::inbox::Rejection;
use contract::shapes::Failure;
use std::sync::Arc;

pub(crate) fn preparer(
    build: impl Fn(&r#loop::ReloadRoute) -> Result<r#loop::ReloadPrepared, Rejection>
    + Send
    + Sync
    + 'static,
    run: impl Fn(
        &str,
        Box<dyn FnOnce() -> Result<r#loop::ReloadPrepared, Rejection> + Send>,
    ) -> Result<Result<r#loop::ReloadPrepared, Rejection>, Failure>
    + Send
    + Sync
    + 'static,
) -> r#loop::PrepareReload {
    let build = Arc::new(build);
    Arc::new(move |route| {
        // debt: two route fields copied here, derive Clone when ReloadRoute's interface next changes.
        let route = r#loop::ReloadRoute {
            model: route.model.clone(),
            credential: route.credential.clone(),
        };
        let build = build.clone();
        run("reload", Box::new(move || build(&route))).map_err(|error| Rejection {
            code: error.code,
            message: error.message,
        })?
    })
}

type Tools = Vec<(String, Arc<dyn contract::tool::Tool>)>;

/// Syncs the skill set's runnable-set baseline to installed command rows.
pub(crate) type CommandsSync = Arc<dyn Fn(&r#loop::RunnableCommands) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Late {
    tools: Tools,
    infos: std::collections::BTreeMap<String, contract::events::ToolInfo>,
}

pub(crate) fn record_declarations(
    declare: doors::Declare,
    late: Arc<std::sync::Mutex<Late>>,
) -> doors::Declare {
    Arc::new(move |name, info| {
        {
            let mut held = support::lock(&late);
            match &info {
                Some(info) => {
                    held.infos.insert(name.to_owned(), info.clone());
                }
                None => {
                    held.infos.remove(name);
                }
            }
        }
        declare(name, info);
    })
}

pub(crate) fn record_load(
    load: r#loop::LoadApproved,
    late: Arc<std::sync::Mutex<Late>>,
    repository: Arc<std::sync::Mutex<crate::session_extensions::RepositoryScope>>,
) -> r#loop::LoadApproved {
    Arc::new(move |outcome| {
        let approved = load(outcome)?;
        support::lock(&late).tools.extend(approved.tools.clone());
        // The offer's skips join the scope the next preparation reads:
        // skipped content stays out of a reloaded or late set.
        support::lock(&repository).skipped = outcome.skipped.clone();
        Ok(approved)
    })
}

/// Which preparation `prepare_inner` runs: an explicit reload
/// classifies, stages and commits the MCP sync, while the late
/// extension-approval preparation skips it, so no slot starts and the
/// grouping, records, failures and carried prompt sources stay empty.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Preparation {
    /// An explicit reload: run the MCP sync.
    Explicit,
    /// The late extension-approval preparation: skip the MCP sync.
    Late,
}

/// Test-only `prepare_inner` stage boundaries, in the order the
/// preparation reaches them. The late preparation skips the `mcp_*`
/// middle, so its record jumps from `inputs_built` to `tools_rebuilt`.
/// No outside seam reaches these steps: the fake servers log a request
/// on arrival, before handling or answering it, and `fakes::within`
/// never joins its work thread, so only an in-process append can say
/// which boundary the prepare passed last.
#[cfg(test)]
pub(crate) const PREPARE_CHECKPOINTS: &[&str] = &[
    "config_loaded",
    "repository_ready",
    "model_resolved",
    "inputs_built",
    "mcp_specs_loaded",
    "mcp_repository_prepared",
    "mcp_classified",
    "mcp_staged",
    "mcp_planned",
    "tools_rebuilt",
    "prepared",
];

/// Test-only rendering of a prepare progress record: the last reached
/// checkpoint, or the first expected one when nothing was reached.
/// The record is read in the failure path: when the prepare wait
/// expired instead, the wait leaves its work thread running, so the
/// read may include later checkpoints.
#[cfg(test)]
pub(crate) fn describe_prepare_progress(checkpoints: &[&str]) -> String {
    match checkpoints.last() {
        None => format!(
            "prepare progress: no checkpoint reached as read in the failure path \
             (when the prepare wait expired, the wait leaves its work thread running, so this may include later checkpoints); \
             the next expected step is {}",
            PREPARE_CHECKPOINTS[0]
        ),
        Some(last) => match PREPARE_CHECKPOINTS
            .iter()
            .position(|known| known == last)
            .and_then(|at| PREPARE_CHECKPOINTS.get(at + 1))
        {
            Some(next) => format!(
                "prepare progress: last reached checkpoint {last} as read in the failure path \
                 (when the prepare wait expired, the wait leaves its work thread running, so this may include later checkpoints); \
                 the next expected step is {next}"
            ),
            None => format!(
                "prepare progress: last reached checkpoint {last} as read in the failure path \
                 (when the prepare wait expired, the wait leaves its work thread running, so this may include later checkpoints); \
                 the prepare reached the final checkpoint"
            ),
        },
    }
}

#[cfg(test)]
impl State {
    /// Records one `prepare_inner` stage boundary: an append only, no
    /// pause and no branch, so production timing is unchanged.
    fn record_prepare_checkpoint(&self, checkpoint: &'static str) {
        support::lock(&self.prepare_progress).push(checkpoint);
    }

    pub(crate) fn prepare_progress(&self) -> Vec<&'static str> {
        support::lock(&self.prepare_progress).clone()
    }

    pub(crate) fn clear_prepare_progress(&self) {
        support::lock(&self.prepare_progress).clear();
    }

    pub(crate) fn prepare_progress_line(&self) -> String {
        describe_prepare_progress(&self.prepare_progress())
    }
}

pub(crate) struct State {
    pub(crate) sources: config::Sources,
    pub(crate) template: r#loop::PromptInputs,
    pub(crate) current: Arc<crate::session_extensions::Current>,
    pub(crate) switching: Arc<crate::switch::Switching>,
    pub(crate) tools: std::sync::Mutex<Tools>,
    pub(crate) kept_infos: std::sync::Mutex<Vec<contract::events::ToolInfo>>,
    pub(crate) prompt_rows: std::sync::Mutex<Vec<contract::events::CommandInfo>>,
    pub(crate) late: Arc<std::sync::Mutex<Late>>,
    /// The repository scope the next preparation reads: the launch
    /// workspace, or the recorded one on resume, and the content the
    /// offer skipped.
    pub(crate) repository: Arc<std::sync::Mutex<crate::session_extensions::RepositoryScope>>,
    pub(crate) declare: doors::Declare,
    pub(crate) replace: Arc<dyn Fn(doors::Replacement) + Send + Sync>,
    /// Syncs the skill set's runnable-set baseline to the installed
    /// command rows: the next turn-start check measures against what the
    /// reload installed, not what a check past the install saw.
    pub(crate) commands_sync: CommandsSync,
    pub(crate) drive: Arc<dyn contract::extension::Drive>,
    pub(crate) model: Arc<dyn contract::model::HostModel>,
    pub(crate) delegate: Arc<dyn contract::extension::HostDelegate>,
    pub(crate) attended: Arc<dyn Fn() -> bool + Send + Sync>,
    pub(crate) answerable: bool,
    pub(crate) log: Arc<log::Log>,
    pub(crate) diag: Arc<log::diag::Diag>,
    pub(crate) names: std::sync::Mutex<Vec<String>>,
    pub(crate) shell_read_only: crate::settings::ShellReadOnly,
    pub(crate) sandbox: crate::settings::SandboxHandle,
    /// Whether the session already reported the sandbox unavailable: set at
    /// startup when the sandbox is enabled and unavailable, and set by the
    /// first reload install that newly reports it, so the notice fires once
    /// per session even when configuration toggles off and on later.
    pub(crate) sandbox_notice_reported: std::sync::atomic::AtomicBool,
    /// The session's servers, kept for the reconnect entry's in-place
    /// restart (`docs/mcp.md`, "Servers, their state and the
    /// per-server commands").
    pub(crate) servers: Arc<mcp::Servers>,
    pub(crate) reconnect_flights: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    /// Test-only record of the `prepare_inner` stage boundaries the
    /// current preparation reached, in order. Production builds have
    /// no such field: the hook below only appends, never pauses or
    /// branches, so a timed-out `fakes::within` worker keeps appending
    /// after the wait expires and the failure read names that.
    #[cfg(test)]
    pub(crate) prepare_progress: std::sync::Mutex<Vec<&'static str>>,
    /// Where restarted servers write binary content and processed
    /// images, as at startup.
    pub(crate) artifacts: std::path::PathBuf,
    pub(crate) images: Arc<dyn contract::images::Images>,
}

/// Whether the startup already reported the sandbox unavailable, mirroring
/// the startup-notice condition: enabled, and failing `check_available`
/// against the startup workspace. A disabled or available sandbox starts
/// unreported, so the first reload that newly breaks it stages the notice.
fn sandbox_notice_reported(
    sources: &config::Sources,
    sandbox: &crate::settings::SandboxHandle,
) -> std::sync::atomic::AtomicBool {
    let reported = {
        let settings = support::lock(sandbox);
        settings.enabled && tools::sandbox_check_available(&settings, &sources.workspace).is_err()
    };
    std::sync::atomic::AtomicBool::new(reported)
}

/// The late preparation skips the sync: no grouping, records,
/// failures or carried prompt sources.
fn empty_sync() -> r#loop::ServerSync {
    r#loop::ServerSync {
        servers: contract::events::ReloadedServers {
            kept: Vec::new(),
            restarted: Vec::new(),
            started: Vec::new(),
            stopped: Vec::new(),
        },
        server_records: Vec::new(),
        server_failures: Vec::new(),
        new_prompts: None,
    }
}

impl State {
    #[allow(
        clippy::too_many_arguments,
        reason = "one session's reload inputs and door wiring"
    )]
    pub(crate) fn new(
        sources: config::Sources,
        template: r#loop::PromptInputs,
        current: Arc<crate::session_extensions::Current>,
        switching: Arc<crate::switch::Switching>,
        tools: &[(String, Arc<dyn contract::tool::Tool>)],
        session: &doors::Session,
        model: Arc<dyn contract::model::HostModel>,
        delegate: Arc<dyn contract::extension::HostDelegate>,
        servers: crate::mcp_servers::SessionServers,
        answerable: bool,
        log: Arc<log::Log>,
        diag: Arc<log::diag::Diag>,
        shell_read_only: crate::settings::ShellReadOnly,
        sandbox: crate::settings::SandboxHandle,
        repository: Arc<std::sync::Mutex<crate::session_extensions::RepositoryScope>>,
        commands_sync: CommandsSync,
    ) -> (
        Self,
        r#loop::LoadApproved,
        Arc<mcp::Servers>,
        mcp::Prompts,
        Vec<contract::events::McpServerFailed>,
    ) {
        // Computed before the literal below moves `sources` and `sandbox`.
        let sandbox_notice_reported = sandbox_notice_reported(&sources, &sandbox);
        let late = Arc::new(std::sync::Mutex::new(Late::default()));
        let servers_handle = Arc::clone(&servers.servers);
        let artifacts = servers.later.artifacts.clone();
        let images = Arc::clone(&servers.later.images);
        let load = record_load(
            servers.later.into_load(
                record_declarations(session.declarer(), late.clone()),
                switching.reloadable().caps,
            ),
            late.clone(),
            repository.clone(),
        );
        let names = tools
            .iter()
            .map(|(_, tool)| tool.definition().name)
            .collect();
        let tools = servers
            .base_tools
            .into_iter()
            .filter(|(owner, tool)| !(owner == "builtin" && tool.definition().name == "web_search"))
            .collect();
        let state = Self {
            sources,
            template,
            current,
            switching,
            tools: std::sync::Mutex::new(tools),
            kept_infos: std::sync::Mutex::new(servers.kept_infos),
            prompt_rows: std::sync::Mutex::new(servers.prompts.commands()),
            late,
            repository,
            declare: session.declarer(),
            replace: session.reloader(),
            commands_sync,
            drive: session.driver(),
            model,
            delegate,
            attended: session.attended(),
            answerable,
            log,
            diag,
            names: std::sync::Mutex::new(names),
            shell_read_only,
            sandbox,
            sandbox_notice_reported,
            servers: servers_handle,
            reconnect_flights: session.reconnect_flights(),
            artifacts,
            images,
            #[cfg(test)]
            prepare_progress: std::sync::Mutex::new(Vec::new()),
        };
        (
            state,
            load,
            servers.servers,
            servers.prompts,
            servers.failed,
        )
    }

    /// The reload preparer and the late preparer: the late one prepares
    /// the same set, but drops every notice the startup already wrote,
    /// so an approval installs the new lines only.
    pub(crate) fn preparers(
        self,
        written: Vec<contract::events::Notice>,
    ) -> (r#loop::PrepareReload, r#loop::PrepareReload) {
        let state = Arc::new(self);
        let reload = {
            let state = state.clone();
            let reads = state.switching.reads();
            preparer(
                move |route| state.prepare_inner(route, Preparation::Explicit),
                move |purpose, work| reads.run(purpose, work),
            )
        };
        let late = {
            let state = state.clone();
            let reads = state.switching.reads();
            preparer(
                move |route| {
                    // The late preparation skips the MCP sync
                    // (`Preparation::Late` below): no slot starts, and no
                    // grouping, records, failures or carried prompt sources.
                    let mut prepared = state.prepare_inner(route, Preparation::Late)?;
                    prepared.notices.retain(|notice| !written.contains(notice));
                    Ok(prepared)
                },
                move |purpose, work| reads.run(purpose, work),
            )
        };
        (reload, late)
    }

    fn prepare_inner(
        self: &Arc<Self>,
        route: &r#loop::ReloadRoute,
        preparation: Preparation,
    ) -> Result<r#loop::ReloadPrepared, Rejection> {
        let failure = |error: Failure| Rejection {
            code: error.code,
            message: error.message,
        };
        let config = Arc::new(
            config::Config::load(config::Sources {
                home: self.sources.home.clone(),
                workspace: self.sources.workspace.clone(),
                project: self.sources.project.clone(),
                overrides: self.sources.overrides.clone(),
            })
            .map_err(|error| Rejection {
                code: error.code(),
                message: error.to_string(),
            })?,
        );
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("config_loaded");
        }
        let now = self.switching.reloadable();
        // The repository set is read again, from the scope's workspace
        // minus the offer's skips: a changed declaration or a skipped
        // content loads, or drops, on reload.
        let scope = support::lock(&self.repository).clone();
        let set = crate::session_extensions::repository_set(
            &self.sources.home,
            &self.sources.project,
            &self.sources.workspace,
            &scope,
            &now.loader.clock,
        );
        let mut extensions = self.current.get().reload_repository(
            &self.sources.home,
            &config,
            now.loader.clock.clone(),
            now.loader.locks.clone(),
            set,
        );
        // A required repository extension that did not load rejects the
        // reload before anything is published: `current` is unchanged.
        crate::session_extensions::required(&extensions).map_err(|error| Rejection {
            code: error.code,
            message: error.message,
        })?;
        // Declared hooks with no extension to run them and any `blocking`
        // entry reject the reload the same way. The gate reads the
        // reloaded session's filtered repository contribution.
        crate::session_extensions::missing_hooks(&config, &extensions).map_err(|error| {
            Rejection {
                code: error.code,
                message: error.message,
            }
        })?;
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("repository_ready");
        }
        if self.diag.debugging() {
            extensions = extensions.with_diagnostics(
                self.diag.timings(),
                self.diag.clone() as Arc<dyn contract::diag::DebugLog>,
            );
        }
        let owners = extensions
            .lua_providers()
            .iter()
            .map(|(extension, lua)| (lua.name().to_owned(), extension.clone()))
            .collect();
        extensions.retain_lua_providers(&[]);
        let extensions = Arc::new(extensions);
        let model = self.switching.resolved(&route.model).map_err(failure)?;
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("model_resolved");
        }
        let mut notices = config.notices().to_vec();
        notices.extend(extensions.notices().iter().cloned());
        let (patch, settings) = crate::parts::reload_inputs(
            &config,
            &self.sources.home,
            &self.sources.project,
            &extensions,
            model.model,
            &route.model,
            &mut notices,
        );
        let mut inputs = self.template.clone();
        inputs.system = patch.system.clone();
        inputs.append = patch.append.clone();
        inputs.extensions = patch.extensions.clone();
        inputs.extension_dirs = patch.extension_dirs.clone();
        inputs.extension_sections = patch.extension_sections.clone();
        inputs.skills_disabled = patch.skills_disabled.clone();
        inputs.deferrals = patch.deferrals.clone();
        inputs.cache_lifetime = patch.cache_lifetime;
        let skills = r#loop::skills(&inputs, &self.sources.workspace);
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("inputs_built");
        }
        // The MCP sync: classify the new person specs against the live
        // slots, stage (re-list keeps, shadow-start replacements and
        // starts), and commit only after every required shadow start
        // succeeded. A required shadow failure rejects the reload with
        // the start's code; the old slots keep serving and the old caches
        // stay byte-identical. No log or declaration write happens here.
        let cache = config::mcp_cache_dir(&self.sources.home);
        let sync_inputs = mcp::SyncInputs {
            workspace: &self.sources.workspace,
            artifacts: &self.artifacts,
            images: &self.images,
            cache: &cache,
            clock: &now.loader.clock,
            version: env!("CARGO_PKG_VERSION"),
        };
        // An explicit reload classifies, stages and commits the MCP
        // sync; the late preparation skips it, so no slot starts, no
        // cache writes, and the grouping, records and failures stay
        // empty.
        let (synced, staged, protected, ready): (
            Option<mcp::Synced>,
            Option<mcp::StagedSync>,
            Vec<String>,
            Vec<String>,
        ) = if preparation == Preparation::Late {
            (None, None, Vec::new(), Vec::new())
        } else {
            let specs = crate::mcp_servers::specs(&config).map_err(failure)?;
            #[cfg(test)]
            {
                self.record_prepare_checkpoint("mcp_specs_loaded");
            }
            notices.extend(specs.notices);
            // Repository servers stay with the offer flow that owns
            // them: never removed while still declared.
            let protected: Vec<String> = specs
                .repository
                .iter()
                .map(|repo| repo.spec.name.clone())
                .collect();
            // Eligible approved repository servers classify like any
            // other server: changed takes the replace path, same-spec
            // healthy re-lists in place, and a server approved
            // in-session starts with its prompt sources joining the
            // `/` list. Repository-unapproved servers are skipped.
            let prepared = crate::mcp_servers::repository_servers::prepare(
                &self.sources.home,
                &self.sources.project,
                scope.workspace.as_ref().unwrap_or(&self.sources.workspace),
                &now.loader.clock,
                &scope.skipped,
                specs.repository,
            );
            if let Some(failed) = prepared.required_failed {
                return Err(Rejection {
                    code: contract::ErrorCode::McpRequiredServerFailed,
                    message: failed.error.message,
                });
            }
            for failed in prepared.failed {
                notices.push(contract::events::Notice {
                    code: failed.error.code,
                    message: failed.error.message,
                    extension: None,
                });
            }
            let ready: Vec<String> = prepared
                .ready
                .iter()
                .map(|spec| spec.name.clone())
                .collect();
            #[cfg(test)]
            {
                self.record_prepare_checkpoint("mcp_repository_prepared");
            }
            let mut sync_specs = specs.specs;
            sync_specs.extend(prepared.ready);
            let mut rows = mcp::classify_sync(sync_specs, &self.servers);
            rows.retain(
                |row| !matches!(row, mcp::SyncRow::Remove { name } if protected.contains(name)),
            );
            #[cfg(test)]
            {
                self.record_prepare_checkpoint("mcp_classified");
            }
            let staged =
                mcp::stage_sync(rows, &self.servers, &sync_inputs).map_err(|error| Rejection {
                    code: error.code,
                    message: error.message,
                })?;
            #[cfg(test)]
            {
                self.record_prepare_checkpoint("mcp_staged");
            }
            // The plan is pure: slots swap and caches write only in
            // the installed hook below, so no fallible step after
            // the plan can leave a half-applied reload.
            let planned = mcp::plan_sync(&staged, &self.servers, &sync_inputs);
            #[cfg(test)]
            {
                self.record_prepare_checkpoint("mcp_planned");
            }
            (Some(planned), Some(staged), protected, ready)
        };
        // Every shadow source joins the carried rows: a started
        // server's rows are new, and a replaced server's rows ride
        // along so `loop` routes those tags to the replacement source
        // — a kept `/name` row keeps its metadata but answers through
        // the new slot. Every row the session already lists stays, and
        // kept servers refresh the cache file only. The baseline is the
        // session's current rows — startup plus every earlier reload's
        // additions — so a second reload keeps the first reload's rows
        // for kept servers.
        let baseline = support::lock(&self.prompt_rows).clone();
        let mut new_prompts = Vec::new();
        if let Some(synced) = &synced {
            new_prompts.extend(synced.prompt_sources.commands());
        }
        // The carried sources answer the new rows through the same path
        // as a startup row: `loop` routes the carried tags to the fetch
        // the sync bundles with the rows below.
        let mut prompt_rows = baseline;
        r#loop::ServerSync::merge_command_rows(&mut prompt_rows, &new_prompts);
        let listed = r#loop::commands(
            &inputs,
            &self.sources.workspace,
            &prompt_rows,
            &extensions.commands(),
        );
        notices.extend(listed.notices);
        // An explicit reload rebuilds the MCP set from the sync outcome;
        // the late preparation keeps the session-held set, so no slot,
        // tool, cache or prompt row changes.
        let (mut tools, mut infos): (Tools, Vec<contract::events::ToolInfo>) = match &synced {
            Some(_) => {
                let held_tools = support::lock(&self.tools);
                let tools: Tools = held_tools
                    .iter()
                    .filter(|(owner, tool)| {
                        // The stale `mcp_resources` never carries: the sync
                        // re-adds it below when a post-sync slot offers
                        // resources, and dropping it here undeclares the stale
                        // one when none does. Synced repository servers
                        // declare fresh from the sync outcome below, so only
                        // unsynced ones (unapproved or failed) keep theirs.
                        (owner == "builtin" && tool.definition().name != mcp::RESOURCES_TOOL)
                            || (protected.contains(owner) && !ready.contains(owner))
                    })
                    .cloned()
                    .collect();
                let mut infos = support::lock(&self.kept_infos).clone();
                infos.retain(|info| {
                    info.name != "web_search"
                        && info.name != mcp::RESOURCES_TOOL
                        && match &info.source {
                            ToolSource::Builtin => true,
                            ToolSource::Mcp { server, .. } => {
                                protected.contains(server) && !ready.contains(server)
                            }
                            ToolSource::Extension { .. } => false,
                        }
                });
                (tools, infos)
            }
            None => (
                support::lock(&self.tools).clone(),
                support::lock(&self.kept_infos).clone(),
            ),
        };
        if let Some((tool, info)) = crate::builtin::web_search(
            model.model.web_search.as_deref(),
            extensions.search_backend().as_ref(),
        )
        .map_err(failure)?
        {
            tools.push(("builtin".into(), tool));
            infos.push(info);
        }
        if let Some(synced) = &synced {
            tools.extend(
                synced
                    .tools
                    .iter()
                    .map(|(owner, tool, _)| (owner.clone(), Arc::clone(tool))),
            );
            infos.extend(synced.tools.iter().map(|(_, _, info)| info.clone()));
        }
        // The session-held MCP set the install below publishes: the
        // next preparation rebuilds from it, so a late approval after a
        // replacing reload keeps the replacement's tools. The
        // `web_search` and extension tools below stay out: each
        // preparation adds its own.
        let stored_tools = tools.clone();
        let stored_infos = infos.clone();
        // MCP registrations follow built-ins, so they keep their startup precedence.
        tools.sort_by_key(|(owner, _)| owner != "builtin");
        crate::mcp_servers::add_extension_tools(&mut tools, &mut infos, extensions.tools())
            .map_err(failure)?;
        let caps = crate::settings::result_caps(&config);
        let mut tools = r#loop::capped(tools, &caps);
        {
            let late = support::lock(&self.late);
            for (owner, tool) in &late.tools {
                let name = tool.definition().name;
                if !tools.iter().any(|(_, held)| held.definition().name == name) {
                    tools.push((owner.clone(), tool.clone()));
                    if let Some(info) = late.infos.get(&name) {
                        infos.push(info.clone());
                    }
                }
            }
        }
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("tools_rebuilt");
        }
        let here = crate::Here {
            workspace: self.sources.workspace.clone(),
            clock: now.loader.clock.clone(),
            cache_diagnostics: config.flag("cache.diagnostics", None).unwrap_or_default(),
        };
        let (reviewer, reviewer_observe, credential_files, publish) =
            self.switching.reviewer_for(route, &config, &here);
        // A reload that newly makes the sandbox unavailable stages the
        // startup-shaped notice, but only on the first transition for the
        // session: prepare computes the candidate settings with the reloaded
        // credential files, and the install below applies them and sets the
        // latch, so later toggles emit nothing.
        let staged_sandbox_notice: Option<contract::events::Notice> = (|| {
            let fiber_home = support::lock(&self.sandbox).fiber_home.clone();
            let candidate =
                crate::settings::sandbox_settings(&config, &fiber_home, &credential_files);
            if !candidate.enabled {
                return None;
            }
            let unavailable =
                tools::sandbox_check_available(&candidate, &self.sources.workspace).err()?;
            if self
                .sandbox_notice_reported
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return None;
            }
            Some(contract::events::Notice {
                code: unavailable.code(),
                message: format!("sandbox unavailable: {unavailable}"),
                extension: None,
            })
        })();
        if let Some(notice) = &staged_sandbox_notice {
            notices.push(notice.clone());
        }
        let sandbox_credentials = credential_files.clone();
        let hooks: Arc<dyn contract::hook::Hooks> = extensions.clone();
        let loaded = extensions.loaded();
        // Owned inputs for the installed hook's apply: the hook runs
        // after `next` moves the loader below, so the clock is cloned
        // while it still borrows.
        let apply_workspace = self.sources.workspace.clone();
        let apply_artifacts = self.artifacts.clone();
        let apply_images = Arc::clone(&self.images);
        let apply_cache = cache.clone();
        let apply_clock = Arc::clone(&now.loader.clock);
        let next = crate::switch::Reloadable {
            config,
            caps,
            backend: extensions.search_backend(),
            hosted_stands: crate::switch::hosted_stands(&tools),
            loader: crate::switch::Loader {
                extensions: extensions.clone(),
                owners,
                ..now.loader
            },
        };
        let shared = self.clone();
        let carried_prompts = new_prompts.clone();
        // Whether the installed set declares `mcp_resources`: the install
        // aligns the session marker with it, so a later in-session
        // repository approval neither offers a second registration scoped
        // to its own servers nor suppresses the needed one.
        let declares_resources = infos.iter().any(|info| info.name == mcp::RESOURCES_TOOL);
        let installed = Box::new(move || {
            // The staged sync applies here, before any declaration:
            // nothing fallible runs after the shadows become live.
            if let Some(staged) = staged {
                let apply_inputs = mcp::SyncInputs {
                    workspace: &apply_workspace,
                    artifacts: &apply_artifacts,
                    images: &apply_images,
                    cache: &apply_cache,
                    clock: &apply_clock,
                    version: env!("CARGO_PKG_VERSION"),
                };
                mcp::apply_sync(staged, &shared.servers, &apply_inputs);
            }
            // The install publishes the session-held MCP set the next
            // preparation rebuilds from, so a late approval after a
            // replacing reload keeps the replacement's tools.
            {
                let mut held_tools = support::lock(&shared.tools);
                *held_tools = stored_tools;
                let mut held_infos = support::lock(&shared.kept_infos);
                *held_infos = stored_infos;
            }
            // The session's current rows grow by this reload's additions,
            // so the next preparation rebuilds from them: rows the session
            // already lists stay, and only new names are appended, while
            // the fetch below still routes every carried tag — including
            // replaced ones — to the replacement source.
            {
                let mut rows = support::lock(&shared.prompt_rows);
                r#loop::ServerSync::merge_command_rows(&mut rows, &carried_prompts);
            }
            extensions.emit_to(Arc::new(log::WeakEmit::new(&shared.log)));
            extensions.drive_to(shared.drive.clone());
            extensions.model_to(shared.model.clone());
            extensions.delegate_to(shared.delegate.clone());
            extensions.answerable(shared.answerable);
            extensions.browse_with(
                Arc::new(extensions::SystemBrowser::default()),
                shared.attended.clone(),
            );
            // The install's rows are what the session answers now: the
            // next turn-start check measures the runnable set against
            // them, even when a check past the install already ran.
            (shared.commands_sync)(&listed.runnable);
            (shared.replace)(doors::Replacement {
                commands: listed.rows,
                skills,
                door: Some(extensions.clone()),
                hooks: Some(extensions.clone()),
            });
            let next_names: Vec<_> = infos.iter().map(|info| info.name.clone()).collect();
            let old_names =
                std::mem::replace(&mut *support::lock(&shared.names), next_names.clone());
            for name in old_names {
                if !next_names.contains(&name) {
                    (shared.declare)(&name, None);
                }
            }
            for info in infos {
                let name = info.name.clone();
                (shared.declare)(&name, Some(info));
            }
            shared.servers.set_resources_declared(declares_resources);
            publish();
            crate::settings::refresh_shell_read_only(&shared.shell_read_only, &next.config);
            crate::settings::refresh_sandbox(&shared.sandbox, &next.config, &sandbox_credentials);
            if staged_sandbox_notice.is_some() {
                shared
                    .sandbox_notice_reported
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            shared.switching.reloaded(next);
            shared.current.swap(extensions);
        });
        #[cfg(test)]
        {
            self.record_prepare_checkpoint("prepared");
        }
        Ok(r#loop::ReloadPrepared {
            prompt: patch,
            tools,
            extensions: loaded,
            notices,
            server_sync: match synced {
                Some(synced) => r#loop::ServerSync {
                    servers: synced.servers,
                    server_records: synced.records,
                    server_failures: synced.failed,
                    // Rows and fetch travel as one value, so new rows
                    // without their fetch cannot be represented: `None`
                    // when the sync carried no new rows.
                    new_prompts: if new_prompts.is_empty() {
                        None
                    } else {
                        let sources = synced.prompt_sources.clone();
                        Some((
                            new_prompts,
                            Arc::new(move |server, prompt, text, cancel| {
                                sources.get(server, prompt, text, cancel)
                            }),
                        ))
                    },
                },
                // The late preparation skips the sync: no grouping,
                // records, failures or carried prompt sources.
                None => empty_sync(),
            },
            hooks: Some(hooks),
            reviewer,
            reviewer_observe,
            credential_files,
            settings,
            installed,
        })
    }
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod tests;
