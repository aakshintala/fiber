//! Tests for the hub start and resume operation timers
//! (`docs/state.md`, "Diagnostic logs"): the `hub.session_start` scope
//! from `start::run` entry through socket bind, and the `hub.resume` scope
//! around the whole common reach. Each test pins one line per entered
//! operation with exactly `op` and `ms`, and proves failures keep their
//! codes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::shapes::Failure;
use contract::{CommandId, ErrorCode, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::connection::{Hub, Refused};
use crate::fake::{FakeStarter, failure};
use crate::resume::SHUTDOWN_BOUND;
use crate::start::{self, Outcome, START_DEADLINE};

/// One named deadline per wait: a start or resume answers before it.
const DEADLINE: Duration = Duration::from_secs(30);

const SID: &str = "s_0123456789abcdef";

/// A path secret the hub's log must never hold.
const SECRET: &str = "the-volume-of-the-meeting-room";

/// resume's retry poll: awaited before every advance, so the test only
/// moves the clock the waiter already sleeps on.
const HELD_POLL: Duration = Duration::from_millis(100);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<FakeClock>,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::with_run("ht");
        Self {
            dir,
            held,
            clock: FakeClock::new(),
        }
    }

    /// A hub writing debug lines: what the timer tests read.
    fn hub(&self, starter: impl crate::Starter + 'static) -> Arc<Hub> {
        let clock = Arc::clone(&self.clock) as Arc<dyn Clock>;
        let diag = log::diag::Diag::new(
            &self.dir,
            log::diag::Process::Hub,
            log::diag::Level::Debug,
            Arc::clone(&clock),
        );
        let starter: Arc<dyn crate::Starter> = Arc::new(starter);
        Arc::new(Hub::new(&self.dir, "0.0.0", starter, clock, diag))
    }

    /// A hub at the default level: what suppression tests read.
    fn hub_info(&self, starter: FakeStarter) -> Arc<Hub> {
        let clock = Arc::clone(&self.clock) as Arc<dyn Clock>;
        Arc::new(crate::testkit::hub(&self.dir, Arc::new(starter), clock))
    }

    fn workspace(&self) -> PathBuf {
        let workspace = self.dir.join("w");
        fs::create_dir_all(&workspace).unwrap();
        workspace
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }

    /// Writes `first` as the first line of `id`'s log in a project.
    fn log_for(&self, id: &str, first: &str) {
        let dir = self
            .dir
            .join("projects")
            .join("-p")
            .join("sessions")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), format!("{first}\n{{}}\n")).unwrap();
    }

    /// Appends `kind` as the last line of `id`'s log.
    fn append_for(&self, id: &str, kind: &str) {
        let log = self
            .dir
            .join("projects")
            .join("-p")
            .join("sessions")
            .join(id)
            .join("events.jsonl");
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(&json!({"kind": kind, "session_id": id, "payload": {}}).to_string());
        text.push('\n');
        fs::write(log, text).unwrap();
    }

    /// Appends `line` as the last line of `id`'s log.
    fn append_line(&self, id: &str, line: &Value) {
        let log = self
            .dir
            .join("projects")
            .join("-p")
            .join("sessions")
            .join(id)
            .join("events.jsonl");
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
        fs::write(log, text).unwrap();
    }

    /// `id`'s log recording `workspace`.
    fn recorded_for(&self, id: &str, workspace: &PathBuf) {
        self.log_for(
            id,
            &json!({
                "kind": "session_started",
                "seq": 0,
                "session_id": id,
                "payload": {"workspace": workspace},
            })
            .to_string(),
        );
    }
}

/// Every `op_timing` line in the hub log, in file order.
fn op_timings(log: &str) -> Vec<Value> {
    log.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| line["code"] == "op_timing")
        .collect()
}

/// The wire promise on one timer line: exactly `op` and `ms`, the whole
/// scope's wait in `ms`, and none of the operation's paths or models.
#[track_caller]
fn assert_op_line(lines: &[Value], op: &str, ms: u64) {
    assert_eq!(lines.len(), 1, "{lines:?}");
    let data = lines[0]
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
    assert_eq!(data["op"], json!(op), "{data}");
    assert_eq!(data["ms"], Value::from(ms), "{data}");
    let line = serde_json::to_string(&lines[0]).unwrap();
    assert!(!line.contains(SECRET), "{line}");
}

/// Runs `start::run` on a thread, received with a wall-clock deadline.
#[track_caller]
fn started(
    hub: &Arc<Hub>,
    workspace: &str,
    model: Option<&str>,
    content: Option<Value>,
) -> Outcome {
    let (done_tx, done_rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    let workspace = workspace.to_owned();
    let model = model.map(str::to_owned);
    thread::Builder::new()
        .name("hub-test-start".to_owned())
        .spawn(move || {
            let outcome = start::run(
                &hub,
                &CommandId("c_start".to_owned()),
                &workspace,
                model.as_deref(),
                &[],
                false,
                content.as_ref(),
            );
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("start answers before its deadline")
}

/// Runs `resume` on a thread, received with a wall-clock deadline.
#[track_caller]
fn resumed(hub: &Arc<Hub>, id: &SessionId) -> Result<UnixStream, Refused> {
    let (done, finished) = mpsc::channel();
    let hub = Arc::clone(hub);
    let id = id.clone();
    thread::spawn(move || {
        done.send(crate::resume::resume(&hub, &id, None))
            .unwrap_or(())
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers before its deadline")
}

/// Runs `resume` on a thread and hands back where its result arrives: the
/// test drives the fake clock while it waits.
fn resuming(hub: &Arc<Hub>, id: &SessionId) -> mpsc::Receiver<Result<UnixStream, Refused>> {
    let (done, finished) = mpsc::channel();
    let hub = Arc::clone(hub);
    let id = id.clone();
    thread::spawn(move || {
        done.send(crate::resume::resume(&hub, &id, None))
            .unwrap_or(())
    });
    finished
}

fn sid() -> SessionId {
    SessionId(SID.to_owned())
}

/// A starter that fails every launch with its io error text.
struct FailStarter;

impl crate::Starter for FailStarter {
    fn start(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _model: Option<&str>,
        _overrides: &[&str],
        _worktree: bool,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other("the starter blew up"))
    }

    fn resume(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _credential: Option<&str>,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other("the starter blew up"))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _from: &SessionId,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other("the starter blew up"))
    }
}

/// A starter that binds the session socket but holds the hub's summary
/// `subscribe` acknowledgement until the test releases it: bind and
/// handshake are separated by a signal, so the timer's endpoint is
/// observable between them.
struct HoldHandshake {
    home: PathBuf,
    subscribed: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl HoldHandshake {
    fn new(home: &Path) -> (Self, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (subscribed_tx, subscribed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Self {
                home: home.to_owned(),
                subscribed: subscribed_tx,
                release: Mutex::new(Some(release_rx)),
            },
            subscribed_rx,
            release_tx,
        )
    }
}

struct Running;

impl crate::Started for Running {
    fn exited(&self) -> Option<Failure> {
        None
    }
}

impl crate::Starter for HoldHandshake {
    fn start(
        &self,
        id: &SessionId,
        _workspace: &std::path::Path,
        _model: Option<&str>,
        _overrides: &[&str],
        _worktree: bool,
    ) -> io::Result<Box<dyn crate::Started>> {
        let listener = UnixListener::bind(self.home.join("run").join(&id.0))?;
        let subscribed = self.subscribed.clone();
        let release = self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        thread::Builder::new()
            .name("hub-test-held-handshake".to_owned())
            .spawn(move || {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut writer = match stream.try_clone() {
                    Ok(writer) => writer,
                    Err(_) => return,
                };
                let mut read = BufReader::new(stream);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match read.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let line: Value = match serde_json::from_slice(&buf) {
                        Ok(line) => line,
                        Err(_) => continue,
                    };
                    if line.get("command").and_then(Value::as_str) != Some("subscribe") {
                        continue;
                    }
                    subscribed.send(()).unwrap_or(());
                    // Held here: the bind the timer stamps is done, and
                    // the acknowledgement it must exclude is still ahead.
                    if let Some(release) = &release {
                        release.recv().unwrap_or(());
                    }
                    let ack = json!({
                        "kind": "command_accepted",
                        "ts": 1,
                        "schema_version": 1,
                        "payload": {"command_id": line.get("id").cloned().unwrap_or(Value::Null)},
                    });
                    let mut bytes = serde_json::to_vec(&ack).unwrap_or_default();
                    bytes.push(b'\n');
                    writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .unwrap_or(());
                    // The held prompt is never sent: hold the connection
                    // open until the starter goes away.
                    buf.clear();
                    match read.read_until(b'\n', &mut buf) {
                        Ok(_) | Err(_) => return,
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("fake session: {error}")))?;
        Ok(Box::new(Running))
    }

    fn resume(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _credential: Option<&str>,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other("the held starter never resumes"))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _from: &SessionId,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other("the held starter never rewinds"))
    }
}

#[test]
fn start_success_writes_one_line_without_workspace_or_model() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    fs::create_dir_all(&workspace).unwrap();
    let outcome = started(&hub, &workspace.to_string_lossy(), Some(SECRET), None);
    let Outcome::Accepted { .. } = outcome else {
        panic!("the start is accepted");
    };
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn an_invalid_workspace_is_an_entered_start_with_one_line() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let outcome = started(&hub, "relative/path", None, None);
    let Outcome::Rejected(refused) = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(refused.code, ErrorCode::InvalidArguments);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn a_starter_error_measures_through_failure() {
    let temp = Temp::new();
    let hub = temp.hub(FailStarter);
    let workspace = temp.workspace();
    let outcome = started(&hub, &workspace.to_string_lossy(), None, None);
    let Outcome::Rejected(refused) = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn a_session_that_exits_first_measures_through_failure() {
    let temp = Temp::new();
    let exited = failure(ErrorCode::NoModel, "No model is configured.");
    let hub = temp.hub(FakeStarter::exit_with(&temp.dir, exited));
    let workspace = temp.workspace();
    let outcome = started(&hub, &workspace.to_string_lossy(), None, None);
    let Outcome::Rejected(refused) = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(refused.code, ErrorCode::NoModel);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn a_bind_error_measures_through_failure() {
    let temp = Temp::new();
    // `run/` is a file, so connecting fails before any bind: not a
    // missing socket, so there is nothing to wait for.
    fs::remove_dir_all(temp.dir.join("run")).unwrap();
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let workspace = temp.workspace();
    let outcome = started(&hub, &workspace.to_string_lossy(), None, None);
    let Outcome::Rejected(refused) = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn a_bind_timeout_reports_the_whole_deadline() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let workspace = temp.workspace();
    let outcome = started(&hub, &workspace.to_string_lossy(), None, None);
    let Outcome::Rejected(refused) = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(refused.code, ErrorCode::IoFailed);
    // The stamp was taken at entry and the failure waited out the whole
    // deadline on the fake clock.
    let want_ms = u64::try_from(START_DEADLINE.as_millis()).expect("the deadline fits in ms");
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", want_ms);
}

#[test]
fn a_held_summary_handshake_ends_the_timer_at_bind() {
    let temp = Temp::new();
    let (starter, subscribed, release) = HoldHandshake::new(&temp.dir);
    let hub = temp.hub(starter);
    let workspace = temp.workspace();
    let content = json!([{"type": "text", "text": "hi"}]);
    let (done_tx, done_rx) = mpsc::channel();
    let running = Arc::clone(&hub);
    let workspace = workspace.to_string_lossy().into_owned();
    thread::Builder::new()
        .name("hub-test-start".to_owned())
        .spawn(move || {
            let outcome = start::run(
                &running,
                &CommandId("c_start".to_owned()),
                &workspace,
                None,
                &[],
                false,
                Some(&content),
            );
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    assert!(
        Deadline::after(DEADLINE).recv(&subscribed).is_ok(),
        "the hub sends its summary subscribe after bind"
    );
    // The bind the timer stamps is done, and the acknowledgement it must
    // exclude is still ahead: moving the clock now must change nothing.
    temp.clock.advance(Duration::from_secs(5));
    release.send(()).unwrap_or(());
    let outcome = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("start answers after the handshake");
    let Outcome::Accepted { first, .. } = outcome else {
        panic!("the start is accepted");
    };
    assert!(first.is_some(), "the content is held past the timer");
    assert_op_line(&op_timings(&temp.hub_log()), "hub.session_start", 0);
}

#[test]
fn live_attachment_writes_one_line_without_starting() {
    let temp = Temp::new();
    let starter = FakeStarter::hang(&temp.dir);
    let hub = temp.hub(starter.clone());
    let _listener = UnixListener::bind(temp.dir.join("run").join(SID)).unwrap();
    let result = resumed(&hub, &sid());
    assert!(result.is_ok(), "the live session is attached");
    assert!(starter.resumed().is_empty(), "no process is started");
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn an_exited_restart_writes_one_line() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.recorded_for(SID, &workspace);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub, &sid()).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn a_held_retry_then_success_reports_the_whole_wait() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.recorded_for(SID, &workspace);
    temp.append_for(SID, "fiber_exited");
    let starter = FakeStarter::held_then_bind(&temp.dir, 2);
    let hub = temp.hub(starter.clone());
    let finished = resuming(&hub, &sid());
    for k in 1..=2 {
        let until = temp.clock.origin() + HELD_POLL * k;
        assert!(
            temp.clock.await_parked(until, DEADLINE),
            "the hub waits a poll after held failure {k}"
        );
        assert!(finished.try_recv().is_err(), "no answer while it waits");
        temp.clock.advance(HELD_POLL);
    }
    let result = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers once the lock is released");
    assert!(result.is_ok());
    let want_ms = u64::try_from((HELD_POLL * 2).as_millis()).expect("the wait fits in ms");
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", want_ms);
}

#[test]
fn a_resume_held_past_the_shutdown_bound_reports_the_bound() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.recorded_for(SID, &workspace);
    temp.append_for(SID, "fiber_exited");
    let starter = FakeStarter::held_then_bind(&temp.dir, usize::MAX);
    let hub = temp.hub(starter.clone());
    let finished = resuming(&hub, &sid());
    let polls = u32::try_from(SHUTDOWN_BOUND.as_millis() / HELD_POLL.as_millis()).unwrap();
    for k in 1..=polls {
        let until = temp.clock.origin() + HELD_POLL * k;
        assert!(
            temp.clock.await_parked(until, DEADLINE),
            "the hub waits poll {k} inside the bound"
        );
        assert!(finished.try_recv().is_err(), "no answer before the bound");
        temp.clock.advance(HELD_POLL);
    }
    let result = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers at the bound");
    let Err(refused) = result else {
        panic!("the resume is held past the bound");
    };
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    let want_ms = u64::try_from(SHUTDOWN_BOUND.as_millis()).expect("the bound fits in ms");
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", want_ms);
}

#[test]
fn a_missing_log_is_one_entered_resume() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let Err(refused) = resumed(&hub, &sid()) else {
        panic!("the resume is refused");
    };
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn a_corrupt_log_is_one_entered_resume() {
    let temp = Temp::new();
    temp.log_for(SID, "this first line parses as nothing");
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let Err(refused) = resumed(&hub, &sid()) else {
        panic!("the resume is refused");
    };
    assert_eq!(refused.code, ErrorCode::LogCorrupt);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn a_stopped_delegate_is_one_entered_resume() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.log_for(
        SID,
        &json!({
            "kind": "session_started",
            "seq": 0,
            "session_id": SID,
            "payload": {"workspace": workspace, "parent": "s_ffffffffffffffff"},
        })
        .to_string(),
    );
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let Err(refused) = resumed(&hub, &sid()) else {
        panic!("the resume is refused");
    };
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn a_starter_failure_is_one_entered_resume() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.recorded_for(SID, &workspace);
    let hub = temp.hub(FailStarter);
    let Err(refused) = resumed(&hub, &sid()) else {
        panic!("the resume is refused");
    };
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn rewinds_resume_entry_writes_one_line() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = sid();
    let next = SessionId("s_ffffffffffffffff".to_owned());
    temp.recorded_for(SID, &workspace);
    temp.append_line(
        SID,
        &json!({
            "kind": "rewound", "session_id": SID, "ts": 2, "schema_version": 1, "seq": 5,
            "payload": {"new_session_id": next.0, "seq": 3, "jobs": []},
        }),
    );
    temp.recorded_for(&next.0, &workspace);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let result = crate::rewind::reach(&hub, &from, &next);
    assert!(result.is_ok(), "the rewound session is reached");
    assert_eq!(starter.resumed(), [(next, workspace)]);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.resume", 0);
}

#[test]
fn two_reaches_write_two_lines() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let _listener = UnixListener::bind(temp.dir.join("run").join(SID)).unwrap();
    assert!(resumed(&hub, &sid()).is_ok());
    assert!(resumed(&hub, &sid()).is_ok());
    let lines = op_timings(&temp.hub_log());
    assert_eq!(lines.len(), 2, "{lines:?}");
    for line in &lines {
        assert_eq!(line["data"]["op"], json!("hub.resume"), "{line}");
    }
}

#[test]
fn info_writes_no_timing_for_start_or_resume() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    temp.recorded_for(SID, &workspace);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub_info(starter.clone());
    let outcome = started(&hub, &workspace.to_string_lossy(), None, None);
    assert!(matches!(outcome, Outcome::Accepted { .. }));
    // The fresh session's socket is live, but resume names the recorded
    // session: its resumed process binds that socket instead.
    assert!(resumed(&hub, &sid()).is_ok());
    let log = temp.hub_log();
    assert!(log.contains("\"code\":\"session_started\""), "{log}");
    assert!(log.contains("\"code\":\"session_resumed\""), "{log}");
    assert!(!log.contains("op_timing"), "{log}");
}
