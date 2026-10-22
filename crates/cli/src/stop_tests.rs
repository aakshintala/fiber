//! Top-level `fiber stop` against a fake hub: the remote flow over a
//! socket pair (accepted counts, rejected, closed, silent, unrelated
//! lines, a missing count) and the remote wrappers' exit codes outright,
//! the local flow over a fake home (empty, started, probed, a partial
//! rejection versus a transport failure) and the stop deadline as the
//! connect deadline plus ten seconds, and the installed-unit flow in a
//! rerun child with a controlled home (manager stop, command path, order,
//! and both outputs).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::HubLine;
use contract::clock::Clock;
use contract::shapes::Failure;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};
use support::clock::System;

use super::{STOP_WITHIN, on_stop_answered, run_local, run_remote, stop_local, stop_remote};
use crate::hub_service::{ManagerRun, Ran, manager};
use crate::hub_unit::{Manager, name, unit_path};

/// One named deadline per blocking call.
const DEADLINE: Duration = Duration::from_secs(10);

/// The rerun child's marker: when present this test binary runs one stop
/// case instead of its tests.
const CHILD: &str = "FIBER_CLI_STOP_CHILD";

/// The case the rerun child runs.
const CASE: &str = "FIBER_CLI_STOP_CASE";

/// The rerun child's Fiber home.
const HOME_VAR: &str = "FIBER_STOP_HOME";

/// Where the rerun child writes the stop's output: the harness owns the
/// child's stdout, so bytes go through a file the parent pins exactly.
const OUT_VAR: &str = "FIBER_STOP_OUT";

/// The hub's answer accepting `stop` with `n` confirmed-closed sessions;
/// the close after it is the stop.
fn accepted(n: u64) -> Value {
    json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_stop", "result": {"sessions_stopped": n}}})
}

/// The hub's answer refusing `stop` partway: two sessions never confirmed.
fn rejected() -> Value {
    json!({"kind": "command_rejected", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_stop", "code": "io_failed",
            "message": "s_aaa still running"}})
}

/// A hub on one end of a socket pair: it reads the one command line,
/// sends it on, then runs `then` with the hub end.
fn fake_hub(
    then: impl FnOnce(UnixStream) + Send + 'static,
) -> (mpsc::Receiver<String>, UnixStream) {
    let (client, hub) = UnixStream::pair().unwrap();
    let (sent, got) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(hub.try_clone().unwrap());
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        sent.send(line).unwrap_or(());
        then(hub);
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

/// `run_remote` on a thread with a fake clock, handing back its result and
/// what it printed: a blocking socket read never parks the test thread
/// past the wait.
fn run_wait(
    client: UnixStream,
    clock: &Arc<FakeClock>,
) -> mpsc::Receiver<(Result<(), Failure>, Vec<u8>)> {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(clock);
    thread::spawn(move || {
        let mut out = Vec::new();
        done_tx
            .send((run_remote(&mut out, &mut connect(client), &*worker), out))
            .unwrap_or(());
    });
    done_rx
}

/// A manager that records each command line in `calls` and answers it from
/// a script: the closure in `run` runs the manager, `calls` replays what ran.
struct Stub {
    calls: Rc<RefCell<Vec<String>>>,
    run: Box<ManagerRun<'static>>,
}

type Script = Box<dyn FnMut(&str) -> std::io::Result<Ran>>;

impl Stub {
    fn new(script: impl FnMut(&str) -> std::io::Result<Ran> + 'static) -> Self {
        let calls: Rc<RefCell<Vec<String>>> = Default::default();
        let inner = Rc::clone(&calls);
        let script: RefCell<Script> = RefCell::new(Box::new(script));
        let run = Box::new(move |program: &str, args: &[String]| {
            let line = format!("{program} {}", args.join(" "));
            inner.borrow_mut().push(line.clone());
            (script.borrow_mut())(&line)
        }) as Box<ManagerRun<'static>>;
        Self { calls, run }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

fn ok() -> Ran {
    Ran {
        success: true,
        code: Some(0),
        stderr: String::new(),
    }
}

#[test]
fn an_accepted_stop_reports_its_count_and_waits_for_the_close() {
    let (got, client) = fake_hub(|hub| {
        let mut bytes = serde_json::to_vec(&accepted(2)).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&run_wait(client, &clock))
        .expect("the stop returns");
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Stopped 2 sessions and the hub.\n"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            &Deadline::after(DEADLINE)
                .recv(&got)
                .expect("the hub reads the command")
        )
        .unwrap(),
        json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}),
        "the one command line is the sessions stop request"
    );
}

#[test]
fn a_rejected_stop_is_a_failure_with_no_count() {
    let (_got, client) = fake_hub(|hub| {
        let mut bytes = serde_json::to_vec(&rejected()).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&run_wait(client, &clock))
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "s_aaa still running");
    assert_eq!(doors::exit_code(&error), 1);
    assert!(out.is_empty());
}

#[test]
fn unrelated_lines_before_the_answer_are_skipped() {
    let (_got, client) = fake_hub(|hub| {
        (&hub).write_all(b"not json\n").unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&hub).write_all(&hello).unwrap();
        let mut bytes = serde_json::to_vec(&accepted(1)).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&run_wait(client, &clock))
        .expect("the stop returns");
    result.unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Stopped 1 sessions and the hub.\n"
    );
}

#[test]
fn a_close_before_the_answer_is_a_failure() {
    let (got, client) = fake_hub(|hub| {
        drop(hub);
    });
    let clock = FakeClock::new();
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&run_wait(client, &clock))
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "the hub closed the connection before answering stop"
    );
    assert_eq!(doors::exit_code(&error), 1);
    assert!(out.is_empty());
    Deadline::after(DEADLINE)
        .recv(&got)
        .expect("the hub reads the command first");
}

#[test]
fn an_answer_without_a_session_count_is_a_failure() {
    let (_got, client) = fake_hub(|hub| {
        let mut bytes = serde_json::to_vec(&json!({"kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_stop", "result": null}}))
        .unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&run_wait(client, &clock))
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "the hub answered stop without a session count"
    );
    assert!(out.is_empty());
}

#[test]
fn a_silent_hub_fails_at_the_deadline() {
    let (_got, client) = fake_hub(|hub| {
        let mut rest = String::new();
        BufReader::new(&hub).read_line(&mut rest).unwrap_or(0);
    });
    let clock = Arc::new(FakeClock::new());
    let due = clock.origin() + STOP_WITHIN;
    let done = run_wait(client, &clock);
    // The exchange parks at the one deadline; one parked advance past it
    // expires the whole stop.
    assert!(
        clock.await_parked(due, Deadline::after(DEADLINE)),
        "the exchange parks at its deadline"
    );
    clock.advance(STOP_WITHIN + Duration::from_secs(1));
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        format!("the hub did not stop in {} s", STOP_WITHIN.as_secs())
    );
    assert_eq!(doors::exit_code(&error), 1);
    assert!(out.is_empty());
}

#[test]
fn stop_within_is_the_connect_deadline_plus_ten_seconds() {
    assert_eq!(
        STOP_WITHIN,
        doors::hub::CONNECT_DEADLINE + Duration::from_secs(10)
    );
}

#[test]
fn remote_stop_returns_zero_when_the_hub_answers_and_closes() {
    let (_got, client) = fake_hub(|hub| {
        let mut bytes = serde_json::to_vec(&accepted(3)).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    assert_eq!(stop_remote(&mut connect(client), &*clock), 0);
}

#[test]
fn remote_stop_returns_nonzero_when_no_hub_listens() {
    let clock = FakeClock::new();
    let mut connect =
        || Err::<doors::hub::Hub, _>(doors::Error::Local(std::io::Error::other("no hub")));
    assert_eq!(stop_remote(&mut connect, &*clock), 1);
}

#[test]
fn remote_stop_returns_nonzero_on_a_rejected_stop() {
    let (_got, client) = fake_hub(|hub| {
        let mut bytes = serde_json::to_vec(&rejected()).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    assert_eq!(stop_remote(&mut connect(client), &*clock), 1);
}

/// A hub on `home`'s `run/hub` for one client: it says `hub_hello`, then
/// sends the one command line it reads, or `None` when the client closes
/// first instead of commanding. When `answer` it answers `stop` with `n`
/// before closing, as the command path's hub.
fn fake_listener(home: &Path, n: u64) -> (mpsc::Receiver<Option<String>>, thread::JoinHandle<()>) {
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (sent, got) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        let read = BufReader::new(&stream).read_line(&mut line);
        let answered = read.is_ok();
        sent.send(read.ok().and_then(|count| (count > 0).then_some(line)))
            .unwrap_or(());
        if answered {
            let mut accepted = serde_json::to_vec(&accepted(n)).unwrap();
            accepted.push(b'\n');
            (&stream).write_all(&accepted).unwrap_or(());
        }
    });
    (got, handle)
}

fn no_starter() -> impl FnMut() -> std::io::Result<()> {
    let started = Rc::new(std::cell::Cell::new(false));
    let flag = Rc::clone(&started);
    move || {
        flag.set(true);
        Ok(())
    }
}

#[test]
fn no_hub_and_no_entries_reports_nothing_running() {
    let dir = fakes::TempDir::new("cli-stop-empty");
    fs::create_dir_all(dir.path().join("run")).unwrap();
    let stub = Stub::new(|_| panic!("no hub and no unit file: the service is untouched"));
    let clock = System;
    let mut out = Vec::new();
    let mut start = no_starter();
    run_local(&mut out, dir.path(), &mut start, &clock, &stub.run).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "Nothing running.\n");
    drop(start);
}

#[test]
fn local_stop_returns_zero_when_no_hub_runs() {
    let dir = fakes::TempDir::new("cli-stop-local-zero");
    let clock = FakeClock::new();
    assert_eq!(stop_local(dir.path(), &mut no_starter(), &*clock,), 0);
}

#[test]
fn local_stop_returns_nonzero_when_the_hub_goes_quiet() {
    let dir = fakes::TempDir::new("cli-stop-local-quiet");
    fs::create_dir_all(dir.path().join("run")).unwrap();
    let listener = UnixListener::bind(dir.path().join("run/hub")).unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        // The hub closes before answering: a transport failure.
    });
    let clock = System;
    assert_eq!(stop_local(dir.path(), &mut no_starter(), &clock), 1);
}

#[test]
fn a_probed_hub_answering_zero_reports_zero_stopped() {
    let dir = fakes::TempDir::new("cli-stop-probed-zero");
    fs::create_dir_all(dir.path().join("run")).unwrap();
    let (got, _hub) = fake_listener(dir.path(), 0);
    let stub = Stub::new(|_| panic!("no unit file: the service is untouched"));
    let clock = System;
    let mut out = Vec::new();
    run_local(&mut out, dir.path(), &mut no_starter(), &clock, &stub.run).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Stopped 0 sessions and the hub.\n"
    );
    let line = Deadline::after(DEADLINE)
        .recv(&got)
        .unwrap()
        .expect("the command path sends stop");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}),
    );
}

#[test]
fn session_entries_start_one_hub_that_reports_its_count() {
    let dir = fakes::TempDir::new("cli-stop-started");
    let run = dir.path().join("run");
    fs::create_dir_all(&run).unwrap();
    // Two session sockets the CLI must never connect to: they stay
    // non-blocking and unaccepted past the run.
    let first = UnixListener::bind(run.join("s_0000000000000001")).unwrap();
    first.set_nonblocking(true).unwrap();
    let second = UnixListener::bind(run.join("s_0000000000000002")).unwrap();
    second.set_nonblocking(true).unwrap();
    let home = dir.path().to_owned();
    let (line_tx, line_rx) = mpsc::channel();
    let mut line_tx = Some(line_tx);
    let mut start = move || -> std::io::Result<()> {
        let line_tx = line_tx.take().expect("the starter runs once");
        let listener = UnixListener::bind(home.join("run/hub"))?;
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
                "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
            .unwrap();
            hello.push(b'\n');
            (&stream).write_all(&hello).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            line_tx.send(line).unwrap_or(());
            let mut answer = serde_json::to_vec(&accepted(2)).unwrap();
            answer.push(b'\n');
            (&stream).write_all(&answer).unwrap();
        });
        Ok(())
    };
    let stub = Stub::new(|_| panic!("no unit file: the service is untouched"));
    let clock = System;
    let mut out = Vec::new();
    run_local(&mut out, dir.path(), &mut start, &clock, &stub.run).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Stopped 2 sessions and the hub.\n"
    );
    let line = Deadline::after(DEADLINE)
        .recv(&line_rx)
        .expect("the started hub reads the stop");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"id": "c_stop", "command": "stop", "args": {"sessions": true}}),
    );
    for held in [&first, &second] {
        assert!(
            matches!(held.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the CLI never connected to a session socket"
        );
    }
}

#[test]
fn a_started_hub_answering_zero_reports_nothing_running() {
    let dir = fakes::TempDir::new("cli-stop-started-zero");
    let run = dir.path().join("run");
    fs::create_dir_all(&run).unwrap();
    let held = UnixListener::bind(run.join("s_0000000000000001")).unwrap();
    held.set_nonblocking(true).unwrap();
    let home = dir.path().to_owned();
    let mut start = move || -> std::io::Result<()> {
        let listener = UnixListener::bind(home.join("run/hub"))?;
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
                "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
            .unwrap();
            hello.push(b'\n');
            (&stream).write_all(&hello).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
            let mut answer = serde_json::to_vec(&accepted(0)).unwrap();
            answer.push(b'\n');
            (&stream).write_all(&answer).unwrap_or(());
        });
        Ok(())
    };
    let stub = Stub::new(|_| panic!("no unit file: the service is untouched"));
    let clock = System;
    let mut out = Vec::new();
    run_local(&mut out, dir.path(), &mut start, &clock, &stub.run).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "Nothing running.\n");
}

#[test]
fn a_transport_failure_skips_the_service_step() {
    let dir = fakes::TempDir::new("cli-stop-transport");
    fs::create_dir_all(dir.path().join("run")).unwrap();
    // A hub that says hello, reads the stop, then closes before
    // answering: no answer arrives at all.
    let listener = UnixListener::bind(dir.path().join("run/hub")).unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
        drop(stream);
    });
    let stub = Stub::new(|_| panic!("a transport failure never reaches the service"));
    let clock = System;
    let mut out = Vec::new();
    let error = run_local(&mut out, dir.path(), &mut no_starter(), &clock, &stub.run).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "the hub closed the connection before answering stop"
    );
    assert!(out.is_empty());
    assert!(stub.calls().is_empty());
    handle.join().unwrap();
}

/// The rerun child running one local stop `case` with a controlled home:
/// panics fail the case, and the stop's own output goes to the child's
/// output file for the parent to pin.
#[test]
fn stop_local_cases() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let case = std::env::var(CASE).unwrap();
    let home = PathBuf::from(std::env::var(HOME_VAR).unwrap());
    let mut out = fs::File::create(std::env::var(OUT_VAR).unwrap()).unwrap();
    let clock = System;
    match case.as_str() {
        "installed-empty" => installed_empty(&home, &mut out, &clock),
        "installed-started" => installed_started(&home, &mut out, &clock),
        "installed-rejected" => installed_rejected(&home, &mut out, &clock),
        case => panic!("unknown stop case: {case}"),
    }
    std::process::exit(0);
}

fn loaded_stub() -> Stub {
    match manager() {
        Manager::Launchd { .. } => {
            // Loaded for the check; gone after the bootout.
            let mut prints = 0;
            Stub::new(move |line| {
                if line.contains(" print ") {
                    prints += 1;
                    return Ok(if prints == 1 { ok() } else { no() });
                }
                Ok(ok())
            })
        }
        Manager::Systemd => Stub::new(|_| Ok(ok())),
    }
}

fn no() -> Ran {
    Ran {
        success: false,
        code: Some(113),
        stderr: "Could not find service\n".to_owned(),
    }
}

fn print_args(service: &crate::hub_unit::Service) -> String {
    match manager() {
        Manager::Launchd { uid } => format!("launchctl print gui/{uid}/{}", service.name),
        Manager::Systemd => systemctl(&format!("is-active {}.service", service.name)),
    }
}

fn stop_args(service: &crate::hub_unit::Service) -> Vec<String> {
    match manager() {
        Manager::Launchd { uid } => vec![
            format!("launchctl bootout gui/{uid}/{}", service.name),
            format!("launchctl print gui/{uid}/{}", service.name),
        ],
        Manager::Systemd => vec![systemctl(&format!("stop {}.service", service.name))],
    }
}

fn systemctl(args: &str) -> String {
    format!("systemctl --user {args}")
}

/// Locates the service for `home` the way the stop does, so the child
/// asserts the exact manager calls for this platform.
fn child_service(home: &Path) -> crate::hub_unit::Service {
    let exe = std::env::current_exe().unwrap();
    crate::hub_unit::Service::locate(manager(), home, &exe, &|key| std::env::var_os(key)).unwrap()
}

fn installed_empty(home: &Path, out: &mut dyn Write, clock: &dyn Clock) {
    let service = child_service(home);
    let stub = loaded_stub();
    let mut start = move || -> std::io::Result<()> {
        panic!("no entries: the starter never runs");
    };
    run_local(out, home, &mut start, clock, &stub.run).unwrap();
    let mut expected = vec![print_args(&service)];
    expected.extend(stop_args(&service));
    assert_eq!(stub.calls(), expected);
}

fn installed_started(home: &Path, out: &mut dyn Write, clock: &dyn Clock) {
    // Two session sockets the CLI must never connect to: they stay
    // non-blocking and unaccepted past the run.
    let first = UnixListener::bind(home.join("run/s_0000000000000001")).unwrap();
    first.set_nonblocking(true).unwrap();
    let second = UnixListener::bind(home.join("run/s_0000000000000002")).unwrap();
    second.set_nonblocking(true).unwrap();
    let service = child_service(home);
    let stub = loaded_stub();
    // The manager must run only after the hub answered: the answer hook
    // logs first, on the same thread that runs the stub below.
    let pushed = Rc::clone(&stub.calls);
    on_stop_answered({
        let pushed = Rc::clone(&pushed);
        move || pushed.borrow_mut().push("answer".to_owned())
    });
    let inner: RefCell<Script> = RefCell::new(Box::new(move |line: &str| {
        let _ = line;
        Ok(ok())
    }));
    let stub_run: Box<ManagerRun<'static>> = Box::new(move |program, args| {
        let line = format!("{program} {}", args.join(" "));
        pushed.borrow_mut().push(line.clone());
        (inner.borrow_mut())(&line)
    });
    let home_owned = home.to_owned();
    let mut start = move || -> std::io::Result<()> {
        let listener = UnixListener::bind(home_owned.join("run/hub"))?;
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
                "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
            .unwrap();
            hello.push(b'\n');
            (&stream).write_all(&hello).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let mut answer = serde_json::to_vec(&accepted(2)).unwrap();
            answer.push(b'\n');
            (&stream).write_all(&answer).unwrap();
        });
        Ok(())
    };
    run_local(out, home, &mut start, clock, &stub_run).unwrap();
    let mut expected = vec!["answer".to_owned(), print_args(&service)];
    expected.extend(stop_args(&service));
    assert_eq!(stub.calls(), expected);
    for held in [&first, &second] {
        assert!(
            matches!(held.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the CLI never connected to a session socket"
        );
    }
}

fn installed_rejected(home: &Path, out: &mut dyn Write, clock: &dyn Clock) {
    let service = child_service(home);
    let stub = loaded_stub();
    // The manager must run only after the hub answered: the answer hook
    // logs first, on the same thread that runs the stub below.
    let pushed = Rc::clone(&stub.calls);
    on_stop_answered({
        let pushed = Rc::clone(&pushed);
        move || pushed.borrow_mut().push("answer".to_owned())
    });
    let inner: RefCell<Script> = RefCell::new(Box::new(move |line: &str| {
        let _ = line;
        Ok(ok())
    }));
    let stub_run: Box<ManagerRun<'static>> = Box::new(move |program, args| {
        let line = format!("{program} {}", args.join(" "));
        pushed.borrow_mut().push(line.clone());
        (inner.borrow_mut())(&line)
    });
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
        line_tx.send(line).unwrap_or(());
        let mut answer = serde_json::to_vec(&rejected()).unwrap();
        answer.push(b'\n');
        (&stream).write_all(&answer).unwrap_or(());
    });
    let mut start = move || -> std::io::Result<()> {
        panic!("the probe answers: the starter never runs");
    };
    let error = run_local(out, home, &mut start, clock, &stub_run).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "s_aaa still running");
    assert!(line_rx.recv().is_ok(), "the hub reads the stop first");
    let mut expected = vec!["answer".to_owned(), print_args(&service)];
    expected.extend(stop_args(&service));
    assert_eq!(
        stub.calls(),
        expected,
        "the service still stops on a rejection"
    );
}

/// Runs one local stop `case` in a child with `user` as HOME and `home`
/// as the Fiber home, returning what the child printed.
fn stop_child(case: &str, user: &Path, home: &Path, out: &Path) -> std::process::Output {
    let name = module_path!().split_once("::").unwrap().1;
    fakes::rerun_prepared(
        &format!("{name}::stop_local_cases"),
        &[
            (CHILD, "1"),
            (CASE, case),
            ("HOME", user.to_str().unwrap()),
            (HOME_VAR, home.to_str().unwrap()),
            (OUT_VAR, out.to_str().unwrap()),
        ],
        Deadline::after(DEADLINE),
        |command| {
            command.env_remove("XDG_CONFIG_HOME");
        },
    )
}

fn check_child(case: &str, dir: &Path, home: &Path, expected: &str) {
    let user = dir.join("user");
    let out = dir.join("out");
    let output = stop_child(case, &user, home, &out);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the {case} child stops cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&out).unwrap(),
        expected.as_bytes(),
        "the {case} child prints exactly"
    );
}

/// The unit file's path for `home` under `user` as HOME.
fn unit_under(home: &Path, user: &Path) -> PathBuf {
    let var = |key: &str| (key == "HOME").then(|| user.as_os_str().to_owned());
    unit_path(manager(), &name(home).unwrap(), &var).unwrap()
}

fn setup(home: &Path, user: &Path, unit: bool, socket: bool) {
    if socket {
        fs::create_dir_all(home.join("run")).unwrap();
    }
    if unit {
        let unit = unit_under(home, user);
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(unit, "x").unwrap();
    }
}

#[test]
fn an_installed_unit_with_no_hub_still_stops_the_service() {
    let dir = fakes::TempDir::new("cli-stop-installed-empty");
    let home = dir.path().join("home");
    setup(&home, &dir.path().join("user"), true, false);
    check_child(
        "installed-empty",
        dir.path(),
        &home,
        "Nothing running.\nThe hub starts again at the next login or on fiber hub install.\n",
    );
}

#[test]
fn an_installed_unit_with_entries_starts_one_hub_and_stops_it() {
    let dir = fakes::TempDir::new("cli-stop-installed-started");
    let home = dir.path().join("home");
    setup(&home, &dir.path().join("user"), true, true);
    check_child(
        "installed-started",
        dir.path(),
        &home,
        "Stopped 2 sessions and the hub.\nThe hub starts again at the next login or on fiber hub install.\n",
    );
}

#[test]
fn a_rejected_stop_still_stops_the_service_then_fails() {
    let dir = fakes::TempDir::new("cli-stop-installed-rejected");
    let home = dir.path().join("home");
    setup(&home, &dir.path().join("user"), true, true);
    let user = dir.path().join("user");
    let out = dir.path().join("out");
    let output = stop_child("installed-rejected", &user, &home, &out);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the installed-rejected child stops cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read(&out).unwrap().is_empty(),
        "a rejection prints nothing"
    );
}

/// Steps the fake clock to the stop bound one second at a time: the answer
/// wait and the close wait share one deadline, so each move waits for a
/// fresh park and neither wait can spend the other's bound early.
fn advance_to_close_expiry(clock: &FakeClock, due: std::time::Instant) {
    for _ in 0..STOP_WITHIN.as_secs() {
        clock.advance_when_parked(
            Some(due),
            None,
            Duration::from_secs(1),
            &Deadline::after(DEADLINE),
        );
    }
}

#[test]
fn a_stalled_close_after_a_rejection_still_reports_the_rejection() {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (got, client) = fake_hub(move |hub| {
        let mut bytes = serde_json::to_vec(&rejected()).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        // The close stalls: the socket stays open past the bound.
        release_rx.recv().unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let due = clock.origin() + STOP_WITHIN;
    let done = run_wait(client, &clock);
    Deadline::after(DEADLINE)
        .recv(&got)
        .expect("the hub reads the command");
    advance_to_close_expiry(&clock, due);
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "s_aaa still running");
    assert!(out.is_empty());
    release_tx.send(()).unwrap();
}

#[test]
fn a_stalled_close_after_an_answer_fails_at_the_deadline() {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (got, client) = fake_hub(move |hub| {
        let mut bytes = serde_json::to_vec(&accepted(2)).unwrap();
        bytes.push(b'\n');
        (&hub).write_all(&bytes).unwrap();
        // The close stalls: the socket stays open past the bound.
        release_rx.recv().unwrap();
        drop(hub);
    });
    let clock = FakeClock::new();
    let due = clock.origin() + STOP_WITHIN;
    let done = run_wait(client, &clock);
    Deadline::after(DEADLINE)
        .recv(&got)
        .expect("the hub reads the command");
    advance_to_close_expiry(&clock, due);
    let (result, out) = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the stop returns");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        format!("the hub did not stop in {} s", STOP_WITHIN.as_secs())
    );
    assert!(out.is_empty());
    release_tx.send(()).unwrap();
}

/// Runs one local stop whose hub answers `answer` then stalls its close:
/// the stepper thread moves the fake clock to the bound while the stop
/// runs on this thread, and the caller holds the close release.
fn stalled_local(
    answer: Value,
    dir: fakes::TempDir,
) -> (
    Result<(), Failure>,
    Vec<u8>,
    mpsc::Sender<()>,
    thread::JoinHandle<()>,
) {
    fs::create_dir_all(dir.path().join("run")).unwrap();
    let listener = UnixListener::bind(dir.path().join("run/hub")).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
        let mut bytes = serde_json::to_vec(&answer).unwrap();
        bytes.push(b'\n');
        (&stream).write_all(&bytes).unwrap_or(());
        // The close stalls: the socket stays open past the bound.
        release_rx.recv().unwrap();
    });
    let stub = Stub::new(|_| panic!("no unit file: the service is untouched"));
    let clock = FakeClock::new();
    let due = clock.origin() + STOP_WITHIN;
    let worker = Arc::clone(&clock);
    let stepper = thread::spawn(move || {
        advance_to_close_expiry(&worker, due);
    });
    let mut out = Vec::new();
    let mut start = no_starter();
    let clock_ref: &FakeClock = &clock;
    let result = run_local(&mut out, dir.path(), &mut start, clock_ref, &stub.run);
    stepper.join().unwrap();
    drop(stub);
    (result, out, release_tx, handle)
}

#[test]
fn a_local_stalled_close_after_a_rejection_still_reports_the_rejection() {
    let dir = fakes::TempDir::new("cli-stop-local-stalled-rejected");
    let (result, out, release, handle) = stalled_local(rejected(), dir);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "s_aaa still running");
    assert!(out.is_empty());
    release.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn a_local_stalled_close_after_an_answer_fails_at_the_deadline() {
    let dir = fakes::TempDir::new("cli-stop-local-stalled-accepted");
    let (result, out, release, handle) = stalled_local(accepted(2), dir);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        format!("the hub did not stop in {} s", STOP_WITHIN.as_secs())
    );
    assert!(out.is_empty());
    release.send(()).unwrap();
    handle.join().unwrap();
}
