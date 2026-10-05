//! `retry.attempts` from configuration reaches the loop's retry policy.

use super::retry_policy;

fn config(overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-retry-policy");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let project = config::ProjectKey::new("test").unwrap();
    let config = config::Config::load(config::Sources {
        home,
        workspace,
        project,
        overrides,
    })
    .unwrap();
    // `root` is dropped here; the configuration was already read.
    config
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
    let started = mcp::start(Vec::new(), &workspace, &clock, "0.0.0");
    crate::mcp_servers::SessionServers {
        failed: Vec::new(),
        servers: started.servers,
    }
}

fn failure(code: contract::ErrorCode) -> contract::shapes::Failure {
    contract::shapes::Failure {
        code,
        message: "no session".to_owned(),
        retry_after: None,
        provider: None,
    }
}

#[test]
fn stop_and_fail_returns_usage_as_2() {
    assert_eq!(
        super::stop_and_fail(empty_servers(), failure(contract::ErrorCode::Usage)),
        2
    );
}

#[test]
fn stop_and_fail_returns_other_failures_as_1() {
    assert_eq!(
        super::stop_and_fail(
            empty_servers(),
            failure(contract::ErrorCode::McpServerUnavailable)
        ),
        1
    );
}
