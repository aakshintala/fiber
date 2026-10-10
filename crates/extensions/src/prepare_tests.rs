//! This platform's binary name, its key and its hex digest, and downloads
//! through the proxy environment (`docs/extensions.md`, "Versions" and
//! "Installing").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::clock::Clock as _;
use fakes::clock::FakeClock;

use super::{INSTALL_STEP_DEADLINE, binary_name, download, hex, platform};
use crate::host::exec::{GRACE, GROUP_POLL};

/// How long a test waits on the run before it fails.
const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

#[test]
fn a_url_names_its_file_without_the_query() {
    assert_eq!(binary_name("https://x/y/tool-1.0?sig=1"), Some("tool-1.0"));
    for bad in ["https://x/y/", "https://x/..", "https://x/y/.?a"] {
        assert_eq!(binary_name(bad), None, "{bad}");
    }
}

#[test]
fn a_digest_is_lowercase_hex() {
    assert_eq!(hex(&[0, 15, 255]), "000fff");
}

#[test]
fn the_platform_key_is_os_and_arch() {
    let key = platform();
    assert!(key.contains('-'), "{key}");
    assert!(!key.contains("macos") && !key.contains("aarch64"), "{key}");
}

/// Present in a re-executed child, absent in the parent.
const PROXY_CHILD: &str = "FIBER_TEST_PREPARE_PROXY_CHILD";

/// The download URL, passed to the child on its environment.
const PROXY_CHILD_URL: &str = "FIBER_TEST_PREPARE_URL";

/// How long the parent waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Downloads the scripted bytes in a child whose environment holds
/// `extra`, on top of the proxy URL and the marker. The child re-runs
/// this same test, which downloads and fails the child when the bytes do
/// not arrive. `None` in the child, after its assertions.
fn download_in_child(
    test: &str,
    extra: &[(&str, &str)],
) -> Option<(fakes::ProviderServer, fakes::ConnectProxy)> {
    if std::env::var_os(PROXY_CHILD).is_some() {
        let url = std::env::var(PROXY_CHILD_URL).unwrap();
        let bytes = download("probe", &url).unwrap();
        assert_eq!(bytes, b"{}");
        return None;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let proxy_url = proxy.url();
    let download_url = format!("{}/tool", server.url());
    let mut env = vec![
        (PROXY_CHILD, "1"),
        ("HTTPS_PROXY", proxy_url.as_str()),
        (PROXY_CHILD_URL, download_url.as_str()),
    ];
    env.extend(extra.iter().copied());
    let output = fakes::rerun(test, &env);
    assert!(
        output.status.success(),
        "the proxy-env child downloaded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some((server, proxy))
}

#[test]
fn download_tunnels_through_the_proxy_environment() {
    let Some((server, proxy)) = download_in_child(
        "prepare::tests::download_tunnels_through_the_proxy_environment",
        &[],
    ) else {
        return;
    };
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let target = format!("127.0.0.1:{port}");
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn download_bypasses_the_proxy_for_no_proxy_hosts() {
    let Some((server, proxy)) = download_in_child(
        "prepare::tests::download_bypasses_the_proxy_for_no_proxy_hosts",
        &[("NO_PROXY", "127.0.0.1")],
    ) else {
        return;
    };
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert_eq!(server.requests().len(), 1);
}

/// A step that never finishes is stopped at the install-step deadline, and
/// the step is gone afterwards.
#[test]
fn a_stalled_install_step_is_stopped_at_its_deadline() {
    let dir = fakes::TempDir::new("fiber-prepare-stall");
    let ready = fakes::children::Ready::new(dir.path());
    // The step ignores SIGTERM, so the stop runs the full grace to SIGKILL.
    // Its pid line proves the trap is set before the clock moves: a TERM
    // before the trap would kill the step at once.
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nwhile :; do :; done\n",
        ready.path().display()
    );
    let manifest: config::Manifest = serde_json::from_value(serde_json::json!({
        "name": "acme",
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": 1,
        "install": ["sh", "-c", script],
    }))
    .unwrap();
    let clock = FakeClock::new();
    let worker_clock = Arc::clone(&clock);
    let worker_dir = dir.path().to_path_buf();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("prepare stall".into())
        .spawn(move || {
            let _sent = done_tx.send(super::prepare(
                &worker_dir,
                &manifest,
                worker_clock.as_ref(),
            ));
        })
        .unwrap();
    let pid = ready.wait(WITHIN)[0];
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, WITHIN),
        "waited {WITHIN:?} for the step to park while running"
    );
    // Past the install-step deadline the run stops.
    clock.advance(INSTALL_STEP_DEADLINE + Duration::from_secs(1));
    let kill_at = clock.now() + GRACE;
    assert!(
        clock.await_parked(kill_at, WITHIN),
        "waited {WITHIN:?} for the step to park for the grace"
    );
    clock.advance(GRACE);
    let err = done_rx
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("waited {WITHIN:?} for the stalled step"))
        .expect_err("a stalled step fails");
    assert!(
        matches!(err, crate::Error::InstallExited { .. }),
        "a stalled step fails as its install step failed: {err}"
    );
    assert!(
        err.to_string().contains("did not finish within"),
        "the failure names the deadline: {err}"
    );
    assert!(
        err.to_string()
            .contains(&format!("{} s", INSTALL_STEP_DEADLINE.as_secs())),
        "the failure names the deadline's seconds: {err}"
    );
    assert!(
        err.to_string().contains("so it was stopped"),
        "the failure names the stop: {err}"
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("a pid probe runs"),
        "the stopped step is gone"
    );
}

/// A step whose program cannot start fails as its install step failed,
/// naming the program.
#[test]
fn a_missing_install_step_program_is_the_spawn_error() {
    let dir = fakes::TempDir::new("fiber-prepare-missing");
    let manifest: config::Manifest = serde_json::from_value(serde_json::json!({
        "name": "acme",
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": 1,
        "install": ["fiber-definitely-missing-xyz"],
    }))
    .unwrap();
    let clock = FakeClock::new();
    let err = super::prepare(dir.path(), &manifest, clock.as_ref()).unwrap_err();
    assert!(
        matches!(err, crate::Error::InstallStep { .. }),
        "a missing program fails as its install step failed: {err}"
    );
    assert!(
        err.to_string()
            .contains("`fiber-definitely-missing-xyz`:"),
        "the spawn error names the program: {err}"
    );
}
