//! `session.idle_exit_ms` reaches the loop's idle delay.

use std::time::Duration;

use super::{hub_idle_exit, idle_exit};
use crate::test_support;

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
