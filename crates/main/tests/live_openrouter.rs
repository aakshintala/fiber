//! The opt-in live test for the openrouter package's `quota()`
//! (`docs/testing.md`, "Live calls and evals"): one quota fetch against the
//! real endpoint. It returns at once unless `FIBER_LIVE_OPENROUTER_KEY` holds
//! a key; it never runs in CI. The package's `init.lua` is compiled in, so
//! the test reads no repository file at run time, and the fetch runs on a
//! thread bounded by a channel deadline instead of hanging the test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::Arc;
use std::time::Duration;

/// The package under test, compiled in.
const INIT: &str = include_str!("../../../providers/openrouter/init.lua");

/// How long the live quota fetch may take.
const LIVE_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn a_live_openrouter_quota_fetch_reports_credit() {
    let Ok(key) = std::env::var("FIBER_LIVE_OPENROUTER_KEY") else {
        return;
    };
    let root = fakes::TempDir::new("fiber-live-openrouter-quota");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.lua"), INIT).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let extension = Arc::new(extensions::LuaExtension::new(
        "github.com/aakshintala/fiber/providers/openrouter",
        dir,
        home,
        clock,
    ));
    let provider = extensions::LuaProvider::new(extension, "openrouter");
    let pair = extensions::CredentialPair {
        credential: "openrouter".to_owned(),
        label: "default".to_owned(),
    };
    let secret = contract::Secret::new(key);
    let (done, fetched) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(provider.fetch_quota(&pair, Some(&secret))));
    let (quota, _) = fakes::Deadline::after(LIVE_DEADLINE)
        .recv(&fetched)
        .expect("the live quota fetch never returned")
        .expect("the live quota fetch failed");
    if let Some(quota) = quota {
        let extensions::Quota::Credit { remaining, limit } = quota else {
            panic!("the live quota is not credit: {quota:?}");
        };
        assert!(
            remaining.is_finite(),
            "the live credit remaining is not finite: {remaining}"
        );
        if let Some(limit) = limit {
            assert!(limit >= 0.0, "the live credit limit is negative: {limit}");
        }
    }
}
