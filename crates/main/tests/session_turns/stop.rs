//! Binary-level tests of top-level `fiber stop`
//! (`docs/invocation.md`, "Commands and flags"): the built `fiber` stops a
//! running hub and its sessions through their own shutdown, and a bare hub
//! `stop` leaves the sessions alone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

impl Setup {
    /// Runs `fiber` with `args`, waiting under the test's [`Deadline`],
    /// and parses stdout's event lines into a [`Run`].
    #[track_caller]
    fn stop_ask(&self, args: &[&str]) -> Run {
        // Ask resolves the project from the working directory, so it runs
        // in the workspace like the session it resumes.
        let mut command = self.fiber(args);
        command.current_dir(self.workspace());
        let output = run_to_exit(self.deadline, "fiber ask", command);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|line| !is_status(line))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Run {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

#[test]
fn stop_stops_the_session_and_the_hub_and_the_session_resumes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    // The first model request lands only after the pre-send fsync, which
    // makes the appended first log line durable: without this wait the
    // stop below can land in the append-to-fsync window and the resume
    // finds no session.
    await_first_request(&server, setup.deadline);
    drop(client);

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions and the hub.\n"
    );
    // The hub unlinks its socket at exit (`docs/state.md`, "What each part holds"), so check sockets only after it exits.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    assert!(
        !hub.wait().success(),
        "the stopped hub exits as SIGTERM does"
    );
    // The stop's print proves its exchange closed (`docs/invocation.md`,
    // "Commands and flags"), so the sockets are asserted at once: the
    // print IS the proof.
    assert!(
        UnixStream::connect(setup.hub_socket()).is_err(),
        "the hub socket is gone"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_err(),
        "the session socket is gone"
    );

    let prefix = &session[..8];
    let second = setup.stop_ask(&["ask", "--resume", prefix, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.session_id(), session);
    guard.wait_gone();
}

#[test]
fn a_bare_stop_leaves_the_session_socket_accepting() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);

    client.send(r#"{"id":"c_stop","command":"stop","args":{}}"#);
    let ack = recv_answer(&client, "c_stop", "the stop acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a bare stop answers with no result: {ack}"
    );
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    assert!(
        !hub.wait().success(),
        "a bare stop ends the hub as SIGTERM does"
    );

    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    // The close ending the socket only proves the peer went away: the
    // session unlinks its path at exit (`docs/state.md`, "What each part holds"), after.
    wait_for_path_gone(
        setup.deadline,
        &setup.session_socket(&session),
        "the closed session to unlink its socket",
    );
    guard.wait_gone();
}

#[test]
fn stop_with_a_dead_hub_starts_one_to_stop_the_orphaned_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    let status = hub.kill_and_wait();
    assert!(!status.success());
    assert!(
        UnixStream::connect(setup.hub_socket()).is_err(),
        "the killed hub left no socket behind"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_ok(),
        "the orphaned session still accepts"
    );

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions and the hub.\n"
    );
    // The stop's print proves its exchange closed (`docs/invocation.md`,
    // "Commands and flags"), so the socket is asserted at once: the print
    // IS the proof.
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_err(),
        "the session socket is gone"
    );
    guard.wait_gone();
}

/// A scripted model step file the tests address as `scripted/<name>`,
/// mirroring the keeper-version acceptance pattern.
fn script(setup: &Setup, name: &str, steps: Value) {
    write_json(&setup.workspace().join(name), &json!({"steps": steps}));
}

fn configured() -> Setup {
    let setup = Setup::new();
    script(&setup, "s.json", json!([{"text": "Ready."}]));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "sandbox": {"enabled": false}}),
    );
    setup
}

/// The ask child's stdout lines, one per fiber event.
fn ask_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let out = child.stdout.take().unwrap();
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    lines
}

/// The next stdout line of `kind`, parsed: the test fails naming `kind`
/// when the child dies first.
fn ask_event(lines: &mpsc::Receiver<String>, kind: &str, deadline: Deadline) -> Value {
    let wait = deadline.start_wait();
    loop {
        let text = wait
            .recv(lines)
            .unwrap_or_else(|error| panic!("waiting for the ask's live stdout {kind}: {error}"));
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == kind {
            return line;
        }
    }
}

fn reap(mut child: Child, deadline: Deadline) -> std::process::ExitStatus {
    let (send, result) = mpsc::channel();
    thread::spawn(move || match send.send(child.wait()) {
        Ok(()) | Err(_) => {}
    });
    deadline.start_wait().recv(&result).unwrap().unwrap()
}

/// The pid a shell sleep records for itself: keeper-hosted jobs lead
/// their own group, so the pid is its pgid. `echo $$` runs in the job's
/// leader before it sleeps, so the file names the group to watch.
fn read_pid(path: &std::path::Path, deadline: Deadline) -> u32 {
    let watched = path.to_path_buf();
    fakes::wait_until("the sleep to record its pid", deadline, move || {
        watched.exists()
    });
    fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap_or_else(|error| panic!("the pid file parses: {error}"))
}

#[test]
fn stop_with_no_hub_stops_a_session_and_a_keeper_holding_a_job() {
    // A live session and a crashed one sharing a home, no hub running:
    // the transient hub stops the session through its close and the
    // keeper through the frozen line, then prints both counts.
    let setup = configured();
    let live_pid = setup.workspace().join("live.pid");
    let job_pid = setup.workspace().join("job.pid");
    let live_command = format!("sh -c 'echo $$ > {}; sleep 120'", live_pid.display());
    let job_command = format!("sh -c 'echo $$ > {}; sleep 120'", job_pid.display());
    script(
        &setup,
        "live.json",
        json!([{"tool_calls": [{"name": "shell", "arguments": {"command": live_command}}]}]),
    );
    script(
        &setup,
        "job.json",
        json!([
            {"tool_calls": [{"name": "shell", "arguments": {"command": job_command, "run_in_background": true}}]},
            {"tool_calls": [{"name": "shell", "arguments": {"command": "sleep 120"}}]},
        ]),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": live_command}),
            json!({"decision": "allow", "tool": "shell", "prefix": job_command}),
        ),
    )
    .unwrap();
    let seed = |setup: &Setup| {
        let mut command = setup.fiber(&["ask", "seed"]);
        command.current_dir(setup.workspace());
        let seed = run_to_exit(setup.deadline, "fiber ask seed", command);
        assert!(seed.status.success(), "the seed asks for an id");
        let seed = complete_lines(&String::from_utf8(seed.stdout).unwrap());
        seed[0]["session_id"].as_str().unwrap().to_owned()
    };
    let live_id = seed(&setup);
    let mut live = setup.fiber(&[
        "ask",
        "--resume",
        &live_id,
        "--model",
        "scripted/live.json",
        "run",
    ]);
    live.current_dir(setup.workspace());
    let (mut live, live_watchdog) = spawn_watched(&mut live);
    let live_group = live.id();
    let live_guard = KillGroup(live_group);
    let live_lines = ask_lines(&mut live);
    // The requested tool call proves the session is mid-turn, not idle;
    // the pid file proves the sleep itself runs.
    ask_event(&live_lines, "tool_call_requested", setup.deadline);
    let live_sleep = read_pid(&live_pid, setup.deadline);
    assert!(
        fakes::group_alive(live_sleep),
        "the live session's sleep runs before the stop"
    );
    let live_sleep_guard = KillGroup(live_sleep);
    let crash_id = seed(&setup);
    let mut crash = setup.fiber(&[
        "ask",
        "--resume",
        &crash_id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    crash.current_dir(setup.workspace());
    let (mut crash, crash_watchdog) = spawn_watched(&mut crash);
    let crash_group = crash.id();
    let crash_guard = KillGroup(crash_group);
    let crash_lines = ask_lines(&mut crash);
    // The start fences the background call's completed adoption: killing
    // now leaves the job keeper-held.
    ask_event(&crash_lines, "job_started", setup.deadline);
    let job = read_pid(&job_pid, setup.deadline);
    assert!(fakes::group_alive(job), "the job runs before the kill");
    let job_guard = KillGroup(job);
    drop(fakes::kill_group(crash_group, "KILL"));
    assert!(!reap(crash, setup.deadline).success(), "the kill lands");
    assert!(
        fakes::group_empties(crash_group, setup.deadline),
        "the crashed session's group emptied"
    );
    std::mem::forget(crash_guard);
    crash_watchdog.stand_down(setup.deadline.cleanup_phase());
    // The keeper takes the crashed socket over without killing the job.
    let keeper_socket = setup.session_socket(&crash_id);
    fakes::wait_until("the keeper socket to accept", setup.deadline, move || {
        UnixStream::connect(&keeper_socket).is_ok()
    });
    assert!(fakes::group_alive(job), "the keeper-held job still runs");

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions, 1 keeper and the hub.\n"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&live_id)).is_err(),
        "the live session socket is gone"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&crash_id)).is_err(),
        "the keeper socket is gone"
    );
    // The hub unlinks its socket at exit, after the stop's print: the print
    // proves the exchange closed, not that the hub process has exited.
    wait_for_path_gone(
        setup.deadline,
        &setup.hub_socket(),
        "the transient hub to unlink its socket",
    );
    // The stop waits for sockets, not groups (`docs/invocation.md`,
    // `stop`): the live turn's foreground tool dies on the session's own
    // shutdown, after the print, so only the keeper-held job must be gone
    // here — the frozen stop ends every job as a shutdown does. The live
    // leftovers die on the guards below.
    assert!(
        fakes::group_empties(job, setup.deadline),
        "the keeper-held job's group emptied"
    );
    std::mem::forget(job_guard);
    live_watchdog.stand_down(setup.deadline.cleanup_phase());
    drop(live_guard);
    drop(live_sleep_guard);
    let _ = reap(live, setup.deadline);
}

#[test]
fn stop_with_a_hub_stops_a_session_and_a_keeper_holding_a_job() {
    // The hub-path twin: a live hub session and a crashed direct-ask
    // session sharing a home, with the hub running throughout.
    let setup = configured();
    let job_pid = setup.workspace().join("job.pid");
    let job_command = format!("sh -c 'echo $$ > {}; sleep 120'", job_pid.display());
    script(
        &setup,
        "job.json",
        json!([
            {"tool_calls": [{"name": "shell", "arguments": {"command": job_command, "run_in_background": true}}]},
            {"tool_calls": [{"name": "shell", "arguments": {"command": "sleep 120"}}]},
        ]),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": job_command}),
        ),
    )
    .unwrap();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let live_id = start_session(&client, &workspace, PROMPT);
    await_first_request(&server, setup.deadline);
    let mut seed_command = setup.fiber(&["ask", "seed"]);
    seed_command.current_dir(setup.workspace());
    let seed = run_to_exit(setup.deadline, "fiber ask seed", seed_command);
    assert!(seed.status.success(), "the seed asks for an id");
    let seed = complete_lines(&String::from_utf8(seed.stdout).unwrap());
    let crash_id = seed[0]["session_id"].as_str().unwrap().to_owned();
    let mut crash = setup.fiber(&[
        "ask",
        "--resume",
        &crash_id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    crash.current_dir(setup.workspace());
    let (mut crash, crash_watchdog) = spawn_watched(&mut crash);
    let crash_group = crash.id();
    let crash_guard = KillGroup(crash_group);
    let crash_lines = ask_lines(&mut crash);
    ask_event(&crash_lines, "job_started", setup.deadline);
    let job = read_pid(&job_pid, setup.deadline);
    assert!(fakes::group_alive(job), "the job runs before the kill");
    let job_guard = KillGroup(job);
    drop(fakes::kill_group(crash_group, "KILL"));
    assert!(!reap(crash, setup.deadline).success(), "the kill lands");
    std::mem::forget(crash_guard);
    crash_watchdog.stand_down(setup.deadline.cleanup_phase());
    let keeper_socket = setup.session_socket(&crash_id);
    fakes::wait_until("the keeper socket to accept", setup.deadline, move || {
        UnixStream::connect(&keeper_socket).is_ok()
    });
    assert!(fakes::group_alive(job), "the keeper-held job still runs");
    drop(client);

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions, 1 keeper and the hub.\n"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&live_id)).is_err(),
        "the live session socket is gone"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&crash_id)).is_err(),
        "the keeper socket is gone"
    );
    // The hub unlinks its socket at exit, after the stop's print: the print
    // proves the exchange closed, not that the hub process has exited.
    wait_for_path_gone(
        setup.deadline,
        &setup.hub_socket(),
        "the hub to unlink its socket",
    );
    assert!(
        fakes::group_empties(job, setup.deadline),
        "the keeper-held job's group emptied"
    );
    std::mem::forget(job_guard);
    guard.wait_gone();
}
