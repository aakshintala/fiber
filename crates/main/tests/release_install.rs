//! Binary-level tests of the release install step `install.sh` runs
//! (`docs/releasing.md`, "Installing"): the built `fiber release-install`
//! puts a fixture release's docs and first-party extensions in place, and
//! `fiber extension list` reads each one as healthy, recorded at the
//! binary's commit. A build with no commit refuses instead. Every run
//! carries a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use fakes::ustar::{archive, gzip, header, sha256};
use serde_json::json;
use support::Deadline;

/// The version the binary under test was built as.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A temporary root holding Fiber home, removed on drop, and the test's
/// deadline.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        Self {
            deadline,
            root: fakes::TempDir::new("fri"),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Runs `fiber` with `args` and waits for it under the test's
    /// [`Deadline`].
    fn fiber(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        watchdog.stand_down(self.deadline.cleanup());
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group, which kills the group if this process dies first.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let watchdog = Watchdog::group(child.id());
    (child, watchdog)
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A release of `anthropic` and `memory` at this binary's version, served
/// at `/download/v<version>/`.
fn serve() -> fakes::ProviderServer {
    let manifest = |short: &str, kind: &str| {
        json!({
            "name": format!("github.com/aakshintala/fiber/{kind}/{short}"),
            "version": VERSION,
            "fiber": VERSION,
            "api": 1,
        })
        .to_string()
    };
    let anthropic = manifest("anthropic", "providers");
    let memory = manifest("memory", "extensions");
    let file = |name: &str, data: &str| header(name, b'0', data.len() as u64, 0o644, "");
    let docs = gzip(&archive(&[(file("README.md", "docs"), b"docs")]));
    let extensions = gzip(&archive(&[
        (
            file("anthropic/extension.json", &anthropic),
            anthropic.as_bytes(),
        ),
        (file("memory/extension.json", &memory), memory.as_bytes()),
    ]));
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let routes = [
        ("fiber-docs.tar.gz", ok(docs.clone())),
        ("fiber-docs.tar.gz.sha256", ok(sha256(&docs).into_bytes())),
        ("fiber-extensions.tar.gz", ok(extensions.clone())),
        (
            "fiber-extensions.tar.gz.sha256",
            ok(sha256(&extensions).into_bytes()),
        ),
    ]
    .map(|(file, response)| (format!("/download/v{VERSION}/{file}"), response));
    fakes::ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// The commit `fiber --version` shows, if the build recorded one.
fn commit(setup: &Setup) -> Option<String> {
    let run = setup.fiber(&["--version"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let line = run.stdout.trim();
    let open = line.find('(')?;
    Some(line.get(open + 1..line.len() - 1).unwrap().to_owned())
}

#[test]
fn the_step_installs_what_extension_list_reads_as_healthy() {
    let setup = Setup::new();
    let server = serve();
    let url = server.url();
    let run = setup.fiber(&["release-install", VERSION, "--base-url", &url]);
    let Some(commit) = commit(&setup) else {
        assert_eq!(run.code, Some(1), "{}", run.stderr);
        assert_eq!(
            run.stderr,
            "fiber: This build records no commit, so it cannot install a release's extensions.\n"
        );
        assert!(server.requests().is_empty());
        return;
    };
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        "fiber: installed memory\nfiber: installed anthropic\nfiber: installed docs\n"
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("docs/README.md")).unwrap(),
        "docs"
    );
    let listed = setup.fiber(&["extension", "list"]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    assert_eq!(
        listed.stdout.lines().collect::<Vec<_>>(),
        [
            format!("github.com/aakshintala/fiber/extensions/memory {VERSION} {commit}"),
            format!("github.com/aakshintala/fiber/providers/anthropic {VERSION} {commit}"),
        ],
        "{}",
        listed.stderr
    );
    assert!(!listed.stdout.contains("damaged"), "{}", listed.stdout);
}

#[test]
fn another_version_is_refused_and_creates_no_home() {
    let setup = Setup::new();
    let server = serve();
    let url = server.url();
    let run = setup.fiber(&["release-install", "999.0.0", "--base-url", &url]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(
        run.stderr,
        format!(
            "fiber: This is Fiber {VERSION}; it cannot install the docs and extensions of 999.0.0.\n"
        )
    );
    assert!(server.requests().is_empty());
    assert!(!setup.home().exists());
}
