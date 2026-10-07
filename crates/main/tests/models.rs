//! Binary-level tests of `fiber models` (`docs/testing.md`, "Levels";
//! `docs/invocation.md`, "Commands and flags"): the built `fiber` runs in
//! its own process group with its own `FIBER_HOME` holding one data
//! provider. Every run carries a wall-clock deadline. Each test runs
//! `fiber` through a hard link in its own temporary root, so a child it
//! re-runs from `current_exe()` carries that unique path on its command
//! line and no other test run's process ever matches it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use std::os::unix::process::{CommandExt, ExitStatusExt};

use fakes::Watchdog;
use serde_json::{Value, json};
use support::{Deadline, group_alive};

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    /// `<CARGO_TARGET_TMPDIR>/<root's name>`, holding the `fiber` link.
    bin: PathBuf,
    deadline: Deadline,
}

impl Drop for Setup {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.bin) {
            Ok(()) | Err(_) => {}
        }
    }
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        // A hard link, not a copy: the bytes and inode are the built
        // binary's, so nothing new is executed (`docs/testing.md`, "Waits
        // and timeouts"), but the path is this test's own. It lives in
        // Cargo's per-target temporary directory, which is on the target
        // directory's filesystem, in a directory named after the root.
        let name = root.path().file_name().unwrap();
        let bin = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        fs::create_dir(&bin).unwrap();
        let exe = bin.join("fiber");
        if let Err(err) = fs::hard_link(env!("CARGO_BIN_EXE_fiber"), &exe) {
            panic!("hard-linking `fiber` to {}: {err}", exe.display());
        }
        Self {
            deadline,
            root,
            bin,
        }
    }

    /// This test's own path to the built `fiber`.
    fn exe(&self) -> PathBuf {
        self.bin.join("fiber")
    }

    /// The refresh child's command line: this test's `fiber` re-run as
    /// `refresh-model-lists`. The path is unique to this test, so no other
    /// test run's process, and no stub with the same arguments, matches.
    fn refresh_pattern(&self) -> String {
        format!("{} refresh-model-lists", self.exe().display())
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
        let mut command = Command::new(self.exe());
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
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                support::kill_group(self.deadline, group, "KILL").unwrap();
                let killed = finished
                    .recv_timeout(self.deadline.cleanup())
                    .map(|output| output.map(|output| output.status.signal()));
                assert!(
                    matches!(killed, Ok(Ok(Some(9)))),
                    "`fiber {}` was not reaped as killed before the cleanup deadline: {killed:?}",
                    args.join(" ")
                );
                assert!(
                    !group_alive(self.deadline, group),
                    "`fiber` left a process behind"
                );
                panic!(
                    "waited until the deadline for `fiber {}` to exit",
                    args.join(" ")
                );
            }
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
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
        match support::kill_group_detached(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Whether any process remains in process group `group`.

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

/// Waits until the cache file's bytes differ from `old`, in slices of
/// 100 ms up to [`DEADLINE`]: the detached refresh child rewrites it.
fn await_refreshed(file: &std::path::Path, old: &[u8]) {
    let (_tx, rx) = mpsc::channel::<()>();
    let slices = DEADLINE.as_millis() / 100;
    for _ in 0..slices {
        match std::fs::read(file) {
            Ok(now) if now != old => return,
            _ => {}
        }
        let _waited = rx.recv_timeout(Duration::from_millis(100));
    }
    panic!("waited until the deadline for the cached model list to refresh");
}

/// Waits until this test's refresh child is gone, up to [`DEADLINE`]: the
/// detached child exits after it rewrites the cache, and a rewritten cache
/// alone never proves it did. On expiry it kills the child and its group,
/// checks they are gone within [`REAP_DEADLINE`], and fails.
fn await_refresh_exit(pattern: &str) {
    if fakes::matching_exits(pattern, deadline.left()) {
        return;
    }
    support::kill_matching(deadline, pattern).unwrap();
    assert!(
        fakes::matching_exits(pattern, deadline.cleanup()),
        "the refresh child outlived SIGKILL past the cleanup deadline"
    );
    panic!("waited until the deadline for the refresh child to exit");
}

#[test]
fn models_prints_a_stale_list_at_once_and_it_is_fresh_on_the_next_run() {
    let setup = Setup::new();
    // Declared after `setup`, so it drops first: a failing run kills this
    // test's refresh child before its directory is removed.
    let refresh_guard = Watchdog::matching(&setup.refresh_pattern());
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
    // A stale cached copy: an ancient mtime is older than any
    // `model_lists.refresh_after`, without reading the clock.
    let file = setup.home().join("cache/models/fixture.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        json!([{ "id": "old", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000 }])
        .to_string(),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    let old = std::fs::read(&file).unwrap();

    let first = setup.fiber(&["models"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.stderr, "");
    assert!(
        first.stdout.contains("fixture/old"),
        "stdout: {}",
        first.stdout
    );

    // The first run returns without waiting: the list is fresh on the
    // next run, once the detached child rewrites the cache.
    await_refreshed(&file, &old);
    let second = setup.fiber(&["models"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.stderr, "");
    assert!(
        second.stdout.contains("fixture/m1"),
        "stdout: {}",
        second.stdout
    );
    assert!(
        !second.stdout.contains("fixture/old"),
        "stdout: {}",
        second.stdout
    );
    // The rewritten cache never proves the detached child exited: wait
    // for it under the deadline, then stand its watchdog down. Dropping
    // the guard without an exit would kill it instead of checking it,
    // and a timeout above drops it, so no run leaves one behind.
    await_refresh_exit(&setup.refresh_pattern());
    refresh_guard.stand_down(setup.deadline.cleanup());
}
