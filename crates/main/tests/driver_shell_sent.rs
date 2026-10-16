//! A sent driver shell (`!`, `send` true) through the built `fiber`
//! (`docs/testing.md`, "Levels"): the line it logs, and a shell still
//! running at settle. Reuses the `fiber ask` patterns of
//! `driver_shell_spill.rs`: the fixture, the ask process group, the held
//! provider, one [`Deadline`], no sleeps and no env vars.

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
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, Setup, Socket, expired, function_call, group_alive, hello, recv_answer, session_dir,
    stream, until,
};

/// `fiber ask` with its stdout kept drained and its stderr kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    stdout: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
    deadline: Deadline,
}

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

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

/// The session id on the ask's first stdout line.
#[track_caller]
fn session_of(running: &Running) -> (String, String) {
    let first_raw: String = running
        .deadline
        .recv(&running.stdout)
        .expect("waited until the deadline for fiber_started");
    let started: Value = serde_json::from_str(&first_raw).unwrap();
    (
        started["session_id"].as_str().unwrap().to_owned(),
        first_raw,
    )
}

/// Drains `running` to its exit, asserting it left no process behind.
#[track_caller]
fn exit_cleanly(running: Running, first_raw: String) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
        deadline,
    } = running;
    let mut raw = vec![first_raw];
    loop {
        match deadline.recv(&stdout) {
            Ok(line) => {
                let done: Value = serde_json::from_str(&line).unwrap();
                let exited = done["kind"] == "fiber_exited";
                raw.push(line);
                if exited {
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
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup());
}

#[test]
fn driver_shell_sent_past_a_mebibyte_logs_its_complete_artifact() {
    const SIZE: usize = 1_048_577;
    let setup = Setup::new();
    let mut fixture = vec![b'x'; SIZE];
    fixture[0] = b'A';
    fixture[500_000] = 0xFF;
    for (at, byte) in "héllo".as_bytes().iter().enumerate() {
        fixture[500_001 + at] = *byte;
    }
    fixture[SIZE - 1] = b'Z';
    assert_ne!(fixture[SIZE - 1], b'\n');
    let input = setup.workspace().join("input.bin");
    fs::write(&input, &fixture).unwrap();
    let marker = setup.workspace().join("shell.done");
    let script = setup.workspace().join("print.sh");
    fs::write(
        &script,
        format!(
            "cat {} && touch {}\n",
            shell_quote(&input),
            shell_quote(&marker)
        ),
    )
    .unwrap();

    let server = ProviderServer::start([
        // The turn's first step runs `sleep 1`: the sent shell ends while
        // the step runs, so every drain after it takes the delivery.
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "sleep 1"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "sleep 1"})
        ),
    )
    .unwrap();
    server.hold();
    let running = start(&setup, &["ask", "go"]);
    let (session_id, first_raw) = session_of(&running);
    assert!(
        server.await_requests(1, running.deadline.left()),
        "the held tool call was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = support::recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");
    assert_eq!(sub["payload"]["command_id"], "c_sub");

    let shell_command = format!("/bin/sh {}", shell_quote(&script));
    let line = json!({
        "id": "c_big",
        "command": "shell",
        "args": {"command": shell_command, "send": true},
    })
    .to_string();
    client.send(&line);
    // The shell ends before the held turn is released: the marker runs
    // last in its command, so the delivery is waiting when the drain runs.
    loop {
        if marker.exists() {
            break;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for the sent shell to end");
        }
        std::thread::yield_now();
    }
    // The loop takes the inbox at a step boundary: release the held turn
    // so the drain runs, then wait for the line it writes.
    server.release();
    let lines = until(&client, "the shell_command line", |line| {
        line["kind"] == "shell_command"
    });
    let logged = lines.last().unwrap();
    assert_eq!(logged["payload"]["command"], shell_command.as_str());
    assert_eq!(logged["payload"]["process"]["exit_code"], 0);
    let artifact = logged["payload"]["artifact"]
        .as_str()
        .expect("a sent shell past the cap names its artifact");
    assert!(
        artifact.starts_with("artifacts/s_"),
        "the original spill file, not a second cut: {artifact}"
    );
    let mut file_bytes = fixture.clone();
    file_bytes.extend_from_slice(b"\nExit code 0.\n");
    let output = logged["payload"]["output"].as_str().unwrap();
    assert!(
        output.contains("bytes cut"),
        "the logged output is the cut shape: {}",
        output.len()
    );
    assert!(
        output.ends_with("Exit code 0.\n"),
        "the logged output ends with its closing line"
    );

    let ack = recv_answer(&client, "c_big", "the shell acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a sent shell answers with no result: {ack}"
    );

    let dir = session_dir(&setup, &session_id);
    let saved = fs::read(dir.join(artifact)).unwrap();
    assert_eq!(saved, file_bytes, "the artifact holds every byte");
    assert_eq!(saved[0], b'A');
    assert_eq!(saved[500_000], 0xFF);
    assert_eq!(saved[SIZE - 1], b'Z');
    let mut entries: Vec<String> = fs::read_dir(dir.join("artifacts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries.len(),
        1,
        "one artifact, no second write: {entries:?}"
    );
    assert_eq!(
        entries[0],
        artifact.trim_start_matches("artifacts/"),
        "the only artifact is the driver's"
    );

    drop(client);
    exit_cleanly(running, first_raw);
}

#[test]
fn driver_shell_sent_ending_after_settle_answers_closing_and_logs_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "go"]);
    let (session_id, first_raw) = session_of(&running);
    assert!(
        server.await_requests(1, running.deadline.left()),
        "the held response was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = support::recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");

    // The shell still blocks when the released turn ends and `close`
    // settles the loop: it ends after the settle.
    client.send(
        r#"{"id":"c_shell","command":"shell","args":{"command":"tail -f /dev/null","send":true}}"#,
    );
    client.send(r#"{"id":"c_close","command":"close"}"#);
    server.release();
    let answer = recv_answer(&client, "c_shell", "the shell answer after settle");
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "closing");
    assert_eq!(
        answer["payload"]["message"],
        "The session ended before answering."
    );
    drop(client);
    exit_cleanly(running, first_raw);

    let events = fs::read_to_string(session_dir(&setup, &session_id).join("events.jsonl")).unwrap();
    assert!(
        events
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .all(|line| line["kind"] != "shell_command"),
        "a shell ending after settle logs no shell_command line"
    );
}
