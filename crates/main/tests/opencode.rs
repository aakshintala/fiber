//! The shipped opencode package's Zen provider (`docs/tools.md`, "Where it
//! comes from"): Zen reports no quota, so the package declares nothing for
//! it, and reading its quota through the public reader reports no quota
//! without any fetch. The package's `init.lua` is compiled in, so the test
//! reads no repository file at run time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::Arc;

/// The package under test, compiled in.
const INIT: &str = include_str!("../../../providers/opencode/init.lua");

#[test]
fn an_opencode_zen_quota_read_reports_no_quota() {
    let root = fakes::TempDir::new("fiber-opencode-zen-quota");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.lua"), INIT).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let extension = Arc::new(extensions::LuaExtension::new(
        "github.com/aakshintala/fiber/providers/opencode",
        dir,
        home,
        clock,
    ));
    let provider = extensions::LuaProvider::new(extension, "opencode-zen");
    assert!(
        !provider.registers("quota").unwrap(),
        "the opencode package declares quota() for opencode-zen"
    );
    let pair = extensions::CredentialPair {
        credential: "opencode".to_owned(),
        label: "default".to_owned(),
    };
    assert_eq!(provider.quota(&pair, None), extensions::QuotaEntry::NoQuota);
}
