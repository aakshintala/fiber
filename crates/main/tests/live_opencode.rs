//! The opt-in live test for the opencode package's `quota()`
//! (`docs/testing.md`, "Live calls and evals"): one quota fetch against the
//! real endpoint. It returns at once unless `FIBER_LIVE_OPENCODE_KEY` holds
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
const INIT: &str = include_str!("../../../providers/opencode/init.lua");

/// How long the live quota fetch may take.
const LIVE_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn a_live_opencode_quota_fetch_reports_three_windows() {
    let Ok(key) = std::env::var("FIBER_LIVE_OPENCODE_KEY") else {
        return;
    };
    let root = fakes::TempDir::new("fiber-live-opencode-quota");
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
    let provider = extensions::LuaProvider::new(extension, "opencode-go");
    let pair = extensions::CredentialPair {
        credential: "opencode".to_owned(),
        label: "default".to_owned(),
    };
    let secret = contract::Secret::new(key);
    let (done, fetched) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(provider.fetch_quota(&pair, Some(&secret))));
    let (quota, _) = fakes::Deadline::after(LIVE_DEADLINE)
        .recv(&fetched)
        .expect("the live quota fetch never returned")
        .expect("the live quota fetch failed");
    let Some(quota) = quota else {
        panic!("the live quota fetch reported nothing");
    };
    let extensions::Quota::Windows(windows) = quota else {
        panic!("the live quota is not windows: {quota:?}");
    };
    let names: Vec<&str> = windows.iter().map(|window| window.name.as_str()).collect();
    assert_eq!(
        names,
        ["rolling", "weekly", "monthly"],
        "the live quota holds unexpected windows: {windows:?}"
    );
    for window in &windows {
        let extensions::WindowUse::Percent { used, .. } = &window.used else {
            panic!("the live window is already reset: {window:?}");
        };
        assert!(
            used.is_finite() && (0.0..=100.0).contains(used),
            "the live window percent is out of range: {window:?}"
        );
    }
}
