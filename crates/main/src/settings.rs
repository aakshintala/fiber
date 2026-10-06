//! Session settings read from configuration with their documented
//! defaults (`docs/configuration.md`).

use std::time::Duration;

use config::Config;
use contract::events::CacheLifetime;

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

/// How a failed model call is retried, from configuration with the
/// documented defaults (`docs/configuration.md`). `attempts` is clamped
/// to `u32`, so a huge configured count never overflows the loop.
/// The prompt-cache lifetime for `model`, from `cache.lifetime`
/// (`docs/prompt-cache.md`, "Cache lifetime" and
/// `docs/configuration.md`). A per-model key wins over the same key at
/// the top level. `"5m"` is five minutes; anything else or absent is the
/// 1-hour default (the config crate's `OneOf` refuses other values).
pub(crate) fn cache_lifetime(config: &Config, model: &str) -> CacheLifetime {
    let lifetime = config
        .get("cache.lifetime", Some(model))
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    if lifetime.as_deref() == Some("5m") {
        CacheLifetime::FiveMinutes
    } else {
        CacheLifetime::OneHour
    }
}

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

#[cfg(test)]
#[path = "retry_policy_tests.rs"]
mod retry_policy_tests;

#[cfg(test)]
#[path = "cache_lifetime_tests.rs"]
mod cache_lifetime_tests;
