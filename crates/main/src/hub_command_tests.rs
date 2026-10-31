//! Tests for the internal hub command: the exit code of a failed start
//! and the hub's configuration.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::time::Duration;

use super::*;

#[test]
fn fail_exits_1() {
    assert_eq!(
        crate::fail(crate::failed(
            ErrorCode::IoFailed,
            "the current directory: gone"
        )),
        1
    );
}

#[test]
fn a_usage_failure_exits_2() {
    assert_eq!(
        crate::fail(crate::failed(ErrorCode::Usage, "FIBER_HOME is empty.")),
        2
    );
}

#[test]
fn finish_passes_the_hub_exit_code_through() {
    assert_eq!(finish(Ok(0)), 0);
    assert_eq!(finish(Ok(143)), 143);
}

#[test]
fn finish_exits_2_for_a_too_long_home_and_1_for_an_io_failure() {
    assert_eq!(finish(Err(hub::StartError::HomeTooLong { max: 103 })), 2);
    let io = hub::StartError::Io {
        path: PathBuf::from("/h/run"),
        source: std::io::Error::other("refused"),
    };
    assert_eq!(finish(Err(io)), 1);
}

/// A Fiber home and a workspace under one temporary root.
fn home_and_workspace(root: &fakes::TempDir) -> (PathBuf, PathBuf) {
    let home = root.path().join("h");
    let workspace = root.path().join("w");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    (home, workspace)
}

#[test]
fn configure_without_a_current_directory_is_io_failed_naming_it() {
    let root = fakes::TempDir::new("hcfg");
    let (home, _) = home_and_workspace(&root);
    let Err(failure) = configure(&home, Err(std::io::Error::other("gone"))) else {
        panic!("no current directory cannot configure");
    };
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert_eq!(failure.message, "the current directory: gone");
}

#[test]
fn configure_with_invalid_json_is_config_invalid() {
    let root = fakes::TempDir::new("hcfg");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(home.join("config.json"), "{").unwrap();
    let Err(failure) = configure(&home, Ok(workspace)) else {
        panic!("invalid JSON cannot configure");
    };
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
    assert!(
        failure.message.contains("config.json"),
        "{}",
        failure.message
    );
}

#[test]
fn configure_reads_the_idle_exit() {
    let root = fakes::TempDir::new("hcfg");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(
        home.join("config.json"),
        r#"{"hub": {"idle_exit_ms": 200}}"#,
    )
    .unwrap();
    assert_eq!(
        configure(&home, Ok(workspace)).unwrap(),
        hub::Settings {
            idle_exit: Duration::from_millis(200),
            level: log::diag::Level::Info,
            port: None,
        }
    );
}

#[test]
fn configure_reads_a_global_port_with_its_origins() {
    let root = fakes::TempDir::new("hport");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(
        home.join("config.json"),
        r#"{"hub": {"port": 4040, "allowed_origins": ["https://a.example"]}}"#,
    )
    .unwrap();
    assert_eq!(
        configure(&home, Ok(workspace)).unwrap().port,
        Some(hub::PortSettings {
            number: 4040,
            allowed_origins: vec!["https://a.example".to_owned()],
        })
    );
}

#[test]
fn configure_without_a_global_port_has_no_port_even_with_origins() {
    let root = fakes::TempDir::new("hport");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(
        home.join("config.json"),
        r#"{"hub": {"allowed_origins": ["https://a.example"]}}"#,
    )
    .unwrap();
    assert_eq!(configure(&home, Ok(workspace)).unwrap().port, None);
}

#[test]
fn configure_ignores_a_project_files_port() {
    let root = fakes::TempDir::new("hport");
    let (home, workspace) = home_and_workspace(&root);
    let (sessions, _) =
        ::cli::project_of(&home, &workspace).unwrap_or_else(|e| panic!("project: {e}"));
    let dir = sessions.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"hub": {"port": 5050}}"#).unwrap();
    assert_eq!(configure(&home, Ok(workspace)).unwrap().port, None);
}

#[test]
fn configure_reads_the_top_of_the_port_range() {
    let root = fakes::TempDir::new("hport");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(home.join("config.json"), r#"{"hub": {"port": 65535}}"#).unwrap();
    assert_eq!(
        configure(&home, Ok(workspace)).unwrap().port,
        Some(hub::PortSettings {
            number: 65535,
            allowed_origins: Vec::new(),
        })
    );
}

#[test]
fn configure_reads_a_global_debug_level() {
    let root = fakes::TempDir::new("hcfg");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(
        home.join("config.json"),
        r#"{"diagnostics": {"level": "debug"}}"#,
    )
    .unwrap();
    assert_eq!(
        configure(&home, Ok(workspace)).unwrap().level,
        log::diag::Level::Debug
    );
}
