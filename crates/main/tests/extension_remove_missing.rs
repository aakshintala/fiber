//! Binary-level test of `fiber extension remove` on a name nothing is
//! installed under (`docs/extensions.md`, "Installing"): it fails with
//! exit 1, names the extension and the directory it
//! looked for, and deletes nothing.

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
use support::Deadline;

struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            deadline: Deadline::start(),
            root: fakes::TempDir::new("fiber-ext-remove-missing"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

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
        let output = match finished.recv_timeout(self.deadline.left()) {
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

#[test]
fn removing_a_name_nothing_is_installed_under_fails() {
    let setup = Setup::new();
    let run = setup.extension(&["remove", "muse"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(!run.stderr.contains("removed"), "{}", run.stderr);
    assert!(run.stderr.contains("is not installed"), "{}", run.stderr);
    assert!(run.stderr.contains("providers/muse"), "{}", run.stderr);
    assert!(run.stderr.contains("extensions/muse"), "{}", run.stderr);
}

#[test]
fn a_leftover_directory_under_another_name_is_not_touched() {
    let setup = Setup::new();
    let other = setup.home().join("extensions/github.com-acme-muse");
    fs::create_dir_all(&other).unwrap();
    let run = setup.extension(&["remove", "muse"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert!(other.exists());
}
