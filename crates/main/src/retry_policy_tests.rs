//! `retry.attempts` from configuration reaches the loop's retry policy.

use super::retry_policy;
use crate::test_support;

fn config(overrides: Vec<String>) -> config::Config {
    test_support::config("fiber-retry-policy", overrides)
}

#[test]
fn retry_policy_clamps_huge_attempts() {
    let retry = retry_policy(&config(vec!["retry.attempts=18446744073709551615".into()]));
    assert_eq!(retry.attempts, u32::MAX);
}

#[test]
fn retry_policy_reads_a_set_initial_delay() {
    let retry = retry_policy(&config(vec!["retry.initial_delay_ms=250".into()]));
    assert_eq!(retry.initial, std::time::Duration::from_millis(250));
}

#[test]
fn retry_policy_reads_a_set_max_delay() {
    let retry = retry_policy(&config(vec!["retry.max_delay_ms=9000".into()]));
    assert_eq!(retry.max, std::time::Duration::from_millis(9000));
}

fn empty_servers() -> crate::mcp_servers::SessionServers {
    let root = fakes::TempDir::new("fiber-stop-and-fail");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let images: std::sync::Arc<dyn contract::images::Images> = std::sync::Arc::new(
        tools::ImageChild::new(std::path::PathBuf::new(), std::path::PathBuf::new()),
    );
    let started = mcp::start(
        Vec::new(),
        &workspace,
        &root.path().join("artifacts"),
        std::sync::Arc::clone(&images),
        &workspace.join("cache"),
        &clock,
        "0.0.0",
    );
    let servers = std::sync::Arc::new(started.servers);
    crate::mcp_servers::SessionServers {
        base_tools: Vec::new(),
        kept_infos: Vec::new(),
        failed: Vec::new(),
        servers: std::sync::Arc::clone(&servers),
        prompts: started.prompts,
        forget: std::sync::Arc::new(|| {}),
        images: std::sync::Arc::clone(&images),
        later: crate::mcp_servers::repository_servers::Later {
            home: root.path().to_path_buf(),
            project: config::ProjectKey::new("test").unwrap(),
            workspace: workspace.clone(),
            artifacts: root.path().join("artifacts"),
            images,
            cache: workspace.join("cache"),
            clock,
            waiting: Vec::new(),
            servers,
        },
    }
}

fn failure(code: contract::ErrorCode) -> contract::shapes::Failure {
    contract::shapes::Failure {
        code,
        message: "no session".to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

#[test]
fn stop_and_fail_returns_usage_as_2() {
    assert_eq!(
        crate::stop_and_fail(empty_servers(), failure(contract::ErrorCode::Usage)),
        2
    );
}

#[test]
fn stop_and_fail_returns_other_failures_as_1() {
    assert_eq!(
        crate::stop_and_fail(
            empty_servers(),
            failure(contract::ErrorCode::McpServerUnavailable)
        ),
        1
    );
}
