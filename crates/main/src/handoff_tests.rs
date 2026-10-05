//! `handoff.*` from configuration reaches the loop's settings.

use super::handoff_settings;

const MODEL: &str = "openai/gpt-5.6";

fn config(overrides: &[&str]) -> config::Config {
    let root = fakes::TempDir::new("fiber-handoff-settings");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    config::Config::load(config::Sources {
        home,
        workspace,
        project: config::ProjectKey::new("test").unwrap(),
        overrides: overrides.iter().map(|o| (*o).to_owned()).collect(),
    })
    .unwrap()
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
