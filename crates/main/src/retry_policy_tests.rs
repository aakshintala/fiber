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
