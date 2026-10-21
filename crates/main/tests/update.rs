//! Binary-level `fiber update` runs (`docs/releasing.md`, "Updating"):
//! the built `fiber` updates itself from a fixture server's releases,
//! `upgrade` runs the same command, and a restarted hub keeps its
//! sessions. A run with no `update` command (the base) fails every test
//! here. Every run carries a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::ustar::{archive, gzip, header, sha256};
use serde_json::json;
use support::{Output, Setup, spawn_watched};

/// The release the fixture server holds above this binary's `0.0.0`.
const VERSION: &str = "0.0.1";

/// The nightly the fixture list names, above `0.0.0` too.
const NIGHTLY: &str = "0.0.1-nightly.20240101";

/// The bytes the fixture binary archives hold: the staged `fiber` binary.
const MARKER: &[u8] = b"binary-from-the-fixture-release";

/// Fiber's own repository as a local one, tagged with the versions the
/// server serves, and the environment that makes `https://github.com/`
/// mean the directory holding it.
struct Github {
    key: String,
}

impl Github {
    #[track_caller]
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("fiber.md"), "fiber").unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "fiber"]);
        for version in [VERSION, NIGHTLY] {
            fakes::git::git(&repo, &["tag", &format!("v{version}")]);
        }
        Self {
            key: format!("url.file://{}/.insteadOf", base.display()),
        }
    }
}

impl Setup {
    /// Runs `binary` with `args` and waits for it under the test's
    /// [`Deadline`]. The hidden `--base-url` points both release roots
    /// at `server`, `channel` sets `FIBER_CHANNEL`, and git resolves
    /// `github.com` to the fixture in `Github`.
    #[track_caller]
    fn run_update(
        &self,
        binary: &Path,
        server: &fakes::ProviderServer,
        github: &Github,
        args: &[&str],
        channel: Option<&str>,
    ) -> Output {
        let mut command = Command::new(binary);
        command
            .args(args)
            .arg("--base-url")
            .arg(server.url())
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", &github.key)
            .env("GIT_CONFIG_VALUE_0", "https://github.com/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(channel) = channel {
            command.env("FIBER_CHANNEL", channel);
        }
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        watchdog.stand_down(self.deadline.cleanup());
        Output::from(output)
    }
}

/// A copy of the built `fiber` the update renames over, so the shared
/// binary stays untouched.
#[track_caller]
fn copy_binary(setup: &Setup) -> PathBuf {
    let copy = setup.root.path().join("fiber-copy");
    fs::copy(env!("CARGO_BIN_EXE_fiber"), &copy).unwrap();
    copy
}

/// The fixture releases: docs, extensions and binary archives for
/// [`VERSION`] and [`NIGHTLY`], the `/releases/latest` redirect at the
/// stable tag, and the nightly list naming [`NIGHTLY`].
fn serve() -> fakes::ProviderServer {
    let manifest = |version: &str| {
        json!({
            "name": "github.com/aakshintala/fiber/providers/anthropic",
            "version": version,
            "fiber": version,
            "api": 1,
        })
        .to_string()
    };
    let file = |name: &str, body: &[u8]| header(name, b'0', body.len() as u64, 0o644, "");
    let mut routes: Vec<(String, fakes::Response)> = Vec::new();
    for version in [VERSION, NIGHTLY] {
        let binary = gzip(&archive(&[(file("fiber", MARKER), MARKER)]));
        let docs = gzip(&archive(&[(file("README.md", b"docs"), b"docs")]));
        let anthropic = manifest(version);
        let extensions = gzip(&archive(&[(
            file("anthropic/extension.json", anthropic.as_bytes()),
            anthropic.as_bytes(),
        )]));
        let ok = |body: Vec<u8>| fakes::Response::status(200, body);
        // Every run fetches every archive: one route per run sharing the
        // server, as a real server answers every request.
        for _ in 0..2 {
            for (name, bytes) in [
                (
                    format!("fiber-{}.tar.gz", env!("FIBER_TARGET")),
                    binary.clone(),
                ),
                ("fiber-docs.tar.gz".to_owned(), docs.clone()),
                ("fiber-extensions.tar.gz".to_owned(), extensions.clone()),
            ] {
                routes.push((
                    format!("/releases/download/v{version}/{name}"),
                    ok(bytes.clone()),
                ));
                routes.push((
                    format!("/releases/download/v{version}/{name}.sha256"),
                    ok(sha256(&bytes).into_bytes()),
                ));
            }
        }
    }
    let mut latest = fakes::Response::status(302, "");
    latest
        .headers
        .push(("location".to_owned(), format!("/releases/tag/v{VERSION}")));
    // Every run follows the redirect and lands on the tag: one route per
    // run, as a real server answers every request.
    for _ in 0..2 {
        routes.push(("/releases/latest".to_owned(), latest.clone()));
        routes.push((
            format!("/releases/tag/v{VERSION}"),
            fakes::Response::status(200, ""),
        ));
    }
    routes.push((
        "/releases".to_owned(),
        fakes::Response::status(
            200,
            json!([{ "tag_name": format!("v{NIGHTLY}"), "prerelease": true, "draft": false }])
                .to_string(),
        ),
    ));
    fakes::ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// Waits for the restarted hub to list `session` as live: the fresh
/// client subscribes to the feed, and the hub fans out the session's
/// `session_status` once its rescan follows the surviving session
/// socket. One event-driven wait replaces polling `sessions`; the
/// status lands under the feed's state lock just before the live entry,
/// so one `sessions` call then lists it.
fn await_live(client: &support::Socket, session: &str) {
    client.send(r#"{"id":"c_feed","command":"feed"}"#);
    let ack = support::recv_answer(client, "c_feed", "the feed acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    support::until(client, "the restarted session's status", |line| {
        line["kind"] == "session_status" && line["session_id"].as_str() == Some(session)
    });
    client.send(r#"{"id":"c_sessions","command":"sessions"}"#);
    let listed = support::recv_answer(client, "c_sessions", "the restarted hub's sessions");
    let live = listed["payload"]["result"]["live"].as_array().unwrap();
    assert!(
        live.iter()
            .any(|row| row["session_id"].as_str() == Some(session)),
        "the restarted hub lists the session: {listed}"
    );
}

#[test]
fn stable_update_replaces_the_running_copy_and_installs_docs() {
    let setup = Setup::new();
    let server = serve();
    let github = Github::new(&setup);
    let copy = copy_binary(&setup);
    let run = setup.run_update(&copy, &server, &github, &["update"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    assert!(setup.home().join("docs").is_dir(), "{}", run.stderr);
    assert!(run.stderr.contains("fiber: updated docs"), "{}", run.stderr);
}

#[test]
fn nightly_update_takes_the_newest_pre_release() {
    let setup = Setup::new();
    let server = serve();
    let github = Github::new(&setup);
    let copy = copy_binary(&setup);
    let run = setup.run_update(&copy, &server, &github, &["update"], Some("nightly"));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    assert!(setup.home().join("docs").is_dir(), "{}", run.stderr);
}

#[test]
fn upgrade_runs_the_same_update_as_update() {
    let setup = Setup::new();
    let server = serve();
    let github = Github::new(&setup);
    let first = copy_binary(&setup);
    let updated = setup.run_update(&first, &server, &github, &["update"], None);
    assert_eq!(updated.code, Some(0), "{}", updated.stderr);
    let setup = Setup::new();
    let github = Github::new(&setup);
    let second = copy_binary(&setup);
    let upgraded = setup.run_update(&second, &server, &github, &["upgrade"], None);
    assert_eq!(
        (upgraded.code, upgraded.stdout, upgraded.stderr),
        (updated.code, updated.stdout, updated.stderr)
    );
    assert_eq!(fs::read(&second).unwrap(), MARKER);
}

#[test]
fn a_restarted_hub_keeps_its_sessions() {
    let setup = Setup::new();
    let model = fakes::ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&model);
    let server = serve();
    let github = Github::new(&setup);
    let slot = Arc::new(Mutex::new(None));
    let (client, hello) = support::connect_hub(&setup, &slot);
    assert_eq!(hello["kind"], "hub_hello");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = support::SessionGuard::arm(setup.deadline, &workspace);
    let session = support::start_session(&client, &workspace, support::PROMPT);
    let copy = copy_binary(&setup);
    let run = setup.run_update(&copy, &server, &github, &["update"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    // The update stops the hub it could not restart through a manager.
    slot.lock().unwrap().take().unwrap().wait();
    drop(client);
    // A fresh hub lists the session the stopped hub ran.
    let slot = Arc::new(Mutex::new(None));
    let (fresh, hello) = support::connect_hub(&setup, &slot);
    assert_eq!(hello["kind"], "hub_hello");
    await_live(&fresh, &session);
    let direct = support::Socket::connect(setup.deadline, &setup.session_socket(&session));
    support::close_session(&direct);
    drop(direct);
    drop(fresh);
    guard.wait_gone();
}
