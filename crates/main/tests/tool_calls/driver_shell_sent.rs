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

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    Running, SessionGuard, Setup, Socket, close_session, close_subscribed_session, connect_hub,
    expired, function_call, hello, recv_answer, session_dir, start_session, stream,
    subscribe_direct, until, until_after, until_replayed,
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

/// A session held at a pending tool approval. The loop is not waiting on a
/// model response then, so it takes a sent shell's start at once
/// (`docs/architecture.md`, "One inbox").
struct Approval {
    server: ProviderServer,
    running: Running,
    client: Socket,
    sub: support::Subscribed,
    request: Value,
}

/// Starts `session_id` with a model that asks for a shell call needing
/// approval, answers `later` after it, and returns once the approval is
/// pending.
#[track_caller]
fn pending_approval(setup: &Setup, session_id: &str, later: Vec<Response>) -> Approval {
    let mut script = vec![stream(&[function_call(
        "call_1",
        "shell",
        &json!({"command": "true"}),
    )])];
    script.extend(later);
    let server = ProviderServer::start(script).unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "true"})
        ),
    )
    .unwrap();
    server.hold();
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let running = start(
        setup,
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
    Approval {
        server,
        running,
        client,
        sub,
        request,
    }
}

/// A named pipe in the workspace, held open for reading and writing so
/// the open never blocks and a line written to it releases one `read`.
fn hold(setup: &Setup, name: &str) -> fs::File {
    let path = setup.workspace().join(name);
    fakes::fifo(&path);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// Releases one `read -r _ < hold`.
fn release(hold: &mut fs::File) {
    writeln!(hold, "go").unwrap();
}

/// Prints `early-4177`, waits for `hold-4177`, then prints `late-4177`.
const HELD_OUTPUT: &str = "printf 'early-4177\\n'; read -r _ < hold-4177; printf 'late-4177\\n'";

fn shell_line(id: &str, command: &str, send: bool) -> String {
    json!({"id": id, "command": "shell", "args": {"command": command, "send": send}}).to_string()
}

/// The accepted or rejected answer to command `id`. A logged line of the
/// command carries the same `command_id`, so the kind matters.
#[track_caller]
fn answer_to(client: &Socket, id: &str) -> Value {
    until(client, "the command's answer", |line| {
        matches!(
            line["kind"].as_str(),
            Some("command_accepted" | "command_rejected")
        ) && line["payload"]["command_id"] == id
    })
    .pop()
    .unwrap()
}

/// A `full` subscription on a second connection: command ids are the
/// session's, so it names its own.
#[track_caller]
fn subscribe_second(client: &Socket) -> support::Subscribed {
    client.send(r#"{"id":"c_sub_late","command":"subscribe","args":{"level":"full"}}"#);
    let ack = answer_to(client, "c_sub_late");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    support::Subscribed::from_ack(None, ack)
}

/// Whether `line` is a delta of command `id` whose text holds `text`.
fn delta_with(line: &Value, id: &str, text: &str) -> bool {
    line["kind"] == "shell_command_delta"
        && line["payload"]["command_id"] == id
        && line["payload"]["text"]
            .as_str()
            .is_some_and(|got| got.contains(text))
}

/// Whether `line` is the logged end of command `id`.
fn end_of(line: &Value, id: &str) -> bool {
    line["kind"] == "shell_command" && line["payload"]["command_id"] == id
}

/// An idle session after its first turn, with a client subscribed to it.
struct Idle {
    _server: ProviderServer,
    hub: Arc<Mutex<Option<support::HubProc>>>,
    session: String,
    direct: Socket,
    guard: SessionGuard,
    sub: support::Subscribed,
}

#[track_caller]
fn idle_session(setup: &Setup) -> Idle {
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "first");
    drop(client);
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    let sub = subscribe_direct(&direct);
    until(&direct, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    Idle {
        _server: server,
        hub,
        session,
        direct,
        guard,
        sub,
    }
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
    let script = setup.workspace().join("print.sh");
    fs::write(&script, format!("cat {}\n", fakes::shell_quote(&input))).unwrap();

    let server = ProviderServer::start([
        // A pending approval lets the loop drain shell deliveries without
        // ending the turn (`docs/testing.md`, "Waits and timeouts").
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
            json!({"decision": "ask", "tool": "shell", "prefix": "true"})
        ),
    )
    .unwrap();
    server.hold();
    // Unlike a headless ask, a session can wait for an approval.
    let session_id = "s_0000000000003890";
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
    let sub = support::subscribe_direct(&client);
    // Deliver only the tool request; later provider replies remain held.
    server.release_one();
    let asked = support::until_after(&client, &sub, "the pending approval", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"].clone();

    let shell_command = format!("/bin/sh {}", fakes::shell_quote(&script));
    let line = json!({
        "id": "c_big",
        "command": "shell",
        "args": {"command": shell_command, "send": true},
    })
    .to_string();
    client.send(&line);
    // A live shell_command proves the output was drained, shaped and
    // delivered. A replay or the script's exit cannot release the turn.
    let lines = support::until_after(&client, &sub, "the live shell delivery", |line| {
        line["kind"] == "shell_command" && line["payload"]["command"] == shell_command
    });
    server.release();
    client.send(
        &json!({"id":"c_approve", "command":"reply",
            "args":{"request_id":request, "decision":"allow"}})
        .to_string(),
    );
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

    let dir = session_dir(&setup, session_id);
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

    close_subscribed_session(&client);
    drop(client);
    exit_cleanly(running);
}

#[test]
fn driver_shell_sent_ending_after_settle_answers_closing_and_logs_nothing() {
    let setup = Setup::new();
    let session_id = "s_0000000000004177";
    let Approval {
        server,
        running,
        client,
        ..
    } = pending_approval(&setup, session_id, vec![hello()]);

    // The shell still blocks when `close` settles the loop: it ends after
    // the settle. The marker runs first in its command, so waiting for it
    // proves the shell started before `close` is sent and the settle path
    // is the one under test.
    let marker = setup.workspace().join("shell.started");
    client.send(&shell_line(
        "c_shell",
        &format!("touch {} && tail -f /dev/null", fakes::shell_quote(&marker)),
        true,
    ));
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
    let answer = answer_to(&client, "c_shell");
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "closing");
    assert_eq!(
        answer["payload"]["message"],
        "The session ended before answering."
    );
    drop(client);
    exit_cleanly(running);

    let events = fs::read_to_string(session_dir(&setup, session_id).join("events.jsonl")).unwrap();
    let logged: Vec<Value> = events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let starts: Vec<&Value> = logged
        .iter()
        .filter(|line| line["kind"] == "shell_command_started")
        .collect();
    assert_eq!(starts.len(), 1, "the start was written before the spawn");
    assert_eq!(starts[0]["payload"]["command_id"], "c_shell");
    assert!(
        logged.iter().all(|line| line["kind"] != "shell_command"),
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
    subscribe_direct(&direct);
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
    let Approval {
        server,
        running,
        client,
        sub,
        request,
    } = pending_approval(&setup, "s_0000000000004178", vec![hello(), hello()]);
    let marker = setup.workspace().join("shell.done");
    client.send(&shell_line(
        "c_shell",
        &format!("echo MARK && touch {}", fakes::shell_quote(&marker)),
        true,
    ));
    // The shell ends while the approval is pending, so its delivery is
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
    // The marker runs before process exit, the cut and the delivery: wait
    // for the logged end, so the loop holds it before the turn goes on.
    until_after(&client, &sub, "the shell_command line", |line| {
        line["kind"] == "shell_command"
    });
    server.release();
    client.send(
        &json!({"id":"c_approve", "command":"reply",
            "args":{"request_id":request, "decision":"allow"}})
        .to_string(),
    );
    until_after(&client, &sub, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let texts = user_texts(&server);
    assert_eq!(texts.len(), 2);
    for request in &texts {
        assert!(
            !request.iter().any(|text| text.contains("MARK")),
            "the running turn never reads the shell: {texts:?}"
        );
    }
    client.send(
        r#"{"id":"c_p2","command":"prompt","args":{"content":[{"type":"text","text":"second"}]}}"#,
    );
    until_after(&client, &sub, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let texts = user_texts(&server);
    assert_eq!(texts.len(), 3);
    assert!(
        texts[2].iter().any(|text| text.contains("MARK")),
        "the next turn reads the shell: {texts:?}"
    );
    close_subscribed_session(&client);
    drop(client);
    exit_cleanly(running);
}

#[test]
fn driver_shell_sent_logs_its_start_then_streams_its_output_then_its_end() {
    let setup = Setup::new();
    let Idle {
        _server,
        hub,
        session,
        direct,
        guard,
        sub,
    } = idle_session(&setup);
    let mut gate = hold(&setup, "hold-4177");
    direct.send(&shell_line("c_stream", HELD_OUTPUT, true));
    let early = until_after(&direct, &sub, "the early output", |line| {
        delta_with(line, "c_stream", "early-4177")
    });
    // Order: the start comes before any delta and before its end
    // (`docs/events.md`, "`shell_command_started`": before any of its
    // `shell_command_delta` lines and before its `shell_command`).
    let started = early
        .iter()
        .position(|line| line["kind"] == "shell_command_started")
        .expect("the start line came before the output");
    assert!(early[started]["seq"].is_number(), "the start is durable");
    assert_eq!(early[started]["payload"]["command_id"], "c_stream");
    assert_eq!(early[started]["payload"]["command"], HELD_OUTPUT);
    let delta = early.last().unwrap();
    assert!(
        delta.get("seq").is_none_or(Value::is_null),
        "a delta is ephemeral: {delta}"
    );
    assert!(
        !early.iter().any(|line| end_of(line, "c_stream")),
        "the command is still running"
    );

    release(&mut gate);
    let ended = until_after(&direct, &sub, "the end of the command", |line| {
        end_of(line, "c_stream")
    });
    // Order: the rest of the output streams before the end (`docs/events.md`,
    // "`shell_command_started`": the start is before its `shell_command`; the
    // deltas stream while it runs).
    assert!(
        ended
            .iter()
            .any(|line| delta_with(line, "c_stream", "late-4177")),
        "the rest of the output streamed before the end"
    );
    let logged = ended.last().unwrap();
    assert!(logged["seq"].is_number());
    assert_eq!(
        logged["payload"]["output"],
        "early-4177\nlate-4177\nExit code 0.\n"
    );
    assert_eq!(logged["payload"]["process"]["exit_code"], 0);
    let ack = answer_to(&direct, "c_stream");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    finish(&setup, &hub, &session, direct, guard);
}

#[test]
fn driver_shell_show_only_streams_to_its_sender_and_logs_no_start() {
    let setup = Setup::new();
    let Idle {
        _server,
        hub,
        session,
        direct,
        guard,
        ..
    } = idle_session(&setup);
    let other = Socket::connect(setup.deadline, &setup.session_socket(&session));
    let other_sub = subscribe_second(&other);
    let mut gate = hold(&setup, "hold-4177");
    direct.send(&shell_line("c_show", HELD_OUTPUT, false));
    let mut sender_saw = until(&direct, "the early output", |line| {
        delta_with(line, "c_show", "early-4177")
    });
    release(&mut gate);
    sender_saw.extend(until(&direct, "the answer", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_show"
    }));
    assert!(
        sender_saw
            .iter()
            .any(|line| delta_with(line, "c_show", "late-4177")),
        "the sender saw the rest before its answer: {sender_saw:?}"
    );

    // A sent command is logged for every client, so its end bounds what
    // the other client has been sent by now.
    direct.send(&shell_line("c_mark", "echo MARK", true));
    let seen = until_after(&other, &other_sub, "the marker command's end", |line| {
        end_of(line, "c_mark")
    });
    assert!(
        !seen
            .iter()
            .any(|line| line["payload"]["command_id"] == "c_show"),
        "another client sees nothing of a show-only command: {seen:?}"
    );

    let events = fs::read_to_string(session_dir(&setup, &session).join("events.jsonl")).unwrap();
    let starts: Vec<Value> = events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["kind"] == "shell_command_started")
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "only the sent command is logged: {starts:?}"
    );
    assert_eq!(starts[0]["payload"]["command_id"], "c_mark");
    drop(other);
    finish(&setup, &hub, &session, direct, guard);
}

#[test]
fn driver_shell_sent_late_attach_sees_the_row_then_output_then_the_full_output() {
    let setup = Setup::new();
    let Idle {
        _server,
        hub,
        session,
        direct,
        guard,
        sub,
    } = idle_session(&setup);
    let mut gate = hold(&setup, "hold-4177");
    direct.send(&shell_line("c_late", HELD_OUTPUT, true));
    until_after(&direct, &sub, "the early output", |line| {
        delta_with(line, "c_late", "early-4177")
    });

    let late = Socket::connect(setup.deadline, &setup.session_socket(&session));
    let late_sub = subscribe_second(&late);
    let replayed = until_replayed(&late, &late_sub, "the running row", |line| {
        line["kind"] == "shell_command_started" && line["payload"]["command_id"] == "c_late"
    });
    assert!(
        !replayed.iter().any(|line| end_of(line, "c_late")),
        "the replay holds the start and no end: {replayed:?}"
    );

    release(&mut gate);
    let streamed = until_after(&late, &late_sub, "the output after attach", |line| {
        delta_with(line, "c_late", "late-4177")
    });
    assert!(
        !streamed.iter().any(|line| end_of(line, "c_late")),
        "the output streams before the end"
    );
    let ended = until_after(&late, &late_sub, "the end of the command", |line| {
        end_of(line, "c_late")
    });
    assert_eq!(
        ended.last().unwrap()["payload"]["output"],
        "early-4177\nlate-4177\nExit code 0.\n",
        "the end holds the whole output, including what streamed before attach"
    );
    drop(late);
    finish(&setup, &hub, &session, direct, guard);
}
