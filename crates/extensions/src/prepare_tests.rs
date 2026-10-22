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
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::{INSTALL_STEP_DEADLINE, binary_name, download, download_with, platform};
use support::group::{GRACE, GROUP_POLL};

/// How long a test waits on the run before it fails.
const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

#[test]
fn the_manifest_name_names_the_binary_file() {
    assert_eq!(
        binary_name("github.com/acme/fiber-acme"),
        Some("fiber-acme")
    );
    assert_eq!(binary_name("acme"), Some("acme"));
    for bad in ["", "a/", "a/.", "a/.."] {
        assert_eq!(binary_name(bad), None, "{bad}");
    }
}

#[test]
fn a_manifest_name_with_no_file_fails_before_any_download() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = format!("{}/tool", server.url());
    let manifest: config::Manifest = serde_json::from_value(serde_json::json!({
        "name": "a/",
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": 1,
        "binaries": { platform(): { "url": url, "sha256": "0".repeat(64) } },
    }))
    .unwrap();
    let dir = fakes::TempDir::new("fiber-prepare-no-file");
    let clock = FakeClock::new();
    let err = super::prepare(dir.path(), &manifest, clock.as_ref()).unwrap_err();
    let crate::Error::Download { name, why } = err else {
        panic!("a nameless binary fails as its download failed: {err}");
    };
    assert_eq!(name, "a/", "the failure names the manifest");
    assert!(why.contains("`a/`"), "the failure names the name: {why}");
    assert!(
        server.requests().is_empty(),
        "no download was made: {:?}",
        server.requests()
    );
}

#[test]
fn prepare_over_an_old_url_named_file_leaves_it_in_place() {
    let body = b"#!/bin/sh\necho hi\n";
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, body.to_vec())]).unwrap();
    let url = format!("{}/tool-1.0", server.url());
    let manifest: config::Manifest = serde_json::from_value(serde_json::json!({
        "name": "acme",
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": 1,
        "binaries": { platform(): { "url": url, "sha256": super::hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref()) } },
    }))
    .unwrap();
    let dir = fakes::TempDir::new("fiber-prepare-stale");
    let stale = dir.path().join("bin").join("tool-1.0");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, b"stale").unwrap();
    let clock = FakeClock::new();
    super::prepare(dir.path(), &manifest, clock.as_ref()).unwrap();
    assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
    assert_eq!(
        std::fs::read(dir.path().join("bin").join("acme")).unwrap(),
        body
    );
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
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
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

/// How long a download-bound test waits on the blocking download before
/// it fails. Far above the 200 ms idle limit under test, far below the
/// 30 s hold, so without the limit the wait outlives the test and fails
/// naming it.
const DOWNLOAD_WITHIN: Duration = Duration::from_secs(10);

/// A download whose server never answers fails as the install's own
/// download error within the test's deadline, not after the hold lets go.
#[test]
fn a_held_download_fails_within_the_limit() {
    let server = fakes::ProviderServer::start([]).unwrap();
    server.hold_from(1, Duration::from_secs(30));
    let url = format!("{}/tool", server.url());
    let err = fakes::within(
        "a held download",
        Deadline::after(DOWNLOAD_WITHIN),
        move || {
            download_with(
                "probe",
                &url,
                None,
                net::Limits::new(Duration::from_secs(1), Duration::from_millis(200)).unwrap(),
            )
            .unwrap_err()
        },
    );
    assert!(
        matches!(err, crate::Error::Download { .. }),
        "a held download fails as its download failed: {err}"
    );
}

/// A download stalled mid-body fails as the install's own download error,
/// and the server sees the client close the connection.
#[test]
fn a_stalled_download_body_fails_and_closes() {
    let server = fakes::ProviderServer::start([fakes::Response::stall(200, "abc", 1000)]).unwrap();
    let url = format!("{}/tool", server.url());
    let err = fakes::within(
        "a stalled download",
        Deadline::after(DOWNLOAD_WITHIN),
        move || {
            download_with(
                "probe",
                &url,
                None,
                net::Limits::new(Duration::from_secs(1), Duration::from_millis(200)).unwrap(),
            )
            .unwrap_err()
        },
    );
    assert!(
        matches!(err, crate::Error::Download { .. }),
        "a stalled download fails as its download failed: {err}"
    );
    assert!(
        server.await_closed(1, Deadline::after(WITHIN)),
        "the server saw the stalled download close"
    );
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
    let pid = ready.wait(fakes::Deadline::after(WITHIN))[0];
    // The spinner shares this test's group: match its argv by pid alone,
    // so a panic anywhere below still kills it.
    let watchdog = fakes::Watchdog::matching(&ready.path().display().to_string());
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, fakes::Deadline::after(WITHIN)),
        "waited {WITHIN:?} for the step to park while running"
    );
    // At the install-step deadline the run stops.
    // Crosses only poll pauses to the install-step deadline; the run is proven parked at its poll (ruling 6260728504).
    clock.advance(INSTALL_STEP_DEADLINE);
    let kill_at = clock.now() + GRACE;
    let answered = crate::stall::await_grace_or_answer(&clock, &done_rx, kill_at);
    let answered =
        crate::stall::advance_past_grace_or_answer(&clock, &done_rx, kill_at, GRACE, answered);
    let answer = match answered {
        Some(answer) => answer,
        None => Deadline::after(WITHIN)
            .recv(&done_rx)
            .unwrap_or_else(|_| panic!("waited {WITHIN:?} for the stalled step")),
    };
    let err = answer.expect_err("a stalled step fails");
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
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
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
        err.to_string().contains("`fiber-definitely-missing-xyz`:"),
        "the spawn error names the program: {err}"
    );
}

/// A member repository whose manifest carries this platform's binary at
/// `url` with digest `sha`, committed and tagged `version`.
fn binary_repo(
    root: &std::path::Path,
    name: &str,
    version: &str,
    url: &str,
    sha: &str,
) -> std::path::PathBuf {
    let repo = root.join(name.replace('/', "-"));
    std::fs::create_dir_all(&repo).unwrap();
    let manifest = serde_json::json!({
        "name": name,
        "version": version,
        "fiber": "0.0.0",
        "api": 1,
        "binaries": { platform(): { "url": url, "sha256": sha } },
    });
    std::fs::write(
        repo.join("extension.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();
    fakes::git::git(&repo, &["init", "--quiet"]);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "release"]);
    fakes::git::git(&repo, &["tag", &format!("v{version}")]);
    repo
}

fn file_origin(dir: std::path::PathBuf) -> crate::Origin {
    crate::Origin::new("git", move |_| format!("file://{}", dir.display()))
}

/// Stages `name` from `repo` for install, as the update's batch plan
/// stages it before the first rename.
fn stage_install<'a>(
    home: &'a std::path::Path,
    name: &str,
    repo: &std::path::Path,
    clock: &'a dyn contract::clock::Clock,
) -> crate::manage::Plan<'a> {
    let origin = file_origin(repo.to_owned());
    crate::manage::plan(
        home,
        &crate::manage::Request::Install(name.into()),
        "0.0.0",
        &origin,
        clock,
    )
    .unwrap()
}

#[test]
fn a_binary_checksum_mismatch_in_pre_stage_renames_nothing() {
    let body = b"#!/bin/sh\necho hi\n";
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, body.to_vec())]).unwrap();
    let url = format!("{}/tool", server.url());
    let root = fakes::TempDir::new("fiber-prepare-mismatch");
    let repo = binary_repo(
        root.path(),
        "github.com/acme/widget",
        "1.0.0",
        &url,
        &"0".repeat(64),
    );
    let home = root.path().join("home");
    let clock = FakeClock::new();
    let plan = stage_install(&home, "github.com/acme/widget", &repo, clock.as_ref());
    let mut failed = None;
    for item in plan.items() {
        if let Err(err) = super::prepare(item.staged(), &item.manifest, clock.as_ref()) {
            failed = Some(err);
            break;
        }
    }
    let err = failed.expect("the pre-stage prepare fails");
    let crate::Error::BinaryChecksum { name, url: got } = err else {
        panic!("a checksum mismatch fails as its checksum failed: {err}");
    };
    assert_eq!(name, "github.com/acme/widget");
    assert_eq!(got, url);
    assert!(
        !home.join("extensions/github.com-acme-widget").exists(),
        "the mismatch renames nothing into place"
    );
    assert!(
        !home.join("extensions/.commit").exists(),
        "the mismatch writes no commit journal"
    );
}

#[test]
fn a_prepared_commit_downloads_nothing() {
    let body = b"#!/bin/sh\necho hi\n";
    let digest = super::hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref());
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, body.to_vec()),
        fakes::Response::status(200, body.to_vec()),
    ])
    .unwrap();
    let url = format!("{}/tool", server.url());
    let root = fakes::TempDir::new("fiber-prepare-no-redownload");
    let repo = binary_repo(
        root.path(),
        "github.com/acme/widget",
        "1.0.0",
        &url,
        &digest,
    );
    let home = root.path().join("home");
    let clock = FakeClock::new();
    let mut plan = stage_install(&home, "github.com/acme/widget", &repo, clock.as_ref());
    for item in plan.items() {
        super::prepare(item.staged(), &item.manifest, clock.as_ref()).unwrap();
    }
    let before = server.requests().len();
    assert_eq!(before, 1, "the pre-stage downloads the binary once");
    plan.commit_prepared().unwrap();
    assert_eq!(
        server.requests().len(),
        before,
        "the commit downloads nothing more"
    );
    assert_eq!(
        std::fs::read(home.join("extensions/github.com-acme-widget/bin/widget")).unwrap(),
        body
    );
}

/// The update's pre-stage downloads the member binary but runs no
/// install step: the step runs once after approval, at the commit.
#[test]
fn prepare_binary_downloads_the_binary_but_runs_no_install_step() {
    let body = b"#!/bin/sh\necho hi\n";
    let digest = super::hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref());
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, body.to_vec())]).unwrap();
    let url = format!("{}/tool", server.url());
    let manifest: config::Manifest = serde_json::from_value(serde_json::json!({
        "name": "acme",
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": 1,
        "binaries": { platform(): { "url": url, "sha256": digest } },
        "install": ["sh", "-c", "touch step-ran"],
    }))
    .unwrap();
    let dir = fakes::TempDir::new("fiber-prepare-binary-only");
    let clock = FakeClock::new();
    super::prepare_binary(dir.path(), &manifest).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("bin").join("acme")).unwrap(),
        body
    );
    assert!(
        !dir.path().join("step-ran").exists(),
        "the pre-stage runs no install step"
    );
    super::install_step(dir.path(), &manifest, clock.as_ref()).unwrap();
    assert!(
        dir.path().join("step-ran").exists(),
        "the commit's install step runs the step"
    );
}
