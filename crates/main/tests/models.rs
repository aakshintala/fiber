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

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use std::os::unix::process::{CommandExt, ExitStatusExt};

use fakes::Watchdog;
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// How long a killed process may take to be reaped or to disappear.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        // A hard link, not a copy: the bytes and inode are the built
        // binary's, so nothing new is executed (`docs/testing.md`, "Waits
        // and timeouts"), but the path is this test's own.
        let link = root.path().join("fiber");
        if let Err(err) = fs::hard_link(env!("CARGO_BIN_EXE_fiber"), &link) {
            panic!(
                "hard-linking `fiber` into {} (the temporary directory must share \
                 a filesystem with the target directory): {err}",
                link.display()
            );
        }
        Self { root }
    }

    /// This test's own path to the built `fiber`.
    fn exe(&self) -> PathBuf {
        self.root.path().join("fiber")
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
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let killed = finished
                    .recv_timeout(REAP_DEADLINE)
                    .map(|output| output.map(|output| output.status.signal()));
                assert!(
                    matches!(killed, Ok(Ok(Some(9)))),
                    "`fiber {}` was not reaped as killed within {REAP_DEADLINE:?}: {killed:?}",
                    args.join(" ")
                );
                assert!(!group_alive(group), "`fiber` left a process behind");
                panic!("waited {DEADLINE:?} for `fiber {}` to exit", args.join(" "));
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
    panic!("waited {DEADLINE:?} for the cached model list to refresh");
}

/// Whether a process whose command line contains `pattern` remains, in
/// slices of 100 ms up to `within`: true once none does.
fn gone_within(pattern: &str, within: Duration) -> bool {
    let (_tx, rx) = mpsc::channel::<()>();
    let slices = within.as_millis() / 100;
    for _ in 0..=slices {
        match fakes::matching(pattern) {
            Ok(matched) if matched.is_empty() => return true,
            _ => {}
        }
        let _waited = rx.recv_timeout(Duration::from_millis(100));
    }
    false
}

/// Waits until this test's refresh child is gone, up to [`DEADLINE`]: the
/// detached child exits after it rewrites the cache, and a rewritten cache
/// alone never proves it did. On expiry it kills the child and its group,
/// checks they are gone within [`REAP_DEADLINE`], and fails.
fn await_refresh_exit(pattern: &str) {
    if gone_within(pattern, DEADLINE) {
        return;
    }
    fakes::kill_matching(pattern).unwrap();
    assert!(
        gone_within(pattern, REAP_DEADLINE),
        "the refresh child outlived SIGKILL by {REAP_DEADLINE:?}"
    );
    panic!("waited {DEADLINE:?} for the refresh child to exit");
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
                 "base_url": "http://127.0.0.1:9/v1" }])
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
    refresh_guard.stand_down(DEADLINE);
}
