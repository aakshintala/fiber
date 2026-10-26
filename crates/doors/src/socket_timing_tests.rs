//! Tests for the doors socket-preparation timer (`docs/state.md`,
//! "Diagnostic logs"): the `startup.socket` scope from entering the
//! shared opening routine through listener readiness, on both the new
//! and the resumed constructor, and the quiet untimed paths.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use contract::diag::Timing;
use contract::events::{ToolInfo, ToolSource, ToolState};
use contract::{ErrorCode, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Value, json};

use crate::Session;

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::{DEADLINE, close_within};

/// A path secret nothing in `logs/` may hold.
const SECRET: &str = "the-volume-of-the-meeting-room";

struct Temp {
    held: fakes::TempDir,
    home: std::path::PathBuf,
    id: SessionId,
    dir: std::path::PathBuf,
    clock: Arc<FakeClock>,
    log: Arc<Log>,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("ds");
        // Short on purpose: the socket path must fit the platform
        // limit, so the secret lives in a tool name instead.
        let home = held.path().join("h");
        let sessions = home.join("projects/p/sessions");
        let id = SessionId(crate::mint("s_"));
        let clock = FakeClock::new();
        let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
        let log = Arc::new(Log::create(&sessions, id.clone(), timed).unwrap());
        let dir = sessions.join(&id.0);
        Self {
            held,
            home,
            id,
            dir,
            clock,
            log,
        }
    }

    /// One declared tool named the secret: the timer must never record
    /// it, or any other argument of the opening.
    fn secret_tools() -> Vec<ToolInfo> {
        vec![ToolInfo {
            name: SECRET.to_owned(),
            source: ToolSource::Builtin,
            state: ToolState::Full,
            bytes: 0,
            tokens: None,
        }]
    }

    /// A debug writer attached to the session, as production attaches
    /// before the door opens, and its callable for the companions.
    fn timing(&self) -> Timing {
        let timed: Arc<dyn Clock> = Arc::clone(&self.clock) as Arc<dyn Clock>;
        let diag = log::diag::Diag::new(
            &self.home,
            log::diag::Process::Session,
            log::diag::Level::Debug,
            timed,
        );
        diag.attach(&self.id);
        diag.timings()
    }

    fn socket(&self) -> std::path::PathBuf {
        self.home.join("run").join(&self.id.0)
    }
}

/// Every line of the session diagnostic file, empty before its first.
fn session_lines(temp: &Temp) -> Vec<Value> {
    let path = temp
        .home
        .join("logs")
        .join(format!("session-{}.log", temp.id.0));
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Every `op_timing` line in `lines`, in file order.
fn op_timings(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

/// The wire promise on one `startup.socket` line: exactly `op` and `ms`,
/// the bind wait in `ms`, and none of the home path anywhere.
#[track_caller]
fn assert_socket_line(lines: &[Value], ms: u64) {
    let timings = op_timings(lines);
    assert_eq!(timings.len(), 1, "{lines:?}");
    let data = timings[0]
        .get("data")
        .expect("an op_timing line carries data");
    let mut keys: Vec<&str> = data
        .as_object()
        .expect("the data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["ms", "op"], "{data}");
    assert_eq!(data["op"], json!("startup.socket"), "{data}");
    assert_eq!(data["ms"], Value::from(ms), "{data}");
    let line = serde_json::to_string(timings[0]).unwrap();
    assert!(!line.contains(SECRET), "{line}");
}

/// Opens a new session with `timing` on a thread bounded by [`DEADLINE`]:
/// a hung open fails the test instead of hanging it.
#[track_caller]
fn open_within(
    temp: &Temp,
    timing: Option<Timing>,
    log: Arc<Log>,
    clock: Arc<dyn Clock>,
    tools: Vec<ToolInfo>,
) -> Result<Session, crate::SessionError> {
    let (tx, rx) = mpsc::channel();
    let (home, dir) = (temp.home.clone(), temp.dir.clone());
    thread::spawn(move || {
        tx.send(Session::open_with_timing(
            &home,
            &dir,
            &log,
            clock,
            tools,
            Box::new(io::sink()),
            timing,
        ))
        .unwrap_or(());
    });
    Deadline::after(DEADLINE).recv(&rx).expect("open returns")
}

/// The pre-attach process file: nothing timed may land there once the
/// writer is attached before the door opens.
fn pid_file(temp: &Temp) -> std::path::PathBuf {
    temp.home
        .join(format!("logs/session-{}.log", std::process::id()))
}

#[test]
fn debug_open_times_the_bind_and_accepts() {
    let temp = Temp::new();
    let session = open_within(
        &temp,
        Some(temp.timing()),
        Arc::clone(&temp.log),
        Arc::clone(&temp.clock) as Arc<dyn Clock>,
        Temp::secret_tools(),
    )
    .expect("the socket binds");
    // Readiness is a connection that answers, not a file that exists.
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    // The timer is already complete before any serve or close call.
    assert_socket_line(&session_lines(&temp), 0);
    assert!(!pid_file(&temp).exists(), "nothing timed lands pre-attach");
    close_within(session, Arc::clone(&temp.log));
    // Printer and connection lifetime add no second line.
    assert_socket_line(&session_lines(&temp), 0);
}

#[test]
fn debug_resume_times_the_bind_and_accepts() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let (home, dir, log) = (temp.home.clone(), temp.dir.clone(), Arc::clone(&temp.log));
    let clock: Arc<dyn Clock> = Arc::clone(&temp.clock) as Arc<dyn Clock>;
    let timing = temp.timing();
    thread::spawn(move || {
        tx.send(Session::resume_with_timing(
            &home,
            &dir,
            &log,
            clock,
            Temp::secret_tools(),
            Box::new(io::sink()),
            Some(timing),
        ))
        .unwrap_or(());
    });
    let session = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("resume returns")
        .expect("the socket binds");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    assert_socket_line(&session_lines(&temp), 0);
    assert!(!pid_file(&temp).exists(), "nothing timed lands pre-attach");
    close_within(session, Arc::clone(&temp.log));
    assert_socket_line(&session_lines(&temp), 0);
}

#[test]
fn open_measures_a_bind_failure_and_deletes_the_directory() {
    let temp = Temp::new();
    let home = temp.held.path().join("h".repeat(200));
    let sessions = home.join("projects/p/sessions");
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let log = Arc::new(Log::create(&sessions, temp.id.clone(), timed).unwrap());
    let dir = sessions.join(&temp.id.0);
    let timed: Arc<dyn Clock> = clock;
    let diag = log::diag::Diag::new(
        &home,
        log::diag::Process::Session,
        log::diag::Level::Debug,
        Arc::clone(&timed) as Arc<dyn Clock>,
    );
    diag.attach(&temp.id);
    let timing = diag.timings();
    let error = match Session::open_with_timing(
        &home,
        &dir,
        &log,
        timed,
        Vec::new(),
        Box::new(io::sink()),
        Some(timing),
    ) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };
    assert!(
        matches!(
            &error,
            crate::SessionError::SocketPathTooLong { max }
            if *max == crate::socket::SOCKET_PATH_MAX
        ),
        "a path past the limit is too long: {error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Usage);
    assert!(!dir.exists(), "a new session's directory is deleted");
    let lines: Vec<Value> =
        fs::read_to_string(home.join("logs").join(format!("session-{}.log", temp.id.0)))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
    assert_socket_line(&lines, 0);
}

#[test]
fn resume_measures_a_bind_failure_and_keeps_the_directory() {
    let temp = Temp::new();
    let home = temp.held.path().join("h".repeat(200));
    let sessions = home.join("projects/p/sessions");
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let log = Arc::new(Log::create(&sessions, temp.id.clone(), timed).unwrap());
    let dir = sessions.join(&temp.id.0);
    let timed: Arc<dyn Clock> = clock;
    let diag = log::diag::Diag::new(
        &home,
        log::diag::Process::Session,
        log::diag::Level::Debug,
        Arc::clone(&timed) as Arc<dyn Clock>,
    );
    diag.attach(&temp.id);
    let timing = diag.timings();
    let error = match Session::resume_with_timing(
        &home,
        &dir,
        &log,
        timed,
        Vec::new(),
        Box::new(io::sink()),
        Some(timing),
    ) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };
    assert!(
        matches!(
            &error,
            crate::SessionError::SocketPathTooLong { max }
            if *max == crate::socket::SOCKET_PATH_MAX
        ),
        "a path past the limit is too long: {error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Usage);
    assert!(
        dir.join("events.jsonl").is_file(),
        "a resumed session is kept"
    );
    let lines: Vec<Value> =
        fs::read_to_string(home.join("logs").join(format!("session-{}.log", temp.id.0)))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
    assert_socket_line(&lines, 0);
}

#[test]
fn open_measures_a_live_socket_and_leaves_it_alone() {
    let temp = Temp::new();
    fs::create_dir_all(temp.home.join("run")).unwrap();
    let _live = UnixListener::bind(temp.socket()).unwrap();
    let error = match open_within(
        &temp,
        Some(temp.timing()),
        Arc::clone(&temp.log),
        Arc::clone(&temp.clock) as Arc<dyn Clock>,
        Vec::new(),
    ) {
        Ok(_) => panic!("a socket a live session holds binds"),
        Err(error) => error,
    };
    assert!(
        matches!(
            &error,
            crate::SessionError::SocketHeld { socket }
            if socket == &temp.socket()
        ),
        "a held socket is held: {error:?}"
    );
    assert_eq!(error.code(), ErrorCode::SessionHeld);
    UnixStream::connect(temp.socket()).expect("the live socket still answers");
    assert!(!temp.dir.exists(), "a refused open cleans up");
    assert_socket_line(&session_lines(&temp), 0);
}

#[test]
fn open_replaces_a_stale_socket_file() {
    let temp = Temp::new();
    fs::create_dir_all(temp.home.join("run")).unwrap();
    fs::write(temp.socket(), b"a dead process's socket").unwrap();
    let session = open_within(
        &temp,
        Some(temp.timing()),
        Arc::clone(&temp.log),
        Arc::clone(&temp.clock) as Arc<dyn Clock>,
        Temp::secret_tools(),
    )
    .expect("a stale file is replaced");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    assert_socket_line(&session_lines(&temp), 0);
    close_within(session, Arc::clone(&temp.log));
}

/// A clock that records any monotonic read: the untimed opening takes
/// none, while the later close may use it for its grace wait.
struct WatchClock {
    read: AtomicBool,
    clock: Arc<FakeClock>,
}

impl WatchClock {
    fn new() -> Self {
        Self {
            read: AtomicBool::new(false),
            clock: FakeClock::new(),
        }
    }
}

impl Clock for WatchClock {
    fn now(&self) -> Instant {
        self.read.store(true, Ordering::SeqCst);
        self.clock.now()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _for: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, _wait: &mut dyn FnMut(Option<Duration>)) {}

    fn subscribe(&self, _wake: Weak<dyn Wake>) {}
}

#[test]
fn untimed_openings_read_no_clock_and_write_no_line() {
    let temp = Temp::new();
    let watched = Arc::new(WatchClock::new());
    let clock: Arc<dyn Clock> = Arc::clone(&watched) as Arc<dyn Clock>;
    let session = open_within(
        &temp,
        None,
        Arc::clone(&temp.log),
        Arc::clone(&clock),
        Vec::new(),
    )
    .expect("the socket binds");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    assert!(
        !watched.read.load(Ordering::SeqCst),
        "the untimed opening reads no clock"
    );
    close_within(session, Arc::clone(&temp.log));
    assert!(
        !temp.home.join("logs").exists(),
        "no timer means no diagnostic file"
    );
}

#[test]
fn info_openings_write_no_line() {
    let temp = Temp::new();
    let timed: Arc<dyn Clock> = Arc::clone(&temp.clock) as Arc<dyn Clock>;
    let diag = log::diag::Diag::new(
        &temp.home,
        log::diag::Process::Session,
        log::diag::Level::Info,
        timed,
    );
    diag.attach(&temp.id);
    let session = open_within(
        &temp,
        Some(diag.timings()),
        Arc::clone(&temp.log),
        Arc::clone(&temp.clock) as Arc<dyn Clock>,
        Vec::new(),
    )
    .expect("the socket binds");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    close_within(session, Arc::clone(&temp.log));
    assert!(
        !temp.home.join("logs").exists(),
        "at info no timer is built and no line is written"
    );
}

#[test]
fn legacy_constructors_are_quiet() {
    let temp = Temp::new();
    // The old entry points stay untimed even under a debug writer.
    let _timing: Timing = temp.timing();
    let session = open_within_legacy_open(&temp).expect("the socket binds");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    close_within(session, Arc::clone(&temp.log));
    assert!(
        !temp.home.join("logs").exists(),
        "the legacy open writes no timer"
    );
    let temp = Temp::new();
    let _timing: Timing = temp.timing();
    let (tx, rx) = mpsc::channel();
    let (home, dir, log) = (temp.home.clone(), temp.dir.clone(), Arc::clone(&temp.log));
    let clock: Arc<dyn Clock> = Arc::clone(&temp.clock) as Arc<dyn Clock>;
    thread::spawn(move || {
        tx.send(Session::resume(
            &home,
            &dir,
            &log,
            clock,
            Vec::new(),
            Box::new(io::sink()),
        ))
        .unwrap_or(());
    });
    let session = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("resume returns")
        .expect("the socket binds");
    UnixStream::connect(temp.socket()).expect("the bound socket accepts");
    close_within(session, Arc::clone(&temp.log));
    assert!(
        !temp.home.join("logs").exists(),
        "the legacy resume writes no timer"
    );
}

/// Opens a new session through the legacy constructor on a bounded
/// thread, like [`open_within`] but without a timing slot.
#[track_caller]
fn open_within_legacy_open(temp: &Temp) -> Result<Session, crate::SessionError> {
    let (tx, rx) = mpsc::channel();
    let (home, dir, log) = (temp.home.clone(), temp.dir.clone(), Arc::clone(&temp.log));
    let clock: Arc<dyn Clock> = Arc::clone(&temp.clock) as Arc<dyn Clock>;
    thread::spawn(move || {
        tx.send(Session::open(
            &home,
            &dir,
            &log,
            clock,
            Vec::new(),
            Box::new(io::sink()),
        ))
        .unwrap_or(());
    });
    Deadline::after(DEADLINE).recv(&rx).expect("open returns")
}
