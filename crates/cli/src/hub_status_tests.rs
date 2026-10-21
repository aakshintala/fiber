//! `fiber hub status` against a test listener playing the hub on
//! `run/hub`: not running, the answer and its deadline, the port, the
//! devices, the installed flag, and both renderings.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{
    Diagnostic, STATUS_WITHIN, Status, diagnose, diagnose_with, installed, probe, probe_with,
    render_json, render_text,
};
use crate::hub_unit::{Manager, name};

/// One named deadline per wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// The probe's own deadline when the test needs it to pass.
const ANSWER: Duration = Duration::from_secs(5);

fn home() -> (fakes::TempDir, PathBuf) {
    let dir = fakes::TempDir::new("cli-hub-status");
    let home = dir.path().join("home");
    fs::create_dir_all(home.join("run")).unwrap();
    (dir, home)
}

fn hub_line(kind: &str, payload: Value) -> String {
    let line = json!({
        "kind": kind, "ts": 1,
        "schema_version": contract::SCHEMA_VERSION, "payload": payload,
    });
    format!("{line}\n")
}

fn accepted(id: &str, version: &str, clients: u64) -> String {
    hub_line(
        "command_accepted",
        json!({"command_id": id, "result": {
            "running": true, "fiber_version": version, "clients": clients,
        }}),
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

#[track_caller]
fn probed(home: &Path, within: Duration) -> Result<Status, Failure> {
    let home = home.to_path_buf();
    let clock = FakeClock::new();
    fakes::within("the probe", Deadline::after(DEADLINE), move || {
        probe(&home, &*clock, within, false)
    })
}

#[test]
fn no_socket_is_not_running_and_starts_no_hub() {
    let (_dir, home) = home();
    let status = probed(&home, ANSWER).unwrap();
    assert_eq!(
        status,
        Status {
            running: false,
            version: None,
            port: None,
            clients: 0,
            devices: Vec::new(),
            installed: false,
        }
    );
    assert!(!home.join("run/hub").exists());
}

#[test]
fn a_stale_socket_is_not_running() {
    let (_dir, home) = home();
    drop(UnixListener::bind(home.join("run/hub")).unwrap());
    assert!(!probed(&home, ANSWER).unwrap().running);
}

#[test]
fn a_hub_that_does_not_say_hub_hello_is_a_failure() {
    let (_dir, home) = home();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let _hub = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"not json\n").unwrap();
        hold(&stream);
    });
    let error = probed(&home, ANSWER).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
}

#[test]
fn a_running_hub_reports_its_version_and_the_other_clients() {
    let (_dir, home) = home();
    let (got, _hub) = fake_hub(&home, |mut stream| {
        stream
            .write_all(accepted("c_status", "1.2.3", 3).as_bytes())
            .unwrap();
        hold(&stream);
    });
    let status = probed(&home, ANSWER).unwrap();
    assert!(status.running);
    assert_eq!(status.version.as_deref(), Some("1.2.3"));
    assert_eq!(status.clients, 2);
    let request: Value =
        serde_json::from_str(&Deadline::after(DEADLINE).recv(&got).unwrap()).unwrap();
    assert_eq!(
        request,
        json!({"id": "c_status", "command": "status", "args": {}})
    );
}

#[test]
fn a_hub_counting_no_clients_reports_none() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        stream
            .write_all(accepted("c_status", "1.2.3", 0).as_bytes())
            .unwrap();
        hold(&stream);
    });
    assert_eq!(probed(&home, ANSWER).unwrap().clients, 0);
}

#[test]
fn a_silent_hub_fails_at_the_deadline() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |stream| hold(&stream));
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (read, result, id) = probe_signalled(&home, &clock);
    // The held answer parks at the one deadline; one parked advance to
    // exactly that instant expires it.
    let wait = Deadline::after(DEADLINE);
    wait.recv(&read).unwrap();
    let due = origin + ANSWER;
    clock.advance_when_parked(Some(due), Some(id), ANSWER, &Deadline::after(DEADLINE));
    let error = Deadline::after(DEADLINE)
        .recv(&result)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "the hub did not answer status in 5 s");
    assert_eq!(clock.now(), due);
}

#[test]
fn an_eof_retry_does_not_extend_the_total_deadline() {
    let (_dir, home) = home();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (wrote_tx, wrote_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (second_tx, second_rx) = mpsc::channel::<()>();
    // First connection trickles one byte, then closes once the test lets
    // it: the EOF retry. The second connection stays silent.
    thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        first.write_all(b"{").unwrap();
        wrote_tx.send(()).unwrap_or(());
        go_rx.recv().unwrap_or(());
        drop(first);
        let (silent, _) = listener.accept().unwrap();
        second_tx.send(()).unwrap_or(());
        hold(&silent);
    });
    let (done_tx, done_rx) = mpsc::channel();
    let (id_tx, id_rx) = mpsc::channel();
    let worker_home = home.clone();
    let worker_clock = Arc::clone(&clock);
    thread::spawn(move || {
        id_tx.send(thread::current().id()).unwrap_or(());
        done_tx
            .send(probe_with(
                &worker_home,
                &*worker_clock,
                ANSWER,
                false,
                &mut || {},
                &mut || {},
            ))
            .unwrap_or(());
    });
    let id = Deadline::after(DEADLINE)
        .recv(&id_rx)
        .expect("the probe worker reports its thread");
    // Past the race and onto the silent retry: its wait parks at the one
    // shared deadline, which one parked advance reaches exactly.
    Deadline::after(DEADLINE).recv(&wrote_rx).unwrap();
    go_tx.send(()).unwrap();
    Deadline::after(DEADLINE).recv(&second_rx).unwrap();
    let due = origin + ANSWER;
    // The race pause moved the clock once; the advance lands exactly on
    // the shared deadline, never past it.
    let left = due.saturating_duration_since(clock.now());
    clock.advance_when_parked(Some(due), Some(id), left, &Deadline::after(DEADLINE));
    // The retry's silent peer gets the shared deadline, not a fresh 5 s:
    // the whole probe fails naming the hello wait, at the one instant.
    let error = Deadline::after(Duration::from_secs(2))
        .recv(&done_rx)
        .expect("the probe fails at its total deadline, not a fresh one")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains("did not say hub_hello in 5 s"),
        "the retry names the shared hello wait: {}",
        error.message
    );
    assert_eq!(clock.now(), due);
}

/// Runs `probe_with` on a worker thread, handing back its read signals,
/// its answer, and its thread id, so the test steps the fake clock
/// through `advance_when_parked` naming that thread and its expected park
/// instant.
fn probe_signalled(
    home: &Path,
    clock: &Arc<FakeClock>,
) -> (
    Receiver<()>,
    Receiver<Result<Status, Failure>>,
    thread::ThreadId,
) {
    let (reads, read) = mpsc::channel();
    let (ids, id) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let home = home.to_path_buf();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        ids.send(thread::current().id()).unwrap_or(());
        let mut before_read = || reads.send(()).unwrap_or(());
        done.send(probe_with(
            &home,
            &*clock,
            ANSWER,
            false,
            &mut before_read,
            &mut || {},
        ))
        .unwrap_or(());
    });
    let id = Deadline::after(DEADLINE)
        .recv(&id)
        .expect("the probe worker reports its thread");
    (read, result, id)
}

/// A hub that writes `first`, waits for the test's go, then writes
/// `second`.
fn two_part_hub(home: &Path, first: String, second: String) -> Sender<()> {
    let (go, wait) = mpsc::channel::<()>();
    let (_got, _hub) = fake_hub(home, move |mut stream| {
        stream.write_all(first.as_bytes()).unwrap();
        wait.recv().unwrap();
        stream.write_all(second.as_bytes()).unwrap();
        hold(&stream);
    });
    go
}

#[test]
fn unrelated_lines_past_the_deadline_end_the_probe() {
    let (_dir, home) = home();
    let other = accepted("c_other", "9.9.9", 1);
    let (_got, _hub) = fake_hub(&home, move |mut stream| {
        stream.write_all(other.as_bytes()).unwrap();
        stream.write_all(other.as_bytes()).unwrap();
        hold(&stream);
    });
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (read, result, id) = probe_signalled(&home, &clock);
    // The worker reads past both unrelated lines, then parks with no
    // answer: one parked advance to exactly the deadline ends the probe.
    let wait = Deadline::after(DEADLINE);
    wait.recv(&read).unwrap();
    let due = origin + ANSWER;
    let left = due.saturating_duration_since(clock.now());
    clock.advance_when_parked(Some(due), Some(id), left, &Deadline::after(DEADLINE));
    let error = Deadline::after(DEADLINE)
        .recv(&result)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "the hub did not answer status in 5 s");
    assert_eq!(clock.now(), due);
}

#[test]
fn an_answer_in_two_writes_inside_the_deadline_is_read() {
    let (_dir, home) = home();
    let answer = accepted("c_status", "1.2.3", 1);
    let (first, second) = answer.split_at(answer.len() / 2);
    let go = two_part_hub(&home, first.to_owned(), second.to_owned());
    let clock = FakeClock::new();
    let (read, result, _) = probe_signalled(&home, &clock);
    let wait = Deadline::after(DEADLINE);
    wait.recv(&read).unwrap();
    wait.recv(&read).unwrap();
    go.send(()).unwrap();
    let status = Deadline::after(DEADLINE).recv(&result).unwrap().unwrap();
    assert_eq!(status.version.as_deref(), Some("1.2.3"));
}

#[test]
fn a_full_send_buffer_waits_for_room_instead_of_failing() {
    // The request fits one small write, so a full send buffer is the only
    // way the write waits: shrink the buffer, flood it, then ask. The
    // write's first block signals room is needed; draining exactly the
    // flood's bytes and answering frees it, and the worker's real-poll
    // slices retry the write without any fake time moving.
    let (mut worker_side, mut peer) = UnixStream::pair().unwrap();
    rustix::net::sockopt::set_socket_send_buffer_size(worker_side.as_fd(), 4096).unwrap();
    worker_side.set_nonblocking(true).unwrap();
    let flood = [0x70u8; 65536];
    let mut flooded = 0;
    loop {
        match worker_side.write(&flood) {
            Ok(wrote) => flooded += wrote,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("flooding the send buffer: {error}"),
        }
    }
    // A large write can refuse outright instead of filling the last
    // room, so top up byte by byte: the loop ends only on a blocked
    // probe, which makes the worker's write below the first thing that
    // can block.
    loop {
        match worker_side.write(&[0x70]) {
            Ok(wrote) => flooded += wrote,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("topping up the send buffer: {error}"),
        }
    }
    assert!(flooded > 0, "the flood filled the send buffer");
    peer.set_nonblocking(true).unwrap();
    // Before the worker can exit: a read timeout set after its end
    // closes is refused, so it goes on while the pair is whole. It only
    // bounds the final request read below.
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let clock = Arc::new(FakeClock::new());
    let origin = clock.origin();
    let due = origin + ANSWER;
    let (waits_tx, waits) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker_clock = Arc::clone(&clock);
    thread::spawn(move || {
        let mut before_read = || {};
        let mut before_wait = || waits_tx.send(()).unwrap_or(());
        let worker: &FakeClock = &worker_clock;
        done_tx
            .send(super::ask(
                &worker_side,
                due,
                ANSWER,
                worker,
                &mut before_read,
                &mut before_wait,
            ))
            .unwrap_or(());
    });
    // The write waits for room: drain exactly the flood's bytes, then
    // answer on the freed connection before the clock moves. An
    // until-empty drain would race the worker's resumed write and eat
    // the status request; the flood's count stops short of it, since
    // the request lands after every flood byte.
    Deadline::after(DEADLINE)
        .recv(&waits)
        .expect("the write waits for room");
    let mut drained = 0;
    let mut piece = [0u8; 65536];
    while drained < flooded {
        let cap = (flooded - drained).min(piece.len());
        match peer.read(&mut piece[..cap]) {
            Ok(0) => panic!("the peer closed with flood bytes still queued"),
            Ok(read) => drained += read,
            Err(error) => panic!("draining the flood: {error}"),
        }
    }
    peer.write_all(accepted("c_status", "9.9.9", 7).as_bytes())
        .unwrap();
    let reply = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the answer arrives")
        .expect("a reply");
    assert_eq!(reply.version, "9.9.9");
    assert_eq!(reply.clients, 7);
    // The request went out whole once the buffer had room.
    peer.set_nonblocking(false).unwrap();
    let mut line = String::new();
    BufReader::new(&peer).read_line(&mut line).unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        request,
        json!({"id": "c_status", "command": "status", "args": {}})
    );
}

/// Whether `stream` is in blocking mode, read from the socket itself.
fn is_blocking(stream: &UnixStream) -> bool {
    !rustix::fs::fcntl_getfl(stream)
        .unwrap()
        .contains(rustix::fs::OFlags::NONBLOCK)
}

#[test]
fn ask_restores_blocking_after_a_reply() {
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(accepted("c_status", "1.2.3", 1).as_bytes())
        .unwrap();
    assert!(is_blocking(&stream), "the stream arrives blocking");
    let clock = FakeClock::new();
    let within = ANSWER;
    fakes::within("the ask", Deadline::after(DEADLINE), move || {
        let reply = super::ask(
            &stream,
            clock.now() + within,
            within,
            &*clock,
            &mut || {},
            &mut || {},
        )
        .unwrap();
        assert_eq!(reply.version, "1.2.3");
        assert!(is_blocking(&stream), "a reply leaves the stream blocking");
    });
}

#[test]
fn ask_restores_blocking_after_a_close_before_the_answer() {
    let (stream, peer) = UnixStream::pair().unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(is_blocking(&stream), "the stream arrives blocking");
    let clock = FakeClock::new();
    let within = ANSWER;
    fakes::within("the ask", Deadline::after(DEADLINE), move || {
        let Err(error) = super::ask(
            &stream,
            clock.now() + within,
            within,
            &*clock,
            &mut || {},
            &mut || {},
        ) else {
            panic!("a close before the answer is an error");
        };
        assert!(error.message.contains("closed"), "{}", error.message);
        assert!(
            is_blocking(&stream),
            "a close before the answer leaves the stream blocking"
        );
    });
}

#[test]
fn ask_restores_blocking_after_the_deadline() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(is_blocking(&stream), "the stream arrives blocking");
    let clock = FakeClock::new();
    let within = Duration::ZERO;
    fakes::within("the ask", Deadline::after(DEADLINE), move || {
        let Err(error) = super::ask(
            &stream,
            clock.now() + within,
            within,
            &*clock,
            &mut || {},
            &mut || {},
        ) else {
            panic!("a deadline is an error");
        };
        assert!(
            error.message.contains("did not answer"),
            "{}",
            error.message
        );
        assert!(
            is_blocking(&stream),
            "a deadline leaves the stream blocking"
        );
    });
}

#[test]
fn an_answer_followed_by_a_close_is_read() {
    // The one test through `ask` reaching the closed-peer `InvalidInput` side (macOS only; Linux never refuses).
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        stream
            .write_all(accepted("c_status", "1.2.3", 1).as_bytes())
            .unwrap();
    });
    let status = probed(&home, ANSWER).unwrap();
    assert_eq!(status.version.as_deref(), Some("1.2.3"));
}

#[test]
fn a_close_before_the_answer_is_a_failure() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, drop);
    let error = probed(&home, ANSWER).unwrap_err();
    assert!(error.message.contains("closed"), "{}", error.message);
}

#[test]
fn a_rejected_status_is_a_failure_with_its_code() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        let rejected = hub_line(
            "command_rejected",
            json!({"command_id": "c_status", "code": "invalid_arguments", "message": "nope"}),
        );
        stream.write_all(rejected.as_bytes()).unwrap();
        hold(&stream);
    });
    let error = probed(&home, ANSWER).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(error.message, "nope");
}

#[test]
fn lines_for_other_commands_are_skipped() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        let lines = [
            "not json\n".to_owned(),
            accepted("c_other", "9.9.9", 7),
            hub_line(
                "command_rejected",
                json!({"command_id": "c_other", "code": "busy", "message": "no"}),
            ),
            accepted("c_status", "1.2.3", 2),
        ]
        .concat();
        stream.write_all(lines.as_bytes()).unwrap();
        hold(&stream);
    });
    let status = probed(&home, ANSWER).unwrap();
    assert_eq!(status.version.as_deref(), Some("1.2.3"));
    assert_eq!(status.clients, 1);
}

#[test]
fn an_answer_missing_its_fields_is_a_failure() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |mut stream| {
        let bare = hub_line(
            "command_accepted",
            json!({"command_id": "c_status", "result": {"running": true}}),
        );
        stream.write_all(bare.as_bytes()).unwrap();
        hold(&stream);
    });
    assert_eq!(probed(&home, ANSWER).unwrap_err().code, ErrorCode::IoFailed);
}

#[test]
fn devices_are_the_sorted_regular_files() {
    let (_dir, home) = home();
    let devices = home.join("hub/devices");
    fs::create_dir_all(devices.join("folder")).unwrap();
    fs::write(devices.join("phone"), "{}").unwrap();
    fs::write(devices.join("laptop"), "{}").unwrap();
    assert_eq!(probed(&home, ANSWER).unwrap().devices, ["laptop", "phone"]);
}

#[test]
fn the_port_is_the_global_hub_port_only() {
    let (_dir, home) = home();
    assert_eq!(probed(&home, ANSWER).unwrap().port, None);
    fs::write(home.join("config.json"), r#"{"hub": {"port": 4040}}"#).unwrap();
    let project = home.join("projects/-w");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("config.json"), r#"{"hub": {"port": 5050}}"#).unwrap();
    assert_eq!(probed(&home, ANSWER).unwrap().port, Some(4040));
}

#[test]
fn a_hub_port_that_is_not_a_port_is_invalid_config() {
    let (_dir, home) = home();
    fs::write(home.join("config.json"), r#"{"hub": {"port": 70000}}"#).unwrap();
    assert_eq!(
        probed(&home, ANSWER).unwrap_err().code,
        ErrorCode::ConfigInvalid
    );
}

#[test]
fn installed_is_whether_the_unit_file_exists() {
    let (dir, home) = home();
    let user = dir.path().join("user");
    let var = {
        let user = user.clone();
        move |key: &str| (key == "HOME").then(|| OsString::from(&user))
    };
    for manager in [Manager::Launchd { uid: 501 }, Manager::Systemd] {
        assert!(!installed(manager, &home, &var).unwrap());
    }
    let name = name(&home).unwrap();
    let plist = user.join("Library/LaunchAgents");
    fs::create_dir_all(&plist).unwrap();
    fs::write(plist.join(format!("{name}.plist")), "x").unwrap();
    assert!(installed(Manager::Launchd { uid: 501 }, &home, &var).unwrap());
    let unit = user.join(".config/systemd/user");
    fs::create_dir_all(&unit).unwrap();
    fs::write(unit.join(format!("{name}.service")), "x").unwrap();
    assert!(installed(Manager::Systemd, &home, &var).unwrap());
}

#[test]
fn the_installed_flag_reaches_the_status() {
    let (_dir, home) = home();
    let clock = FakeClock::new();
    let status = fakes::within("the probe", Deadline::after(DEADLINE), move || {
        probe(&home, &*clock, ANSWER, true)
    })
    .unwrap();
    assert!(status.installed);
}

fn running() -> Status {
    Status {
        running: true,
        version: Some("0.0.1".to_owned()),
        port: Some(4040),
        clients: 1,
        devices: vec!["laptop".to_owned(), "phone".to_owned()],
        installed: true,
    }
}

fn stopped() -> Status {
    Status {
        running: false,
        version: None,
        port: None,
        clients: 0,
        devices: Vec::new(),
        installed: false,
    }
}

#[test]
fn the_text_names_each_field() {
    assert_eq!(
        render_text(&running()),
        "running: yes\nversion: 0.0.1\nport: 4040\nclients: 1\ndevices: laptop, phone\ninstalled: yes"
    );
    assert_eq!(
        render_text(&stopped()),
        "running: no\nversion: none\nport: none\nclients: 0\ndevices: none\ninstalled: no"
    );
}

#[test]
fn the_json_is_one_line_in_field_order() {
    assert_eq!(
        render_json(&running()).unwrap(),
        r#"{"running":true,"version":"0.0.1","port":4040,"clients":1,"devices":["laptop","phone"],"installed":true}"#
    );
    assert_eq!(
        render_json(&stopped()).unwrap(),
        r#"{"running":false,"version":null,"port":null,"clients":0,"devices":[],"installed":false}"#
    );
}

#[test]
fn devices_is_empty_when_absent_and_an_error_when_not_a_dir() {
    let (_dir, home) = home();
    let probed = home.clone();
    let empty = fakes::within("the probe", Deadline::after(DEADLINE), move || {
        super::devices(&probed).unwrap()
    });
    assert!(empty.is_empty());
    // `hub/devices` as a file fails with `NotADirectory`, not `NotFound`.
    fs::create_dir_all(home.join("hub")).unwrap();
    fs::write(home.join("hub/devices"), b"x").unwrap();
    let error = super::devices(&home).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
}

/// The child's marker: set, the test runs `hub_status` and exits with its
/// code, without touching the service manager: no hub runs, so nothing is
/// started and no manager is called.
const CHILD: &str = "FIBER_CLI_HUB_STATUS_CHILD";

/// Which rendering the child asks for.
const CASE: &str = "FIBER_CLI_HUB_STATUS_CASE";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(10);

/// Runs the child case `case` with `vars` in its environment and returns
/// its exit code.
#[track_caller]
fn child_exit(test: &str, case: &str, vars: &[(&str, &Path)]) -> i32 {
    let output = fakes::rerun_prepared(
        &format!("{test}::hub_status_exits_without_the_manager"),
        &[(CHILD, "1"), (CASE, case)],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env_remove("XDG_CONFIG_HOME");
            for (key, value) in vars {
                command.env(key, value);
            }
        },
    );
    output.status.code().expect("the child exits with a code")
}

#[test]
fn hub_status_exits_without_the_manager() {
    if std::env::var_os(CHILD).is_some() {
        let clock = FakeClock::new();
        let code = match std::env::var(CASE).unwrap().as_str() {
            "text" => super::hub_status(false, &*clock),
            "json" => super::hub_status(true, &*clock),
            case => panic!("unknown hub status child case: {case}"),
        };
        std::process::exit(code);
    }
    let name = module_path!().split_once("::").unwrap().1;
    // A relative `FIBER_HOME` is a usage error before anything runs.
    assert_eq!(
        child_exit(name, "text", &[("FIBER_HOME", Path::new("relative"))]),
        2
    );
    let dir = fakes::TempDir::new("cli-hub-status-exit");
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let user = dir.path().join("user");
    fs::create_dir_all(&user).unwrap();
    // With no hub running the status is 0, in text and in JSON.
    assert_eq!(
        child_exit(
            name,
            "text",
            &[("FIBER_HOME", home.as_path()), ("HOME", user.as_path())]
        ),
        0
    );
    assert_eq!(
        child_exit(
            name,
            "json",
            &[("FIBER_HOME", home.as_path()), ("HOME", user.as_path())]
        ),
        0
    );
}

/// A hub that answers `status` six seconds in: it says `hub_hello`, reads
/// the one command, writes one unrelated line, then waits (bounded) for
/// the test's release before writing the answer. The unrelated line and
/// the answer arrive in separate reads, which the release guarantees.
fn slow_hub(home: &Path, answer: String) -> Sender<()> {
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (go, wait) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(hub_line("hub_hello", json!({})).as_bytes())
            .unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let other = accepted("c_other", "9.9.9", 7);
        stream.write_all(other.as_bytes()).unwrap();
        // Bounded, so the 5-second case ends its thread without the
        // release the late probe never sends.
        Deadline::after(DEADLINE).recv(&wait).unwrap_or(());
        stream.write_all(answer.as_bytes()).unwrap_or(());
        hold(&stream);
    });
    go
}

#[test]
fn the_status_window_is_the_connect_deadline_plus_the_capture_limit() {
    // 5 s connect + 10 s capture (`docs/invocation.md`, "A session's
    // environment"): the sum, not the product.
    assert_eq!(STATUS_WITHIN, Duration::from_secs(15));
}

#[test]
fn a_hub_answering_six_seconds_in_is_read_within_the_status_window() {
    for (within, reads) in [(STATUS_WITHIN, true), (Duration::from_secs(5), false)] {
        let (_dir, home) = home();
        let go = slow_hub(&home, accepted("c_status", "1.2.3", 1));
        let clock: Arc<FakeClock> = FakeClock::new();
        let origin = clock.origin();
        let (done_tx, done_rx) = mpsc::channel();
        let (id_tx, id_rx) = mpsc::channel();
        let (read_tx, read_rx) = mpsc::channel::<()>();
        let worker_home = home.clone();
        let worker_clock = Arc::clone(&clock);
        thread::spawn(move || {
            id_tx.send(thread::current().id()).unwrap_or(());
            let mut before_read = || read_tx.send(()).unwrap_or(());
            done_tx
                .send(probe_with(
                    &worker_home,
                    &*worker_clock,
                    within,
                    false,
                    &mut before_read,
                    &mut || {},
                ))
                .unwrap_or(());
        });
        let id = Deadline::after(DEADLINE)
            .recv(&id_rx)
            .expect("the probe worker reports its thread");
        let wait = Deadline::after(DEADLINE);
        // The first read is due once the unrelated line was read: six
        // seconds in, six short of the status window, past the short one.
        wait.recv(&read_rx).unwrap();
        let due = origin + within;
        let ahead = Duration::from_secs(6).min(due.saturating_duration_since(clock.now()));
        clock.advance_when_parked(Some(due), Some(id), ahead, &Deadline::after(DEADLINE));
        let got: Result<Status, Failure> = if reads {
            go.send(()).unwrap_or(());
            // The release is the completing event: the capped real poll
            // delivers the answer with no further advance.
            wait.recv(&done_rx).expect("the probe returns")
        } else {
            Err(wait.recv(&done_rx).expect("the probe returns").unwrap_err())
        };
        if reads {
            let status = got.unwrap();
            assert_eq!(status.version.as_deref(), Some("1.2.3"));
            assert_eq!(clock.now(), origin + Duration::from_secs(6));
        } else {
            let error = got.unwrap_err();
            assert_eq!(error.code, ErrorCode::IoFailed);
            assert!(
                error.message.contains("did not answer status"),
                "{}",
                error.message
            );
            assert_eq!(clock.now(), due);
        }
    }
}

/// An accepted answer carrying `variables`.
fn accepted_variables(id: &str, version: &str, clients: u64, variables: Value) -> String {
    hub_line(
        "command_accepted",
        json!({"command_id": id, "result": {
            "running": true, "fiber_version": version, "clients": clients,
            "variables": variables,
        }}),
    )
}

/// Runs `diagnose_with` on a worker thread, handing back its read and
/// wait signals, its answer, and its thread id, so the test steps the
/// fake clock naming that thread and its expected park instant.
fn diagnose_signalled(
    home: &Path,
    clock: &Arc<FakeClock>,
    within: Duration,
) -> (
    Receiver<()>,
    Receiver<()>,
    Receiver<Diagnostic>,
    thread::ThreadId,
) {
    let (reads, read) = mpsc::channel();
    let (waits_tx, waits) = mpsc::channel();
    let (ids, id) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let home = home.to_path_buf();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        ids.send(thread::current().id()).unwrap_or(());
        let mut before_read = || reads.send(()).unwrap_or(());
        let mut before_wait = || waits_tx.send(()).unwrap_or(());
        done.send(diagnose_with(
            &home,
            &*clock,
            within,
            &mut before_read,
            &mut before_wait,
        ))
        .unwrap_or(());
    });
    let id = Deadline::after(DEADLINE)
        .recv(&id)
        .expect("the diagnose worker reports its thread");
    (read, waits, result, id)
}

/// The diagnose answer for a hub serving `answer`, through the public
/// entry point: served at once, the capped real poll delivers it with no
/// parked advance.
fn diagnosed(home: &Path, answer: &str, within: Duration) -> Diagnostic {
    let home = home.to_path_buf();
    let answer = answer.to_owned();
    let (_got, _hub) = fake_hub(&home, move |mut stream| {
        stream.write_all(answer.as_bytes()).unwrap();
        hold(&stream);
    });
    let clock: Arc<FakeClock> = FakeClock::new();
    fakes::within("the diagnose", Deadline::after(DEADLINE), move || {
        diagnose(&home, &*clock, within)
    })
}

fn login_shell() -> Value {
    json!({
        "path": "/hub/bin:/usr/bin",
        "names": ["EDITOR", "FIBER_HOME"],
        "source": "login_shell",
    })
}

#[test]
fn answered_variables_name_the_login_shell() {
    let (_dir, home) = home();
    let answer = accepted_variables("c_status", "1.2.3", 3, login_shell());
    let Diagnostic::Connected(Ok(reply)) = diagnosed(&home, &answer, ANSWER) else {
        panic!("a full answer connects");
    };
    assert_eq!(reply.version, "1.2.3");
    assert_eq!(reply.clients, 3);
    let variables = reply.variables.expect("login-shell variables");
    assert_eq!(variables.path, "/hub/bin:/usr/bin");
    assert_eq!(variables.names, ["EDITOR", "FIBER_HOME"]);
    assert_eq!(
        variables.source,
        contract::events::VariablesSource::LoginShell
    );
}

#[test]
fn inherited_variables_say_the_capture_failed() {
    let (_dir, home) = home();
    let answer = accepted_variables(
        "c_status",
        "1.2.3",
        1,
        json!({"path": "/usr/bin", "names": [], "source": "inherited"}),
    );
    let Diagnostic::Connected(Ok(reply)) = diagnosed(&home, &answer, ANSWER) else {
        panic!("an inherited answer connects");
    };
    let variables = reply.variables.expect("inherited variables");
    assert_eq!(variables.path, "/usr/bin");
    assert!(variables.names.is_empty());
    assert_eq!(
        variables.source,
        contract::events::VariablesSource::Inherited
    );
}

#[test]
fn a_silent_status_exchange_connects_err() {
    let (_dir, home) = home();
    let (_got, _hub) = fake_hub(&home, |stream| hold(&stream));
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (read, _waited, result, id) = diagnose_signalled(&home, &clock, ANSWER);
    let due = origin + ANSWER;
    Deadline::after(DEADLINE).recv(&read).unwrap();
    clock.advance_when_parked(Some(due), Some(id), ANSWER, &Deadline::after(DEADLINE));
    let Diagnostic::Connected(Err(failure)) = Deadline::after(DEADLINE).recv(&result).unwrap()
    else {
        panic!("a silent exchange connects err");
    };
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert_eq!(failure.message, "the hub did not answer status in 5 s");
    assert_eq!(clock.now(), due);
}

#[test]
fn a_rejected_status_exchange_connects_err_with_its_code() {
    let (_dir, home) = home();
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": "c_status", "code": "invalid_arguments", "message": "nope"}),
    );
    let Diagnostic::Connected(Err(failure)) = diagnosed(&home, &rejected, ANSWER) else {
        panic!("a rejection connects err");
    };
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(failure.message, "nope");
}

#[test]
fn missing_variables_are_an_unknown_environment() {
    let (_dir, home) = home();
    let Diagnostic::Connected(Ok(reply)) =
        diagnosed(&home, &accepted("c_status", "1.2.3", 1), ANSWER)
    else {
        panic!("an answer without variables connects");
    };
    assert_eq!(
        reply.variables.unwrap_err(),
        "the hub's answer to status has no variables"
    );
}

#[test]
fn malformed_variables_are_an_unknown_environment() {
    let (_dir, home) = home();
    let answer = accepted_variables("c_status", "1.2.3", 1, json!("login_shell"));
    let Diagnostic::Connected(Ok(reply)) = diagnosed(&home, &answer, ANSWER) else {
        panic!("an answer with malformed variables connects");
    };
    assert_eq!(
        reply.variables.unwrap_err(),
        "the hub's answer to status has malformed variables"
    );
}

#[test]
fn a_wrong_command_id_is_skipped_then_answered() {
    let (_dir, home) = home();
    let lines = [
        "not json\n".to_owned(),
        accepted("c_other", "9.9.9", 7),
        accepted_variables("c_status", "1.2.3", 2, login_shell()),
    ]
    .concat();
    let Diagnostic::Connected(Ok(reply)) = diagnosed(&home, &lines, ANSWER) else {
        panic!("the matching answer connects");
    };
    assert_eq!(reply.version, "1.2.3");
    assert_eq!(reply.clients, 2);
    assert!(reply.variables.is_ok());
}

#[test]
fn a_partially_written_answer_is_read_after_a_wait() {
    let (_dir, home) = home();
    let answer = accepted_variables("c_status", "1.2.3", 1, login_shell());
    let (first, second) = answer.split_at(answer.len() / 2);
    let first = first.to_owned();
    let second = second.to_owned();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (_got, _hub) = fake_hub(&home, move |mut stream| {
        stream.write_all(first.as_bytes()).unwrap();
        stream.flush().unwrap();
        go_rx.recv().unwrap_or(());
        stream.write_all(second.as_bytes()).unwrap();
        hold(&stream);
    });
    let clock: Arc<FakeClock> = FakeClock::new();
    let (read, waited, result, _id) = diagnose_signalled(&home, &clock, ANSWER);
    let wait = Deadline::after(DEADLINE);
    // The first part holds no answer line: the exchange waits at least
    // once, then the release completes it with no parked advance.
    wait.recv(&read).unwrap();
    wait.recv(&waited).unwrap();
    go_tx.send(()).unwrap();
    let Diagnostic::Connected(Ok(reply)) = wait.recv(&result).unwrap() else {
        panic!("the completed answer connects");
    };
    assert_eq!(reply.version, "1.2.3");
    assert!(reply.variables.is_ok());
}

#[test]
fn the_exchange_window_holds_then_releases_then_expires() {
    // A held answer five seconds in does not time out: the exchange waits
    // past the old connect deadline.
    let (_dir, home5) = home();
    let answer = accepted_variables("c_status", "1.2.3", 1, login_shell());
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (_got, _hub) = fake_hub(&home5, move |mut stream| {
        go_rx.recv().unwrap_or(());
        stream.write_all(answer.as_bytes()).unwrap();
        hold(&stream);
    });
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (read, _waited, result, id) = diagnose_signalled(&home5, &clock, STATUS_WITHIN);
    let due = origin + STATUS_WITHIN;
    Deadline::after(DEADLINE).recv(&read).unwrap();
    clock.advance_when_parked(
        Some(due),
        Some(id),
        Duration::from_secs(5),
        &Deadline::after(DEADLINE),
    );
    assert!(
        result.try_recv().is_err(),
        "a held answer five seconds in is still waiting"
    );
    // Released before fifteen seconds it succeeds.
    go_tx.send(()).unwrap();
    let Diagnostic::Connected(Ok(reply)) = Deadline::after(DEADLINE).recv(&result).unwrap() else {
        panic!("the released answer connects");
    };
    assert_eq!(reply.version, "1.2.3");
    assert_eq!(clock.now(), origin + Duration::from_secs(5));

    // Exactly fifteen seconds expires.
    let (_dir_held, home_held) = home();
    let (_got, _hub) = fake_hub(&home_held, |stream| hold(&stream));
    let clock: Arc<FakeClock> = FakeClock::new();
    let origin = clock.origin();
    let (read, _waited, result, id) = diagnose_signalled(&home_held, &clock, STATUS_WITHIN);
    let due = origin + STATUS_WITHIN;
    Deadline::after(DEADLINE).recv(&read).unwrap();
    clock.advance_when_parked(
        Some(due),
        Some(id),
        STATUS_WITHIN,
        &Deadline::after(DEADLINE),
    );
    let Diagnostic::Connected(Err(failure)) = Deadline::after(DEADLINE).recv(&result).unwrap()
    else {
        panic!("an answer held to the window connects err");
    };
    assert_eq!(failure.message, "the hub did not answer status in 15 s");
    assert_eq!(clock.now(), due);
}
