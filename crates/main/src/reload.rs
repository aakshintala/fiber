//! Preparation and publication of configuration reloads (`docs/configuration.md`).
//! Plus the hub side of the MCP driver commands: the server-rows reader
//! and the login and reconnect entries (`docs/mcp.md`, "Servers, their
//! state and the per-server commands").
use contract::diag::DebugLog as _;
use contract::inbox::Rejection;
use contract::shapes::Failure;
use extensions::Browser as _;
use std::collections::HashMap;
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

/// The hub side of the MCP driver commands, installed where the session
/// installs its other hub providers (`docs/mcp.md`, "Servers, their
/// state and the per-server commands").
pub(crate) struct McpProviders {
    /// The `mcp_servers` answer's reader.
    pub refresh: doors::ServersRefresh,
    /// The `mcp_login` runner, guarding its own in-flight map.
    pub login: doors::McpLogin,
    /// The `mcp_reconnect` entry, merging its own registry news.
    pub reconnect: r#loop::ReconnectServer,
}

impl State {
    /// Builds the MCP hub providers from the session's own configuration,
    /// servers, declaration writer and log: the same inputs the startup
    /// used, so queries, logins and reconnects agree with it.
    pub(crate) fn mcp_providers(&self) -> McpProviders {
        let waiting = login_waiting();
        McpProviders {
            refresh: servers_refresh(
                self.sources.clone(),
                Arc::clone(&self.servers),
                Arc::downgrade(&self.log),
                self.log.clock().clone(),
                Arc::clone(&waiting),
            ),
            login: mcp_login(self.sources.clone(), waiting, self.log.clock().clone()),
            reconnect: reconnect_server(
                self.sources.clone(),
                Arc::clone(&self.servers),
                self.declare.clone(),
                self.artifacts.clone(),
                Arc::clone(&self.images),
                self.log.clock().clone(),
            ),
        }
    }
}

/// The login in-flight map: one entry per server with a login running,
/// holding its authorization URL while it waits for the callback. One
/// map per provider call keeps each test's logins apart; the session
/// shares the one its providers close over.
fn login_waiting() -> Arc<std::sync::Mutex<HashMap<String, Option<String>>>> {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

/// Reads the `mcp_servers` answer from the tools answer's rows: one row
/// per configured server, in configuration order, folding the session's
/// own log so a mid-session death reads failed exactly as the startup
/// notice did (`docs/mcp.md`, "Servers, their state and the per-server
/// commands").
fn servers_refresh(
    sources: config::Sources,
    servers: Arc<mcp::Servers>,
    log: std::sync::Weak<log::Log>,
    clock: Arc<dyn contract::clock::Clock>,
    waiting: Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
) -> doors::ServersRefresh {
    Arc::new(move |tools: Vec<contract::events::ToolInfo>| {
        let declared = cli::declared_servers(
            &sources.home,
            &sources.workspace,
            &clock,
            &sources.overrides,
        )
        .map_err(Failure::from)?;
        let mut failed: HashMap<String, contract::events::McpServerFailed> = HashMap::new();
        if let Some(log) = log.upgrade() {
            let count = usize::try_from(log.count()).unwrap_or(usize::MAX);
            let envelopes = log.range(0, count).map_err(|error| Failure {
                code: error.code(),
                message: error.to_string(),
                retry_after_ms: None,
                provider: None,
            })?;
            for envelope in &envelopes {
                let event =
                    contract::events::Event::from_envelope(envelope).map_err(|error| Failure {
                        code: contract::ErrorCode::LogCorrupt,
                        message: error.to_string(),
                        retry_after_ms: None,
                        provider: None,
                    })?;
                match event {
                    Some(contract::events::Event::McpServerFailed(failed_server)) => {
                        failed.insert(failed_server.server.clone(), failed_server);
                    }
                    Some(contract::events::Event::McpServerReady(ready)) => {
                        failed.remove(&ready.server);
                    }
                    _ => {}
                }
            }
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for info in &tools {
            if let contract::events::ToolSource::Mcp { server, .. } = &info.source {
                *counts.entry(server).or_default() += 1;
            }
        }
        let waiting = waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows: Vec<contract::events::McpServerRow> = declared
            .iter()
            .map(|declared| {
                let count = || {
                    let count = counts
                        .get(declared.name.as_str())
                        .copied()
                        .unwrap_or_default();
                    u64::try_from(count).unwrap_or(u64::MAX)
                };
                let (state, reason, tools) = if servers.running(&declared.name) {
                    (
                        contract::events::McpServerState::Ready,
                        String::new(),
                        Some(count()),
                    )
                } else {
                    match failed.get(&declared.name) {
                        Some(failed)
                            if failed.reason == contract::events::ServerFailure::NotLoggedIn =>
                        {
                            (
                                contract::events::McpServerState::NeedsLogin,
                                failed.error.message.clone(),
                                Some(count()),
                            )
                        }
                        Some(failed) => (
                            contract::events::McpServerState::Failed,
                            failed.error.message.clone(),
                            Some(count()),
                        ),
                        None => (
                            contract::events::McpServerState::Connecting,
                            String::new(),
                            None,
                        ),
                    }
                };
                contract::events::McpServerRow {
                    name: declared.name.clone(),
                    layer: match declared.layer {
                        config::Layer::Global => contract::events::McpServerLayer::Global,
                        config::Layer::Project => contract::events::McpServerLayer::Project,
                        config::Layer::Repository => contract::events::McpServerLayer::Repository,
                    },
                    state,
                    reason,
                    tools,
                    approved: declared.approved,
                    auth_url: waiting.get(&declared.name).cloned().flatten(),
                }
            })
            .collect();
        Ok(rows)
    })
}

/// Runs one server's `fiber mcp login` flow on the login thread the door
/// spawns: the authorization URL the flow presents lands in the shared
/// waiting map, so the `mcp_servers` answer shows `auth_url` while it
/// waits, and the stored token is picked up by the reconnect the thread
/// delivers before it answers (`docs/mcp.md`, "Servers, their state and
/// the per-server commands").
fn mcp_login(
    sources: config::Sources,
    waiting: Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
    clock: Arc<dyn contract::clock::Clock>,
) -> doors::McpLogin {
    Arc::new(
        move |server: &str, deliver: Arc<dyn Fn(contract::inbox::Delivery) + Send + Sync>| {
            {
                let mut waiting = waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if waiting.contains_key(server) {
                    return Err(Failure {
                        code: contract::ErrorCode::Busy,
                        message: format!(
                            "A login for `{server}` is already in flight; answer its browser prompt first."
                        ),
                        retry_after_ms: None,
                        provider: None,
                    });
                }
                waiting.insert(server.to_owned(), None);
            }
            let finish = |waiting: &Arc<std::sync::Mutex<HashMap<String, Option<String>>>>| {
                waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(server);
            };
            let url = match cli::login_target(
                &sources.home,
                &sources.workspace,
                server,
                &|| {},
                &clock,
                &sources.overrides,
            ) {
                Ok(url) => url,
                Err(error) => {
                    finish(&waiting);
                    return Err(login_target_failure(error));
                }
            };
            if mcp::url_has_userinfo(&url) {
                finish(&waiting);
                return Err(Failure::from(cli::CliError::UrlHasUserinfo {
                    server: server.to_owned(),
                }));
            }
            let files = config::McpTokenFiles::new(sources.home.clone());
            let present = |auth_url: &str| {
                waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(server.to_owned(), Some(auth_url.to_owned()));
                extensions::SystemBrowser::default().open(auth_url);
            };
            let outcome = mcp::login(&files, &url, &present, clock.as_ref());
            waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(server.to_owned(), None);
            if let Err(error) = outcome {
                finish(&waiting);
                return Err(Failure {
                    code: error.code(),
                    message: error.to_string(),
                    retry_after_ms: None,
                    provider: None,
                });
            }
            let (tx, rx) = std::sync::mpsc::channel::<contract::inbox::Answer>();
            deliver(contract::inbox::Delivery::McpReconnect(
                contract::commands::McpServerArgs {
                    server: server.to_owned(),
                },
                contract::inbox::Ack(Box::new(move |answer| {
                    drop(tx.send(answer));
                })),
            ));
            // The reconnect serves the login's token; a turn in progress
            // rejects it, and the server starts on its first call
            // instead. Either way the login itself succeeded. Every
            // delivery path answers — a loop that cannot has aborted or
            // is exiting with the process — so the wait ends with the
            // answer, or with a dropped acknowledgement nobody needs.
            let answer = rx.recv();
            finish(&waiting);
            match answer {
                Err(_) => Ok(()),
                Ok(Ok(_)) => Ok(()),
                Ok(Err(rejection)) if rejection.code == contract::ErrorCode::Busy => Ok(()),
                Ok(Err(rejection)) => Err(Failure {
                    code: rejection.code,
                    message: rejection.message,
                    retry_after_ms: None,
                    provider: None,
                }),
            }
        },
    )
}

/// Maps a login target failure to the hub's answer: an undeclared or
/// stdio server is not runnable through a login, so `invalid_arguments`
/// carries the CLI's own sentence; approval and configuration failures
/// keep the CLI's codes.
fn login_target_failure(error: cli::CliError) -> Failure {
    if matches!(
        error,
        cli::CliError::McpServerUnknown { .. } | cli::CliError::McpServerStdio { .. }
    ) {
        Failure {
            code: contract::ErrorCode::InvalidArguments,
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    } else {
        Failure::from(error)
    }
}

/// Restarts one failed server for the `mcp_reconnect` command: declared
/// and approved first, through the same checks the CLI's `mcp login`
/// shares, then served in place when the session started it, or adopted
/// from a fresh single-server start when it never did. The declaration
/// writer moves with the restart, so the `mcp_servers` answer agrees
/// with the registry. Both the adopted start and the declared snapshot
/// load the configuration with the session's own overrides, as a reload
/// does (`docs/mcp.md`, "Servers, their state and the per-server
/// commands").
fn reconnect_server(
    sources: config::Sources,
    servers: Arc<mcp::Servers>,
    declare: doors::Declare,
    artifacts: std::path::PathBuf,
    images: Arc<dyn contract::images::Images>,
    clock: Arc<dyn contract::clock::Clock>,
) -> r#loop::ReconnectServer {
    Arc::new(move |server: &str| {
        let (_, project) =
            cli::project_of(&sources.home, &sources.workspace).map_err(|failure| Rejection {
                code: failure.code,
                message: failure.message,
            })?;
        let config = config::Config::load(config::Sources {
            home: sources.home.clone(),
            workspace: sources.workspace.clone(),
            project,
            overrides: sources.overrides.clone(),
        })
        .map_err(|error| Rejection {
            code: error.code(),
            message: error.to_string(),
        })?;
        let declared = cli::declared_servers(
            &sources.home,
            &sources.workspace,
            &clock,
            &sources.overrides,
        )
        .map_err(|error| {
            let failure = Failure::from(error);
            Rejection {
                code: failure.code,
                message: failure.message,
            }
        })?;
        let Some(row) = declared.iter().find(|declared| declared.name == server) else {
            let failure = login_target_failure(cli::CliError::McpServerUnknown {
                server: server.to_owned(),
            });
            return Err(Rejection {
                code: failure.code,
                message: failure.message,
            });
        };
        if !row.approved {
            let failure = Failure::from(cli::CliError::McpUnapproved {
                server: server.to_owned(),
            });
            return Err(Rejection {
                code: failure.code,
                message: failure.message,
            });
        }
        let (used, removed, records) = if servers.has_slot(server) {
            let Some(restarted) = mcp::restart(&servers, server, &artifacts, &images) else {
                return Err(Rejection {
                    code: contract::ErrorCode::InvalidArguments,
                    message: format!(
                        "The MCP server `{server}` stopped before its reconnect ran; try it again."
                    ),
                });
            };
            (restarted.used, restarted.removed, restarted.records)
        } else {
            adopt_server(
                &config, &sources, &servers, server, &artifacts, &images, &clock,
            )?
        };
        for name in &removed {
            declare(name.as_str(), None);
        }
        for (_, tool, info) in &used {
            declare(&tool.definition().name, Some(info.clone()));
        }
        Ok(r#loop::Reconnected {
            tools: used
                .into_iter()
                .map(|(owner, tool, _)| (owner, tool))
                .collect(),
            removed,
            records,
        })
    })
}

/// What a reconnect adopted and restarted: the loop registry's news
/// and the log's lines.
type Adopted = (
    Vec<(
        String,
        Arc<dyn contract::tool::Tool>,
        contract::events::ToolInfo,
    )>,
    Vec<String>,
    Vec<contract::tool::ServerRecord>,
);

/// Starts the one server the session never started and adopts its slot,
/// so the restart serves it exactly once: a second failure stays a
/// failure the log carries, and a success declares its tools. A declared
/// server with nothing runnable, or a repository server the offer flow
/// owns, is `invalid_arguments`, not a start.
#[allow(
    clippy::too_many_arguments,
    reason = "one server's startup inputs, as at session start"
)]
fn adopt_server(
    config: &config::Config,
    sources: &config::Sources,
    servers: &Arc<mcp::Servers>,
    server: &str,
    artifacts: &std::path::Path,
    images: &Arc<dyn contract::images::Images>,
    clock: &Arc<dyn contract::clock::Clock>,
) -> Result<Adopted, Rejection> {
    let invalid = |message: String| Rejection {
        code: contract::ErrorCode::InvalidArguments,
        message,
    };
    let specs = crate::mcp_servers::specs(config).map_err(|failure| Rejection {
        code: failure.code,
        message: failure.message,
    })?;
    if specs.repository.iter().any(|spec| spec.spec.name == server) {
        return Err(invalid(format!(
            "The MCP server `{server}` is a repository server: the session starts it with its repository, not through a reconnect."
        )));
    }
    let Some(spec) = specs.specs.iter().find(|spec| spec.name == server).cloned() else {
        return Err(invalid(format!(
            "The MCP server `{server}` has nothing runnable to start; fix its declaration and reload."
        )));
    };
    let started = mcp::start_observed(
        vec![spec],
        &sources.workspace,
        artifacts,
        Arc::clone(images),
        &config::mcp_cache_dir(&sources.home),
        clock,
        env!("CARGO_PKG_VERSION"),
        servers.timing(),
    );
    let records = started
        .failed
        .into_iter()
        .map(contract::tool::ServerRecord::Failed)
        .collect();
    let used = started
        .tools
        .into_iter()
        .filter_map(|(owner, tool)| {
            started
                .infos
                .iter()
                .find(|info| info.name == tool.definition().name)
                .cloned()
                .map(|info| (owner, tool, info))
        })
        .collect();
    servers.adopt(started.servers);
    Ok((used, Vec::new(), records))
}
