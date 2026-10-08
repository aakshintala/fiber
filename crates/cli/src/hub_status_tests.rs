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
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::Failure;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{
    AnswerRead, Status, classify_answer, ignore_closed_peer, installed, is_answer_timeout,
    is_closed_peer, is_missing, probe, probe_with, render_json, render_text,
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

fn probed(home: &Path, within: Duration) -> Result<Status, Failure> {
    let home = home.to_path_buf();
    let clock = FakeClock::new();
    fakes::within("the probe", DEADLINE, move || {
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
    let request: Value = serde_json::from_str(&got.recv_timeout(DEADLINE).unwrap()).unwrap();
    assert_eq!(request, json!({"id": "c_status", "command": "status"}));
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
    let error = probed(&home, Duration::from_millis(10)).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains("did not answer status"),
        "{}",
        error.message
    );
}

#[test]
fn an_eof_retry_does_not_extend_the_total_deadline() {
    let (_dir, home) = home();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let clock = FakeClock::new();
    let (checked_read, checked) = mpsc::channel::<()>();
    let (resume, wait_resume) = mpsc::channel::<()>();
    let (second_accepted, second) = mpsc::channel::<()>();
    let (go, wait_go) = mpsc::channel::<()>();
    // First connection trickles one byte, then closes once the test lets
    // it: the EOF retry. The second connection stays silent.
    thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        first.write_all(b"{").unwrap();
        wait_go.recv_timeout(DEADLINE).unwrap();
        drop(first);
        let (silent, _) = listener.accept().unwrap();
        second_accepted.send(()).unwrap();
        hold(&silent);
    });
    let (done, result) = mpsc::channel();
    let probed_home = home.clone();
    let probed_clock = Arc::clone(&clock);
    thread::spawn(move || {
        let mut reads = 0;
        let mut before_read = || {
            reads += 1;
            if reads == 2 {
                checked_read.send(()).unwrap_or(());
                wait_resume.recv_timeout(DEADLINE).unwrap();
            }
        };
        done.send(probe_with(
            &probed_home,
            &*probed_clock,
            ANSWER,
            false,
            &mut before_read,
            &mut || {},
        ))
        .unwrap_or(());
    });
    // The handshake reader has checked the deadline after consuming the
    // first byte and is paused before its next read. Advance time, close
    // that peer, then let the reader observe EOF and retry with 100 ms left.
    checked.recv_timeout(DEADLINE).unwrap();
    clock.advance(ANSWER.checked_sub(Duration::from_millis(100)).unwrap());
    go.send(()).unwrap();
    resume.send(()).unwrap();
    second.recv_timeout(DEADLINE).unwrap();
    // The retry's silent peer gets the remaining 100 ms, not a fresh
    // 5 s: the whole probe fails at the one deadline, well within a wall
    // bound far below a fresh handshake.
    let error = result
        .recv_timeout(Duration::from_secs(2))
        .expect("the probe fails at its total deadline, not a fresh one")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
}

/// Runs `probe_with` on a thread, sending a signal before each read.
fn probe_signalled(
    home: &Path,
    clock: &Arc<FakeClock>,
) -> (Receiver<()>, Receiver<Result<Status, Failure>>) {
    let (reads, read) = mpsc::channel();
    let (done, result) = mpsc::channel();
    let home = home.to_path_buf();
    let clock = Arc::clone(clock);
    thread::spawn(move || {
        let mut before_read = || reads.send(()).unwrap_or(());
        done.send(probe_with(
            &home,
            &*clock,
            ANSWER,
            false,
            &mut || {},
            &mut before_read,
        ))
        .unwrap_or(());
    });
    (read, result)
}

/// A hub that writes `first`, waits for the test's go, then writes
/// `second`.
fn two_part_hub(home: &Path, first: String, second: String) -> Sender<()> {
    let (go, wait) = mpsc::channel::<()>();
    let (_got, _hub) = fake_hub(home, move |mut stream| {
        stream.write_all(first.as_bytes()).unwrap();
        wait.recv_timeout(DEADLINE).unwrap();
        stream.write_all(second.as_bytes()).unwrap();
        hold(&stream);
    });
    go
}

#[test]
fn unrelated_lines_past_the_deadline_end_the_probe() {
    let (_dir, home) = home();
    let other = accepted("c_other", "9.9.9", 1);
    let go = two_part_hub(&home, other.clone(), other);
    let clock = FakeClock::new();
    let (read, result) = probe_signalled(&home, &clock);
    // The second read is due once the first unrelated line was read.
    read.recv_timeout(DEADLINE).unwrap();
    read.recv_timeout(DEADLINE).unwrap();
    // Exactly at the deadline no time is left.
    clock.advance(ANSWER);
    go.send(()).unwrap();
    let error = result.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "the hub did not answer status in 5 s");
    assert!(read.try_recv().is_err(), "no third read");
}

#[test]
fn an_answer_in_two_writes_inside_the_deadline_is_read() {
    let (_dir, home) = home();
    let answer = accepted("c_status", "1.2.3", 1);
    let (first, second) = answer.split_at(answer.len() / 2);
    let go = two_part_hub(&home, first.to_owned(), second.to_owned());
    let clock = FakeClock::new();
    let (read, result) = probe_signalled(&home, &clock);
    read.recv_timeout(DEADLINE).unwrap();
    read.recv_timeout(DEADLINE).unwrap();
    go.send(()).unwrap();
    let status = result.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(status.version.as_deref(), Some("1.2.3"));
}

#[test]
fn an_answer_followed_by_a_close_is_read() {
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
    let status = fakes::within("the probe", DEADLINE, move || {
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
fn only_a_missing_devices_dir_is_no_devices() {
    assert!(is_missing(std::io::ErrorKind::NotFound));
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::NotADirectory,
        std::io::ErrorKind::TimedOut,
        std::io::ErrorKind::WouldBlock,
        std::io::ErrorKind::InvalidInput,
        std::io::ErrorKind::ConnectionReset,
    ] {
        assert!(!is_missing(kind), "{kind:?}");
    }
}

#[test]
fn devices_is_empty_when_absent_and_an_error_when_not_a_dir() {
    let (_dir, home) = home();
    let probed = home.clone();
    let empty = fakes::within("the probe", DEADLINE, move || {
        super::devices(&probed).unwrap()
    });
    assert!(empty.is_empty());
    // `hub/devices` as a file fails with `NotADirectory`, not `NotFound`.
    fs::create_dir_all(home.join("hub")).unwrap();
    fs::write(home.join("hub/devices"), b"x").unwrap();
    let error = super::devices(&home).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
}

#[test]
fn only_a_timeout_is_a_late_answer() {
    assert!(is_answer_timeout(std::io::ErrorKind::WouldBlock));
    assert!(is_answer_timeout(std::io::ErrorKind::TimedOut));
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::NotADirectory,
        std::io::ErrorKind::InvalidInput,
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::UnexpectedEof,
    ] {
        assert!(!is_answer_timeout(kind), "{kind:?}");
    }
}

#[test]
fn a_status_answer_read_is_classified() {
    use std::io::ErrorKind;
    fn error(kind: ErrorKind) -> std::io::Error {
        std::io::Error::new(kind, "injected for the test")
    }
    assert!(matches!(classify_answer(Ok(0)), AnswerRead::Closed));
    assert!(matches!(classify_answer(Ok(1)), AnswerRead::Bytes(1)));
    assert!(matches!(
        classify_answer(Err(error(ErrorKind::WouldBlock))),
        AnswerRead::Late
    ));
    assert!(matches!(
        classify_answer(Err(error(ErrorKind::TimedOut))),
        AnswerRead::Late
    ));
    for kind in [
        ErrorKind::NotFound,
        ErrorKind::ConnectionReset,
        ErrorKind::InvalidInput,
        ErrorKind::UnexpectedEof,
    ] {
        assert!(
            matches!(classify_answer(Err(error(kind))), AnswerRead::Lost(_)),
            "{kind:?}"
        );
    }
}

#[test]
fn only_the_closed_peer_refusal_is_ignored() {
    use std::io::ErrorKind;
    fn error(kind: ErrorKind) -> std::io::Error {
        std::io::Error::new(kind, "injected for the test")
    }
    assert!(is_closed_peer(ErrorKind::InvalidInput));
    for kind in [
        ErrorKind::NotFound,
        ErrorKind::TimedOut,
        ErrorKind::WouldBlock,
        ErrorKind::ConnectionReset,
    ] {
        assert!(!is_closed_peer(kind), "{kind:?}");
    }
    assert!(ignore_closed_peer(Ok(())).is_ok());
    assert!(ignore_closed_peer(Err(error(ErrorKind::InvalidInput))).is_ok());
    for kind in [
        ErrorKind::TimedOut,
        ErrorKind::WouldBlock,
        ErrorKind::NotFound,
        ErrorKind::ConnectionReset,
    ] {
        let folded = ignore_closed_peer(Err(error(kind)));
        assert_eq!(folded.unwrap_err().kind(), kind, "{kind:?}");
    }
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
fn child_exit(test: &str, case: &str, vars: &[(&str, &Path)]) -> i32 {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{test}::hub_status_exits_without_the_manager"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env(CASE, case)
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    for (key, value) in vars {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for the hub status child to exit");
    };
    status.code().expect("the child exits with a code")
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
