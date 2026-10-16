//! `handoff.*` for the session's model (`docs/configuration.md`).

use config::Config;
use r#loop::HandoffSettings;

/// The `handoff.*` settings for `model`, with the documented defaults for
/// any key absent or of another type. A per-model key wins over the same
/// key at the top level.
pub(crate) fn handoff_settings(config: &Config, model: &str) -> HandoffSettings {
    HandoffSettings {
        enabled: config
            .flag("handoff.enabled", Some(model))
            .unwrap_or_default(),
        tokens: config
            .count("handoff.tokens", Some(model))
            .unwrap_or_default(),
        window_fraction: config
            .get("handoff.window_fraction", Some(model))
            .and_then(|(value, _)| value.as_f64())
            .unwrap_or_default(),
        nudge: config
            .flag("handoff.nudge", Some(model))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
