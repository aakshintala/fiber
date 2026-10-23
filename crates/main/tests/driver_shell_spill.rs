//! A driver shell past 1 MiB returns its complete artifact
//! (`docs/testing.md`, "Levels"): the built `fiber` runs `fiber ask` in its
//! own process group with its own `FIBER_HOME`, holding the ordinary hello
//! reply while the test drives a `!!` shell through the session socket.

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

use contract::events::CommandResult;
use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{HELLO_KINDS, Running, Setup, Socket, answered, expired, hello, session_dir, until};

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

#[test]
fn driver_shell_past_a_mebibyte_returns_its_complete_artifact() {
    const SIZE: usize = 1_048_577;
    const HEAD_KEPT: usize = 8192;
    const TAIL_KEPT: usize = 8192;
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
    fs::write(&script, format!("cat {}\n", shell_quote(&input))).unwrap();

    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "go"]);
    let first_raw: String = setup
        .deadline
        .recv(&running.stdout)
        .expect("waited until the deadline for fiber_started");
    let started: Value = serde_json::from_str(&first_raw).unwrap();
    let session_id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, setup.deadline),
        "the held response was requested"
    );

    let client = Socket::connect(setup.deadline, &setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"summary"}}"#);
    let sub = support::recv(&client, "the subscribe acknowledgement");
    assert_eq!(sub["kind"], "command_accepted", "{sub}");
    assert_eq!(sub["payload"]["command_id"], "c_sub");

    let shell_command = format!("/bin/sh {}", shell_quote(&script));
    let line = json!({
        "id": "c_big",
        "command": "shell",
        "args": {"command": shell_command, "send": false},
    })
    .to_string();
    client.send(&line);
    let waited = until(&client, "the shell acknowledgement", |line| {
        line["payload"].get("command_id") == Some(&json!("c_big"))
    });
    let ack = answered(&waited, "c_big");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let result: CommandResult = serde_json::from_value(ack["payload"]["result"].clone()).unwrap();
    let CommandResult::Shell {
        output,
        artifact,
        process,
    } = result
    else {
        panic!("the driver shell answers with a shell result: {result:?}")
    };
    assert_eq!(process.exit_code, Some(0));
    assert_eq!(process.signal, None);
    assert!(!process.timed_out);

    let mut file_bytes = fixture.clone();
    file_bytes.extend_from_slice(b"\nExit code 0.\n");
    let head = String::from_utf8(file_bytes[..HEAD_KEPT].to_vec()).unwrap();
    let tail = String::from_utf8(file_bytes[file_bytes.len() - TAIL_KEPT..].to_vec()).unwrap();
    let removed = u64::try_from(file_bytes.len() - HEAD_KEPT - TAIL_KEPT).unwrap();
    let dir = session_dir(&setup, &session_id);
    let absolute = dir.join(artifact.as_deref().unwrap_or("<none>"));
    let notice = format!(
        "[{removed} bytes cut. The full output is in {}; read it with `read`.]",
        absolute.display()
    );
    let expected = format!("{head}\n{notice}\n{tail}");
    assert_eq!(
        output.len(),
        expected.len(),
        "the bounded response keeps the head, the notice and the tail"
    );
    let first_diff = output
        .bytes()
        .zip(expected.bytes())
        .position(|(got, want)| got != want);
    assert_eq!(
        first_diff, None,
        "the bounded response is the shaped head, notice and tail"
    );
    assert!(
        output.ends_with("Exit code 0.\n"),
        "the response ends with its closing line ({} bytes)",
        output.len()
    );

    let artifact = artifact.expect("a spilled driver shell names its artifact");
    assert!(
        artifact.starts_with("artifacts/s_"),
        "the original spill file, not a second cut: {artifact}"
    );
    assert!(
        !artifact.starts_with("artifacts/o_"),
        "no second artifact from the driver command: {artifact}"
    );
    let saved = fs::read(dir.join(&artifact)).unwrap();
    assert_eq!(
        saved.len(),
        file_bytes.len(),
        "the artifact holds every byte"
    );
    let first_diff = saved
        .iter()
        .zip(file_bytes.iter())
        .position(|(got, want)| got != want);
    assert_eq!(
        first_diff, None,
        "the artifact is the fixture plus its closing"
    );
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

    server.release();
    drop(client);

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
        !fakes::group_alive(group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup_phase());

    let lines: Vec<Value> = raw
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<&str> = lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, HELLO_KINDS);
    assert!(
        lines.iter().all(|line| line["kind"] != "shell_command"),
        "the driver adds no shell_command event"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line["payload"].get("command_id") == Some(&json!("c_big"))),
        "the shell acknowledgement is only on the sending socket"
    );
    let stdout_durable: String = raw
        .iter()
        .zip(lines.iter())
        .filter(|(_, line)| line.get("seq").is_some())
        .map(|(line, _)| format!("{line}\n"))
        .collect();
    let log_text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert_eq!(stdout_durable, log_text);
    assert!(
        !log_text.contains("c_big"),
        "the log holds no acknowledgement of the driver command"
    );
}
