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
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Running, SessionGuard, Setup, Socket, close_session, connect_hub, expired, function_call,
    group_alive, hello, recv, recv_answer, session_dir, start_session, stream, until,
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

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

/// The session id on the ask's first stdout line.
#[track_caller]
fn session_of(running: &Running) -> String {
    let first_raw: String = running
        .deadline
        .recv(&running.stdout)
        .expect("waited until the deadline for fiber_started");
    let started: Value = serde_json::from_str(&first_raw).unwrap();
    started["session_id"].as_str().unwrap().to_owned()
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
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup_phase());
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
        // The turn's first step returns at once: the sent shell already
        // ended while the provider was held, so every drain after release
        // takes the delivery.
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "true"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "true"})
        ),
    )
    .unwrap();
    server.hold();
    let running = start(&setup, &["ask", "go"]);
    let session_id = session_of(&running);
    assert!(
        server.await_requests(1, running.deadline),
        "the held tool call was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = recv(&client, "the subscribe acknowledgement");
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
    exit_cleanly(running);
}

#[test]
fn driver_shell_sent_ending_after_settle_answers_closing_and_logs_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "go"]);
    let session_id = session_of(&running);
    assert!(
        server.await_requests(1, running.deadline),
        "the held response was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");

    // The shell still blocks when the released turn ends and `close`
    // settles the loop: it ends after the settle. The marker runs first
    // in its command, so waiting for it proves the shell started before
    // `close` is sent and the settle path is the one under test.
    let marker = setup.workspace().join("shell.started");
    let shell = json!({
        "id": "c_shell",
        "command": "shell",
        "args": {"command": format!("touch {} && tail -f /dev/null", shell_quote(&marker)), "send": true},
    })
    .to_string();
    client.send(&shell);
    loop {
        if marker.exists() {
            break;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for the sent shell to start");
        }
        std::thread::yield_now();
    }
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
    exit_cleanly(running);

    let events = fs::read_to_string(session_dir(&setup, &session_id).join("events.jsonl")).unwrap();
    assert!(
        events
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .all(|line| line["kind"] != "shell_command"),
        "a shell ending after settle logs no shell_command line"
    );
}

/// Every user text of an openai-responses request body, in order.
fn request_texts(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "user")
        .flat_map(|item| match &item["content"] {
            Value::String(text) => vec![text.clone()],
            Value::Array(parts) => parts
                .iter()
                .filter(|part| part["type"] == "input_text")
                .filter_map(|part| part["text"].as_str().map(str::to_owned))
                .collect(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
        })
        .collect()
}

fn user_texts(server: &ProviderServer) -> Vec<Vec<String>> {
    server
        .requests()
        .iter()
        .map(|request| request_texts(&request.body))
        .collect()
}

/// Subscribes `direct` at `full`: the session socket takes no `session_id`.
#[track_caller]
fn subscribe_full(direct: &Socket) {
    direct.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let sub = recv(direct, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");
    assert_eq!(sub["payload"]["command_id"], "c_sub");
}

/// The hub's session over, the direct socket closed, the hub killed and
/// every session gone.
#[track_caller]
fn finish(
    setup: &Setup,
    hub: &Arc<Mutex<Option<support::HubProc>>>,
    session: &str,
    direct: Socket,
    guard: SessionGuard,
) {
    drop(direct);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    let closer = Socket::connect(setup.deadline, &setup.session_socket(session));
    close_session(&closer);
    drop(closer);
    guard.wait_gone();
}

#[test]
fn driver_shell_sent_joins_the_next_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "first");
    drop(client);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    subscribe_full(&direct);
    until(&direct, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    direct.send(r#"{"id":"c_shell","command":"shell","args":{"command":"echo MARK","send":true}}"#);
    let lines = until(&direct, "the shell_command line", |line| {
        line["kind"] == "shell_command"
    });
    let logged = lines.last().unwrap();
    assert_eq!(logged["payload"]["command"], "echo MARK");
    assert_eq!(logged["payload"]["output"], "MARK\nExit code 0.\n");
    let ack = recv_answer(&direct, "c_shell", "the shell acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a sent shell answers with no result: {ack}"
    );
    direct.send(
        r#"{"id":"c_p2","command":"prompt","args":{"content":[{"type":"text","text":"second"}]}}"#,
    );
    until(&direct, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let texts = user_texts(&server);
    assert_eq!(texts.len(), 2);
    assert!(
        !texts[0].iter().any(|text| text.contains("MARK")),
        "the first turn ended before the shell: {texts:?}"
    );
    assert!(
        texts[1].iter().any(|text| text.contains("MARK")),
        "the next turn reads the shell: {texts:?}"
    );
    finish(&setup, &hub, &session, direct, guard);
}

#[test]
fn driver_shell_sent_during_a_turn_joins_the_next_turn_only() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "first");
    drop(client);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    subscribe_full(&direct);
    assert!(
        server.await_requests(1, setup.deadline),
        "the held turn was requested"
    );
    let marker = setup.workspace().join("shell.done");
    let shell = json!({
        "id": "c_shell",
        "command": "shell",
        "args": {"command": format!("echo MARK && touch {}", shell_quote(&marker)), "send": true},
    })
    .to_string();
    direct.send(&shell);
    // The shell ends before the held turn is released, so its delivery is
    // waiting when the turn's drains run and the next prompt names it.
    loop {
        if marker.exists() {
            break;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited until the deadline for the sent shell to end");
        }
        std::thread::yield_now();
    }
    server.release();
    // The marker runs before process exit, the cut and the delivery:
    // the delivery can still be in flight when the first turn completes.
    // Wait for both the turn and the `shell_command` line, in either
    // order, so the next turn starts only after the loop has taken the
    // delivery and holds it.
    let mut first = until(&direct, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    if !first.iter().any(|line| line["kind"] == "shell_command") {
        let shell_lines = until(&direct, "the shell_command line", |line| {
            line["kind"] == "shell_command"
        });
        first.extend(shell_lines);
    }
    let texts = user_texts(&server);
    assert_eq!(texts.len(), 1);
    assert!(
        !texts[0].iter().any(|text| text.contains("MARK")),
        "the running turn never reads the shell: {texts:?}"
    );
    direct.send(
        r#"{"id":"c_p2","command":"prompt","args":{"content":[{"type":"text","text":"second"}]}}"#,
    );
    until(&direct, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let texts = user_texts(&server);
    assert_eq!(texts.len(), 2);
    assert!(
        texts[1].iter().any(|text| text.contains("MARK")),
        "the next turn reads the shell: {texts:?}"
    );
    finish(&setup, &hub, &session, direct, guard);
}
