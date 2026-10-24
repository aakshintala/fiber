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

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::children::Ready;
use fakes::{Deadline, Watchdog};
use serde_json::json;
use support::pty::{Grid, Run};

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

/// A fresh temporary root holding Fiber home and the workspace.
fn temp_root() -> fakes::TempDir {
    let root = fakes::TempDir::new("fh");
    fs::create_dir_all(root.path().join("h")).unwrap();
    fs::create_dir_all(root.path().join("w")).unwrap();
    root
}

/// Warms the built `fiber` before a PTY test's deadline starts: these
/// tests exec `CARGO_BIN_EXE_fiber` itself through `Run::spawn`, and a
/// first exec can stall for seconds on macOS before `main` runs
/// (`docs/testing.md`, "Waits and timeouts"). `-V` is rejected by the
/// usage path before anything reads home or config, and exits 2. The
/// stall lands in the warm-up's own `START_BOUND`.
fn warm_built_fiber() {
    fakes::warm_up(Path::new(env!("CARGO_BIN_EXE_fiber")), &["-V"], 2);
}

impl Setup {
    fn new() -> Self {
        let root = temp_root();
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
        // The first exec of a fresh path can stall on macOS before `main`
        // runs, so warm this test's own link here: `-V` is rejected by the
        // usage path before anything reads home or config, and exits 2.
        // The stall lands in the warm-up's own `START_BOUND`, and the
        // deadline below starts after it.
        fakes::warm_up(&exe, &["-V"], 2);
        let deadline = Deadline::start();
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

/// Whether the grid shows the open model picker: the picker's filter row.
/// The open picker proves the Ctrl+L keystroke landed.
fn picker_open(screen: &Grid) -> bool {
    screen.rows.iter().any(|row| row.contains("Type to search"))
}

/// Whether the grid shows the picker's catalogue read asked: the refreshing
/// mark once the cached lists arrived or the reading line while they have
/// not. The asked mark proves the catalogue read was asked, so the
/// credential command runs and writes its ready line.
fn picker_read_asked(screen: &Grid) -> bool {
    screen
        .rows
        .iter()
        .any(|row| row.contains("refreshing") || row.contains("Reading the model lists"))
}

/// Whether the grid shows the picker's filter query `query`: the filter
/// row draws `›` and the query bold once the loop handled the typing.
fn picker_query_shown(screen: &Grid, query: &str) -> bool {
    let needle = format!("› {query}");
    screen.rows.iter().any(|row| row.contains(&needle))
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
    if fakes::matching_exits(pattern, deadline) {
        return;
    }
    support::kill_matching(deadline, pattern).unwrap();
    fakes::try_matching_exits(pattern, deadline.cleanup_phase()).unwrap_or_else(|err| {
        panic!("the refresh child outlived SIGKILL past the cleanup deadline: {err}")
    });
}

/// One live member of a process group seen by `ps`: its pid and state.
#[derive(Debug, PartialEq, Eq)]
struct GroupMember {
    pid: u32,
    stat: String,
}

/// Parses `ps -o pid= -o pgid= -o stat=` output for the live members of
/// `pgid`. Errors on a malformed line and on output that lists no process
/// at all, so an empty or failed observation is never proof the group is
/// empty; only rows carrying this `pgid` are members.
fn parse_ps_members(output: &str, pgid: u32) -> Result<Vec<GroupMember>, String> {
    let mut members = Vec::new();
    let mut rows = 0;
    for (index, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        rows += 1;
        let mut fields = line.split_whitespace();
        let (pid, gid, stat) = match (fields.next(), fields.next(), fields.next()) {
            (Some(pid), Some(gid), Some(stat)) => (pid, gid, stat),
            _ => {
                return Err(format!(
                    "ps line {} is not `pid pgid stat`: {line:?}",
                    index + 1
                ));
            }
        };
        let pid: u32 = pid
            .parse()
            .map_err(|_| format!("ps pid is not a number: {pid:?}"))?;
        let gid: u32 = gid
            .parse()
            .map_err(|_| format!("ps pgid is not a number: {gid:?}"))?;
        if gid == pgid {
            members.push(GroupMember {
                pid,
                stat: stat.to_owned(),
            });
        }
    }
    if rows == 0 {
        return Err("ps listed no processes".to_owned());
    }
    Ok(members)
}

/// Lists the live members of `pgid` through `ps`, run in its own watched
/// group with its output received under `deadline`. Sends no signal and
/// makes no machine-wide assertion. A `ps` that never exits is killed and
/// reported, never read as empty.
#[track_caller]
fn observe_group_members(deadline: Deadline, pgid: u32) -> Result<Vec<GroupMember>, String> {
    let mut command = Command::new("ps");
    command
        .args(["-ax", "-o", "pid=", "-o", "pgid=", "-o", "stat="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let (child, watchdog) = support::spawn_watched(&mut command);
    let ps_group = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let wait = deadline.left().min(MUST_SUCCEED_WITHIN);
    let output = match Deadline::after(wait).recv(&finished) {
        Ok(output) => output.map_err(|error| format!("ps could not be reaped: {error}"))?,
        Err(_) => {
            match fakes::kill_group(ps_group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            watchdog.stand_down(deadline.cleanup_phase());
            return Err(format!("ps did not exit within {wait:?}"));
        }
    };
    watchdog.stand_down(deadline.cleanup_phase());
    if !output.status.success() {
        return Err(format!(
            "ps exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("ps output is not text: {error}"))?;
    parse_ps_members(&text, pgid)
}

#[test]
fn ps_parser_lists_only_the_asked_group() {
    let output = "  101   100 Ss\n  202   200 SN\n  203   200 R\n";
    assert_eq!(
        parse_ps_members(output, 200).unwrap(),
        vec![
            GroupMember {
                pid: 202,
                stat: "SN".to_owned()
            },
            GroupMember {
                pid: 203,
                stat: "R".to_owned()
            },
        ]
    );
    assert_eq!(parse_ps_members(output, 300).unwrap(), vec![]);
}

#[test]
fn ps_parser_rejects_empty_and_malformed_output() {
    assert!(
        parse_ps_members("", 200).is_err(),
        "empty is no observation"
    );
    assert!(
        parse_ps_members("   \n", 200).is_err(),
        "blank is no observation"
    );
    assert!(
        parse_ps_members("  202\n", 200).is_err(),
        "a short line is malformed"
    );
    assert!(
        parse_ps_members("  pid   pgid stat\n", 200).is_err(),
        "a header is malformed"
    );
    assert!(
        parse_ps_members("  abc   200 SN\n", 200).is_err(),
        "a non-numeric pid is malformed"
    );
}

#[test]
fn ps_observer_runs_and_finds_no_member_in_an_absent_group() {
    // A group id past the pid range holds nothing: `ps` runs and reports
    // no member, proving the observer itself works.
    let deadline = Deadline::start();
    assert_eq!(observe_group_members(deadline, 4_000_000).unwrap(), vec![]);
}

/// The fork-during-kill orphan: a credential command forking as the kill
/// lands outlives one SIGKILL, so the group still holds a member when
/// Fiber exits. macOS only: it forces the kernel interleaving by
/// repeating the run, 20 attempts sharing one deadline.
#[cfg(target_os = "macos")]
#[test]
fn sigint_to_fiber_models_during_a_credential_fork_loop_leaves_no_group_member_fork_loop() {
    // One link warmed once before the clock below starts: every attempt
    // reuses this path with a fresh root, so the first-exec stall lands
    // in the warm-up's own `START_BOUND`, never in the deadline.
    let mut setup = Setup::new();
    setup.deadline = Deadline::start();
    for attempt in 0..20 {
        setup.root = temp_root();
        let ready = Ready::new(setup.root.path());
        let fifo = ready.path().to_path_buf();
        // Before the spawn: a command-line watchdog covers a spawn no
        // later watchdog can reach.
        let matching = Watchdog::matching(&fifo.display().to_string());
        // The ready line first, then the fork loop: the signal lands while
        // the command forks on every run.
        install(
            &setup.home(),
            &format!(
                "echo $$ > '{}'; while :; do sleep 600 & done",
                fifo.display()
            ),
        );
        let (fiber_pid, child, watchdog) = setup.start(&["models"]);
        // The first line proves the command ran: the signal below lands
        // while the command forks on every run.
        let pgid = ready.wait(setup.deadline)[0];
        let group = Watchdog::group(pgid);
        fakes::kill_pid(fiber_pid, "INT").unwrap();
        let output = setup.finish("`fiber models` to exit", fiber_pid, child);
        assert_eq!(
            output.status.code(),
            Some(130),
            "attempt {attempt}: stderr: {}",
            stderr(&output)
        );
        // Observed at Fiber exit, before the guarded cleanup below kills
        // a survivor: the entry point exits once every such group is
        // empty (`docs/invocation.md`, "Shutdown"), so any member, in
        // any state, is the orphan.
        let members = observe_group_members(setup.deadline, pgid)
            .unwrap_or_else(|error| panic!("attempt {attempt}: observing group {pgid}: {error}"));
        // Guarded cleanup before the assertion: a survivor must not
        // outlive the attempt either way.
        match fakes::kill_group(pgid, "KILL") {
            Ok(_) | Err(_) => {}
        }
        group.stand_down(setup.deadline.cleanup_phase());
        watchdog.stand_down(setup.deadline.cleanup_phase());
        matching.stand_down(setup.deadline.cleanup_phase());
        assert!(
            members.is_empty(),
            "attempt {attempt}: group {pgid} still holds {members:?} at Fiber exit"
        );
    }
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
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    // The ready line proves the descendant started before the signal: the
    // sleep forks before the echo, and the shell waits for it.
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(fiber_pid, "INT").unwrap();
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn sighup_to_fiber_models_while_a_credential_command_runs_exits_129_and_kills_its_group() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(fiber_pid, "HUP").unwrap();
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(129),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn sigterm_to_refresh_model_lists_while_a_credential_command_runs_exits_143_and_kills_its_group() {
    let setup = Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let (fiber_pid, child, watchdog) = setup.start(&["refresh-model-lists", "acme"]);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(fiber_pid, "TERM").unwrap();
    let output = setup.finish("`fiber refresh-model-lists` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(143),
        "stderr: {}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn fiber_models_with_nothing_in_flight_exits_successfully() {
    let setup = Setup::new();
    // Declared after `setup`, so it drops first: a failing run kills this
    // test's refresh child before its directory is removed.
    let refresh_guard = Watchdog::matching(&setup.refresh_pattern());
    // The command exits at once with a key, so nothing is in flight when
    // `fiber models` settles its reads on exit: under the `==`/`!=` mutant
    // that settle waits the full bound, so the deadline below fails
    // the test at once.
    install(&setup.home(), "printf key");
    let (fiber_pid, child, watchdog) = setup.start(&["models"]);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let output = match deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(deadline, fiber_pid, &finished, "`fiber models` to exit"),
    };
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    refresh_guard.stand_down(setup.deadline.cleanup_phase());
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
    let first = ready.wait(setup.deadline)[0];
    let first_group = Watchdog::group(first);
    let output = setup.finish("`fiber models` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    // The list is stale, so `fiber models` starts its detached refresh
    // child, which runs the command again: the second line.
    let second = ready.wait(setup.deadline)[0];
    let second_group = Watchdog::group(second);
    await_refresh_exit(setup.deadline, &setup.refresh_pattern());
    assert!(
        fakes::group_empties(first, setup.deadline),
        "the normal return killed what the command left"
    );
    assert!(
        fakes::group_empties(second, setup.deadline),
        "the refresh child is gone with its group"
    );
    first_group.stand_down(setup.deadline.cleanup_phase());
    second_group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
    refresh_guard.stand_down(setup.deadline.cleanup_phase());
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
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    let output = setup.finish("`fiber refresh-model-lists` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        fakes::group_empties(pgid, setup.deadline),
        "the normal return killed what the command left"
    );
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn sigterm_to_the_terminal_while_a_picker_read_runs_exits_143_and_kills_its_group() {
    warm_built_fiber();
    let setup = support::Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    // Ctrl+L opens the picker, whose catalogue read is asked through the
    // process's shared reads.
    run.write(b"\x0c");
    run.wait_screen("the open picker", picker_open);
    run.wait_screen("the picker's asked catalogue read", picker_read_asked);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(run.id(), "TERM").unwrap();
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(143));
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn sighup_to_the_terminal_while_a_picker_read_runs_exits_129_and_kills_its_group() {
    warm_built_fiber();
    let setup = support::Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    // Ctrl+L opens the picker, whose catalogue read is asked through the
    // process's shared reads.
    run.write(b"\x0c");
    run.wait_screen("the open picker", picker_open);
    run.wait_screen("the picker's asked catalogue read", picker_read_asked);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(run.id(), "HUP").unwrap();
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(129));
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

/// Deterministic repro for #2738: a queued catalogue read restarts
/// without its asked mark, so the picker's asked wait reaches its
/// deadline on every run. The first read hangs in its credential command
/// (its ready line proves it runs instead of queuing behind the startup
/// cached read); a refresh while it hangs queues a wider read behind it,
/// and killing the first read's group answers it with an error, which
/// clears the mark and starts the queued read markless. The store that
/// clears the mark on an older read's answer, and the queued start
/// without one, live in the picker's rendering (`crates/tui`), not in
/// this file.
#[test]
fn queued_picker_read_restarts_without_its_asked_mark_2738() {
    warm_built_fiber();
    let setup = support::Setup::new();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    install(
        &setup.home(),
        &format!("sleep 600 & echo $$ > '{}'; wait", fifo.display()),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    // Ctrl+L opens the picker, whose catalogue read is asked through the
    // process's shared reads.
    run.write(b"\x0c");
    run.wait_screen("the open picker", picker_open);
    let first = ready.wait(setup.deadline)[0];
    // Ctrl+R queues a wider read behind the hanging one; the typed query
    // proves the loop handled the refresh first, since it handles its
    // inputs in order.
    run.write(b"\x12");
    run.write(b"x");
    run.wait_screen("the picker's filter query", |screen| {
        picker_query_shown(screen, "x")
    });
    // Killing the first read's group answers it with an error and starts
    // the queued read: the second ready line proves the queued read runs
    // its credential command.
    fakes::kill_group(first, "KILL").unwrap();
    let second = ready.wait(setup.deadline)[0];
    let second_group = Watchdog::group(second);
    // The restarted read hangs in its credential command with no mark:
    // this wait reaches its deadline while the bug stands.
    run.wait_screen("the picker's asked catalogue read", picker_read_asked);
    fakes::kill_pid(run.id(), "HUP").unwrap();
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(129));
    assert!(fakes::group_empties(second, setup.deadline));
    second_group.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}
