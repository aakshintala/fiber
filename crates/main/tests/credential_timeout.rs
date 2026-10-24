//! Binary-level tests of a `command` credential source's `timeout`
//! (`docs/testing.md`, "Levels"): a command still running at its timeout
//! fails the read, and a signal while one runs kills it and writes nothing to stdout or stderr.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Output};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog, children::Ready};
use serde_json::{Value, json};
use support::{Setup, hello_inline_completed as hello, is_status, write_json};

/// Spawns `fiber` with `args` in its own process group, guarded by a
/// watchdog that kills the group if the test process dies first.
#[track_caller]
fn start(setup: &Setup, args: &[&str]) -> (u32, Child, Watchdog) {
    let child = setup.fiber(args).spawn().unwrap();
    let pid = child.id();
    let watchdog = Watchdog::group(pid);
    (pid, child, watchdog)
}

/// Waits for `child` under the test's deadline: on expiry its group is
/// killed and the test fails naming `what`.
#[track_caller]
fn finish(setup: &Setup, what: &str, group: u32, child: Child) -> Output {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, what),
    }
}

/// Stdout's JSON lines, without status lines.
fn lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !is_status(line))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Stderr as text.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// No session directory exists under Fiber home.
#[track_caller]
fn assert_no_session_dir(setup: &Setup) {
    let mut dirs = vec![setup.home()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                assert!(
                    !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("s_")),
                    "a session directory was created: {}",
                    path.display()
                );
                dirs.push(path);
            }
        }
    }
}

/// The project's sessions directory: `fiber` runs with the test root as
/// its directory, so the project key is the root's.
fn sessions(setup: &Setup) -> PathBuf {
    let workspace = fs::canonicalize(setup.root.path()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// `fiber ask hi` to completion, returning its session id.
#[track_caller]
fn ask_once(setup: &Setup) -> String {
    let (pid, child, watchdog) = start(setup, &["ask", "hi"]);
    let output = finish(setup, "`fiber ask hi` to exit", pid, child);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let id = lines(&output)[0]["session_id"].as_str().unwrap().to_owned();
    watchdog.stand_down(setup.deadline.cleanup_phase());
    id
}

#[test]
fn a_credential_command_still_running_at_its_timeout_fails_session_start() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    // Before the spawn: a command-line watchdog covers a spawn no later
    // watchdog can reach.
    let matching = Watchdog::matching(&fifo.display().to_string());
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credentials": {"default": {
            "command": ["sh", "-c", "echo $$ > \"$1\"; sleep 600", "sh", fifo.to_string_lossy(), "sk-in-argument"],
            "timeout": "1s",
        }}}}}),
    );
    let (fiber_pid, child, watchdog) = start(&setup, &["ask", "hi"]);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    let output = finish(&setup, "`fiber ask hi` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let lines = lines(&output);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["kind"], "fiber_exited");
    assert_eq!(lines[0]["payload"]["error"]["code"], "credential_missing");
    let message = lines[0]["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("`sh` was still running after 1s"),
        "{message}"
    );
    assert!(
        stderr(&output).contains("`sh` was still running after 1s"),
        "{}",
        stderr(&output)
    );
    assert!(!message.contains("sk-in-argument"), "{message}");
    assert!(
        !stderr(&output).contains("sk-in-argument"),
        "{}",
        stderr(&output)
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    assert_no_session_dir(&setup);
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_signal_while_a_startup_credential_command_runs_kills_it_and_writes_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    // No `timeout`: the 30 s default. The signal lands long before it.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credentials": {"default": {
            "command": ["sh", "-c", "echo $$ > \"$1\"; sleep 600", "sh", fifo.to_string_lossy()],
        }}}}}),
    );
    let (fiber_pid, child, watchdog) = start(&setup, &["ask", "hi"]);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(fiber_pid, "TERM").unwrap();
    let output = finish(&setup, "`fiber ask hi` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(143),
        "stderr: {}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty(), "a recorded signal writes nothing");
    assert!(
        stderr(&output).is_empty(),
        "a recorded signal writes nothing"
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    assert_no_session_dir(&setup);
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_signal_while_a_resumed_sessions_credential_command_runs_kills_it_and_leaves_the_log() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let id = ask_once(&setup);
    let events = sessions(&setup).join(&id).join("events.jsonl");
    let before = fs::read(&events).unwrap();
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "providers": {"fake": {"credentials": {"default": {
            "command": ["sh", "-c", "echo $$ > \"$1\"; sleep 600", "sh", fifo.to_string_lossy()],
        }}}}}),
    );
    let (fiber_pid, child, watchdog) = start(&setup, &["ask", "--resume", &id, "hi"]);
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    fakes::kill_pid(fiber_pid, "TERM").unwrap();
    let output = finish(&setup, "`fiber ask --resume` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(143),
        "stderr: {}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty(), "a recorded signal writes nothing");
    assert!(
        stderr(&output).is_empty(),
        "a recorded signal writes nothing"
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    assert_eq!(fs::read(&events).unwrap(), before);
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_signal_while_a_resumed_session_starts_its_mcp_server_exits_143_writing_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let id = ask_once(&setup);
    let events = sessions(&setup).join(&id).join("events.jsonl");
    let before = fs::read(&events).unwrap();
    // A server that writes its pid, then never answers `initialize`. It
    // ignores SIGTERM, so the stop's kill after the grace is the path
    // exercised; its startup deadline is far past any bound here.
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    let script = format!(
        "trap '' TERM\necho $$ > '{}'\nexec sleep 3600\n",
        fifo.display()
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"slow": {
            "command": "/bin/sh",
            "args": ["-c", script],
            "startup_timeout_ms": 600_000,
        }}}}),
    );
    let (fiber_pid, child, watchdog) = start(&setup, &["ask", "--resume", &id, "hi"]);
    ready.wait(setup.deadline);
    fakes::kill_pid(fiber_pid, "TERM").unwrap();
    let output = finish(&setup, "`fiber ask --resume` to exit", fiber_pid, child);
    assert_eq!(
        output.status.code(),
        Some(143),
        "stderr: {}",
        stderr(&output)
    );
    assert!(output.stdout.is_empty(), "a recorded signal writes nothing");
    assert!(
        stderr(&output).is_empty(),
        "a recorded signal writes nothing"
    );
    assert_eq!(fs::read(&events).unwrap(), before);
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_startup_failure_after_a_credential_command_kills_what_it_left() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let ready = Ready::new(setup.root.path());
    let fifo = ready.path().to_path_buf();
    let matching = Watchdog::matching(&fifo.display().to_string());
    // The command gives a key and leaves `sleep` in its group; the MCP
    // server below is refused after the credential read.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m",
        "providers": {"fake": {"credentials": {"default": {
            "command": ["sh", "-c", "sleep 600 >/dev/null 2>&1 & echo $$ > \"$1\"; printf key", "sh", fifo.to_string_lossy()],
        }}}},
        "mcp": {"servers": {"fx": {
            "command": "/bin/sh",
            "url": "http://127.0.0.1:9/mcp",
            "required": true,
        }}}}),
    );
    let (fiber_pid, child, watchdog) = start(&setup, &["ask", "hi"]);
    // The line proves the credential command ran before the failure below.
    let pgid = ready.wait(setup.deadline)[0];
    let group = Watchdog::group(pgid);
    let output = finish(&setup, "`fiber ask hi` to exit", fiber_pid, child);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let lines = lines(&output);
    assert_eq!(
        lines.last().unwrap()["payload"]["error"]["code"],
        "config_invalid"
    );
    assert!(fakes::group_empties(pgid, setup.deadline));
    group.stand_down(setup.deadline.cleanup_phase());
    watchdog.stand_down(setup.deadline.cleanup_phase());
    matching.stand_down(setup.deadline.cleanup_phase());
}
