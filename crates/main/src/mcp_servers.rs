//! MCP servers from configuration (`docs/configuration.md`, "MCP
//! servers"): each `mcp.servers` entry becomes an [`mcp::ServerSpec`], with
//! the documented defaults. A repository's servers need a person's approval
//! (`docs/mcp.md`, "A repository's servers"), which lands with #599: until
//! then a server whose `command`, `args` or `env` effectively comes from
//! the repository is not started, and each skipped server yields a `notice`
//! with code `repository_code_skipped`. Servers with a `url` and no
//! `command` are #593's and are skipped silently. A server with neither is
//! skipped silently too: there is nothing to start.
//!
//! debt: repository approval moves to #599; the trigger is that ticket landing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Source};
use contract::ErrorCode;
use contract::events::{McpServerFailed, Notice, ToolInfo};
use contract::shapes::Failure;
use contract::tool::Tool;
use serde_json::Value;

/// What `main` starts: the specs and the skip notices for the log.
pub(crate) struct Specs {
    /// One per stdio server to start.
    pub specs: Vec<mcp::ServerSpec>,
    /// One per repository-declared server that was skipped.
    pub notices: Vec<Notice>,
}

/// Reads the merged `mcp.servers`: one spec per entry with a `command`,
/// skipping the rest as above.
pub(crate) fn specs(config: &Config) -> Specs {
    let mut specs = Vec::new();
    let mut notices = Vec::new();
    let Some((Value::Object(servers), _)) = config.get("mcp.servers", None) else {
        return Specs { specs, notices };
    };
    // `serde_json::Map` is a `BTreeMap` without `preserve_order`
    // (`docs/dependencies.md`), so this is name order.
    for name in servers.keys() {
        match spec(config, name) {
            Keep::Yes(spec) => specs.push(spec),
            Keep::Skipped(notice) => {
                if let Some(notice) = notice {
                    notices.push(notice);
                }
            }
        }
    }
    Specs { specs, notices }
}

/// What a session started: the failures for the log and the running
/// servers for the session's end.
pub(crate) struct SessionServers {
    /// One per server that failed to start or missed its deadline.
    pub failed: Vec<McpServerFailed>,
    /// The running servers, stopped when the session ends.
    pub servers: mcp::Servers,
    /// Run on every completed handoff: clears what the file tools have seen.
    pub forget: Arc<dyn Fn() + Send + Sync>,
    /// The image child's driver, which pasted images are processed through.
    pub images: Arc<dyn contract::images::Images>,
}

/// The tools one session registers: the built-ins, every MCP server's
/// tools after them, and the driver shell, with the failures for the log
/// and the running servers for the session's end.
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
    specs: Vec<mcp::ServerSpec>,
    web_search: Option<&str>,
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
    let (mut tools, mut infos, driver, forget, images) =
        crate::builtin::builtin(fiber, workspace, artifacts, clock, jobs, locks, web_search)?;
    // Every spec starts with the session, except a cached non-required
    // one, which is declared from its cache and starts on its first call;
    // a server that fails is left out and its failure is returned for the
    // log, written after `fiber_started`. A `required` failure stops what
    // started and fails the session before any line is written.
    let started = mcp::start(
        specs,
        workspace,
        &home.join("cache").join("mcp"),
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
    infos.sort_by(|left, right| left.name.cmp(&right.name));
    let servers = SessionServers {
        failed: started.failed,
        servers: started.servers,
        forget,
        images,
    };
    Ok((tools, infos, driver, servers))
}

enum Keep {
    Yes(mcp::ServerSpec),
    Skipped(Option<Notice>),
}

fn spec(config: &Config, name: &str) -> Keep {
    let command = server_field(config, name, "command")
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let Some(command) = command else {
        // A `url` server (#593) or nothing to start at all.
        return Keep::Skipped(None);
    };
    // debt: approval covers the exact declaration (#599); until it lands, a
    // repository's `command` or `args` skips the server, as does any
    // effective `env` entry from the repository: `Config::get` names an
    // object's highest layer, so a person's higher-layer `env: {}` would
    // otherwise mask a repository's `BASH_ENV`, which stays merged.
    if from_repository(config, name, "command")
        || from_repository(config, name, "args")
        || env_from_repository(config, name)
    {
        return Keep::Skipped(Some(Notice {
            code: ErrorCode::RepositoryCodeSkipped,
            message: format!(
                "The repository declares the MCP server `{name}`, which nobody approved: it was not started. Run `fiber approve` in the repository to approve it.",
            ),
            extension: None,
        }));
    }
    let args = server_field(config, name, "args").map_or(Vec::new(), |(value, _)| strings(&value));
    let env =
        server_field(config, name, "env").map_or(BTreeMap::new(), |(value, _)| string_map(&value));
    Keep::Yes(mcp::ServerSpec {
        name: name.to_owned(),
        command,
        args,
        env,
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
        required: server_field(config, name, "required")
            .and_then(|(value, _)| value.as_bool())
            .unwrap_or(false),
    })
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

fn strings(value: &Value) -> Vec<String> {
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
