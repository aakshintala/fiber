//! `session.idle_exit_ms` reaches the loop's idle delay.

use std::time::Duration;

use super::{hub_idle_exit, idle_exit};
use crate::test_support;
use extensions::SessionExtensions;

fn config(overrides: Vec<String>) -> config::Config {
    test_support::config("fiber-idle-exit", overrides)
}

#[test]
fn idle_exit_uses_a_set_value() {
    let idle = idle_exit(&config(vec!["session.idle_exit_ms=5000".into()]));
    assert_eq!(idle, Duration::from_millis(5000));
}

#[test]
fn idle_exit_zero_is_an_immediate_deadline() {
    let idle = idle_exit(&config(vec!["session.idle_exit_ms=0".into()]));
    assert_eq!(idle, Duration::ZERO);
}

#[test]
fn hub_idle_exit_uses_a_set_value() {
    let idle = hub_idle_exit(&config(vec!["hub.idle_exit_ms=7000".into()]));
    assert_eq!(idle, Duration::from_millis(7000));
}

#[test]
fn session_idle_exit_ms_reaches_the_loop_idle_exit() {
    let rig = test_support::Rig::new("fiber-idle-exit-wire");
    let project = config::ProjectKey::new("test").unwrap();
    let extensions = SessionExtensions::default();
    let model: config::ModelData = serde_json::from_value(serde_json::json!({
        "id": "gpt-5.6",
        "protocol": "openai-responses",
        "base_url": "https://x/v1",
        "context_window": 1000,
    }))
    .unwrap();

    let config = rig.config(["session.idle_exit_ms=9000"]);
    let mut notices = Vec::new();
    let settings = crate::parts::reload_inputs(
        &config,
        &rig.home,
        &project,
        &extensions,
        &model,
        "test/gpt-5.6",
        &mut notices,
    )
    .1;
    assert_eq!(settings.idle_exit, Some(Duration::from_millis(9000)));

    let zero = rig.config(["session.idle_exit_ms=0"]);
    let mut notices = Vec::new();
    let settings = crate::parts::reload_inputs(
        &zero,
        &rig.home,
        &project,
        &extensions,
        &model,
        "test/gpt-5.6",
        &mut notices,
    )
    .1;
    assert_eq!(settings.idle_exit, Some(Duration::ZERO));
}
