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
            let mut held = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        late.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tools
            .extend(approved.tools.clone());
        // The offer's skips join the scope the next preparation reads:
        // skipped content stays out of a reloaded or late set.
        repository
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .skipped = outcome.skipped.clone();
        Ok(approved)
    })
}

/// Which preparation `prepare_inner` runs: an explicit reload
/// classifies, stages and commits the MCP sync, while the late
/// extension-approval preparation skips it, so no slot starts and the
/// grouping, records, failures and carried prompt sources stay empty.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preparation {
    /// An explicit reload: run the MCP sync.
    Explicit,
    /// The late extension-approval preparation: skip the MCP sync.
    Late,
}

pub(crate) struct State {
    pub(crate) sources: config::Sources,
    pub(crate) template: r#loop::PromptInputs,
    pub(crate) current: Arc<crate::session_extensions::Current>,
    pub(crate) switching: Arc<crate::switch::Switching>,
    pub(crate) tools: Tools,
    pub(crate) kept_infos: Vec<contract::events::ToolInfo>,
    pub(crate) prompt_rows: std::sync::Mutex<Vec<contract::events::CommandInfo>>,
    pub(crate) late: Arc<std::sync::Mutex<Late>>,
    /// The repository scope the next preparation reads: the launch
    /// workspace, or the recorded one on resume, and the content the
    /// offer skipped.
    pub(crate) repository: Arc<std::sync::Mutex<crate::session_extensions::RepositoryScope>>,
    pub(crate) declare: doors::Declare,
    pub(crate) replace: Arc<dyn Fn(doors::Replacement) + Send + Sync>,
    pub(crate) drive: Arc<dyn contract::extension::Drive>,
    pub(crate) model: Arc<dyn contract::model::HostModel>,
    pub(crate) delegate: Arc<dyn contract::extension::HostDelegate>,
    pub(crate) attended: Arc<dyn Fn() -> bool + Send + Sync>,
    pub(crate) answerable: bool,
    pub(crate) log: Arc<log::Log>,
    pub(crate) diag: Arc<log::diag::Diag>,
    pub(crate) names: std::sync::Mutex<Vec<String>>,
    pub(crate) shell_read_only: crate::settings::ShellReadOnly,
    /// The session's servers, kept for the reconnect entry's in-place
    /// restart (`docs/mcp.md`, "Servers, their state and the
    /// per-server commands").
    pub(crate) servers: Arc<mcp::Servers>,
    /// Where restarted servers write binary content and processed
    /// images, as at startup.
    pub(crate) artifacts: std::path::PathBuf,
    pub(crate) images: Arc<dyn contract::images::Images>,
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
        repository: Arc<std::sync::Mutex<crate::session_extensions::RepositoryScope>>,
    ) -> (
        Self,
        r#loop::LoadApproved,
        Arc<mcp::Servers>,
        mcp::Prompts,
        Vec<contract::events::McpServerFailed>,
    ) {
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
            tools,
            kept_infos: servers.kept_infos,
            prompt_rows: std::sync::Mutex::new(servers.prompts.commands()),
            late,
            repository,
            declare: session.declarer(),
            replace: session.reloader(),
            drive: session.driver(),
            model,
            delegate,
            attended: session.attended(),
            answerable,
            log,
            diag,
            names: std::sync::Mutex::new(names),
            shell_read_only,
            servers: servers_handle,
            artifacts,
            images,
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
        use std::sync::PoisonError;
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
        let now = self.switching.reloadable();
        // The repository set is read again, from the scope's workspace
        // minus the offer's skips: a changed declaration or a skipped
        // content loads, or drops, on reload.
        let scope = self
            .repository
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
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
        let (patch, settings) = crate::parts::reload_inputs(
            &config,
            &self.sources.home,
            &self.sources.project,
            &extensions,
            model.model,
            &route.model,
        );
        let mut inputs = self.template.clone();
        inputs.system = patch.system.clone();
        inputs.append = patch.append.clone();
        inputs.extensions = patch.extensions.clone();
        inputs.extension_dirs = patch.extension_dirs.clone();
        inputs.extension_sections = patch.extension_sections.clone();
        inputs.skills_disabled = patch.skills_disabled.clone();
        inputs.cache_lifetime = patch.cache_lifetime;
        let skills = r#loop::skills(&inputs, &self.sources.workspace);
        let mut notices = config.notices().to_vec();
        notices.extend(extensions.notices().iter().cloned());
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
        let (synced, protected, ready): (Option<mcp::Synced>, Vec<String>, Vec<String>) =
            if preparation == Preparation::Late {
                (None, Vec::new(), Vec::new())
            } else {
                let specs = crate::mcp_servers::specs(&config).map_err(failure)?;
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
                // `/` list. Unapproved stay skipped exactly as before.
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
                let mut sync_specs = specs.specs;
                sync_specs.extend(prepared.ready);
                let mut rows = mcp::classify_sync(sync_specs, &self.servers);
                rows.retain(
                    |row| !matches!(row, mcp::SyncRow::Remove { name } if protected.contains(name)),
                );
                let staged =
                    mcp::stage_sync(rows, &self.servers, &sync_inputs).map_err(|error| {
                        Rejection {
                            code: error.code,
                            message: error.message,
                        }
                    })?;
                (
                    Some(mcp::commit_sync(staged, &self.servers, &sync_inputs)),
                    protected,
                    ready,
                )
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
        let baseline = self
            .prompt_rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
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
                let tools: Tools = self
                    .tools
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
                let mut infos = self.kept_infos.clone();
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
            None => (self.tools.clone(), self.kept_infos.clone()),
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
        // MCP registrations follow built-ins, so they keep their startup precedence.
        tools.sort_by_key(|(owner, _)| owner != "builtin");
        crate::mcp_servers::add_extension_tools(&mut tools, &mut infos, extensions.tools())
            .map_err(failure)?;
        let caps = crate::settings::result_caps(&config);
        let mut tools = r#loop::capped(tools, &caps);
        {
            let late = self.late.lock().unwrap_or_else(PoisonError::into_inner);
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
        let here = crate::Here {
            workspace: self.sources.workspace.clone(),
            clock: now.loader.clock.clone(),
            cache_diagnostics: config.flag("cache.diagnostics", None).unwrap_or_default(),
        };
        let (reviewer, reviewer_observe, credential_files, publish) =
            self.switching.reviewer_for(route, &config, &here);
        let hooks: Arc<dyn contract::hook::Hooks> = extensions.clone();
        let loaded = extensions.loaded();
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
            // The session's current rows grow by this reload's additions,
            // so the next preparation rebuilds from them: rows the session
            // already lists stay, and only new names are appended, while
            // the fetch below still routes every carried tag — including
            // replaced ones — to the replacement source.
            {
                let mut rows = shared
                    .prompt_rows
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
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
            (shared.replace)(doors::Replacement {
                commands: listed.rows,
                skills,
                door: Some(extensions.clone()),
                hooks: Some(extensions.clone()),
            });
            let next_names: Vec<_> = infos.iter().map(|info| info.name.clone()).collect();
            let old_names = std::mem::replace(
                &mut *shared.names.lock().unwrap_or_else(PoisonError::into_inner),
                next_names.clone(),
            );
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
            shared.switching.reloaded(next);
            shared.current.swap(extensions);
        });
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
