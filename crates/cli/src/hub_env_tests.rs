//! The hub's captured placeholder values against a test listener playing
//! the hub on `run/hub`: the requested-names answer, the absent hub, the
//! refusal, the silent hub's one deadline, the close after and before the
//! answer, the hard and the blocked write, the write that never drains,
//! the deadline's definition, and the pure `lookup` merge.
//! Tests never touch the real home and never set the process environment.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::{FakeClock, SystemClock};
use serde_json::{Value, json};

use super::{HUB_ENV_WITHIN, ask, hub_env, hub_env_with, lookup};

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
    let request: Value =
        serde_json::from_str(&Deadline::after(DEADLINE).recv(&got).unwrap()).unwrap();
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

#[test]
fn hub_env_within_is_the_connect_deadline_plus_ten_seconds() {
    assert_eq!(
        HUB_ENV_WITHIN,
        doors::hub::CONNECT_DEADLINE + Duration::from_secs(10),
    );
}

#[test]
fn a_close_after_the_answer_still_answers() {
    let (_dir, home) = home();
    let (got, _hub) = fake_hub(&home, |mut stream| {
        stream
            .write_all(accepted("c_env", json!({ABSENT_NAME: "adb-2.example.test"})).as_bytes())
            .unwrap();
        // Close: dropping the stream. The buffered answer stays readable,
        // so the exchange's answer wins over the restore.
    });
    let clock = FakeClock::new();
    let answered = fakes::within("the close after the answer", DEADLINE, move || {
        hub_env(&home, &[ABSENT_NAME.to_owned()], &*clock)
    });
    assert_eq!(
        answered,
        Some(BTreeMap::from([(
            ABSENT_NAME.to_owned(),
            "adb-2.example.test".to_owned()
        )]))
    );
    let request: Value = serde_json::from_str(
        &Deadline::after(DEADLINE).recv_or_fail(&got, "the hub's copy of the request"),
    )
    .unwrap();
    assert_eq!(
        request,
        json!({"id": "c_env", "command": "placeholder_env",
            "args": {"names": [ABSENT_NAME]}}),
    );
}

#[test]
fn a_close_before_any_answer_answers_nothing() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |_stream| {
        // Answer nothing: dropping the stream closes it.
    });
    let clock = FakeClock::new();
    let answered = fakes::within("the close before any answer", DEADLINE, move || {
        hub_env(&home, &[ABSENT_NAME.to_owned()], &*clock)
    });
    assert_eq!(answered, None);
}

#[test]
fn a_hard_write_error_answers_nothing_without_waiting() {
    let (client, _peer) = UnixStream::pair().unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let clock = FakeClock::new();
    let mut before_wait = || panic!("a refused write never waits for the deadline");
    let answered = ask(
        &client,
        clock.origin() + HUB_ENV_WITHIN,
        &[ABSENT_NAME.to_owned()],
        &*clock,
        &mut || {},
        &mut before_wait,
    );
    assert_eq!(answered, None);
}

/// Placeholder variable names numerous enough that one request overflows
/// the socket buffers, so the write waits until the hub reads.
const BIG_COUNT: usize = 30_000;

fn big_names() -> Vec<String> {
    (0..BIG_COUNT)
        .map(|each| format!("FIBER_TEST_HUB_ENV_BIG_{each:06}"))
        .collect()
}

/// What the blocked-write worker reports: each wait, then its answer.
enum WriteBlocked {
    Waited,
    Done(Option<BTreeMap<String, String>>),
}

#[test]
fn a_blocked_write_still_delivers_once_the_hub_reads() {
    let (_dir, home) = home();
    let names = big_names();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (proceed, wait) = mpsc::channel::<()>();
    let (request_done, request) = mpsc::channel::<String>();
    let answer_wire = accepted("c_env", json!({ABSENT_NAME: "big-1.example.test"}));
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(hub_line("hub_hello", json!({})).as_bytes())
            .unwrap();
        // Hold the request unread until the client has blocked in its
        // write: its send buffer fills, so the first wait below is the
        // write path, never the read path.
        wait.recv().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        request_done.send(line).unwrap_or(());
        stream.write_all(answer_wire.as_bytes()).unwrap();
        hold(&stream);
    });
    let (events_sent, events) = mpsc::channel();
    let worker_home = home.clone();
    thread::spawn(move || {
        let mut before_wait = || events_sent.send(WriteBlocked::Waited).unwrap_or(());
        let answered = hub_env_with(
            &worker_home,
            &names,
            &SystemClock,
            &mut || {},
            &mut before_wait,
        );
        events_sent.send(WriteBlocked::Done(answered)).unwrap_or(());
    });
    // One deadline for the whole wait: the worker's answer, which the
    // release and the hub's read stand between.
    let wait = Deadline::after(DEADLINE);
    let mut blocked = false;
    let answered = loop {
        match wait.recv_or_fail(&events, "the blocked write, then its answer") {
            WriteBlocked::Waited => {
                if !blocked {
                    blocked = true;
                    proceed.send(()).unwrap_or(());
                }
            }
            WriteBlocked::Done(answered) => break answered,
        }
    };
    assert!(
        blocked,
        "the write filled its buffer and waited for the hub to read"
    );
    assert_eq!(
        answered,
        Some(BTreeMap::from([(
            ABSENT_NAME.to_owned(),
            "big-1.example.test".to_owned()
        )]))
    );
    let line: Value = serde_json::from_str(
        &Deadline::after(DEADLINE).recv_or_fail(&request, "the hub's copy of the request"),
    )
    .unwrap();
    assert_eq!(
        line["args"]["names"].as_array().unwrap().len(),
        BIG_COUNT,
        "the whole request arrived after the waits"
    );
}

#[test]
fn a_write_that_never_drains_answers_nothing_at_the_one_deadline() {
    let (_dir, home) = home();
    let names = big_names();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (release, released) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(hub_line("hub_hello", json!({})).as_bytes())
            .unwrap();
        // Never read: the client's send buffer fills and its write waits
        // out the one deadline.
        released.recv().unwrap_or(());
    });
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
            &names,
            &*worker_clock,
            &mut || {},
            &mut before_wait,
        ))
        .unwrap_or(());
    });
    let id = Deadline::after(DEADLINE).recv_or_fail(&id, "the hub query worker thread");
    // The held write parks at the one deadline; one parked advance to
    // exactly that instant expires it.
    Deadline::after(DEADLINE).recv_or_fail(&signal, "the write's wait for the deadline");
    let due = origin + HUB_ENV_WITHIN;
    let left = due.saturating_duration_since(clock.now());
    clock.advance_when_parked(Some(due), Some(id), left, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE).recv_or_fail(&result, "the write's answer"),
        None,
        "a hub that never reads falls back to the process environment"
    );
    assert_eq!(clock.now(), due);
    drop(release);
}
