//! The person's driver command has no timeout while the agent's `shell`
//! keeps its own (`docs/testing.md`, "Levels"): a socket-level combined
//! proof on the built `fiber`, plus a literal pty test typing `!sleep 5`
//! under a 1 s agent timeout and asserting the sent row. The agent blocks
//! on a fifo nothing releases, so only its `timeout_ms: 1000` can end it;
//! the driver blocks on a fifo the test releases after that timeout fires,
//! then completes. Reuses the `fiber session` patterns of
//! `driver_shell_sent.rs`: the fixture, the held provider, one
//! [`Deadline`], no sleeps and no env vars; and the pty `Run`/`wait_screen`
//! sent-row pattern of `shell_look.rs`. `!sleep 5` sits below the 25-second
//! bare-wait refusal, so the guard is not in play for any test here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::pty::Run;
use support::{
    Running, Setup, Socket, TRUECOLOUR, expired, function_call, hello, stream, subscribe_direct,
    until_after, until_close, write_json,
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

/// Single-quotes `path` for a shell command, as in `driver_shell_sent.rs`.
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

#[test]
fn agent_shell_timeout_stands_while_driver_shell_has_none() {
    let setup = Setup::new();
    // Two blocked reads, no sleeps: the agent call blocks on a fifo nothing
    // ever releases, so only its 1 s timeout can end it; the driver call
    // blocks on a fifo the test holds open and releases after that timeout
    // fires (`docs/testing.md`, "Waits and timeouts").
    let agent_fifo = setup.workspace().join("agent-block");
    let driver_fifo = setup.workspace().join("driver-block");
    fakes::fifo(&agent_fifo);
    fakes::fifo(&driver_fifo);
    // Held open for reading and writing: the open never blocks, and the
    // held writer lets the driver's later reader open succeed at once, so
    // the test releases the driver call with a line whenever it chooses.
    // `read` returns after one line with no EOF, so the hold stays open
    // past the release (`docs/testing.md`, "Waits and timeouts").
    let mut driver_hold = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&driver_fifo)
        .unwrap();
    let agent_command = format!("cat {}", shell_quote(&agent_fifo));
    let driver_command = format!("read -r _ < {}", shell_quote(&driver_fifo));
    let server = ProviderServer::start([
        // A pending approval lets the loop drain shell deliveries without
        // ending the turn (`docs/testing.md`, "Waits and timeouts").
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": agent_command, "timeout_ms": 1000}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "cat"})
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

    // The person's command travels the exact shape the terminal sends
    // for it (`crates/tui/src/shell.rs`, `command()`). It runs while the
    // turn waits for the approval, exactly as in `driver_shell_sent.rs`.
    client.send(
        &json!({"id": "c_bang", "command": "shell",
            "args": {"command": driver_command.as_str(), "send": true}})
        .to_string(),
    );
    // Sent before the allow below, so the session admits it before the
    // agent call starts; it then blocks on its fifo, outliving the agent's
    // bound. Its answer arrives with its delivery, read in the compound
    // wait below.

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

    // The agent's bound fired while the driver call stayed blocked. Release
    // it with a line: `read` with exit 0 proves it read that line, so the
    // call completes on this release — admitted before the timeout,
    // completed after.
    writeln!(driver_hold, "go").unwrap();

    server.release();
    // One wait takes all three lines: each `until_after` consumes every
    // line it reads, so separate waits would eat each other's evidence.
    let mut delivered: Option<Value> = None;
    let mut ended = false;
    let mut answered: Option<Value> = None;
    until_after(
        &client,
        &sub,
        "the driver delivery, its answer and the turn end",
        |line| {
            if line["kind"] == "shell_command"
                && line["payload"]["command"].as_str() == Some(driver_command.as_str())
            {
                delivered = Some(line.clone());
            }
            if line["kind"] == "turn_completed" {
                ended = true;
            }
            if line["kind"] == "command_accepted"
                && line["payload"]["command_id"].as_str() == Some("c_bang")
            {
                answered = Some(line.clone());
            }
            delivered.is_some() && ended && answered.is_some()
        },
    );
    let logged = delivered.expect("the driver delivery arrives with the turn");
    assert_eq!(logged["payload"]["command"], json!(driver_command.as_str()));
    assert_eq!(logged["payload"]["process"]["exit_code"], 0);
    let output = logged["payload"]["output"].as_str().unwrap();
    assert!(
        output.ends_with("Exit code 0.\n"),
        "the driver output ends with its closing line: {output}"
    );

    let ack = answered.expect("the sent shell is answered with its delivery");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a sent shell answers with no result: {ack}"
    );

    // The connection is already subscribed (`subscribe_direct` above: a
    // second subscribe is rejected as already-subscribed), so close on it
    // directly and drain to the exit.
    client.send(r#"{"id":"c_close","command":"close"}"#);
    until_close(&client);
    drop(client);
    exit_cleanly(running);
}

/// The scripted turns behind the pty test: an instant reply idling the
/// fresh session; then one agent `shell` call with a 1 s timeout, blocked
/// on a fifo nothing releases, so only that timeout can end its turn; then
/// an instant reply ending it.
fn script(setup: &Setup, agent_command: &str) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["ready"]}, {"tool_calls": [
            {"name": "shell", "arguments": {"command": agent_command, "timeout_ms": 1000}},
        ]}, {"text": ["ready"]}]}),
    );
}

/// An attached session: the first frame's end proves the input reader
/// runs before any prompt goes out, so no later output gates what the test
/// types. Returns the run and the offset the turn finished title is waited
/// from.
fn attached(setup: &Setup) -> (Run, usize) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    // A standing allow for the agent's blocking call: it runs unattended,
    // and only its 1 s timeout can end it (`call_view.rs` pins home/rules
    // for an attached run).
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "cat"})
        ),
    )
    .unwrap();
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    let from = run.ready();
    (run, from)
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
    // The agent call blocks on a fifo nothing releases: only its 1 s
    // timeout can end its turn. The person's `!sleep 5` runs past it.
    let agent_fifo = setup.workspace().join("agent-block");
    fakes::fifo(&agent_fifo);
    script(&setup, &format!("cat {}", shell_quote(&agent_fifo)));
    let (mut run, from) = attached(&setup);
    // The first prompt idles the fresh session fast: its text step ends
    // the turn at once. `ready` in `attached` covers `go`, and the turn
    // finished title gates the person's command, never a grid match alone
    // (`docs/testing/screens.md`, "Screens").
    run.write(b"go\r");
    run.turn_finished(from);
    // The person's command runs while the prompt stays free, its output
    // going with the next prompt (`docs/tui.md`, "The input box").
    run.write(b"!sleep 5\r");
    // The next prompt starts the scripted turn while the person's command
    // runs. The offset follows the first turn's title, so the wait below
    // gates this turn's end, never the earlier one (`support/pty.rs`,
    // `Run::turn_finished`).
    let from = run.output().len();
    run.write(b"go\r");
    // The 1 s agent timeout ends the turn while the person's command runs
    // on past it.
    run.turn_finished(from);
    // The sent row: the command, the sent note and the exit status, as a
    // set. `docs/tui.md` "The input box" promises no order for these
    // rows, so none is asserted (`docs/testing.md`, "Event streams").
    // `sleep` prints nothing, so no output row joins the set.
    run.wait_screen("the sent shell row", |grid| {
        let rows = grid.rows.clone();
        let command = rows.iter().any(|row| row.contains("! sleep 5"));
        let note = rows
            .iter()
            .any(|row| row.contains("sent with your next prompt"));
        let exit = rows.iter().any(|row| row.contains("exit 0"));
        let settled = !rows.iter().any(|row| row.trim_end().ends_with("shell"));
        command && note && exit && settled
    });
    quit(run);
}
