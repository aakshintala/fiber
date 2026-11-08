//! `compaction.*` from configuration reaches the loop's settings.

use super::compaction_settings;
use crate::test_support;

const MODEL: &str = "openai/gpt-5.6";

fn config(overrides: &[&str]) -> config::Config {
    test_support::config("fiber-compaction-settings", overrides)
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let settings = compaction_settings(&config(&[]), MODEL);
    assert_eq!(settings, r#loop::CompactionSettings::default());
}

#[test]
fn each_key_is_read_from_the_configuration() {
    let settings = compaction_settings(
        &config(&[
            "compaction.enabled=false",
            "compaction.tokens=123456",
            "compaction.window_fraction=0.5",
            "compaction.nudge=false",
        ]),
        MODEL,
    );
    assert!(!settings.enabled);
    assert_eq!(settings.tokens, 123_456);
    assert!((settings.window_fraction - 0.5).abs() < f64::EPSILON);
    assert!(!settings.nudge);
}

#[test]
fn a_per_model_key_wins_for_that_model_only() {
    let overrides = [
        "compaction.tokens=300000",
        "models.\"openai/gpt-5.6\".compaction.tokens=200000",
        "models.\"openai/gpt-5.6\".compaction.window_fraction=0.4",
        "models.\"openai/gpt-5.6\".compaction.enabled=false",
        "models.\"openai/gpt-5.6\".compaction.nudge=false",
    ];
    let config = config(&overrides);

    let own = compaction_settings(&config, MODEL);
    assert_eq!(own.tokens, 200_000);
    assert!((own.window_fraction - 0.4).abs() < f64::EPSILON);
    assert!(!own.enabled);
    assert!(!own.nudge);

    let other = compaction_settings(&config, "anthropic/claude");
    assert_eq!(other.tokens, 300_000);
    assert!((other.window_fraction - 0.7).abs() < f64::EPSILON);
    assert!(other.enabled);
    assert!(other.nudge);
}
