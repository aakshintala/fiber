//! `session.idle_exit_ms` reaches the loop's idle delay.

use std::time::Duration;

use super::idle_exit;

fn config(overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-idle-exit");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let project = config::ProjectKey::new("test").unwrap();
    config::Config::load(config::Sources {
        home,
        workspace,
        project,
        overrides,
    })
    .unwrap()
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
