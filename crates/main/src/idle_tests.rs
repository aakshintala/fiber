//! `session.idle_exit_ms` reaches the loop's idle delay.

use std::time::Duration;

use super::idle_exit;
use crate::test_support;

fn config(overrides: Vec<String>) -> config::Config {
    test_support::config("fiber-idle-exit", overrides)
}

#[test]
fn idle_exit_uses_a_set_value() {
    let idle = idle_exit(&config(vec!["session.idle_exit_ms=5000".into()]));
    assert_eq!(idle, Some(Duration::from_millis(5000)));
}

#[test]
fn idle_exit_zero_is_an_immediate_deadline() {
    let idle = idle_exit(&config(vec!["session.idle_exit_ms=0".into()]));
    assert_eq!(idle, Some(Duration::ZERO));
}

#[test]
fn idle_exit_falls_back_to_thirty_minutes() {
    let idle = idle_exit(&config(Vec::new()));
    assert_eq!(idle, Some(Duration::from_millis(1_800_000)));
}
