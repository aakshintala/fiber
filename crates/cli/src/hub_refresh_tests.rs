//! `fiber hub refresh` against a fake hub socket: the two commands in
//! order, the printed line for each source, a rejected `refresh` and a
//! connection closed before the answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::HubLine;
use fakes::Deadline;
use serde_json::{Map, Value, json};

use super::run;

/// One named deadline per wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// The hub's answer to `refresh`: accepted with no `result`.
fn refreshed() -> Value {
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_refresh"}})
}

/// The hub's answer to `status` with `source`.
fn status(source: &str) -> Value {
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
    "payload": {"command_id": "c_status", "result": {
        "running": true, "fiber_version": "0.0.1", "clients": 1,
        "variables": {"path": "/usr/bin:/bin", "names": ["HOME"], "source": source},
    }}})
}

/// A fake hub that reads two commands and answers each in turn, sending
/// every command line it read on `got`.
fn fake_hub(answers: Vec<Value>) -> (mpsc::Receiver<String>, UnixStream) {
    let (client, hub) = UnixStream::pair().unwrap();
    let (sent, got) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(hub.try_clone().unwrap());
        for answer in &answers {
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            sent.send(line).unwrap_or(());
            let mut bytes = serde_json::to_vec(answer).unwrap();
            bytes.push(b'\n');
            (&hub).write_all(&bytes).unwrap();
        }
    });
    (got, client)
}

/// `connect` reaching `client`, with a `hub_hello` nobody reads.
fn connect(client: UnixStream) -> impl FnMut() -> Result<doors::hub::Hub, doors::Error> {
    let mut client = Some(client);
    move || {
        Ok((
            client.take().expect("one connection"),
            HubLine {
                kind: "hub_hello".to_owned(),
                ts: 1,
                schema_version: 1,
                payload: Map::new(),
            },
        ))
    }
}

/// The two command lines the hub read, in order: one deadline for the
/// whole wait, never one per line.
fn received(got: &mpsc::Receiver<String>) -> Vec<Value> {
    let wait = Deadline::after(DEADLINE);
    (0..2)
        .map(|_| {
            let line = wait.recv(got).unwrap();
            serde_json::from_str(&line).unwrap()
        })
        .collect()
}

/// `run` on a thread, with its result received under one deadline: a
/// blocking socket read never parks the test thread past the wait.
fn run_wait(client: UnixStream) -> (Result<(), contract::shapes::Failure>, Vec<u8>) {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let result = run(&mut out, &mut connect(client));
        done_tx.send((result, out)).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("run returns")
}

#[test]
fn a_login_shell_status_prints_where_sessions_start() {
    let (got, client) = fake_hub(vec![refreshed(), status("login_shell")]);
    let (result, out) = run_wait(client);
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "sessions now start with the login shell's environment\n"
    );
    assert_eq!(
        received(&got),
        [
            json!({"id": "c_refresh", "command": "refresh", "args": {}}),
            json!({"id": "c_status", "command": "status", "args": {}}),
        ]
    );
}

#[test]
fn an_inherited_status_prints_the_fallback() {
    let (got, client) = fake_hub(vec![refreshed(), status("inherited")]);
    let (result, out) = run_wait(client);
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "the login-shell capture failed; sessions start with the hub's inherited environment\n"
    );
    assert_eq!(received(&got).len(), 2);
}

#[test]
fn a_status_without_variables_is_a_failure() {
    let bare = json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
    "payload": {"command_id": "c_status", "result": {
        "running": true, "fiber_version": "0.0.1", "clients": 1,
    }}});
    let (_got, client) = fake_hub(vec![refreshed(), bare]);
    let (result, out) = run_wait(client);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(out.is_empty(), "nothing is printed without variables");
}

#[test]
fn a_status_with_an_unknown_source_is_a_failure() {
    let (_got, client) = fake_hub(vec![refreshed(), status("elsewhere")]);
    let (result, out) = run_wait(client);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(out.is_empty(), "nothing is printed for an unknown source");
}

#[test]
fn a_rejected_refresh_is_a_failure_with_its_code() {
    let rejected = json!({"kind": "command_rejected", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_refresh", "code": "busy", "message": "try later"}});
    let (got, client) = fake_hub(vec![rejected]);
    let (result, out) = run_wait(client);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert_eq!(error.message, "try later");
    assert!(out.is_empty());
    let line = Deadline::after(DEADLINE).recv(&got).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"id": "c_refresh", "command": "refresh", "args": {}}),
    );
}

#[test]
fn a_connection_closed_before_the_answer_is_a_failure() {
    let (client, hub) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut read = BufReader::new(hub);
        let mut line = String::new();
        read.read_line(&mut line).unwrap_or(0);
        // No answer: the connection closes here.
    });
    let (result, out) = run_wait(client);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(out.is_empty());
}

#[test]
fn hub_refresh_exits_zero_on_success_and_one_on_refusal() {
    if let Ok(case) = std::env::var("FIBER_CLI_HUB_REFRESH_CHILD") {
        let answers = match case.as_str() {
            "login" => vec![refreshed(), status("login_shell")],
            "inherited" => vec![refreshed(), status("inherited")],
            _ => vec![
                json!({"kind": "command_rejected", "ts": 1, "schema_version": 1,
                    "payload": {"command_id": "c_refresh", "code": "busy", "message": "try later"}}),
            ],
        };
        let (_got, client) = fake_hub(answers);
        std::process::exit(super::hub_refresh(&mut connect(client)));
    }
    let name = module_path!().split_once("::").unwrap().1;
    for (case, code, word) in [
        ("login", 0, "login shell"),
        ("inherited", 0, "inherited environment"),
        ("refused", 1, "try later"),
    ] {
        let output = fakes::rerun_prepared(
            &format!("{name}::hub_refresh_exits_zero_on_success_and_one_on_refusal"),
            &[("FIBER_CLI_HUB_REFRESH_CHILD", case)],
            Deadline::after(DEADLINE),
            |_| {},
        );
        assert_eq!(output.status.code(), Some(code), "{case}");
        let out = String::from_utf8(output.stdout).unwrap();
        let err = String::from_utf8(output.stderr).unwrap();
        if code == 0 {
            assert!(out.contains(word), "{case}: {out:?}");
            assert_eq!(err, "", "{case}");
        } else {
            assert!(!out.contains("sessions now start"), "{case}: {out:?}");
            assert!(!out.contains("inherited environment\n"), "{case}: {out:?}");
            assert!(err.contains("fiber: try later"), "{case}: {err:?}");
        }
    }
}
