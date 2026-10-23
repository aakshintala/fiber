//! Session settings read from configuration with their documented
//! defaults (`docs/configuration.md`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::{Config, ModelData, ProjectKey, Sources};
use contract::events::{CacheLifetime, Notice};
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use serde_json::Value;

/// How long an idle session waits before it exits, from
/// `session.idle_exit_ms` (`docs/configuration.md`). `0` exits at the first
/// empty wait.
pub(crate) fn idle_exit(config: &Config) -> Duration {
    let ms = config
        .count("session.idle_exit_ms", None)
        .unwrap_or_default();
    Duration::from_millis(ms)
}

/// How long a hub a client started stays running with no client
/// connected, from `hub.idle_exit_ms` (`docs/configuration.md`). `0`
/// exits at the first empty wait.
pub(crate) fn hub_idle_exit(config: &Config) -> Duration {
    let ms = config.count("hub.idle_exit_ms", None).unwrap_or_default();
    Duration::from_millis(ms)
}

/// When a reviewer block hands the call to a person, from configuration
/// with the documented defaults (`docs/configuration.md`).
pub(crate) fn block_limits(config: &Config) -> r#loop::BlockLimits {
    let limit = |key: &str| config.count(key, None).unwrap_or_default();
    r#loop::BlockLimits {
        consecutive: limit("reviewer.block_limits.consecutive"),
        session: limit("reviewer.block_limits.session"),
    }
}

/// The prompt-cache lifetime for `model`, from `cache.lifetime`
/// (`docs/prompt-cache.md`, "Cache lifetime" and
/// `docs/configuration.md`). A per-model key wins over the same key at
/// the top level. `"5m"` is five minutes; anything else is one hour (the
/// config crate's `OneOf` refuses other values).
pub(crate) fn cache_lifetime(config: &Config, model: &str) -> CacheLifetime {
    if config
        .text("cache.lifetime", Some(model))
        .is_some_and(|value| value == "5m")
    {
        CacheLifetime::FiveMinutes
    } else {
        CacheLifetime::OneHour
    }
}

/// How many cache lifetimes after the last turn an idle session keeps its
/// prompt cache warm: `cache.warm_cap` when `cache.warm_idle` is set, else
/// `None`, which never warms (`docs/prompt-cache.md`, "Warming while idle").
/// The config crate refuses 12 or more.
pub(crate) fn warm(config: &Config) -> Option<u32> {
    let on = config.flag("cache.warm_idle", None).unwrap_or_default();
    if !on {
        return None;
    }
    let cap = config.count("cache.warm_cap", None).unwrap_or_default();
    Some(u32::try_from(cap).unwrap_or(u32::MAX))
}

/// How a failed model call is retried, from configuration with the
/// documented defaults (`docs/configuration.md`). `attempts` is clamped
/// to `u32`, so a huge configured count never overflows the loop.
pub(crate) fn retry_policy(config: &Config) -> r#loop::Retry {
    let count = |key: &str| config.count(key, None).unwrap_or_default();
    r#loop::Retry {
        attempts: u32::try_from(count("retry.attempts")).unwrap_or(u32::MAX),
        initial: Duration::from_millis(count("retry.initial_delay_ms")),
        max: Duration::from_millis(count("retry.max_delay_ms")),
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod idle_tests;

/// The session's one reasoning setting (`docs/model-routing.md`,
/// "Thinking"): the `:level` suffix first, then the session's own choice,
/// then the configured value (`models."<reference>".thinking` over the
/// top-level `thinking` key), else the model's own default. A level asked
/// for now (suffix or session) that the model does not take is
/// `invalid_arguments` before `session_started`. A configured level it does
/// not take is ignored: the model's default applies and a
/// `config_key_ignored` notice is pushed to `notices`. A model with no
/// levels and no default resolves to `None`.
pub(crate) fn thinking(
    suffix: Option<ThinkingLevel>,
    session: Option<ThinkingLevel>,
    config: &Config,
    model: &ModelData,
    reference: &str,
    notices: &mut Vec<Notice>,
) -> Result<Option<ThinkingLevel>, Failure> {
    if let Some(level) = suffix.or(session) {
        return match extensions::thinking_refusal(level, model, reference) {
            Some(message) => Err(Failure {
                code: ErrorCode::InvalidArguments,
                message,
                retry_after_ms: None,
                provider: None,
            }),
            None => Ok(Some(level)),
        };
    }
    // The layer that set the winning value also names its key: a layer's
    // per-model key beats its own top-level key, but a higher layer's
    // top-level key beats a lower layer's per-model key.
    let per_model = format!("models.\"{reference}\".thinking");
    let winner = config.get("thinking", Some(reference));
    let key = match (&winner, config.get(&per_model, None)) {
        (Some((_, layer)), Some((_, own))) if *layer == own => per_model,
        _ => "thinking".to_owned(),
    };
    let configured = winner
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .and_then(|name| name.parse::<ThinkingLevel>().ok());
    match configured {
        Some(level) if model.thinking_levels.contains(&level) => Ok(Some(level)),
        Some(level) => {
            notices.push(Notice {
                code: ErrorCode::ConfigKeyIgnored,
                message: format!(
                    "The `{key}` setting `{level}` was ignored: model `{reference}` {}. \
                     The model's own default applies.",
                    extensions::takes(model),
                ),
                extension: None,
            });
            Ok(model.thinking_default)
        }
        None => Ok(model.thinking_default),
    }
}

#[cfg(test)]
#[path = "retry_policy_tests.rs"]
mod retry_policy_tests;

#[cfg(test)]
#[path = "cache_lifetime_tests.rs"]
mod cache_lifetime_tests;

#[cfg(test)]
#[path = "thinking_tests.rs"]
mod thinking_tests;

#[cfg(test)]
#[path = "warm_tests.rs"]
mod warm_tests;

/// The model's declared context window, in tokens
/// (`docs/configuration.md`, "Keys"): `context_window` is required, so a
/// model that declares none — or a zero window — is `no_model` before any
/// credential is read or any connection made. The loop holds the window
/// as a plain `u64` and trusts it to be positive.
pub(crate) fn context_window(model: &ModelData, reference: &str) -> Result<u64, Failure> {
    extensions::context_window(model, reference).map_err(|error| Failure {
        code: error.code(),
        message: error.to_string(),
        retry_after_ms: None,
        provider: None,
    })
}

#[cfg(test)]
#[path = "context_window_tests.rs"]
mod context_window_tests;

/// Each configured `tools."<name>".max_result_bytes`, by the tool's
/// registered name (`docs/configuration.md`, "Keys"; `docs/tools.md`,
/// "Bounded results"). The key is not a per-model key, so the caps are
/// read with no model. An entry whose `max_result_bytes` is absent or not
/// a whole number gives no entry.
pub(crate) fn result_caps(config: &Config) -> r#loop::ResultCaps {
    let mut caps = r#loop::ResultCaps::new();
    let tools = config
        .merged(None)
        .get("tools")
        .cloned()
        .unwrap_or_default();
    let Value::Object(tools) = tools else {
        return caps;
    };
    for (name, entry) in tools {
        if let Value::Object(entry) = entry
            && let Some(cap) = entry.get("max_result_bytes").and_then(Value::as_u64)
        {
            caps.insert(name, cap);
        }
    }
    caps
}

#[cfg(test)]
#[path = "result_caps_tests.rs"]
mod result_caps_tests;

/// Which registered tools the preamble sends deferred (`docs/tools.md`,
/// "Tool search"): `tools."<name>".deferred` by tool name, and
/// `mcp.servers."<server>".declare_in_full` by server. A non-boolean
/// value leaves no entry. `tools."tool_search".deferred` has no effect:
/// `tool_search` is never deferred, so the key gives no entry and one
/// `config_key_ignored` notice. A key on a name the registry does not
/// hold is kept: the build iterates the registry, so it is a silent
/// no-op there. The result never holds `tool_search`.
pub(crate) fn deferrals(config: &Config, notices: &mut Vec<Notice>) -> r#loop::Deferrals {
    let mut out = r#loop::Deferrals::default();
    let merged = config.merged(None);
    if let Value::Object(tools) = merged.get("tools").cloned().unwrap_or_default() {
        for (name, entry) in tools {
            let Value::Object(entry) = entry else {
                continue;
            };
            let Some(deferred) = entry.get("deferred").and_then(Value::as_bool) else {
                continue;
            };
            if name == contract::provider::TOOL_SEARCH {
                notices.push(Notice {
                    code: contract::ErrorCode::ConfigKeyIgnored,
                    message: "`tools.\"tool_search\".deferred` has no effect: `tool_search` is never deferred."
                        .to_owned(),
                    extension: None,
                });
            } else {
                out.tools.insert(name, deferred);
            }
        }
    }
    if let Value::Object(mcp) = merged.get("mcp").cloned().unwrap_or_default()
        && let Value::Object(servers) = mcp.get("servers").cloned().unwrap_or_default()
    {
        for (name, entry) in servers {
            if let Value::Object(entry) = entry
                && entry.get("declare_in_full").and_then(Value::as_bool) == Some(true)
            {
                out.full_servers.insert(name);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "deferrals_tests.rs"]
mod deferrals_tests;

/// Re-reads `skills.disabled` at each turn start, with the check for
/// added and removed skills (`docs/configuration.md`, "When Fiber reads
/// configuration"): loads the configuration as at session start, `-c`
/// overrides included, and unions the layers. A failure keeps the
/// last-known list and raises its notice once, with the error's own code.
pub(crate) fn skills_disabled_reader(
    home: PathBuf,
    workspace: PathBuf,
    project: ProjectKey,
    overrides: Vec<String>,
) -> r#loop::DisabledReader {
    Arc::new(move || {
        match Config::load(Sources {
            home: home.clone(),
            workspace: workspace.clone(),
            project: project.clone(),
            overrides: overrides.clone(),
        }) {
            Ok(config) => Ok(config.union_list("skills.disabled")),
            Err(error) => Err(Notice {
                code: error.code(),
                message: format!(
                    "Fiber could not re-read skills.disabled: {error}. The last list read stays in force."
                ),
                extension: None,
            }),
        }
    })
}

#[cfg(test)]
#[path = "skills_disabled_tests.rs"]
mod skills_disabled_tests;

/// The person's `shell.read_only` entries as the shell classifier takes
/// them: one `(command, flags)` pair per command, shared with every
/// [`tools::Shell`] clone so a reload can replace the contents without
/// rebuilding the shell.
pub(crate) type ShellReadOnly = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// Each person-configured `shell.read_only."<command>".flags` entry, by
/// command name (`docs/configuration.md`, "Keys"; `docs/tools.md",
/// "Other command-line tools"). The key is person-only: a repository
/// value is ignored with a notice before merging, so the checked merged
/// configuration holds personal entries only. An entry without a complete
/// string `flags` array gives no entry; an explicit empty array gives the
/// command with no flags. Entries come out in name order: the merged
/// object's map iterates its keys in order.
pub(crate) fn shell_read_only(config: &Config) -> Vec<(String, Vec<String>)> {
    let mut entries = Vec::new();
    let read_only = config
        .merged(None)
        .get("shell")
        .and_then(|shell| shell.get("read_only"))
        .cloned()
        .unwrap_or_default();
    let Value::Object(read_only) = read_only else {
        return entries;
    };
    for (name, entry) in read_only {
        let Value::Object(entry) = entry else {
            continue;
        };
        let Some(Value::Array(flags)) = entry.get("flags") else {
            continue;
        };
        let mut listed = Vec::new();
        let mut complete = true;
        for flag in flags {
            let Value::String(flag) = flag else {
                complete = false;
                break;
            };
            listed.push(flag.clone());
        }
        if complete {
            entries.push((name, listed));
        }
    }
    entries
}

/// Replaces the shared `shell.read_only` list with the checked
/// configuration's entries, so a reload applies the fresh list without
/// rebuilding the shell (`docs/configuration.md`, "When Fiber reads
/// configuration"). The next list is computed before the lock is taken;
/// only the assignment runs under it. A poisoned lock is recovered.
pub(crate) fn refresh_shell_read_only(handle: &ShellReadOnly, config: &Config) {
    let next = shell_read_only(config);
    *support::lock(handle) = next;
}

#[cfg(test)]
#[path = "shell_read_only_tests.rs"]
mod shell_read_only_tests;
