//! `debug_writer_for`: `Some` at debug, `None` at info. Building the
//! writer never fails the command: no assertion here touches the
//! filesystem beyond the config read.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::Arc;

use super::debug_writer_for;

fn config_with(home: &std::path::Path, workspace: &std::path::Path, level: &str) -> config::Config {
    std::fs::write(
        home.join("config.json"),
        serde_json::json!({"diagnostics": {"level": level}}).to_string(),
    )
    .unwrap();
    let (_, project) = crate::project_of(home, workspace).unwrap();
    config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .unwrap()
}

#[test]
fn debug_config_builds_a_debugging_writer() {
    let tmp = fakes::TempDir::new("fiber-debug-writer");
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config = config_with(&home, &workspace, "debug");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let writer = debug_writer_for(&home, &config, &clock).expect("debug builds a writer");
    assert!(writer.debugging(), "the writer debugs");
}

#[test]
fn info_config_builds_no_writer() {
    let tmp = fakes::TempDir::new("fiber-debug-writer");
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config = config_with(&home, &workspace, "info");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    assert!(
        debug_writer_for(&home, &config, &clock).is_none(),
        "info stays silent"
    );
}
