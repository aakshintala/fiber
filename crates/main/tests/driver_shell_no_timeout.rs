//! The person's `!` command has no timeout while the agent's `shell` keeps
//! its own (`docs/testing.md`, "Levels"): a socket-level combined proof on
//! the built `fiber` (an agent `sleep 5` with `timeout_ms: 1000` fails
//! `timeout` while a driver `sleep 5` completes), plus a literal pty test
//! typing `!sleep 5` and asserting the sent row. Reuses the `fiber session`
//! patterns of `driver_shell_sent.rs`: the fixture, the held provider, one
//! [`Deadline`], no sleeps and no env vars; and the pty `Run`/`wait_screen`
//! sent-row pattern of `shell_look.rs`. `sleep 5` sits below the 25-second
//! bare-wait refusal, so the guard is not in play for any test here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::pty::Run;
use support::{
    Running, Setup, Socket, TRUECOLOUR, expired, function_call, hello, recv_answer, stream,
    subscribe_direct, until_after, until_close, write_json,
};

#[track_caller]
fn start(setup: &Setup, args: &[&str]) -> Running {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_copy = Arc::clone(&stderr_text);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut reader, &mut buf) {
            Ok(_) | Err(_) => {}
        }
        *stderr_copy.lock().unwrap() = buf;
    });
    let (tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    Running {
        child,
        watchdog,
        group,
        stdout: stdout_rx,
        stderr: stderr_text,
        deadline: setup.deadline,
    }
}

/// Drains `running` to its exit, asserting it left no process behind.
#[track_caller]
fn exit_cleanly(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
        deadline,
    } = running;
    loop {
        match deadline.recv(&stdout) {
            Ok(line) => {
                let done: Value = serde_json::from_str(&line).unwrap();
                if done["kind"] == "fiber_exited" {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for fiber_exited")
            }
        }
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => expired(deadline, group, &finished, "fiber to exit"),
    };
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(
        !fakes::group_alive(group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup_phase());
}

#[test]
fn agent_shell_timeout_stands_while_driver_shell_has_none() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        // A pending approval lets the loop drain shell deliveries without
        // ending the turn (docs/testing.md, "Waits and timeouts").
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "sleep 5", "timeout_ms": 1000}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "sleep"})
        ),
    )
    .unwrap();
    server.hold();
    // Unlike a headless ask, a session can wait for an approval.
    let session_id = "s_0000000000003891";
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let running = start(
        &setup,
        &[
            "session",
            "--id",
            session_id,
            "--workspace",
            &workspace,
            "--prompt",
            "go",
        ],
    );
    assert!(
        server.await_requests(1, running.deadline),
        "the held tool call was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(session_id));
    let sub = subscribe_direct(&client);
    // Deliver only the tool request; later provider replies remain held.
    server.release_one();
    let asked = until_after(&client, &sub, "the pending approval", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"].clone();

    // The person's `!sleep 5` travels the exact bytes the terminal sends
    // for it (`crates/tui/src/shell.rs`, `command()`). It runs while the
    // turn waits for the approval, exactly as in `driver_shell_sent.rs`.
    client.send(
        &json!({"id": "c_bang", "command": "shell",
            "args": {"command": "sleep 5", "send": true}})
        .to_string(),
    );
    let lines = until_after(&client, &sub, "the live shell delivery", |line| {
        line["kind"] == "shell_command" && line["payload"]["command"] == json!("sleep 5")
    });
    let logged = lines.last().unwrap();
    assert_eq!(logged["payload"]["command"], json!("sleep 5"));
    assert_eq!(logged["payload"]["process"]["exit_code"], 0);
    let output = logged["payload"]["output"].as_str().unwrap();
    assert!(
        output.ends_with("Exit code 0.\n"),
        "the driver output ends with its closing line: {output}"
    );

    let ack = recv_answer(&client, "c_bang", "the shell acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a sent shell answers with no result: {ack}"
    );

    // The agent call runs under its own 1 s timeout and fails there.
    client.send(
        &json!({"id": "c_allow", "command": "reply",
            "args": {"request_id": request, "decision": "allow"}})
        .to_string(),
    );
    let done = until_after(&client, &sub, "the agent shell timeout", |line| {
        line["kind"] == "tool_call_completed"
    });
    let completed = done.last().unwrap();
    assert_eq!(completed["payload"]["status"], "failed", "{completed}");
    assert_eq!(
        completed["payload"]["error"]["code"], "timeout",
        "{completed}"
    );
    assert_eq!(
        completed["payload"]["process"]["timed_out"], true,
        "{completed}"
    );
    let agent_text: String = completed["payload"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        agent_text.contains("Timed out after 1000 ms"),
        "the agent timeout names its bound: {agent_text}"
    );

    server.release();
    until_after(&client, &sub, "the turn completed", |line| {
        line["kind"] == "turn_completed"
    });
    // The connection is already subscribed (`subscribe_direct` above: a
    // second subscribe is rejected as already-subscribed), so close on it
    // directly and drain to the exit.
    client.send(r#"{"id":"c_close","command":"close"}"#);
    until_close(&client);
    drop(client);
    exit_cleanly(running);
}

/// The scripted turn behind the pty test: one instant reply, so the
/// session idles with an empty draft.
fn script(setup: &Setup) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["ready"]}]}),
    );
}

/// An attached idle session: started with `go`, settled on its closing
/// line, the draft empty.
fn attached(setup: &Setup) -> Run {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the idle session", |grid| {
        grid.rows.iter().any(|row| row.contains("▣ completed"))
    });
    run
}

/// The first row holding `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

/// Quits the binary, successfully.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn bang_sleep_five_draws_the_sent_row() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!sleep 5\r");
    // The sent row: the command, the sent note, then the exit status, in
    // that order. `sleep` prints nothing, so no output row sits between
    // the note and the exit.
    let mut snapshot: Vec<String> = Vec::new();
    run.wait_screen("the sent shell row", |grid| {
        let rows = grid.rows.clone();
        let command = rows.iter().position(|row| row.contains("! sleep 5"));
        let note = rows
            .iter()
            .position(|row| row.contains("sent with your next prompt"));
        let exit = rows.iter().position(|row| row.contains("exit 0"));
        let ordered = match (command, note, exit) {
            (Some(command), Some(note), Some(exit)) => command < note && note < exit,
            _ => false,
        };
        let settled = !rows.iter().any(|row| row.trim_end().ends_with("shell"));
        let done = ordered && settled;
        if done {
            snapshot = rows;
        }
        done
    });
    let at =
        |needle: &str| find_row(&snapshot, needle).unwrap_or_else(|| panic!("the {needle} row"));
    let command = at("! sleep 5");
    let note = at("sent with your next prompt");
    let exit = at("exit 0");
    assert!(
        command < note && note < exit,
        "the row reads in order: {snapshot:?}"
    );
    assert!(
        !snapshot.iter().any(|row| row.trim_end().ends_with("shell")),
        "the marker left with the draft"
    );
    quit(run);
}
