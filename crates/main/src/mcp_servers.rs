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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Source};
use contract::ErrorCode;
use contract::events::{McpServerFailed, Notice, ToolInfo, ToolSource};
use contract::shapes::Failure;
use contract::tool::Tool;
use serde_json::Value;

/// What `main` starts: the specs and the skip notices for the log.
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

/// Reads the merged `mcp.servers`: one spec per entry with a `command`
/// or a `url`, partitioned into the person's servers and the
/// repository's. A server is the repository's when the repository layer
/// declares an entry of that name holding at least one value, or when
/// any of its effective `command`, `args`, `env` or `url` entries comes
/// from the repository layer, so nothing offered ever runs unapproved: a
/// person's server the repository only tweaks needs approval too. An
/// entry with both `command` and `url` is never started: without
/// `required` it is skipped with a `notice`, with `required` the whole
/// read fails, before the partition, whatever layers supplied them.
pub(crate) fn specs(config: &Config) -> Result<Specs, Failure> {
    let mut specs = Vec::new();
    let mut repository = Vec::new();
    let mut notices = Vec::new();
    let Some((Value::Object(servers), _)) = config.get("mcp.servers", None) else {
        return Ok(Specs {
            specs,
            repository,
            notices,
        });
    };
    // The repository's own declarations, after the key check: an entry
    // with no value is not a declaration and gates nothing.
    let workspace = config.extensions().workspace().to_path_buf();
    let declared = config::declared(&workspace).ok();
    // `serde_json::Map` is a `BTreeMap` without `preserve_order`
    // (`docs/dependencies.md`), so this is name order.
    for name in servers.keys() {
        let entry = spec(config, name);
        let spec = match entry {
            Entry::Neither => continue,
            Entry::Both { name, required } => {
                if required {
                    return Err(Failure {
                        code: ErrorCode::ConfigInvalid,
                        message: format!(
                            "The required MCP server `{name}` declares both `command` and `url`, so Fiber cannot start it. Keep the one you mean."
                        ),
                        retry_after_ms: None,
                        provider: None,
                    });
                }
                notices.push(Notice {
                    code: ErrorCode::ConfigInvalid,
                    message: format!(
                        "The MCP server `{name}` declares both `command` and `url`, so Fiber starts neither. Keep the one you mean."
                    ),
                    extension: None,
                });
                continue;
            }
            Entry::Userinfo { name, required } => {
                if required {
                    return Err(Failure {
                        code: ErrorCode::ConfigInvalid,
                        message: format!(
                            "The required MCP server `{name}` has a user name or password in its `url`, so Fiber cannot start it. Take them out of the URL."
                        ),
                        retry_after_ms: None,
                        provider: None,
                    });
                }
                notices.push(Notice {
                    code: ErrorCode::ConfigInvalid,
                    message: format!("The MCP server `{name}` has a user name or password in its `url`, so Fiber does not start it. Take them out of the URL."),
                    extension: None,
                });
                continue;
            }
            Entry::Spec(spec) => spec,
        };
        let declared_entry = declared
            .as_ref()
            .is_some_and(|declared| declared.mcp_servers.contains_key(name));
        // An error reading the declarations puts every server with a
        // `command` on the repository side: nothing runs unapproved.
        let declared_entry = declared_entry || declared.is_none();
        if declared_entry
            || from_repository(config, name, "command")
            || from_repository(config, name, "args")
            || from_repository(config, name, "url")
            || env_from_repository(config, name)
        {
            repository.push(RepositorySpec {
                spec,
                fields: program_fields(config, name),
            });
        } else {
            specs.push(spec);
        }
    }
    Ok(Specs {
        specs,
        repository,
        notices,
    })
}

/// What one `mcp.servers` entry declares: a spec, both transports
/// (never started), or nothing to start.
#[allow(
    clippy::large_enum_variant,
    reason = "the spec is built and matched at once; boxing would allocate per entry"
)]
enum Entry {
    /// A runnable spec, of either transport.
    Spec(mcp::ServerSpec),
    /// Both `command` and `url`, with whether the entry is `required`.
    Both { name: String, required: bool },
    /// A URL with userinfo must never reach ureq's Basic authentication.
    Userinfo { name: String, required: bool },
    /// Neither: nothing to start.
    Neither,
}

/// What a session started: the failures for the log and the running
/// servers for the session's end.
pub(crate) struct SessionServers {
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

/// The tools one session registers: the built-ins, every MCP server's
/// tools after them, the extensions' `extension_tools` after those, and the
/// driver shell, with the failures for the log and the running servers for
/// the session's end.
#[allow(
    clippy::type_complexity,
    reason = "one session's tools, as builtin's tuple carries them"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "one session's tools needs home and the recorded executable alongside builtin's arguments"
)]
pub(crate) fn session_tools(
    fiber: Result<PathBuf, String>,
    home: &Path,
    workspace: &Path,
    artifacts: &Path,
    clock: &Arc<dyn contract::clock::Clock>,
    jobs: &Arc<jobs::Registry>,
    locks: &Arc<tools::PathLocks>,
    project: &config::ProjectKey,
    specs: Vec<mcp::ServerSpec>,
    repository: Vec<RepositorySpec>,
    skipped: &[r#loop::OfferKey],
    web_search: Option<(Arc<dyn Tool>, ToolInfo)>,
    delegates: &crate::delegates::Delegates,
    skills: Arc<dyn contract::skills::Skills>,
    extension_tools: Vec<(String, Arc<dyn Tool>)>,
) -> Result<
    (
        Vec<(String, Arc<dyn Tool>)>,
        Vec<ToolInfo>,
        Arc<dyn Tool>,
        SessionServers,
    ),
    Failure,
> {
    let fiber = fiber.map_err(|message| crate::failed(ErrorCode::IoFailed, message))?;
    let (mut tools, mut infos, driver, forget, images) = crate::builtin::builtin(
        fiber, home, workspace, artifacts, clock, jobs, locks, web_search, delegates, skills,
    )?;
    // A repository server whose current content is approved starts with
    // the session exactly as a person's server does; any other waits for
    // the post-offer load. Every spec starts with the session, except a
    // cached non-required one, which is declared from its cache and starts
    // on its first call; a server that fails is left out and its failure
    // is returned for the log, written after `fiber_started`. A
    // `required` failure stops what started and fails the session before
    // any line is written.
    let prepared =
        repository_servers::prepare(home, project, workspace, clock, skipped, repository);
    if let Some(failure) = prepared.required_failed {
        return Err(Failure {
            code: ErrorCode::McpRequiredServerFailed,
            message: failure.error.message.clone(),
            retry_after_ms: None,
            provider: None,
        });
    }
    let mut specs = specs;
    specs.extend(prepared.ready);
    let cache = home.join("cache").join("mcp");
    let started = mcp::start(
        specs,
        workspace,
        artifacts,
        std::sync::Arc::clone(&images),
        &cache,
        clock,
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(failure) = started.required_failed {
        started.servers.stop();
        return Err(Failure {
            code: ErrorCode::McpRequiredServerFailed,
            message: failure.error.message.clone(),
            retry_after_ms: None,
            provider: None,
        });
    }
    tools.extend(started.tools);
    infos.extend(started.infos);
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
        cache,
        clock: Arc::clone(clock),
        waiting: prepared.waiting.into_iter().map(|(spec, _)| spec).collect(),
        servers: Arc::clone(&servers),
    };
    let servers = SessionServers {
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
fn add_extension_tools(
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

/// What the server `name` declares: a spec, both transports, or
/// nothing. The merged `command`, `args`, `env` and `url`
/// are the person's and the repository's merged key by key; a
/// repository's share is partitioned out by `specs`, never skipped here.
fn spec(config: &Config, name: &str) -> Entry {
    let command = server_field(config, name, "command")
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let url =
        server_field(config, name, "url").and_then(|(value, _)| value.as_str().map(str::to_owned));
    let required = server_field(config, name, "required")
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(false);
    if url.as_deref().is_some_and(mcp::url_has_userinfo) {
        return Entry::Userinfo {
            name: name.to_owned(),
            required,
        };
    }
    let transport = match (command, url) {
        (Some(_), Some(_)) => {
            return Entry::Both {
                name: name.to_owned(),
                required,
            };
        }
        (Some(command), None) => {
            let args =
                server_field(config, name, "args").map_or(Vec::new(), |(value, _)| strings(&value));
            let env = server_field(config, name, "env")
                .map_or(BTreeMap::new(), |(value, _)| string_map(&value));
            mcp::Transport::Stdio(mcp::StdioSpec {
                command,
                args,
                env,
                run: None,
            })
        }
        (None, Some(url)) => mcp::Transport::Http {
            url,
            tokens: Arc::new(config.credentials().mcp_token_files()),
        },
        (None, None) => return Entry::Neither,
    };
    let spec = mcp::ServerSpec {
        name: name.to_owned(),
        transport,
        startup_timeout: millis(
            config,
            name,
            "startup_timeout_ms",
            mcp::DEFAULT_STARTUP_TIMEOUT,
        ),
        call_timeout: millis(config, name, "timeout_ms", mcp::DEFAULT_CALL_TIMEOUT),
        enabled: server_field(config, name, "tools.enabled").map(|(value, _)| strings(&value)),
        disabled: server_field(config, name, "tools.disabled")
            .map_or(Vec::new(), |(value, _)| strings(&value)),
        hints: hints(config, name),
        required,
    };
    Entry::Spec(spec)
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
                &format!("mcp.servers.{}.env.{}", quoted(server), quoted(key)),
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

/// The merged value of `mcp.servers."<name>".<field>` and its layer.
fn server_field(config: &Config, server: &str, field: &str) -> Option<(Value, Source)> {
    config.get(&format!("mcp.servers.{}.{field}", quoted(server)), None)
}

/// Whether the merged value of one field effectively comes from the
/// repository's layer.
fn from_repository(config: &Config, server: &str, field: &str) -> bool {
    server_field(config, server, field)
        .is_some_and(|(_, source)| matches!(source, Source::Repository(_)))
}

/// Whether any effective `env` entry comes from the repository's layer:
/// `env` merges key by key, so each entry's provenance is checked
/// individually. `command` (`Str`) and `args` (`StrList`, which replaces)
/// are whole values, so [`from_repository`] on the field is exact for them.
fn env_from_repository(config: &Config, server: &str) -> bool {
    let Some((Value::Object(env), _)) = server_field(config, server, "env") else {
        return false;
    };
    env.keys().any(|key| {
        config
            .get(
                &format!("mcp.servers.{}.env.{}", quoted(server), quoted(key)),
                None,
            )
            .is_some_and(|(_, source)| matches!(source, Source::Repository(_)))
    })
}

/// A dotted-path segment, quoted when it holds a dot.
fn quoted(segment: &str) -> String {
    if segment.contains('.') {
        format!("\"{segment}\"")
    } else {
        segment.to_owned()
    }
}

fn millis(config: &Config, server: &str, field: &str, default: Duration) -> Duration {
    server_field(config, server, field)
        .and_then(|(value, _)| value.as_u64())
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

fn string_map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The person's hint overrides, by tool: the merged
/// `tools."<tool>".hints` objects (`config` already drops a repository's
/// with a notice, so the merged value is the person's).
fn hints(config: &Config, server: &str) -> BTreeMap<String, mcp::Hints> {
    let mut overrides = BTreeMap::new();
    let Some((Value::Object(tools), _)) =
        config.get(&format!("mcp.servers.{}.tools", quoted(server)), None)
    else {
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

#[cfg(test)]
#[path = "mcp_servers_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_elicit_tests.rs"]
mod elicit_tests;
