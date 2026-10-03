//! Choosing step 7's limits from configuration (`docs/configuration.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use super::block_limits;

fn config(overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-reviewer-limits");
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
fn block_limits_default_without_configuration() {
    let limits = block_limits(&config(Vec::new()));
    assert_eq!(limits.consecutive, 3);
    assert_eq!(limits.session, 20);
}

#[test]
fn block_limits_reads_configuration() {
    let limits = block_limits(&config(vec![
        "reviewer.block_limits.consecutive=7".into(),
        "reviewer.block_limits.session=9".into(),
    ]));
    assert_eq!(limits.consecutive, 7);
    assert_eq!(limits.session, 9);
}
