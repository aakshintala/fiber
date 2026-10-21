//! Preparation and publication of configuration reloads (`docs/configuration.md`).
use contract::diag::DebugLog as _;
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

pub(crate) struct State {
    pub(crate) sources: config::Sources,
    pub(crate) template: r#loop::PromptInputs,
    pub(crate) current: Arc<crate::session_extensions::Current>,
    pub(crate) switching: Arc<crate::switch::Switching>,
    pub(crate) tools: Tools,
    pub(crate) kept_infos: Vec<contract::events::ToolInfo>,
    pub(crate) server_names: Vec<String>,
    pub(crate) prompt_rows: Vec<contract::events::CommandInfo>,
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
            server_names: servers.server_names,
            prompt_rows: servers.prompts.commands(),
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
                move |route| state.prepare(route),
                move |purpose, work| reads.run(purpose, work),
            )
        };
        let late = {
            let state = state.clone();
            let reads = state.switching.reads();
            preparer(
                move |route| {
                    let mut prepared = state.prepare(route)?;
                    prepared.notices.retain(|notice| !written.contains(notice));
                    Ok(prepared)
                },
                move |purpose, work| reads.run(purpose, work),
            )
        };
        (reload, late)
    }

    fn prepare(
        self: &Arc<Self>,
        route: &r#loop::ReloadRoute,
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
        // Declared hooks with no extension to run them and any `blocking`
        // entry reject the reload the same way.
        crate::session_extensions::missing_hooks(&config, &extensions).map_err(|error| {
            Rejection {
                code: error.code,
                message: error.message,
            }
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
        let listed = r#loop::commands(
            &inputs,
            &self.sources.workspace,
            &self.prompt_rows,
            &extensions.commands(),
        );
        let skills = r#loop::skills(&inputs, &self.sources.workspace);
        let mut notices = config.notices().to_vec();
        notices.extend(extensions.notices().iter().cloned());
        notices.extend(listed.notices);
        let mut tools = self.tools.clone();
        let mut infos = self.kept_infos.clone();
        infos.retain(|info| info.name != "web_search");
        if let Some((tool, info)) = crate::builtin::web_search(
            model.model.web_search.as_deref(),
            extensions.search_backend().as_ref(),
        )
        .map_err(failure)?
        {
            tools.push(("builtin".into(), tool));
            infos.push(info);
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
        let installed = Box::new(move || {
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
            servers: self.server_names.clone(),
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
