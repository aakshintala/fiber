//! Binary-level tests of a damaged extension (`docs/extensions.md`,
//! "Installing"): with one healthy extension and one directory whose
//! install record is missing or unreadable, `list` shows both, `install`
//! and `update` skip it with one line, and `remove` deletes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
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
use serde_json::json;
use support::{Deadline, Setup};

/// The damaged extension's full name and directory.
const BROKEN: &str = "github.com/aakshintala/fiber/providers/opencode";
/// The fix sentence `list` prints first on stdout.
const FIX: &str =
    "`opencode` is damaged; run `fiber extension remove opencode`, then install it again.";
/// The skip line `install` and `update` print once on stderr.
const SKIP: &str = "fiber: `opencode` is damaged, so its dependency minimums are unknown and the versions chosen did not count them; run `fiber extension remove opencode`, then install it again.";

impl Setup {
    fn new_with_sources() -> Self {
        let deadline = Deadline::start();
        let setup = Self {
            deadline,
            root: fakes::TempDir::new("fiber-ext-damaged"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup.source("healthy", "acme");
        setup.source("broken", BROKEN);
        setup.source("fresh", "example.com/acme/fresh");
        setup
    }

    fn broken_dir(&self) -> PathBuf {
        self.home().join("extensions/opencode")
    }

    /// A source package with this manifest name and version `v1`.
    fn source(&self, dir: &str, name: &str) {
        let path = self.root.path().join("src").join(dir);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("extension.json"),
            json!({"name": name, "version": "v1", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
    }

    fn src(&self, dir: &str) -> String {
        self.root
            .path()
            .join("src")
            .join(dir)
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// Runs `fiber extension` with `args` and no terminal, so it does not
    /// ask.
    #[track_caller]
    fn extension(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["extension"])
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
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber extension {}` to exit", args.join(" ")),
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

// debt: `spawn_watched` copies `approve.rs`'s, as the other binary test
// files in this crate each do; move every copy into `fakes` together when a
// change to one has to be made in all.

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

/// How many of stderr's lines are the skip line.
fn skips(run: &Run) -> usize {
    run.stderr.lines().filter(|line| *line == SKIP).count()
}

#[track_caller]
fn damaged_flow(invalid: bool) {
    let setup = Setup::new_with_sources();
    let installed = setup.extension(&["install", &setup.src("healthy")]);
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    let installed = setup.extension(&["install", &setup.src("broken")]);
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    if invalid {
        fs::write(setup.broken_dir().join(".fiber.json"), "{").unwrap();
    } else {
        fs::remove_file(setup.broken_dir().join(".fiber.json")).unwrap();
    }

    let listed = setup.extension(&["list"]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    assert_eq!(
        listed.stdout.lines().collect::<Vec<_>>(),
        [FIX, "acme v1 local"],
        "stdout: {:?}",
        listed.stdout
    );

    let run = setup.extension(&["install", &setup.src("fresh")]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(skips(&run), 1, "stderr: {:?}", run.stderr);
    let skip = run.stderr.lines().position(|line| line == SKIP).unwrap();
    let installed = run
        .stderr
        .lines()
        .position(|line| line.starts_with("fiber: installed"))
        .unwrap();
    assert!(skip < installed, "stderr: {:?}", run.stderr);

    let run = setup.extension(&["update"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(skips(&run), 1, "stderr: {:?}", run.stderr);

    let run = setup.extension(&["remove", "opencode"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!setup.broken_dir().exists());
}

#[test]
fn a_missing_record_is_listed_skipped_and_removable() {
    damaged_flow(false);
}

#[test]
fn an_unreadable_record_is_listed_skipped_and_removable() {
    damaged_flow(true);
}
