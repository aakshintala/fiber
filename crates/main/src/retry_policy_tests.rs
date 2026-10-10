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

fn empty_servers() -> crate::mcp_servers::SessionServers {
    let root = fakes::TempDir::new("fiber-stop-and-fail");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let started = mcp::start(
        Vec::new(),
        &workspace,
        &workspace.join("cache"),
        &clock,
        "0.0.0",
    );
    crate::mcp_servers::SessionServers {
        failed: Vec::new(),
        servers: started.servers,
        prompts: started.prompts,
        forget: std::sync::Arc::new(|| {}),
        images: std::sync::Arc::new(tools::ImageChild::new(
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
        )),
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
