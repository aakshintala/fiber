//! Binary-level tests of `fiber models` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` runs in
//! its own process group with its own `FIBER_HOME` holding one data
//! provider. Every run carries a wall-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use std::os::unix::process::CommandExt;

use fakes::Watchdog;
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs the provider `acme` serving `big` with a context window and
    /// prices, and makes `acme/big` the configured model.
    fn provider(&self) {
        let dir = self.home().join("extensions").join("acme");
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join("acme.json"),
            json!({
                "name": "acme",
                "models": [{
                    "id": "big",
                    "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:9/v1",
                    "context_window": 200000,
                    "cost": {"input": 3.0, "output": 15.0},
                }],
            })
            .to_string(),
        )
        .unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "acme/big"}),
        );
    }

    /// Runs `fiber` with `args` and waits for it under [`DEADLINE`].
    fn fiber(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(!group_alive(group), "`fiber` left a process behind");
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(!group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn write(file: &PathBuf, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

#[test]
fn models_lists_the_installed_models_as_text() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "  model     context  in $/M  out $/M\n\
         * acme/big  200000   3       15\n"
    );
}

#[test]
fn models_json_prints_one_object_per_row() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "{\"model\":\"acme/big\",\"context_window\":200000,\
         \"input\":3.0,\"output\":15.0,\"default\":true}\n"
    );
}

#[test]
fn models_search_filters_by_substring() {
    let setup = Setup::new();
    setup.provider();
    let run = setup.fiber(&["models", "acme"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("acme/big"), "stdout: {}", run.stdout);
    let run = setup.fiber(&["models", "nothing-matches-this"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "");
}

#[test]
fn help_models_matches_models_help() {
    let setup = Setup::new();
    setup.provider();
    let via_help = setup.fiber(&["help", "models"]);
    let via_flag = setup.fiber(&["models", "--help"]);
    assert_eq!(via_help.code, Some(0), "stderr: {}", via_help.stderr);
    assert_eq!(via_flag.code, Some(0), "stderr: {}", via_flag.stderr);
    assert_eq!(via_help.stderr, "");
    assert_eq!(via_flag.stderr, "");
    assert_eq!(via_help.stdout, via_flag.stdout);
    assert!(
        via_flag
            .stdout
            .lines()
            .any(|line| line.starts_with("Usage: fiber models")),
        "stdout: {}",
        via_flag.stdout
    );
}

#[test]
fn models_runs_a_lua_providers_models_with_no_cached_copy() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(
        200,
        json!({"data": [{"id": "m1", "context_length": 1000}]}).to_string(),
    )])
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.url",
        &config::Secret::new(server.url()),
    )
    .unwrap();
    config::store_secret(
        &setup.home(),
        "fixture.api_key",
        &config::Secret::new("k1".into()),
    )
    .unwrap();
    let run = setup.fiber(&["models"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert!(run.stdout.contains("fixture/m1"), "stdout: {}", run.stdout);
}
