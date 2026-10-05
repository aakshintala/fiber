//! `handoff.*` for the session's model (`docs/configuration.md`).

use config::Config;
use r#loop::HandoffSettings;

/// The `handoff.*` settings for `model`, with the documented defaults for
/// any key absent or of another type. A per-model key wins over the same
/// key at the top level.
pub(crate) fn handoff_settings(config: &Config, model: &str) -> HandoffSettings {
    let defaults = HandoffSettings::default();
    let get = |key: &str| config.get(key, Some(model)).map(|(value, _)| value);
    HandoffSettings {
        enabled: get("handoff.enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(defaults.enabled),
        tokens: get("handoff.tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(defaults.tokens),
        window_fraction: get("handoff.window_fraction")
            .and_then(|value| value.as_f64())
            .unwrap_or(defaults.window_fraction),
        nudge: get("handoff.nudge")
            .and_then(|value| value.as_bool())
            .unwrap_or(defaults.nudge),
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
