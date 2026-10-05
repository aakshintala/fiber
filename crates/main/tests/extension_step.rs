//! Binary-level tests of `fiber extension install` and `fiber extension
//! update` (`docs/extensions.md`, "Installing"; `docs/testing.md`,
//! "Levels"): the built `fiber` installs a package whose install step
//! records its own directory, and the test runs the script the step left
//! behind, proving the step ran at the extension's final path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use serde_json::json;

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// The install step: it records its directory in `where`, then leaves a
/// script that reads the payload through that recorded directory. The
/// script only works when the step ran where the files stayed.
const STEP: &str = r#"pwd > where; printf '#!/bin/sh\ncat "%s/payload.txt"\n' "$(pwd)" > run.sh"#;

/// A temporary root holding Fiber home and the extension source.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-ext-step"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        setup.source("one\n", "v1.0.0");
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn source_dir(&self) -> PathBuf {
        self.root.path().join("src")
    }

    fn installed(&self) -> PathBuf {
        self.home().join("extensions/acme")
    }

    /// The source package with this payload and version.
    fn source(&self, payload: &str, version: &str) {
        let dir = self.source_dir();
        write(
            &dir.join("extension.json"),
            &json!({
                "name": "acme", "version": version, "fiber": "0.0.0", "api": 1,
                "install": ["sh", "-c", STEP],
            })
            .to_string(),
        );
        write(&dir.join("payload.txt"), payload);
    }

    /// Runs `fiber extension` with `args` and no terminal, so it does not
    /// ask.
    fn extension(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["extension"])
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
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
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                panic!(
                    "waited {DEADLINE:?} for `fiber extension {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        watchdog.stand_down(DEADLINE);
        Run {
            code: output.status.code(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

fn write(file: &PathBuf, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
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
    stderr: String,
}

/// The installed directory, and what the step recorded there: its path is
/// the canonical installed path, and its script reads the payload.
fn assert_step_at_final_path(setup: &Setup, payload: &str) {
    let dir = setup.installed();
    let canonical = fs::canonicalize(&dir).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("where")).unwrap(),
        format!("{}\n", canonical.display())
    );
    let out = Command::new("sh").arg(dir.join("run.sh")).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8(out.stdout).unwrap(), payload);
}

#[test]
fn an_install_step_runs_at_the_final_path() {
    let setup = Setup::new();
    let run = setup.extension(&["install", setup.source_dir().to_str().unwrap()]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_step_at_final_path(&setup, "one\n");
}

#[test]
fn an_update_step_runs_at_the_final_path_again() {
    let setup = Setup::new();
    let installed = setup.extension(&["install", setup.source_dir().to_str().unwrap()]);
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    setup.source("two\n", "v1.1.0");
    let run = setup.extension(&["update", "acme"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_step_at_final_path(&setup, "two\n");
}
