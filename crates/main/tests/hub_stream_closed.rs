//! A subscriber through the hub over a log with an unreadable line
//! mid-page (`docs/invocation.md`, "What the hub speaks"): the client
//! receives its acknowledgement, every line before the bad one, then
//! `stream_closed`, and its next command for that session is rejected
//! `not_subscribed`, while another session on the same connection keeps
//! streaming.

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
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

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
    let status = match finished.recv_timeout(deadline.left()) {
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

/// The session id of a started `fiber ask`: its first stdout line.
fn session_of(running: &Running, what: &str) -> String {
    let started: Value = serde_json::from_str(
        &running
            .stdout
            .recv_timeout(running.deadline.left())
            .unwrap_or_else(|_| panic!("waited until the deadline for {what}")),
    )
    .unwrap();
    started["session_id"].as_str().unwrap().to_owned()
}

/// Whether `line` is a durable line of `session`.
fn is_durable_of(line: &Value, session: &str) -> bool {
    line.get("session_id").and_then(Value::as_str) == Some(session)
        && line.get("seq").and_then(Value::as_u64).is_some()
}

#[test]
fn a_hub_subscriber_gets_every_line_before_an_unreadable_one_then_stream_closed() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    // Both responses held, as `hold`, but with a cap past the test's own
    // deadline: the default cap would answer 500 while A still waits,
    // and its retries would stream through the open relay mid-test.
    server.hold_from(1, Duration::from_secs(300));
    let running_b = start(&setup, &["ask", "hi"]);
    let id_b = session_of(&running_b, "B's session_started");
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the held response was requested"
    );
    let running_a = start(&setup, &["ask", "hi"]);
    let id_a = session_of(&running_a, "A's session_started");
    assert!(
        server.await_requests(2, setup.deadline.left()),
        "both held responses were requested"
    );

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(&format!(
        "{{\"id\":\"c_sub_b\",\"session_id\":\"{id_b}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let sub_b = until(&client, "the B subscribe acknowledgement", |line| {
        line.get("payload")
            .and_then(|p| p.get("command_id"))
            .and_then(Value::as_str)
            == Some("c_sub_b")
            && (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
    });
    assert_eq!(
        sub_b.last().unwrap()["kind"],
        "command_accepted",
        "{sub_b:?}"
    );

    // Damage A's log mid-page, as a full subscriber over a damaged log
    // does: one line overwritten with `x`s, every line on one page.
    let log_path =
        log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(&id_a);
    let events = log_path.join("events.jsonl");
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
    let mut start_at = 0;
    for line in file_lines.iter().take(bad) {
        start_at += line.len();
    }
    let damaged = file_lines[bad].to_vec();
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&vec![b'x'; damaged.len() - 1], start_at as u64)
        .unwrap();
    drop(file);
    let before: Vec<Value> = file_lines[..bad]
        .iter()
        .map(|line| serde_json::from_slice(line).unwrap())
        .filter(|line: &Value| line.get("seq").and_then(Value::as_u64).is_some())
        .collect();

    // Through the hub, the client receives the acknowledgement, every
    // durable line before the bad one, then `stream_closed`.
    client.send(&format!(
        "{{\"id\":\"c_sub_a\",\"session_id\":\"{id_a}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let got = until(&client, "stream_closed for the closed session", |line| {
        line.get("kind").and_then(Value::as_str) == Some("stream_closed")
    });
    let closed = got.last().unwrap();
    assert_eq!(closed["kind"], "stream_closed", "{closed:?}");
    assert_eq!(
        closed["payload"]
            .as_object()
            .map(|payload| payload.keys().collect::<Vec<_>>()),
        Some(vec![&"session_id".to_owned()]),
        "payload holds session_id only: {closed}"
    );
    assert_eq!(closed["payload"]["session_id"], id_a, "{closed}");
    assert!(closed.get("seq").is_none(), "no seq: {closed}");
    assert!(
        closed.get("session_id").is_none(),
        "no envelope session_id: {closed}"
    );
    assert_eq!(
        got.iter()
            .filter(|line| line.get("kind").and_then(Value::as_str) == Some("stream_closed"))
            .count(),
        1,
        "one stream_closed: {got:?}"
    );
    let ack_a = got
        .iter()
        .find(|line| {
            line.get("payload")
                .and_then(|p| p.get("command_id"))
                .and_then(Value::as_str)
                == Some("c_sub_a")
        })
        .expect("the A subscribe acknowledgement");
    assert_eq!(ack_a["kind"], "command_accepted", "{ack_a}");
    let closed_at = got
        .iter()
        .position(|line| line["kind"] == "stream_closed")
        .unwrap();
    let ack_at = got.iter().position(|line| line == ack_a).unwrap();
    assert!(
        ack_at < closed_at,
        "the acknowledgement comes first: {got:?}"
    );
    let relayed: Vec<&Value> = got[..closed_at]
        .iter()
        .filter(|line| is_durable_of(line, &id_a))
        .collect();
    assert_eq!(
        relayed,
        before.iter().collect::<Vec<_>>(),
        "every durable line before the bad one arrives once, in order"
    );

    // Its next command for that session is rejected `not_subscribed`, with
    // no A line carrying `seq` between the close and the answer.
    client.send(&format!(
        "{{\"id\":\"c_tools_a\",\"session_id\":\"{id_a}\",\"command\":\"tools\"}}"
    ));
    let tools_got = until(&client, "the tools rejection", |line| {
        line.get("payload")
            .and_then(|p| p.get("command_id"))
            .and_then(Value::as_str)
            == Some("c_tools_a")
            && (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
    });
    let tools_ack = tools_got.last().unwrap();
    assert_eq!(tools_ack["kind"], "command_rejected", "{tools_ack}");
    assert_eq!(
        tools_ack["payload"]["code"], "not_subscribed",
        "{tools_ack}"
    );
    assert_eq!(
        tools_ack["payload"]["command_id"], "c_tools_a",
        "{tools_ack}"
    );
    assert!(
        tools_got.iter().all(|line| !is_durable_of(line, &id_a)),
        "no A line repeats after the close: {tools_got:?}"
    );

    // Restore the damaged line before either session runs on: a session
    // that reads its own damaged log fails its turn instead of streaming.
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&damaged, start_at as u64).unwrap();
    drop(file);
    assert_eq!(fs::read(&events).unwrap(), whole);
    // B keeps streaming on the same connection: its run ends with no
    // second `stream_closed` and no A line in the window. `fiber_exited`
    // carries no `session_id`; A's end reaches no relay, since only its
    // never-subscribed connection remains, so any end here is B's.
    // Both sessions are released together: `release_one` wakes every
    // waiter with no oldest-first order, while A streams invisibly
    // either way, since a connection that never subscribed has no writer.
    server.release();
    let b_got = until(&client, "B's fiber_exited", |line| {
        line.get("kind").and_then(Value::as_str) == Some("fiber_exited")
    });
    assert!(
        b_got
            .iter()
            .all(|line| line.get("kind").and_then(Value::as_str) != Some("stream_closed")),
        "no second stream_closed: {b_got:?}"
    );
    assert!(
        b_got.iter().all(|line| !is_durable_of(line, &id_a)),
        "no A line arrives in the window: {b_got:?}"
    );

    server.release();
    finish(running_a);
    finish(running_b);
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    guard.wait_gone();
}
