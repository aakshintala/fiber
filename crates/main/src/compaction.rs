//! `compaction.*` for the session's model (`docs/configuration.md`).

use config::Config;
use r#loop::CompactionSettings;

/// The `compaction.*` settings for `model`. The config schema supplies the
/// defaults and refuses a value of another type. A per-model key wins
/// over the same key at the top level.
pub(crate) fn compaction_settings(config: &Config, model: &str) -> CompactionSettings {
    CompactionSettings {
        enabled: config
            .flag("compaction.enabled", Some(model))
            .unwrap_or_default(),
        tokens: config
            .count("compaction.tokens", Some(model))
            .unwrap_or_default(),
        window_fraction: config
            .get("compaction.window_fraction", Some(model))
            .and_then(|(value, _)| value.as_f64())
            .unwrap_or_default(),
        nudge: config
            .flag("compaction.nudge", Some(model))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
