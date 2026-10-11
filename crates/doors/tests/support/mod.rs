//! Shared helpers for the doors crate-integration tests and the unit tests
//! that include them: one temporary home, one hang bound, one opened
//! session, one deadline-bounded line wait, and the response helpers, each
//! defined once.

#![allow(dead_code, reason = "each test file uses its own subset")]

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::Clock;
use contract::events::ToolInfo;
use doors::{Session, mint};
use fakes::Client;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::Value;

/// One hang bound for every wait: twice it plus run time stays under
/// nextest's 120 s kill (`docs/testing.md`, "Waits and timeouts").
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// One deadline for a whole `until` wait or one `subscribe` acknowledgement.
/// The busiest test makes seven such waits and closes its session once under
/// [`DEADLINE`]: 7 x 6 + 10 = 52 s, at most half of nextest's 120 s kill
/// (`docs/testing.md`, "Waits and timeouts").
pub(crate) const UNTIL: Duration = Duration::from_secs(6);

/// How long the reader inside `until` blocks on one receive, so it notices a
/// missed deadline within this bound instead of one more [`UNTIL`].
pub(crate) const SLICE: Duration = Duration::from_secs(1);

/// A temporary directory a test owns: the path, held until drop.
pub(crate) struct Temp(
    pub(crate) PathBuf,
    #[allow(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
);

impl Temp {
    /// A fresh directory under the system temporary directory.
    pub(crate) fn new() -> Self {
        let held = fakes::TempDir::new("fd");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}

/// A writer the test reads back after the session is done with it.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) buf: Arc<Mutex<Vec<u8>>>,
    pub(crate) ready: Arc<Condvar>,
}

impl Shared {
    /// An empty buffer no session has written to yet.
    pub(crate) fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
            ready: Arc::new(Condvar::new()),
        }
    }

    /// Every line written so far, parsed.
    pub(crate) fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.buf.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        self.ready.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A session opened on a fresh home, with the handles a test needs to drive
/// it and to close it.
pub(crate) struct Opened {
    pub(crate) _temp: Temp,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) log: Arc<Log>,
    pub(crate) home: PathBuf,
    pub(crate) sessions: PathBuf,
    pub(crate) id: SessionId,
    pub(crate) dir: PathBuf,
    pub(crate) socket: PathBuf,
    pub(crate) out: Shared,
    pub(crate) session: Session,
}

impl Opened {
    /// A session with `tools` on a fresh home, printing to a [`Shared`] the
    /// test reads back.
    pub(crate) fn open(tools: Vec<ToolInfo>) -> Self {
        let temp = Temp::new();
        let home = temp.0.join("h");
        let sessions = home.join("projects/p/sessions");
        let id = SessionId(mint("s_"));
        let dir = sessions.join(&id.0);
        let clock = FakeClock::new();
        let timed = Arc::clone(&clock);
        let timed: Arc<dyn Clock> = timed;
        let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
        let out = Shared::new();
        let session =
            Session::open(&home, &dir, &log, timed, tools, Box::new(out.clone())).unwrap();
        Self {
            _temp: temp,
            clock,
            log,
            home: home.clone(),
            sessions,
            id: id.clone(),
            dir,
            socket: home.join("run").join(&id.0),
            out,
            session,
        }
    }

    /// Closes the session and returns its temporary directory, still present
    /// when the session kept it.
    #[track_caller]
    pub(crate) fn close(self) -> Temp {
        let Opened {
            session,
            log,
            _temp,
            ..
        } = self;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            session.close(log);
            if let Ok(()) = tx.send(()) {}
        });
        Deadline::after(DEADLINE).recv(&rx).expect("close returned");
        _temp
    }
}

/// Closes `session` on a thread bounded by [`DEADLINE`]: a hung close fails
/// the test instead of hanging it.
#[track_caller]
pub(crate) fn close_within(session: Session, log: Arc<Log>) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = tx.send(()) {}
    });
    Deadline::after(DEADLINE).recv(&rx).expect("close returned");
}

/// The next line the client reads, within [`DEADLINE`].
pub(crate) fn next(client: &Client) -> Value {
    client.recv(DEADLINE).expect("a line arrived")
}

/// Lines up to and including the first one `done` accepts, read under one
/// [`UNTIL`] deadline for the whole wait, not one per line. Fails naming the
/// wait when it passes.
#[track_caller]
pub(crate) fn until(client: &Client, mut done: impl FnMut(&Value) -> bool + Send) -> Vec<Value> {
    let stop = AtomicBool::new(false);
    let got = thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let stop = &stop;
        scope.spawn(move || {
            let mut lines = Vec::new();
            let mut idle = 0;
            while !stop.load(Ordering::SeqCst) {
                let Some(line) = client.recv(SLICE) else {
                    // A closed socket answers at once: stop instead of spinning.
                    idle += 1;
                    if idle > UNTIL.as_secs() {
                        break;
                    }
                    continue;
                };
                idle = 0;
                let finished = done(&line);
                lines.push(line);
                if finished {
                    if let Ok(()) = tx.send(lines) {}
                    return;
                }
            }
        });
        let waited = Deadline::after(UNTIL).recv(&rx);
        stop.store(true, Ordering::SeqCst);
        waited
    });
    got.expect("the awaited line arrived within one deadline for the whole wait")
}

/// The `kind` of a line from the session.
pub(crate) fn kind(line: &Value) -> &str {
    line["kind"].as_str().unwrap()
}

/// The `command_id` a line answers, if it answers one.
pub(crate) fn command_id(line: &Value) -> Option<&str> {
    line["payload"].get("command_id").and_then(Value::as_str)
}

/// The durable `seq`s of `lines`, in order.
pub(crate) fn seqs(lines: &[Value]) -> Vec<u64> {
    lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect()
}

/// The `kind` of each of `lines`, in order.
pub(crate) fn kinds(lines: &[Value]) -> Vec<&str> {
    lines.iter().map(kind).collect()
}

/// Sends one command line to the session.
pub(crate) fn send(client: &Client, line: &str) {
    client.send(line).unwrap();
}

/// The acknowledgement for `id`, skipping events that belong to the session.
#[track_caller]
pub(crate) fn response(client: &Client, id: &str) -> Value {
    until(client, |line| command_id(line) == Some(id))
        .into_iter()
        .next_back()
        .unwrap()
}

/// The `subscribe` command line for `id` at `level`, for tests that read the
/// acknowledgement themselves instead of through [`subscribe`].
pub(crate) fn subscribe_line(id: &str, level: &str) -> String {
    format!(r#"{{"id":"{id}","command":"subscribe","args":{{"level":"{level}"}}}}"#)
}

/// Subscribes at `level` and returns the acknowledgement, which carries the
/// clock's time.
pub(crate) fn subscribe(client: &Client, id: &str, level: &str) -> Value {
    send(client, &subscribe_line(id, level));
    let line = client
        .recv(UNTIL)
        .expect("the subscribe acknowledgement arrived");
    assert_eq!(kind(&line), "command_accepted", "{line}");
    assert_eq!(command_id(&line).unwrap(), id);
    assert_eq!(
        line["ts"].as_u64(),
        Some(1_700_000_000_000),
        "an acknowledgement's ts comes from the clock"
    );
    line
}

/// The rejection `code` and `message` of a `command_rejected` line.
pub(crate) fn rejection(line: &Value) -> (&str, &str) {
    assert_eq!(kind(line), "command_rejected", "{line}");
    (
        line["payload"]["code"].as_str().unwrap(),
        line["payload"]["message"].as_str().unwrap(),
    )
}

/// The `hub_hello` line a hub speaks first, without its newline.
pub(crate) fn hello_line() -> Vec<u8> {
    br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#
        .to_vec()
}
