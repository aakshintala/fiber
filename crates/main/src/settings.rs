//! Session settings read from configuration with their documented
//! defaults (`docs/configuration.md`).

use std::time::Duration;

use config::{Config, ModelData};
use contract::events::CacheLifetime;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use serde_json::Value;

/// How long an idle session waits before it exits, from
/// `session.idle_exit_ms` (`docs/configuration.md`). `0` exits at the first
/// empty wait. A missing value is 30 minutes.
pub(crate) fn idle_exit(config: &Config) -> Option<Duration> {
    let ms = config
        .get("session.idle_exit_ms", None)
        .and_then(|(value, _)| value.as_u64())
        .unwrap_or(1_800_000);
    Some(Duration::from_millis(ms))
}

/// How long a hub a client started stays running with no client
/// connected, from `hub.idle_exit_ms` (`docs/configuration.md`). `0`
/// exits at the first empty wait. A missing value is 30 minutes.
pub(crate) fn hub_idle_exit(config: &Config) -> Duration {
    let ms = config
        .get("hub.idle_exit_ms", None)
        .and_then(|(value, _)| value.as_u64())
        .unwrap_or(1_800_000);
    Duration::from_millis(ms)
}

/// When a reviewer block hands the call to a person, from configuration
/// with the documented defaults (`docs/configuration.md`).
pub(crate) fn block_limits(config: &Config) -> r#loop::BlockLimits {
    let limit = |key: &str, default: u64| {
        config
            .get(key, None)
            .and_then(|(value, _)| value.as_u64())
            .unwrap_or(default)
    };
    r#loop::BlockLimits {
        consecutive: limit("reviewer.block_limits.consecutive", 3),
        session: limit("reviewer.block_limits.session", 20),
    }
}

/// The prompt-cache lifetime for `model`, from `cache.lifetime`
/// (`docs/prompt-cache.md`, "Cache lifetime" and
/// `docs/configuration.md`). A per-model key wins over the same key at
/// the top level. `"5m"` is five minutes; anything else or absent is the
/// 1-hour default (the config crate's `OneOf` refuses other values).
pub(crate) fn cache_lifetime(config: &Config, model: &str) -> CacheLifetime {
    if config
        .get("cache.lifetime", Some(model))
        .is_some_and(|(value, _)| value == "5m")
    {
        CacheLifetime::FiveMinutes
    } else {
        CacheLifetime::OneHour
    }
}

/// How many cache lifetimes after the last turn an idle session keeps its
/// prompt cache warm: `cache.warm_cap` when `cache.warm_idle` is set, else
/// `None`, which never warms (`docs/prompt-cache.md`, "Warming while idle").
/// A missing cap is the default, 2 lifetimes; the config crate refuses 12
/// or more.
pub(crate) fn warm(config: &Config) -> Option<u32> {
    let on = config
        .get("cache.warm_idle", None)
        .is_some_and(|(value, _)| value == true);
    if !on {
        return None;
    }
    let cap = config
        .get("cache.warm_cap", None)
        .and_then(|(value, _)| value.as_u64())
        .unwrap_or(2);
    Some(u32::try_from(cap).unwrap_or(u32::MAX))
}

/// How a failed model call is retried, from configuration with the
/// documented defaults (`docs/configuration.md`). `attempts` is clamped
/// to `u32`, so a huge configured count never overflows the loop.
pub(crate) fn retry_policy(config: &Config) -> r#loop::Retry {
    let count = |key: &str, default: u64| {
        config
            .get(key, None)
            .and_then(|(value, _)| value.as_u64())
            .unwrap_or(default)
    };
    r#loop::Retry {
        attempts: u32::try_from(count("retry.attempts", 3)).unwrap_or(u32::MAX),
        initial: Duration::from_millis(count("retry.initial_delay_ms", 2000)),
        max: Duration::from_millis(count("retry.max_delay_ms", 60000)),
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod idle_tests;

/// The session's one reasoning setting (`docs/model-routing.md`,
/// "Thinking"): the `:level` suffix first, then the session's own choice,
/// then the configured value (`models."<reference>".thinking` over the
/// top-level `thinking` key), else the model's own default. A chosen level
/// the model does not take is `invalid_arguments` before `session_started`.
/// A model with no levels and no default resolves to `None`.
pub(crate) fn thinking(
    suffix: Option<ThinkingLevel>,
    session: Option<ThinkingLevel>,
    config: &Config,
    model: &ModelData,
    reference: &str,
) -> Result<Option<ThinkingLevel>, Failure> {
    let configured = config
        .get("thinking", Some(reference))
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .and_then(|name| name.parse::<ThinkingLevel>().ok());
    if let Some(level) = suffix.or(session).or(configured) {
        if model.thinking_levels.contains(&level) {
            Ok(Some(level))
        } else {
            let takes = model
                .thinking_levels
                .iter()
                .map(|l| format!("`{l}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let takes = if takes.is_empty() {
                "takes no thinking level".to_owned()
            } else {
                format!("takes {takes}")
            };
            Err(Failure {
                code: ErrorCode::InvalidArguments,
                message: format!(
                    "The thinking level `{level}` is not one model `{reference}` takes: it {takes}.",
                ),
                retry_after_ms: None,
                provider: None,
            })
        }
    } else {
        Ok(model.thinking_default)
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
