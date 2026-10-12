//! Binary-level tests that a `command` credential source run outside a
//! session is cancelled on every exit (`docs/testing.md`, "Levels"):
//! `fiber models`, `fiber refresh-model-lists` and the terminal's model
//! picker each kill the process groups their credential commands left, on
//! a normal return and on a signal.
//!
//! Every run carries a wall-clock deadline. The `fiber models` runs go
//! through a hard link in their own temporary root, so a refresh child
//! the run re-runs from `current_exe()` carries that unique path on its
//! command line and no other test run's process ever matches it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::children::Ready;
use fakes::{Deadline, Watchdog};
use serde_json::json;
use support::pty::Run;

/// A temporary root holding Fiber home and the workspace, removed on drop,
/// with this test's own path to the built `fiber`.
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
        let root = fakes::TempDir::new("fh");
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
    /// test run's process matches.
    fn refresh_pattern(&self) -> String {
        format!("{} refresh-model-lists", self.exe().display())
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Spawns `fiber` with `args` in its own process group, guarded by a
    /// watchdog that kills the group if the test process dies first.
    #[track_caller]
    fn start(&self, args: &[&str]) -> (u32, Child, Watchdog) {
        let mut command = Command::new(self.exe());
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = support::spawn_watched(&mut command);
        let pid = child.id();
        (pid, child, watchdog)
    }

    /// Waits for `child` under the test's deadline: on expiry its group is
    /// killed and the test fails naming `what`.
    #[track_caller]
    fn finish(&self, what: &str, group: u32, child: Child) -> Output {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, what),
        }
    }
}

/// Stderr as text.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Installs the ruling-4 fixture under `home`: a provider `acme` serving
/// `m1` from its data file, with a Lua handle registering no `credential`
/// (so the read falls through to the command source), a cached list dated
/// just past the epoch (stale under any clock), and a `command` credential
/// source running `script`.
fn install(home: &Path, script: &str) {
    let dir = home.join("extensions").join("acme");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "providers": {"acme": ["http://127.0.0.1:1/v1"]}})
        .to_string(),
    )
    .unwrap();
    support::write_record(&dir);
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({"name": "acme", "models": [{"id": "m1", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join("acme.lua"),
        "fiber.provider(\"acme\", {})\n",
    )
    .unwrap();
    let cache = home.join("cache/models/acme.json");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(
        &cache,
        json!([{"id": "m1", "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}])
        .to_string(),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    support::write_json(
        &home.join("config.json"),
        &json!({"model": "acme/m1", "hub": {"idle_exit_ms": 1000},
                "providers": {"acme": {"credentials": {"default": {
            "command": ["sh", "-c", script],
        }}}}}),
    );
}

/// Waits until this test's refresh child is gone, under the test's
/// [`Deadline`]: the detached child exits after it runs, and nothing else
/// proves it did. On expiry it kills the child and its group, checks they
/// are gone before the cleanup deadline, and fails.
#[track_caller]
fn await_refresh_exit(deadline: Deadline, pattern: &str) {
    if fakes::matching_exits(pattern, deadline.left()) {
        return;
    }
    support::kill_matching(deadline, pattern).unwrap();
    fakes::try_matching_exits(pattern, deadline.cleanup()).unwrap_or_else(|err| {
        panic!("the refresh child outlived SIGKILL past the cleanup deadline: {err}")
    });
}

#[test]
fn sigint_to_fiber_models_while_a_credential_command_runs_exits_130_and_kills_its_group() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    // Before the spawn: a command-line watchdog covers a spawn no later
    // watchdog can reach.
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("echo $$ > '{}'; sleep 600", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    // The first line proves the command ran: the signal below lands while
    // the command sleeps on every run.
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    support::kill_pid(setup.deadline, fiber_pid, "INT").unwrap();
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline.left()));
    group.stand_down(setup.deadline.cleanup());
    watchdog.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}

#[test]
fn sighup_to_fiber_models_while_a_credential_command_runs_exits_129_and_kills_its_group() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("echo $$ > '{}'; sleep 600", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    support::kill_pid(setup.deadline, fiber_pid, "HUP").unwrap();
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(129),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline.left()));
    group.stand_down(setup.deadline.cleanup());
    watchdog.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}

#[test]
fn sigterm_to_refresh_model_lists_while_a_credential_command_runs_exits_143_and_kills_its_group() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("echo $$ > '{}'; sleep 600", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["refresh-model-lists", "acme"]);
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    support::kill_pid(setup.deadline, fiber_pid, "TERM").unwrap();
    let output = setup.finish("`fiber refresh-model-lists` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(143),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline.left()));
    group.stand_down(setup.deadline.cleanup());
    watchdog.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}

#[test]
fn fiber_models_returning_normally_kills_what_its_credential_command_left() {
    let setup = Setup::new();
    // Declared after `setup`, so it drops first: a failing run kills this
    // test's refresh child before its directory is removed.
    let refresh_guard = Watchdog::matching(&setup.refresh_pattern());
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    // The command gives a key and leaves `sleep` in its group.
    install(
        &setup.home(),
        &format!(
            "sleep 600 >/dev/null 2>&1 & echo $$ > '{}'; printf key",
            fifo.display()
        ),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    // The first line proves the credential command ran.
    let first = ready.wait(setup.deadline.left())[0];
    let first_group = Watchdog::group(first);
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    // The list is stale, so `fiber models` starts its detached refresh
    // child, which runs the command again: the second line.
    let second = ready.wait(setup.deadline.left())[0];
    let second_group = Watchdog::group(second);
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    assert!(
        fakes::group_empties(first, setup.deadline.left()),
        "the normal return killed what the command left"
    );
    assert!(
        fakes::group_empties(second, setup.deadline.left()),
        "the refresh child is gone with its group"
    );
    first_group.stand_down(setup.deadline.cleanup());
    second_group.stand_down(setup.deadline.cleanup());
    watchdog.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
    refresh_guard.stand_down(setup.deadline.cleanup());
}

#[test]
fn refresh_model_lists_returning_normally_kills_what_its_credential_command_left() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    // The command gives a key and leaves `sleep` in its group.
    install(
        &setup.home(),
        &format!(
            "sleep 600 >/dev/null 2>&1 & echo $$ > '{}'; printf key",
            fifo.display()
        ),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["refresh-model-lists", "acme"]);
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    let output = setup.finish("`fiber refresh-model-lists` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        fakes::group_empties(pgid, setup.deadline.left()),
        "the normal return killed what the command left"
    );
    group.stand_down(setup.deadline.cleanup());
    watchdog.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}

#[test]
fn sigterm_to_the_terminal_while_a_picker_read_runs_exits_143_and_kills_its_group() {
    let setup = support::Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("echo $$ > '{}'; sleep 600", fifo.display()),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    // Ctrl+L opens the picker, whose catalogue read runs through the
    // process's shared reads.
    run.write(b"\x0c");
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    support::kill_pid(setup.deadline, run.id(), "TERM").unwrap();
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(143));
    assert!(fakes::group_empties(pgid, setup.deadline.left()));
    group.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}

#[test]
fn sighup_to_the_terminal_while_a_picker_read_runs_exits_129_and_kills_its_group() {
    let setup = support::Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("echo $$ > '{}'; sleep 600", fifo.display()),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    // Ctrl+L opens the picker, whose catalogue read runs through the
    // process's shared reads.
    run.write(b"\x0c");
    let pgid = ready.wait(setup.deadline.left())[0];
    let group = Watchdog::group(pgid);
    support::kill_pid(setup.deadline, run.id(), "HUP").unwrap();
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(129));
    assert!(fakes::group_empties(pgid, setup.deadline.left()));
    group.stand_down(setup.deadline.cleanup());
    matching.stand_down(setup.deadline.cleanup());
}
