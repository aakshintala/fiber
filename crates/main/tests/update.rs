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
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::ustar::{archive, gzip, header, sha256};
use serde_json::json;
use support::{Output, Setup, spawn_watched};

/// The release the fixture server holds above this binary's `0.0.0`.
const VERSION: &str = "0.0.1";

/// The nightly the fixture list names, above `0.0.0` too.
const NIGHTLY: &str = "0.0.1-nightly.20240101";

/// The newer nightly the pair fixture serves above [`NIGHTLY`]: a
/// nightly-floored install moves onto it.
const NIGHTLY_NEXT: &str = "0.0.1-nightly.20240102";

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
        Self::with_tags(setup, &[VERSION, NIGHTLY])
    }

    /// The same fixture with exactly `tags`: a nightly-only repository
    /// holds no stable tag, so only a nightly minimum can resolve what it
    /// serves.
    #[track_caller]
    fn with_tags(setup: &Setup, tags: &[&str]) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        fs::create_dir_all(repo.join("providers/anthropic")).unwrap();
        fs::write(repo.join("fiber.md"), "fiber").unwrap();
        // The installed extension's package, committed before the
        // served tags, so an update that clones it finds the package.
        fs::write(
            repo.join("providers/anthropic/extension.json"),
            json!({
                "name": "github.com/aakshintala/fiber/providers/anthropic",
                "version": VERSION,
                "fiber": "0.0.0",
                "api": 1,
            })
            .to_string(),
        )
        .unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "fiber"]);
        for version in tags {
            fakes::git::git(&repo, &["tag", &format!("v{version}")]);
        }
        Self {
            key: format!("url.file://{}/.insteadOf", base.display()),
        }
    }
}

/// A second local repository outside the release extensions archive, for
/// one extension the archive never covers: a stable tag and a nightly tag
/// on the same base, with the extension manifest committed at each.
struct Sidecar {
    commit: String,
}

impl Sidecar {
    const NAME: &str = "github.com/aakshintala/fiber-side/widgets";
    const VERSION: &str = "0.2.0";
    const NIGHTLY: &str = "0.2.0-nightly.20240102";

    #[track_caller]
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber-side");
        fs::create_dir_all(repo.join("widgets")).unwrap();
        let manifest = |version: &str| {
            json!({
                "name": Self::NAME,
                "version": version,
                "fiber": "0.0.0",
                "api": 1,
            })
            .to_string()
        };
        fs::write(repo.join("widgets/extension.json"), manifest(Self::VERSION)).unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "fiber-side"]);
        fakes::git::git(&repo, &["tag", &format!("v{}", Self::VERSION)]);
        fs::write(repo.join("widgets/extension.json"), manifest(Self::NIGHTLY)).unwrap();
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "nightly"]);
        fakes::git::git(&repo, &["tag", &format!("v{}", Self::NIGHTLY)]);
        let commit = fakes::git::git(
            &repo,
            &["rev-list", "-n", "1", &format!("v{}", Self::VERSION)],
        );
        Self { commit }
    }
}

/// The home as an install writes it: the manifest plus the install record
/// carrying the version, the request flag, the floor and the given commit
/// as the source.
#[track_caller]
fn install_git_record(
    setup: &Setup,
    dir: &str,
    name: &str,
    version: &str,
    floor: &str,
    commit: &str,
) {
    let dir = setup.home().join(dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({
            "name": name,
            "version": version,
            "fiber": "0.0.0",
            "api": 1,
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join(".fiber.json"),
        json!({
            "name": name,
            "version": version,
            "requested": true,
            "floor": floor,
            "source": { "commit": commit },
        })
        .to_string(),
    )
    .unwrap();
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
        watchdog.stand_down(self.deadline.cleanup_phase());
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

/// The binary archive the fixture publishes for every release: the
/// mapped triple's archive — on Linux the musl archive whatever libc
/// the test binary was built against, and the running target's own
/// archive elsewhere.
fn binary_archive() -> String {
    format!(
        "fiber-{}.tar.gz",
        extensions::release_triple(env!("FIBER_TARGET")).unwrap()
    )
}

/// The fixture releases for `versions`: docs, extensions and binary
/// archives per version, the `/releases/latest` redirect at `latest`, and
/// the nightly list naming `nightlies` as pre-releases.
fn serve_versions(versions: &[&str], latest: &str, nightlies: &[&str]) -> fakes::ProviderServer {
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
    for version in versions {
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
                (binary_archive(), binary.clone()),
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
    let mut latest_response = fakes::Response::status(302, "");
    latest_response
        .headers
        .push(("location".to_owned(), format!("/releases/tag/v{latest}")));
    // Every run follows the redirect and lands on the tag: one route per
    // run, as a real server answers every request.
    for _ in 0..2 {
        routes.push(("/releases/latest".to_owned(), latest_response.clone()));
        routes.push((
            format!("/releases/tag/v{latest}"),
            fakes::Response::status(200, ""),
        ));
    }
    let list: Vec<serde_json::Value> = nightlies
        .iter()
        .map(|version| {
            json!({ "tag_name": format!("v{version}"), "prerelease": true, "draft": false })
        })
        .collect();
    routes.push((
        "/releases".to_owned(),
        fakes::Response::status(200, serde_json::Value::Array(list).to_string()),
    ));
    fakes::ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// The fixture releases: docs, extensions and binary archives for
/// [`VERSION`] and [`NIGHTLY`], the `/releases/latest` redirect at the
/// stable tag, and the nightly list naming [`NIGHTLY`].
fn serve() -> fakes::ProviderServer {
    serve_versions(&[VERSION, NIGHTLY], VERSION, &[NIGHTLY])
}

/// The fixture releases for the nightly pair: docs, extensions and binary
/// archives for [`NIGHTLY`] and [`NIGHTLY_NEXT`], with the nightly list
/// naming both as pre-releases, so the nightly channel moves to
/// [`NIGHTLY_NEXT`].
fn serve_nightly_pair() -> fakes::ProviderServer {
    serve_versions(
        &[NIGHTLY, NIGHTLY_NEXT],
        NIGHTLY_NEXT,
        &[NIGHTLY, NIGHTLY_NEXT],
    )
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

/// A copy of the built `fiber` on another filesystem than the home, so
/// the final rename into place must not cross filesystems: `/dev/shm`
/// is a separate mount from the test root on Linux.
#[cfg(target_os = "linux")]
#[track_caller]
fn copy_binary_other_filesystem() -> PathBuf {
    assert!(
        Path::new("/dev/shm").is_dir(),
        "`/dev/shm` is absent: the cross-filesystem update test needs it"
    );
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = PathBuf::from(format!(
        "/dev/shm/fiber-update-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("fiber");
    fs::copy(env!("CARGO_BIN_EXE_fiber"), &copy).unwrap();
    dir
}

/// The staged binary sits beside the installed one, so the rename into
/// place stays on one filesystem even when the home lives elsewhere.
#[cfg(target_os = "linux")]
#[test]
fn update_across_filesystems_replaces_the_running_copy() {
    let dir = copy_binary_other_filesystem();
    let copy = dir.join("fiber");
    let setup = Setup::new();
    let server = serve();
    let github = Github::new(&setup);
    let run = setup.run_update(&copy, &server, &github, &["update"], None);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    fs::remove_dir_all(&dir).unwrap_or(());
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
    let run = setup.run_update(
        &copy,
        &server,
        &github,
        &["update", "--channel", "nightly"],
        None,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    assert!(setup.home().join("docs").is_dir(), "{}", run.stderr);
}

/// Home as a nightly install writes it, with an installed extension
/// whose record carries the nightly version: `fiber update --channel
/// nightly` installs the newest nightly over it.
#[test]
fn nightly_update_with_an_installed_extension_replaces_the_running_copy() {
    const NAME: &str = "github.com/aakshintala/fiber/providers/anthropic";
    let setup = Setup::new();
    let server = serve();
    let github = Github::new(&setup);
    // The manifest plus the install record the nightly install writes:
    // the version from the record, requested, and the release tag's
    // commit as the source.
    let dir = setup.home().join("extensions/anthropic");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({
            "name": NAME,
            "version": NIGHTLY,
            "fiber": "0.0.0",
            "api": 1,
        })
        .to_string(),
    )
    .unwrap();
    let repo = setup.root.path().join("gh/aakshintala/fiber");
    let commit = fakes::git::git(&repo, &["rev-list", "-n", "1", &format!("v{NIGHTLY}")]);
    fs::write(
        dir.join(".fiber.json"),
        json!({
            "name": NAME,
            "version": NIGHTLY,
            "requested": true,
            "source": { "commit": commit },
        })
        .to_string(),
    )
    .unwrap();
    let copy = copy_binary(&setup);
    let run = setup.run_update(
        &copy,
        &server,
        &github,
        &["update", "--channel", "nightly"],
        None,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    assert!(setup.home().join("docs").is_dir(), "{}", run.stderr);
    assert!(run.stderr.contains("fiber: updated docs"), "{}", run.stderr);
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(".fiber.json")).unwrap()).unwrap();
    assert_eq!(record["version"], NIGHTLY, "{record}");
    assert_eq!(record["requested"], true, "{record}");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("extension.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], NAME, "{manifest}");
}

/// Home as a nightly install at [`NIGHTLY`] with the nightly floor writes
/// it: `fiber update --channel nightly` moves the first-party extension to
/// [`NIGHTLY_NEXT`], the newest nightly the pair fixture serves.
#[test]
fn nightly_update_moves_a_nightly_floored_extension_to_the_newest_nightly() {
    const NAME: &str = "github.com/aakshintala/fiber/providers/anthropic";
    let setup = Setup::new();
    let server = serve_nightly_pair();
    let github = Github::with_tags(&setup, &[NIGHTLY, NIGHTLY_NEXT]);
    // The version from the record, the nightly floor, and the release
    // tag's commit as the source.
    let repo = setup.root.path().join("gh/aakshintala/fiber");
    let commit = fakes::git::git(&repo, &["rev-list", "-n", "1", &format!("v{NIGHTLY}")]);
    install_git_record(
        &setup,
        "extensions/anthropic",
        NAME,
        NIGHTLY,
        NIGHTLY,
        &commit,
    );
    let copy = copy_binary(&setup);
    let run = setup.run_update(
        &copy,
        &server,
        &github,
        &["update", "--channel", "nightly"],
        None,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    assert!(setup.home().join("docs").is_dir(), "{}", run.stderr);
    assert!(run.stderr.contains("fiber: updated docs"), "{}", run.stderr);
    let record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(setup.home().join("extensions/anthropic/.fiber.json")).unwrap(),
    )
    .unwrap();
    // docs/extensions.md:1797, 1805-1807.
    assert_eq!(record["version"], NIGHTLY_NEXT, "{record}\n{}", run.stderr);
    // The archive adoption keeps the installed floor: the move lands on
    // the new nightly, and the old nightly stays the minimum.
    // docs/extensions.md:1798-1799.
    assert_eq!(record["floor"], NIGHTLY, "{record}\n{}", run.stderr);
    assert_eq!(record["requested"], true, "{record}\n{}", run.stderr);
}

/// Home with a stable-floored extension outside the release archive:
/// `fiber update --channel nightly` keeps it at its stable version even
/// though its repository also serves a nightly tag.
#[test]
fn nightly_update_keeps_a_stable_floored_extension_outside_the_archive() {
    let setup = Setup::new();
    let server = serve_nightly_pair();
    let github = Github::with_tags(&setup, &[NIGHTLY, NIGHTLY_NEXT]);
    let sidecar = Sidecar::new(&setup);
    install_git_record(
        &setup,
        "extensions/widgets",
        Sidecar::NAME,
        Sidecar::VERSION,
        Sidecar::VERSION,
        &sidecar.commit,
    );
    let copy = copy_binary(&setup);
    let run = setup.run_update(
        &copy,
        &server,
        &github,
        &["update", "--channel", "nightly"],
        None,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fs::read(&copy).unwrap(), MARKER);
    let record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(setup.home().join("extensions/widgets/.fiber.json")).unwrap(),
    )
    .unwrap();
    // docs/extensions.md:1805-1807.
    assert_eq!(record["version"], Sidecar::VERSION, "{record}");
    // docs/extensions.md:1798-1799.
    assert_eq!(record["floor"], Sidecar::VERSION, "{record}");
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
