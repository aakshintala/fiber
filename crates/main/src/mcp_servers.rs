//! MCP servers from configuration (`docs/configuration.md`, "MCP
//! servers"): each `mcp.servers` entry becomes an [`mcp::ServerSpec`], with
//! the documented defaults: `command` without `url` is a stdio server,
//! `url` without `command` a remote one. A server the repository declares
//! starts only when its current content is approved (`docs/mcp.md`, "A
//! repository's servers"): it runs from its pinned copy in Fiber home,
//! and one approved in the session's own offer starts before that
//! session's first model request. A server with both `command` and `url`
//! is never started: a `notice` with code `config_invalid` says so, and
//! a `required` one fails the session instead. A server with neither is
//! skipped silently: there is nothing to start.

pub(crate) mod repository_servers;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Source};
use contract::ErrorCode;
use contract::diag::DebugLog as _;
use contract::events::{McpServerFailed, Notice, ToolInfo, ToolSource};
use contract::inbox::Rejection;
use contract::shapes::Failure;
use contract::tool::Tool;
use extensions::Browser as _;
use serde_json::Value;

/// What `main` starts: the specs and the skip notices for the log.
#[derive(Default)]
pub(crate) struct Specs {
    /// One per stdio server to start.
    pub specs: Vec<mcp::ServerSpec>,
    /// One per repository server: approved ones start from their pinned
    /// copies, the rest wait for the post-offer load.
    pub repository: Vec<RepositorySpec>,
    /// Notices for the log; startup adds its own. A skipped repository
    /// server is named by the repository's offer instead.
    pub notices: Vec<Notice>,
}

/// A repository's server: the merged spec with the values the repository
/// supplied beside it.
pub(crate) struct RepositorySpec {
    /// The spec from the merged configuration, with `run: None`.
    pub spec: mcp::ServerSpec,
    /// The merged values whose source is the repository's layer.
    pub fields: ProgramFields,
}

/// The merged `command`, `args`, `env` and `url` the repository's layer
/// supplies: what the approved declaration must still hold for the server
/// to start. The values can hold argument tokens and environment secrets,
/// so only their shape prints (`docs/code-quality.md`, "What a reviewer
/// checks").
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ProgramFields {
    /// The merged `command`, when the repository supplies it.
    pub command: Option<Value>,
    /// The merged `args`, when the repository supplies them.
    pub args: Option<Value>,
    /// The merged `env` entries the repository supplies, by key.
    pub env: BTreeMap<String, Value>,
    /// The merged `url`, when the repository supplies it.
    pub url: Option<Value>,
}

impl std::fmt::Debug for ProgramFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramFields")
            .field("command", &self.command.as_ref().map(|_| "redacted"))
            .field("args", &self.args.as_ref().map(|_| "redacted"))
            .field(
                "env",
                &self
                    .env
                    .keys()
                    .map(|name| (name, "redacted"))
                    .collect::<BTreeMap<_, _>>(),
            )
            // A token can ride in the URL, so only its presence prints.
            .field("url", &self.url.as_ref().map(|_| "redacted"))
            .finish()
    }
}

/// Reads the merged `mcp.servers` through the shared startup
/// classification: one spec per runnable entry, partitioned into the
/// person's servers and the repository's, which start only approved. A
/// `required` refusal fails the whole read; any other refusal becomes a
/// `notice`.
pub(crate) fn specs(config: &Config) -> Result<Specs, Failure> {
    // The classification both `main` and `fiber doctor` share; the six
    // refusal sentences live there, byte for byte.
    let checked =
        config::mcp_servers(config, &mcp::url_has_userinfo).map_err(|refusal| Failure {
            code: ErrorCode::ConfigInvalid,
            message: refusal.message(),
            retry_after_ms: None,
            provider: None,
        })?;
    let mut specs = Vec::new();
    let mut repository = Vec::new();
    let mut notices = Vec::new();
    for refusal in checked.refused {
        notices.push(Notice {
            code: ErrorCode::ConfigInvalid,
            message: refusal.message(),
            extension: None,
        });
    }
    for server in checked.servers {
        let name = server.name.clone();
        let repository_side = server.repository;
        let built = spec(config, server);
        if repository_side {
            repository.push(RepositorySpec {
                spec: built,
                fields: program_fields(config, &name),
            });
        } else {
            specs.push(built);
        }
    }
    Ok(Specs {
        specs,
        repository,
        notices,
    })
}

/// Builds the spec for a classified server: the transport with its
/// secret reader, timeouts, hints and tool gates from the merged
/// configuration. Each secret reference is read when the server starts,
/// through the closure below, so the values never enter the spec.
fn spec(config: &Config, server: config::McpServer) -> mcp::ServerSpec {
    let config::McpServer {
        name,
        required,
        transport,
        ..
    } = server;
    let transport = match transport {
        config::McpTransport::Stdio {
            command,
            args,
            env,
            secrets,
        } => {
            // Each reference is read when the server starts, through the
            // closure below, so the values never enter the spec.
            let credentials = config.credentials().clone();
            let reader: mcp::SecretReader =
                Arc::new(move |secret: &str| match credentials.secret(secret) {
                    Ok(Some(value)) => Ok(value.expose().to_owned()),
                    Ok(None) => Err(mcp::SecretError::Missing),
                    Err(error) => Err(mcp::SecretError::Unreadable(error.to_string())),
                });
            mcp::Transport::Stdio(mcp::StdioSpec {
                command,
                args,
                env,
                secrets,
                reader,
                run: None,
            })
        }
        config::McpTransport::Remote { url } => mcp::Transport::Http {
            url,
            tokens: Arc::new(config.credentials().mcp_token_files()),
        },
    };
    mcp::ServerSpec {
        name: name.clone(),
        transport,
        startup_timeout: millis(config, &name, "startup_timeout_ms", Duration::ZERO),
        call_timeout: millis(config, &name, "timeout_ms", mcp::DEFAULT_CALL_TIMEOUT),
        enabled: server_field(config, &name, "tools.enabled").map(|(value, _)| strings(&value)),
        disabled: config.list(&config::mcp_server_key(&name, "tools.disabled"), None),
        hints: hints(config, &name),
        required,
    }
}

/// What a session started: the failures for the log and the running
/// servers for the session's end.
pub(crate) struct SessionServers {
    /// Uncapped built-in and MCP tools before extension registrations.
    pub base_tools: Vec<(String, Arc<dyn Tool>)>,
    /// Built-in and MCP declarations before extension replacements.
    pub kept_infos: Vec<ToolInfo>,
    /// One per server that failed to start or missed its deadline.
    pub failed: Vec<McpServerFailed>,
    /// The running servers, stopped when the session ends.
    pub servers: Arc<mcp::Servers>,
    /// Every runnable prompt of every server that listed, tagged with its
    /// server's name (`docs/mcp.md`, "Prompts and resources").
    pub prompts: mcp::Prompts,
    /// Run on every completed handoff: clears what the file tools have seen.
    pub forget: Arc<dyn Fn() + Send + Sync>,
    /// The image child's driver, which pasted images are processed through.
    pub images: Arc<dyn contract::images::Images>,
    /// The repository servers the post-offer load may still start.
    pub later: repository_servers::Later,
}

/// MCP startup runs beside extension loading; this owner joins it when
/// tools are composed, and joins then stops its servers if composition is
/// abandoned. Its image driver is shared with MCP and the session tools.
pub(crate) struct PendingMcp {
    worker: Option<
        std::thread::JoinHandle<Result<(repository_servers::Prepared, mcp::Started), Failure>>,
    >,
    started: Option<(repository_servers::Prepared, mcp::Started)>,
    images: Arc<dyn contract::images::Images>,
}

impl PendingMcp {
    fn join(&mut self) -> Result<(), Failure> {
        let Some(worker) = self.worker.take() else {
            return Err(crate::failed(
                ErrorCode::IoFailed,
                "MCP startup was already joined",
            ));
        };
        let (prepared, started) = worker.join().map_err(|_| {
            crate::failed(
                ErrorCode::IoFailed,
                "the MCP startup thread stopped unexpectedly",
            )
        })??;
        if let Some(failure) = &started.required_failed {
            let failure = Failure {
                code: ErrorCode::McpRequiredServerFailed,
                message: failure.error.message.clone(),
                retry_after_ms: None,
                provider: None,
            };
            started.servers.stop();
            return Err(failure);
        }
        self.started = Some((prepared, started));
        Ok(())
    }

    fn take_started(&mut self) -> Result<(repository_servers::Prepared, mcp::Started), Failure> {
        self.started
            .take()
            .ok_or_else(|| crate::failed(ErrorCode::IoFailed, "MCP startup has not joined"))
    }
}

impl Drop for PendingMcp {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take()
            && let Ok(Ok((_, started))) = worker.join()
        {
            started.servers.stop();
        }
        if let Some((_, started)) = self.started.take() {
            started.servers.stop();
        }
    }
}

/// Starts repository preparation and configured MCP servers in parallel
/// with process extension loading. Required failures stop before this
/// owner returns them; dropping an unfinished owner joins and reaps starts.
#[allow(
    clippy::too_many_arguments,
    reason = "one pending session start carries MCP specs, repository scope, image driver and timing"
)]
pub(crate) fn start_session_tools(
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn contract::clock::Clock>,
    project: &config::ProjectKey,
    specs: Result<(Vec<mcp::ServerSpec>, Vec<RepositorySpec>), Failure>,
    skipped: Vec<r#loop::OfferKey>,
    images: Arc<dyn contract::images::Images>,
    timing: Option<contract::diag::Timing>,
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
) -> Result<PendingMcp, Failure> {
    let home = home.to_path_buf();
    let workspace = workspace.to_path_buf();
    let artifacts = artifacts.to_path_buf();
    let project = project.clone();
    let clock = Arc::clone(clock);
    let worker_images = Arc::clone(&images);
    let worker = std::thread::Builder::new()
        .name("session MCP startup".to_owned())
        .spawn(move || {
            let (mut specs, repository) = specs?;
            let prepared = repository_servers::prepare(
                &home, &project, &workspace, &clock, &skipped, repository,
            );
            if let Some(failure) = &prepared.required_failed {
                return Err(Failure {
                    code: ErrorCode::McpRequiredServerFailed,
                    message: failure.error.message.clone(),
                    retry_after_ms: None,
                    provider: None,
                });
            }
            specs.extend(prepared.ready.iter().cloned());
            let cache = config::mcp_cache_dir(&home);
            let started = mcp::start_observed(
                specs,
                &workspace,
                &artifacts,
                worker_images,
                &cache,
                &clock,
                env!("CARGO_PKG_VERSION"),
                timing,
                // The session's sink observes each MCP request at
                // debug; at any other level the servers stay silent.
                debug,
            );
            Ok((prepared, started))
        })
        .map_err(|error| {
            crate::failed(ErrorCode::IoFailed, format!("MCP startup thread: {error}"))
        })?;
    Ok(PendingMcp {
        worker: Some(worker),
        started: None,
        images,
    })
}

/// The tools one session registers: the built-ins, every MCP server's
/// tools after them, the extensions' `extension_tools` after those, and the
/// driver shell, with the failures for the log and the running servers for
/// the session's end. `timing` times every MCP connection attempt the
/// session starts, now and in the post-offer load; `None` stays untimed.
/// `debug` observes each `web_fetch` request the session makes; `None`
/// stays silent.
#[allow(
    clippy::type_complexity,
    reason = "one session's tools, as builtin's tuple carries them"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "one session's tools needs home and the recorded executable alongside builtin's arguments"
)]
pub(crate) fn session_tools(
    fiber: PathBuf,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn contract::clock::Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    project: &config::ProjectKey,
    mut pending: PendingMcp,
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
    extension_tools: Vec<(String, Arc<dyn Tool>)>,
    timing: Option<contract::diag::Timing>,
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
    shell_read_only: crate::settings::ShellReadOnly,
    sandbox: crate::settings::SandboxHandle,
    protection: Option<Arc<tools::ProtectedPaths>>,
) -> Result<
    (
        Vec<(String, Arc<dyn Tool>)>,
        Vec<ToolInfo>,
        Arc<dyn Tool>,
        SessionServers,
    ),
    Failure,
> {
    pending.join()?;
    let images = Arc::clone(&pending.images);
    let (mut tools, mut infos, driver, forget, images) = crate::builtin::builtin_timed(
        fiber,
        home,
        workspace,
        artifacts,
        clock,
        jobs,
        locks,
        images,
        web_search,
        delegates,
        skills,
        timing.clone(),
        debug,
        shell_read_only,
        sandbox,
        protection,
    )?;
    let (prepared, started) = pending.take_started()?;
    // The pending start settles repository approval and server startup
    // before adding tools, preserving built-in, MCP, then extension order.
    tools.extend(started.tools);
    infos.extend(started.infos);
    let base_tools = tools.clone();
    let kept_infos = infos.clone();
    if let Err(failure) = add_extension_tools(&mut tools, &mut infos, extension_tools) {
        started.servers.stop();
        return Err(failure);
    }
    // An `mcp_resources` already declared, by a server or an extension,
    // survives: the post-offer load never offers a second registration.
    if declares_resources(&tools) {
        started.servers.mark_resources_declared();
    }
    let mut failed = prepared.failed;
    failed.extend(started.failed);
    let servers = Arc::new(started.servers);
    let later = repository_servers::Later {
        home: home.to_path_buf(),
        project: project.clone(),
        workspace: workspace.to_path_buf(),
        artifacts: artifacts.to_path_buf(),
        images: Arc::clone(&images),
        cache: config::mcp_cache_dir(home),
        clock: Arc::clone(clock),
        waiting: prepared.waiting.into_iter().map(|(spec, _)| spec).collect(),
        servers: Arc::clone(&servers),
    };
    let servers = SessionServers {
        base_tools,
        kept_infos,
        failed,
        servers,
        prompts: started.prompts,
        forget,
        images,
        later,
    };
    Ok((tools, infos, driver, servers))
}

/// Whether the session already declares `mcp_resources`, through an MCP
/// server or an extension: the post-offer load never offers a second
/// registration then.
fn declares_resources(tools: &[(String, Arc<dyn Tool>)]) -> bool {
    tools
        .iter()
        .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL)
}

/// Adds the extensions' tools after the built-in and MCP tools, so one
/// named like either replaces it when the loop registers them, and the
/// replacement is recorded (`docs/architecture.md`, "Tool seam"). No two
/// of them share a name: the extensions settled that. The `tools` answer
/// lists the tool that is declared, so a replaced row goes. Infos end
/// sorted by name.
pub(crate) fn add_extension_tools(
    tools: &mut Vec<(String, Arc<dyn Tool>)>,
    infos: &mut Vec<ToolInfo>,
    extension_tools: Vec<(String, Arc<dyn Tool>)>,
) -> Result<(), Failure> {
    for (extension, tool) in extension_tools {
        let source = ToolSource::Extension {
            extension: extension.clone(),
        };
        let info = crate::builtin::info(tool.as_ref(), source)?;
        infos.retain(|row| row.name != info.name);
        infos.push(info);
        tools.push((extension, tool));
    }
    infos.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

/// The merged value of `mcp.servers."<name>".<field>` and its layer.
fn server_field(config: &Config, server: &str, field: &str) -> Option<(Value, Source)> {
    config.get(&config::mcp_server_key(server, field), None)
}

/// The merged `command`, `args`, `env` and `url` the repository's layer
/// supplies: exactly the values the approved declaration must still hold.
fn program_fields(config: &Config, server: &str) -> ProgramFields {
    let one = |field: &str| {
        server_field(config, server, field)
            .and_then(|(value, source)| matches!(source, Source::Repository(_)).then_some(value))
    };
    let mut env = BTreeMap::new();
    if let Some((Value::Object(merged), _)) = server_field(config, server, "env") {
        for key in merged.keys() {
            if let Some((value, Source::Repository(_))) = config.get(
                &format!(
                    "mcp.servers.{}.env.{}",
                    config::quoted_segment(server),
                    config::quoted_segment(key)
                ),
                None,
            ) {
                env.insert(key.clone(), value);
            }
        }
    }
    ProgramFields {
        command: one("command"),
        args: one("args"),
        env,
        url: one("url"),
    }
}

/// The `field` timeout in milliseconds: the schema default when the key
/// has one, else `default`.
fn millis(config: &Config, server: &str, field: &str, default: Duration) -> Duration {
    config
        .count(&config::mcp_server_key(server, field), None)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

pub(crate) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The person's hint overrides, by tool: the merged
/// `tools."<tool>".hints` objects (`config` already drops a repository's
/// with a notice, so the merged value is the person's).
fn hints(config: &Config, server: &str) -> BTreeMap<String, mcp::Hints> {
    let mut overrides = BTreeMap::new();
    let Some((Value::Object(tools), _)) = config.get(
        &format!("mcp.servers.{}.tools", config::quoted_segment(server)),
        None,
    ) else {
        return overrides;
    };
    for (tool, entry) in tools {
        // `enabled` and `disabled` are lists, so they never hold `hints`:
        // no guard is needed to skip them here.
        if let Some(hints) = entry.get("hints").and_then(Value::as_object) {
            overrides.insert(tool.clone(), mcp::Hints::from_override(hints));
        }
    }
    overrides
}

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

impl crate::reload::State {
    /// Builds the MCP hub providers from the session's own configuration,
    /// servers, declaration writer and log: the same inputs the startup
    /// used, so queries, logins and reconnects agree with it. `settled`
    /// is the startup's failures, which the commit writes after the door
    /// starts serving: seeding the reader with them keeps every query's
    /// rows on the settled outcome from the first one served. The
    /// boundary is the log length at this call: lines before it are the
    /// resume's history, lines from it this startup's own.
    pub(crate) fn mcp_providers(
        &self,
        settled: &[contract::events::McpServerFailed],
    ) -> McpProviders {
        // One entry per server with a login running, holding its
        // authorization URL while it waits for the callback. The session
        // shares the one map its providers close over.
        let waiting = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let seed: HashMap<String, contract::events::McpServerFailed> = settled
            .iter()
            .map(|failure| (failure.server.clone(), failure.clone()))
            .collect();
        // Lines already in the log predate this startup (a resume's
        // history): the reader folds them first, so the seed — this
        // startup's settlements — wins over them.
        let boundary = self.log.count();
        McpProviders {
            refresh: servers_refresh(
                seed,
                boundary,
                self.sources.clone(),
                Arc::clone(&self.servers),
                Arc::downgrade(&self.log),
                self.log.clock().clone(),
                Arc::clone(&waiting),
            ),
            login: mcp_login(
                self.sources.clone(),
                waiting,
                self.log.clock().clone(),
                // The session writer: `Some` at debug, `None` at info.
                self.diag
                    .debugging()
                    .then(|| Arc::clone(&self.diag) as Arc<dyn contract::diag::DebugLog>),
            ),
            reconnect: reconnect_server(
                self.sources.clone(),
                Arc::clone(&self.servers),
                self.declare.clone(),
                self.artifacts.clone(),
                Arc::clone(&self.images),
                self.log.clock().clone(),
                Arc::clone(&self.reconnect_flights),
            ),
        }
    }
}

/// Reads the `mcp_servers` answer from the tools answer's rows: one row
/// per configured server, in configuration order, folding the session's
/// own log so a mid-session death reads failed exactly as the startup
/// notice did (`docs/mcp.md`, "Servers, their state and the per-server
/// commands"). `seed` carries the startup's settled failures, so a
/// query served before the commit wrote their lines reads the settled
/// rows; `boundary` is the log length when the reader was built, so the
/// resume's history folds before the seed and this startup's lines fold
/// after it. A reader built outside the startup passes an empty seed
/// with a zero boundary, so every line folds after it, exactly as
/// before.
fn servers_refresh(
    seed: HashMap<String, contract::events::McpServerFailed>,
    boundary: u64,
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
        // Three phases, in log order: the resume's history, this
        // startup's settled failures, then this startup's commit and
        // later lines. A query served before the commit wrote the
        // settled lines reads the seed, so readers before and after
        // the commit agree; a restart's ready still clears its row,
        // and an old ready never erases a fresh settlement.
        let mut failed: HashMap<String, contract::events::McpServerFailed> = HashMap::new();
        if let Some(log) = log.upgrade() {
            let count = usize::try_from(log.count()).unwrap_or(usize::MAX);
            let envelopes = log.range(0, count).map_err(|error| Failure {
                code: error.code(),
                message: error.to_string(),
                retry_after_ms: None,
                provider: None,
            })?;
            let decode = |envelope: &contract::Envelope| {
                contract::events::Event::from_envelope(envelope).map_err(|error| Failure {
                    code: contract::ErrorCode::LogCorrupt,
                    message: error.to_string(),
                    retry_after_ms: None,
                    provider: None,
                })
            };
            let split = usize::try_from(boundary)
                .unwrap_or(usize::MAX)
                .min(envelopes.len());
            for envelope in envelopes.iter().take(split) {
                fold_server_line(&mut failed, &decode(envelope)?);
            }
            failed.extend(seed.clone());
            for envelope in envelopes.iter().skip(split) {
                fold_server_line(&mut failed, &decode(envelope)?);
            }
        } else {
            // The log is gone: the seed is all the reader knows.
            failed = seed.clone();
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for info in &tools {
            if let contract::events::ToolSource::Mcp { server, .. } = &info.source {
                *counts.entry(server).or_default() += 1;
            }
        }
        let waiting = support::lock(&waiting);
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

/// Folds one decoded log line into the failed map: a failure records
/// its row, a ready clears it, any other line leaves the map alone.
fn fold_server_line(
    failed: &mut HashMap<String, contract::events::McpServerFailed>,
    event: &Option<contract::events::Event>,
) {
    match event {
        Some(contract::events::Event::McpServerFailed(failed_server)) => {
            failed.insert(failed_server.server.clone(), failed_server.clone());
        }

        Some(contract::events::Event::McpServerReady(ready)) => {
            failed.remove(&ready.server);
        }
        _ => {}
    }
}

/// Runs one server's `fiber mcp login` flow on the login thread the door
/// spawns: the authorization URL the flow presents lands in the shared
/// waiting map, so the `mcp_servers` answer shows `auth_url` while it
/// waits, and the stored token is picked up by the reconnect the thread
/// delivers before it answers (`docs/mcp.md`, "Servers, their state and
/// the per-server commands"). The hub keeps its login silent: `debug`
/// is always `None` here, so the flow writes no `http_request` line.
fn mcp_login(
    sources: config::Sources,
    waiting: Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
    clock: Arc<dyn contract::clock::Clock>,
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
) -> doors::McpLogin {
    Arc::new(
        move |server: &str, deliver: Arc<dyn Fn(contract::inbox::Delivery) + Send + Sync>| {
            {
                let mut waiting = support::lock(&waiting);
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
                support::lock(waiting).remove(server);
            };
            let (url, _) = match cli::login_target(
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
                support::lock(&waiting).insert(server.to_owned(), Some(auth_url.to_owned()));
                extensions::SystemBrowser::default().open(auth_url);
            };
            let outcome =
                mcp::login_observed(&files, &url, &present, clock.as_ref(), debug.clone());
            support::lock(&waiting).insert(server.to_owned(), None);
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
    flights: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
) -> r#loop::ReconnectServer {
    struct Flight<'a> {
        flights: &'a std::sync::Mutex<std::collections::BTreeSet<String>>,
        server: &'a str,
    }
    impl Drop for Flight<'_> {
        fn drop(&mut self) {
            support::lock(self.flights).remove(self.server);
        }
    }
    Arc::new(move |server: &str, collect: &mut dyn FnMut(&str)| {
        let (_, project) =
            cli::project_of(&sources.home, &sources.workspace).map_err(|failure| Rejection {
                code: failure.code(),
                message: failure.to_string(),
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
        // The declared rows, rejecting unknown and unapproved names:
        // only the reconnect entry reads them, and only here.
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
        let adoption = if servers.has_slot(server) {
            None
        } else {
            Some(adoption_spec(&config, server)?)
        };
        support::lock(&flights).insert(server.to_owned());
        let flight = Flight {
            flights: &flights,
            server,
        };
        let (used, removed, records) = if let Some(spec) = adoption {
            adopt_server(spec, &sources, &servers, &artifacts, &images, &clock)?
        } else {
            let Some(restarted) = mcp::restart(&servers, server, &artifacts, &images) else {
                return Err(Rejection {
                    code: contract::ErrorCode::InvalidArguments,
                    message: format!(
                        "The MCP server `{server}` stopped before its reconnect ran; try it again."
                    ),
                });
            };
            (restarted.used, restarted.removed, restarted.records)
        };
        for name in &removed {
            declare(name.as_str(), None);
        }
        for (_, tool, info) in &used {
            declare(&tool.definition().name, Some(info.clone()));
        }
        collect(server);
        drop(flight);
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

/// Rejects servers the repository offer owns or that have nothing
/// runnable before publishing an executing start.
fn adoption_spec(config: &config::Config, server: &str) -> Result<mcp::ServerSpec, Rejection> {
    let invalid = |message: String| Rejection {
        code: contract::ErrorCode::InvalidArguments,
        message,
    };
    let specs = specs(config).map_err(|failure| Rejection {
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
    Ok(spec)
}

/// Starts the validated server and adopts its slot. Failure is an outcome
/// written by the loop, not a rejected driver command.
fn adopt_server(
    spec: mcp::ServerSpec,
    sources: &config::Sources,
    servers: &Arc<mcp::Servers>,
    artifacts: &std::path::Path,
    images: &Arc<dyn contract::images::Images>,
    clock: &Arc<dyn contract::clock::Clock>,
) -> Result<Adopted, Rejection> {
    let started = mcp::start_observed(
        vec![spec],
        &sources.workspace,
        artifacts,
        Arc::clone(images),
        &config::mcp_cache_dir(&sources.home),
        clock,
        env!("CARGO_PKG_VERSION"),
        servers.timing(),
        // The session's sink follows the reconnect: structs keep their
        // shape, so it is read from the servers.
        servers.debug_sink(),
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

#[cfg(test)]
#[path = "mcp_servers_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_elicit_tests.rs"]
mod elicit_tests;
