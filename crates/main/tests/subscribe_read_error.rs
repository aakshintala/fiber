//! A full subscriber over a log with an unreadable line mid-page
//! (`docs/invocation.md`, `subscribe`): the client receives its
//! acknowledgement, every line before the bad one, then end of file, while
//! the session runs on and other connections are unaffected.

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
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::Value;
use support::*;

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

#[track_caller]
fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
        deadline,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match deadline.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            expired(deadline, group, &finished, "fiber to exit")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("fiber's wait thread ended before fiber exited")
        }
    };
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup());
}

/// Connects a raw socket to `path`, on a thread bounded by the deadline.
#[track_caller]
fn connect(deadline: Deadline, path: &std::path::Path) -> UnixStream {
    let target = path.to_owned();
    bounded(
        deadline,
        &format!("a connection to {}", path.display()),
        move || UnixStream::connect(target),
    )
    .expect("the socket accepted before the deadline")
}

/// Sends `line` (plus its newline) on the raw socket.
#[track_caller]
fn send(stream: &mut UnixStream, deadline: Deadline, line: &str, what: &str) {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    write_line(stream, deadline, &bytes, what).expect("writing the socket");
}

/// One raw line, newline included, or `None` at end of file.
fn recv(reader: &mut BufReader<UnixStream>, deadline: Deadline, what: &str) -> Option<String> {
    read_line(reader, deadline, what).expect("reading the socket")
}

#[test]
fn a_full_subscriber_gets_every_line_before_an_unreadable_one_then_end_of_file() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "hi"]);
    let started: Value = serde_json::from_str(
        &setup
            .deadline
            .recv(&running.stdout)
            .expect("waited until the deadline for session_started"),
    )
    .unwrap();
    let session_id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the held response was requested"
    );
    let log_path =
        log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(&session_id);
    let events = log_path.join("events.jsonl");

    // The observer subscribes `full` before anything is damaged, and reads
    // raw lines to the end of the test, keeping every line's bytes.
    let mut observed: Vec<String> = Vec::new();
    let obs_stream = connect(setup.deadline, &setup.session_socket(&session_id));
    let mut obs_write = obs_stream.try_clone().unwrap();
    let mut obs_read = BufReader::new(obs_stream);
    send(
        &mut obs_write,
        setup.deadline,
        r#"{"id":"c_obs_sub","command":"subscribe","args":{"level":"full"}}"#,
        "sending the observer subscribe",
    );
    let ack = recv(
        &mut obs_read,
        setup.deadline,
        "the observer subscribe acknowledgement",
    )
    .expect("the observer subscribe acknowledgement");
    let parsed: Value = serde_json::from_str(ack.trim_end()).unwrap();
    assert_eq!(parsed["kind"], "command_accepted", "{parsed}");
    assert_eq!(parsed["payload"]["command_id"], "c_obs_sub");
    observed.push(ack);

    let whole = fs::read(&events).unwrap();
    let file_lines: Vec<&[u8]> = whole.split_inclusive(|b| *b == b'\n').collect();
    assert!(
        file_lines.len() >= 3,
        "the log holds a page to corrupt: {whole:?}"
    );
    assert!(
        file_lines.len() < 1_024 && whole.len() < 1024 * 1024,
        "every line is on one page"
    );
    let bad = file_lines.len() / 2;
    assert!(bad > 0 && bad + 1 < file_lines.len());
    let mut start = 0;
    for line in file_lines.iter().take(bad) {
        start += line.len();
    }
    let damaged = file_lines[bad].to_vec();
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&vec![b'x'; damaged.len() - 1], start as u64)
        .unwrap();
    drop(file);
    let before: Vec<u8> = file_lines[..bad].concat();

    // A second socket subscribes `full` over the damaged log: accepted, then
    // every line before the bad one, then end of file.
    let bad_stream = connect(setup.deadline, &setup.session_socket(&session_id));
    let mut bad_write = bad_stream.try_clone().unwrap();
    let mut bad_read = BufReader::new(bad_stream);
    send(
        &mut bad_write,
        setup.deadline,
        r#"{"id":"c_bad_sub","command":"subscribe","args":{"level":"full"}}"#,
        "sending the damaged subscribe",
    );
    let ack = recv(
        &mut bad_read,
        setup.deadline,
        "the damaged subscribe acknowledgement",
    )
    .expect("the damaged subscribe acknowledgement");
    let ack: Value = serde_json::from_str(ack.trim_end()).unwrap();
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_bad_sub");
    let mut got = Vec::new();
    while let Some(line) = recv(&mut bad_read, setup.deadline, "a line before the bad one") {
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(
            parsed.get("seq").and_then(Value::as_u64).is_some(),
            "no unsequenced line arrives: {parsed}"
        );
        got.push(line);
    }
    let bytes: Vec<u8> = got.concat().as_bytes().to_vec();
    assert_eq!(bytes, before, "every line before the bad one arrives");

    // The session runs on and the observer is unaffected.
    send(
        &mut obs_write,
        setup.deadline,
        r#"{"id":"c_obs_tools","command":"tools"}"#,
        "sending the observer tools",
    );
    loop {
        let line = recv(&mut obs_read, setup.deadline, "the observer tools answer")
            .expect("the observer tools answer");
        observed.push(line);
        let parsed: Value = serde_json::from_str(observed.last().unwrap().trim_end()).unwrap();
        if parsed
            .get("payload")
            .and_then(|p| p.get("command_id"))
            .and_then(Value::as_str)
            == Some("c_obs_tools")
        {
            assert_eq!(parsed["kind"], "command_accepted", "{parsed}");
            break;
        }
    }

    // A read that fails partway keeps only whole lines: cut the file inside
    // the same line, after restoring its bytes first.
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&damaged, start as u64).unwrap();
    drop(file);
    let cut_at = start + damaged.len() / 2;
    let whole = fs::read(&events).unwrap();
    let cut = whole[cut_at..].to_vec();
    fs::OpenOptions::new()
        .write(true)
        .open(&events)
        .unwrap()
        .set_len(cut_at as u64)
        .unwrap();
    let cut_stream = connect(setup.deadline, &setup.session_socket(&session_id));
    let mut cut_write = cut_stream.try_clone().unwrap();
    let mut cut_read = BufReader::new(cut_stream);
    send(
        &mut cut_write,
        setup.deadline,
        r#"{"id":"c_cut_sub","command":"subscribe","args":{"level":"full"}}"#,
        "sending the cut subscribe",
    );
    let ack = recv(
        &mut cut_read,
        setup.deadline,
        "the cut subscribe acknowledgement",
    )
    .expect("the cut subscribe acknowledgement");
    let ack: Value = serde_json::from_str(ack.trim_end()).unwrap();
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let mut got = Vec::new();
    while let Some(line) = recv(&mut cut_read, setup.deadline, "a whole line before the cut") {
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(
            parsed.get("seq").and_then(Value::as_u64).is_some(),
            "no partial line arrives: {parsed}"
        );
        got.push(line);
    }
    assert_eq!(got.concat().as_bytes(), before.as_slice());
    // The file is byte for byte what it was before the damage.
    let mut file = fs::OpenOptions::new().append(true).open(&events).unwrap();
    std::io::Write::write_all(&mut file, &cut).unwrap();
    drop(file);
    assert_eq!(fs::read(&events).unwrap(), whole);

    server.release();
    loop {
        let line = recv(&mut obs_read, setup.deadline, "fiber_exited")
            .expect("the observer stays until fiber_exited");
        observed.push(line);
        let parsed: Value = serde_json::from_str(observed.last().unwrap().trim_end()).unwrap();
        if parsed.get("kind").and_then(Value::as_str) == Some("fiber_exited") {
            break;
        }
    }
    finish(running);
    let durable: Vec<u8> = observed
        .iter()
        .filter(|line| {
            serde_json::from_str::<Value>(line.trim_end())
                .map(|parsed| parsed.get("seq").and_then(Value::as_u64).is_some())
                .unwrap_or(false)
        })
        .flat_map(|line| line.as_bytes().to_vec())
        .collect();
    assert_eq!(durable, fs::read(&events).unwrap());
}
