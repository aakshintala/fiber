//! `handoff.*` from configuration reaches the loop's settings.

use super::handoff_settings;
use crate::test_support;

const MODEL: &str = "openai/gpt-5.6";

fn config(overrides: &[&str]) -> config::Config {
    test_support::config("fiber-handoff-settings", overrides)
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let settings = handoff_settings(&config(&[]), MODEL);
    assert_eq!(settings, r#loop::HandoffSettings::default());
    assert!(settings.enabled);
    assert_eq!(settings.tokens, 400_000);
    assert!((settings.window_fraction - 0.7).abs() < f64::EPSILON);
    assert!(settings.nudge);
}

#[test]
fn each_key_is_read_from_the_configuration() {
    let settings = handoff_settings(
        &config(&[
            "handoff.enabled=false",
            "handoff.tokens=123456",
            "handoff.window_fraction=0.5",
            "handoff.nudge=false",
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
        "handoff.tokens=300000",
        "models.\"openai/gpt-5.6\".handoff.tokens=200000",
        "models.\"openai/gpt-5.6\".handoff.window_fraction=0.4",
        "models.\"openai/gpt-5.6\".handoff.enabled=false",
        "models.\"openai/gpt-5.6\".handoff.nudge=false",
    ];
    let config = config(&overrides);

    let own = handoff_settings(&config, MODEL);
    assert_eq!(own.tokens, 200_000);
    assert!((own.window_fraction - 0.4).abs() < f64::EPSILON);
    assert!(!own.enabled);
    assert!(!own.nudge);

    let other = handoff_settings(&config, "anthropic/claude");
    assert_eq!(other.tokens, 300_000);
    assert!((other.window_fraction - 0.7).abs() < f64::EPSILON);
    assert!(other.enabled);
    assert!(other.nudge);
}
