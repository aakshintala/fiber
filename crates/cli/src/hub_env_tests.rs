//! The hub's captured placeholder values against a test listener playing
//! the hub on `run/hub`: the requested-names answer, the absent hub, the
//! refusal and the silent hub's one deadline, and the pure `lookup` merge.
//! Tests never touch the real home and never set the process environment.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{HUB_ENV_WITHIN, hub_env, hub_env_with, lookup};

/// One named deadline per blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// Placeholder variable names no test process sets: each test reads its
/// absence first and sets nothing.
const ABSENT_NAME: &str = "FIBER_TEST_HUB_ENV_ABSENT";
const GAP_NAME: &str = "FIBER_TEST_HUB_ENV_GAP";

fn home() -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new("cli-hub-env");
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("run")).unwrap();
    (dir, home)
}

fn hub_line(kind: &str, payload: Value) -> String {
    let line = json!({
        "kind": kind, "ts": 1,
        "schema_version": contract::SCHEMA_VERSION, "payload": payload,
    });
    format!("{line}\n")
}

fn accepted(id: &str, values: Value) -> String {
    hub_line(
        "command_accepted",
        json!({"command_id": id, "result": {"values": values}}),
    )
}

fn rejected(id: &str) -> String {
    hub_line(
        "command_rejected",
        json!({"command_id": id, "code": "invalid_arguments",
            "message": "The arguments do not fit this command."}),
    )
}

/// A hub on `home`'s `run/hub` for one client: it says `hub_hello`, reads
/// the one command line and sends it on, then runs `then` with the stream.
fn fake_hub(
    home: &Path,
    then: impl FnOnce(UnixStream) + Send + 'static,
) -> (Receiver<String>, JoinHandle<()>) {
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (sent, got) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(hub_line("hub_hello", json!({})).as_bytes())
            .unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        sent.send(line).unwrap_or(());
        then(stream);
    });
    (got, handle)
}

/// Holds the connection open until the client closes it.
fn hold(stream: &UnixStream) {
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut rest = String::new();
    BufReader::new(stream).read_line(&mut rest).unwrap_or(0);
}

#[test]
fn lookup_reads_the_hub_answer_first_and_holds_its_gaps() {
    let answered = Some(BTreeMap::from([(
        ABSENT_NAME.to_owned(),
        "hub-value".to_owned(),
    )]));
    let process = |name: &str| {
        assert!(
            std::env::var_os(name).is_none(),
            "the test process holds no {name}"
        );
        Some("process-value".to_owned())
    };
    assert_eq!(
        lookup(&answered, &process, ABSENT_NAME),
        Some("hub-value".to_owned())
    );
    // The hub answered without `GAP_NAME`: its absence stays a gap even
    // though the process environment holds a value.
    assert_eq!(lookup(&answered, &process, GAP_NAME), None);
    assert_eq!(
        lookup(&None, &process, GAP_NAME),
        Some("process-value".to_owned())
    );
}

#[test]
fn the_hub_answer_fills_a_value_the_client_lacks() {
    assert!(
        std::env::var_os(ABSENT_NAME).is_none(),
        "the test process lacks {ABSENT_NAME}"
    );
    let (_dir, home) = home();
    let (got, _hub) = fake_hub(&home, |mut stream| {
        stream
            .write_all(accepted("c_env", json!({ABSENT_NAME: "adb-1.example.test"})).as_bytes())
            .unwrap();
        hold(&stream);
    });
    let clock = FakeClock::new();
    let answered = fakes::within("the hub answer", DEADLINE, move || {
        hub_env(&home, &[ABSENT_NAME.to_owned()], &*clock)
    });
    assert_eq!(
        answered,
        Some(BTreeMap::from([(
            ABSENT_NAME.to_owned(),
            "adb-1.example.test".to_owned()
        )]))
    );
    let request: Value = serde_json::from_str(&Deadline::after(DEADLINE).recv(&got).unwrap())
        .unwrap();
    assert_eq!(
        request,
        json!({"id": "c_env", "command": "placeholder_env",
            "args": {"names": [ABSENT_NAME]}}),
    );
}

#[test]
fn an_absent_hub_answers_nothing() {
    let (_dir, home) = home();
    let clock = FakeClock::new();
    let answered = fakes::within("the absent hub", DEADLINE, move || {
        hub_env(&home, &[ABSENT_NAME.to_owned()], &*clock)
    });
    assert_eq!(answered, None);
}

#[test]
fn a_refusing_hub_answers_nothing() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        stream.write_all(rejected("c_env").as_bytes()).unwrap();
        hold(&stream);
    });
    let clock = FakeClock::new();
    let answered = fakes::within("the refusal", DEADLINE, move || {
        hub_env(&home, &[ABSENT_NAME.to_owned()], &*clock)
    });
    assert_eq!(answered, None);
}

#[test]
fn a_silent_hub_answers_nothing_at_the_one_deadline() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |stream| hold(&stream));
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (signalled, signal) = mpsc::channel();
    let (ids, id) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let worker_home = home.clone();
    let worker_clock = Arc::clone(&clock);
    thread::spawn(move || {
        ids.send(thread::current().id()).unwrap_or(());
        let mut before_wait = || signalled.send(()).unwrap_or(());
        done.send(hub_env_with(
            &worker_home,
            &[ABSENT_NAME.to_owned()],
            &*worker_clock,
            &mut || {},
            &mut before_wait,
        ))
        .unwrap_or(());
    });
    let id = Deadline::after(DEADLINE)
        .recv(&id)
        .expect("the hub query worker reports its thread");
    // The held answer parks at the one deadline; one parked advance to
    // exactly that instant expires it.
    Deadline::after(DEADLINE).recv(&signal).unwrap();
    let due = origin + HUB_ENV_WITHIN;
    let left = due.saturating_duration_since(clock.now());
    clock.advance_when_parked(Some(due), Some(id), left, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&result).unwrap(),
        None,
        "a silent hub falls back to the process environment"
    );
    assert_eq!(clock.now(), due);
}
