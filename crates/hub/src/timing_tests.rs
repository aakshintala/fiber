//! Tests for the hub start, resume and replay operation timers
//! (`docs/state.md`, "Diagnostic logs"): the `hub.session_start` scope
//! from `start::run` entry through socket bind, the `hub.resume` scope
//! around the whole common reach, and the `hub.subscribe_replay` scope
//! from a replay write through its acknowledgement. Each test pins one
//! line per entered operation with exactly `op` and `ms`, and proves
//! failures keep their codes.

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

use crate::connection::{Hub, Refused, lock};
use crate::fake::{FakeStarter, failure};
use crate::relay::{Kept, Relay, Relays, Replayed, attach, attach_rejoin};
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
        read_hub_log(&self.dir)
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

/// The hub log's text, empty before its first line.
fn read_hub_log(dir: &std::path::Path) -> String {
    fs::read_to_string(dir.join("logs").join("hub.log")).unwrap_or_default()
}

/// The hub log's `op_timing` lines once `want` completed: completion
/// removes the slot under the lock and writes the line after, so the
/// test waits for the line, never the slot.
#[track_caller]
fn await_op_lines(temp: &Temp, want: usize) -> Vec<Value> {
    crate::testkit::await_true("the timer completes", {
        let dir = temp.dir.clone();
        move || op_timings(&read_hub_log(&dir)).len() == want
    });
    op_timings(&temp.hub_log())
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
                None,
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
        _continues: Option<&SessionId>,
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
        _continues: Option<&SessionId>,
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
                None,
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
    // No code under test waits on the clock here; start::run is past await_bind and blocks on a socket read for the acknowledgement (ruling 6179406086).
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
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits a poll after held failure {k}"
        );
        assert!(finished.try_recv().is_err(), "no answer while it waits");
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
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
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits poll {k} inside the bound"
        );
        assert!(finished.try_recv().is_err(), "no answer before the bound");
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
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

/// A subscribed session id of the test's own: the hub mints the shape,
/// and the relay checks it, so every replay test uses a fresh one.
const REPLAY_SID: &str = "s_aaaaaaaaaaaaaaaa";

/// A `subscribe` line at `level`, without its `session_id`: what a
/// replay sends again.
fn subscribe_line(id: &str, level: &str) -> serde_json::Map<String, Value> {
    json!({"id": id, "command": "subscribe", "args": {"level": level}})
        .as_object()
        .unwrap()
        .clone()
}

/// A connection's relays with replay timing from the debug `hub`:
/// what production dispatch builds at debug.
fn timing_relays(hub: &Arc<Hub>) -> Arc<Mutex<Relays>> {
    Arc::new(Mutex::new(Relays {
        timing_hub: Some(Arc::downgrade(hub)),
        ..Relays::default()
    }))
}

/// The test end of a client connection: the hub writes here, the test
/// reads with a deadline.
fn client() -> (Arc<Mutex<UnixStream>>, UnixStream) {
    let (write, read) = UnixStream::pair().unwrap();
    read.set_read_timeout(Some(DEADLINE)).unwrap();
    (Arc::new(Mutex::new(write)), read)
}

/// One line the client connection receives, without its newline.
#[track_caller]
fn client_line(read: &UnixStream) -> Value {
    let mut line = String::new();
    BufReader::new(read.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the client receives a line");
    serde_json::from_str(line.trim_end()).unwrap()
}

/// The acknowledgement the session sends for `id`: accepted, or
/// rejected with `invalid_arguments` when `accept` is false.
fn ack_bytes(id: &Value, accept: bool) -> Vec<u8> {
    let ack = if accept {
        json!({
            "kind": "command_accepted", "ts": 1, "schema_version": 1,
            "payload": {"command_id": id},
        })
    } else {
        json!({
            "kind": "command_rejected", "ts": 1, "schema_version": 1,
            "payload": {
                "code": "invalid_arguments",
                "command_id": id,
                "message": "The arguments do not fit this command.",
            },
        })
    };
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    bytes
}

/// Joins a relay thread after its session end, failing at the caller's
/// line instead of hanging when the thread does not end.
#[track_caller]
fn join_relay(thread: thread::JoinHandle<()>) {
    let (ended_tx, ended) = mpsc::channel();
    thread::spawn(move || ended_tx.send(thread.join().is_ok()).unwrap_or(()));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&ended).ok(),
        Some(true),
        "the relay thread ends after its session closed"
    );
}

/// Takes the relay thread's handle out of its entry: the worker already
/// ended, so nothing observes the missing handle.
#[track_caller]
fn take_thread(relays: &Arc<Mutex<Relays>>, session: &str, epoch: u64) -> thread::JoinHandle<()> {
    lock(relays)
        .entries
        .iter_mut()
        .find(|entry| entry.session == session && entry.epoch == epoch)
        .and_then(|entry| entry.thread.take())
        .expect("the relay thread is still entered")
}

/// An initial replay through `attach`, its acknowledgement withheld:
/// the session end the test drives, and the replayed command id.
#[track_caller]
fn replaying(
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    session: &str,
    temp: &Temp,
    want_pending: usize,
) -> (UnixStream, Value) {
    let (hub_end, session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    attach(
        session,
        hub_end,
        hub,
        writer,
        relays,
        Some(subscribe_line("c_kept", "full")),
        None,
        false,
    );
    // The hub's replay, under an id of its own: the worker cannot have
    // completed it, since no acknowledgement was sent.
    let mut line = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the hub replays the kept subscription");
    let replay: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(replay["command"], json!("subscribe"), "{replay}");
    let id = replay.get("id").cloned().unwrap();
    assert_ne!(id, json!("c_kept"), "the replay carries a minted id");
    crate::testkit::await_true("the replay registers", {
        let watching = Arc::clone(relays);
        move || lock(&watching).pending_replay.len() == want_pending
    });
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no completed timer while the acknowledgement is withheld"
    );
    (session_end, id)
}

#[test]
fn an_initial_replay_completes_on_its_accepted_acknowledgement() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, client_read) = client();
    let (session_end, id) = replaying(&hub, &writer, &relays, REPLAY_SID, &temp, 1);
    let epoch = lock(&relays).entries[0].epoch;
    let mut session_end = session_end;
    session_end.write_all(&ack_bytes(&id, true)).unwrap();
    session_end.flush().unwrap();
    // The whole wait is over in no clock time: the timer reports zero.
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    // The acknowledgement was the hub's own: the client never sees it.
    // A live line behind it is what the client reads first.
    session_end
        .write_all(b"{\"kind\":\"session_status\",\"ts\":1,\"schema_version\":1,\"payload\":{}}\n")
        .unwrap();
    session_end.flush().unwrap();
    assert_eq!(client_line(&client_read)["kind"], json!("session_status"));
    // The acknowledgement came before the disconnect: ending now
    // completes nothing twice.
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.subscribe_replay", 0);
}

#[test]
fn an_initial_replay_completes_on_its_rejected_acknowledgement() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    let (session_end, id) = replaying(&hub, &writer, &relays, REPLAY_SID, &temp, 1);
    let mut session_end = session_end;
    session_end.write_all(&ack_bytes(&id, false)).unwrap();
    session_end.flush().unwrap();
    crate::testkit::await_true("the replay completes", {
        let watching = Arc::clone(&relays);
        move || lock(&watching).pending_replay.is_empty()
    });
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    let epoch = lock(&relays).entries[0].epoch;
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.subscribe_replay", 0);
}

#[test]
fn a_kept_subscription_rejoin_times_its_replay_to_acknowledgement() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, client_read) = client();
    // The sweep's candidate: a kept subscription with its mark still
    // current, as the production rejoin path collects it.
    let mark = crate::rejoin::Mark::now(&temp.dir, REPLAY_SID);
    let candidate = {
        let mut held = lock(&relays);
        held.subscribed
            .push((REPLAY_SID.to_owned(), subscribe_line("c_kept", "full")));
        let epoch = held.mint();
        assert!(crate::rejoin::admit(
            &mut held, REPLAY_SID, false, mark, epoch
        ));
        epoch
    };
    let (hub_end, session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    attach_rejoin(REPLAY_SID, hub_end, &hub, &writer, &relays, candidate);
    // The hub's replay, under an id of its own: the worker cannot have
    // completed it, since no acknowledgement was sent.
    let mut line = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the rejoin replays the kept subscription");
    let replay: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(replay["command"], json!("subscribe"), "{replay}");
    let id = replay.get("id").cloned().unwrap();
    assert_ne!(id, json!("c_kept"), "the replay carries a minted id");
    crate::testkit::await_true("the replay registers", {
        let watching = Arc::clone(&relays);
        move || lock(&watching).pending_replay.len() == 1
    });
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no completed timer while the acknowledgement is withheld"
    );
    let mut session_end = session_end;
    session_end.write_all(&ack_bytes(&id, true)).unwrap();
    session_end.flush().unwrap();
    // The whole wait is over in no clock time: the timer reports zero.
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    // The acknowledgement was the hub's own: the client never sees it.
    // The reopening boundary precedes every fold line.
    let reopened = client_line(&client_read);
    assert_eq!(reopened["kind"], "stream_reopened");
    assert_eq!(reopened["payload"]["session_id"], REPLAY_SID);
    assert!(reopened["payload"].get("last_seq").is_none());
    assert!(reopened.get("seq").is_none());
    session_end
        .write_all(b"{\"kind\":\"session_status\",\"ts\":1,\"schema_version\":1,\"payload\":{}}\n")
        .unwrap();
    session_end.flush().unwrap();
    assert_eq!(client_line(&client_read)["kind"], json!("session_status"));
    // The acknowledgement came before the disconnect: ending now
    // completes nothing twice.
    let epoch = lock(&relays).entries[0].epoch;
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.subscribe_replay", 0);
}

#[test]
fn a_stale_rejoin_candidate_times_nothing() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    let (hub_end, session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    // No kept subscription and no mark: the candidate is stale, so the
    // early return keeps no timer.
    attach_rejoin(REPLAY_SID, hub_end, &hub, &writer, &relays, 7);
    let mut reader = BufReader::new(session_end.try_clone().unwrap());
    let mut buf = String::new();
    assert_eq!(
        reader.read_line(&mut buf).unwrap(),
        0,
        "a stale rejoin writes no replay"
    );
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no kept replay, no timer"
    );
}

#[test]
fn a_command_without_a_replay_writes_no_timer() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, client_read) = client();
    let (hub_end, mut session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    let stripped = json!({"id": "c_1", "command": "prompt", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let mut bytes = serde_json::to_vec(&stripped).unwrap();
    bytes.push(b'\n');
    attach(
        REPLAY_SID,
        hub_end,
        &hub,
        &writer,
        &relays,
        None,
        Some((CommandId("c_1".to_owned()), bytes, stripped)),
        false,
    );
    let mut line = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the command reaches the session");
    let command: Value = serde_json::from_str(line.trim_end()).unwrap();
    session_end
        .write_all(&ack_bytes(&command["id"], true))
        .unwrap();
    session_end.flush().unwrap();
    assert_eq!(client_line(&client_read)["kind"], json!("command_accepted"));
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no replay, no timer"
    );
    let epoch = lock(&relays).entries[0].epoch;
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
}

#[test]
fn a_users_own_subscribe_writes_no_timer() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, client_read) = client();
    let (hub_end, mut session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    let stripped = subscribe_line("c_user", "full");
    let mut bytes = serde_json::to_vec(&stripped).unwrap();
    bytes.push(b'\n');
    attach(
        REPLAY_SID,
        hub_end,
        &hub,
        &writer,
        &relays,
        None,
        Some((CommandId("c_user".to_owned()), bytes, stripped.clone())),
        false,
    );
    let mut line = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the subscribe reaches the session");
    let command: Value = serde_json::from_str(line.trim_end()).unwrap();
    session_end
        .write_all(&ack_bytes(&command["id"], true))
        .unwrap();
    session_end.flush().unwrap();
    // The client's own acknowledgement is forwarded, and its level is
    // kept for the next reconnect: only a replay is timed, never this.
    assert_eq!(client_line(&client_read)["kind"], json!("command_accepted"));
    assert_eq!(lock(&relays).subscription(REPLAY_SID), Some(stripped));
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no replay, no timer"
    );
    let epoch = lock(&relays).entries[0].epoch;
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
}

#[test]
fn an_unrelated_acknowledgement_completes_nothing() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let epoch = lock(&relays).mint();
    let timer = log::diag::timed(
        &hub.diag,
        contract::diag::Op::HubSubscribeReplay,
        None,
        None,
    )
    .expect("debug times the replay");
    lock(&relays)
        .pending_replay
        .insert((REPLAY_SID.to_owned(), epoch, "c_replay".to_owned()), timer);
    // Another id, and another epoch, each finish nothing: the slot
    // stays until its own acknowledgement arrives.
    assert!(
        lock(&relays)
            .take_replay(REPLAY_SID, epoch, "c_other")
            .is_none()
    );
    assert!(
        lock(&relays)
            .take_replay(REPLAY_SID, epoch + 1, "c_replay")
            .is_none()
    );
    assert_eq!(lock(&relays).pending_replay.len(), 1);
    assert!(op_timings(&temp.hub_log()).is_empty());
    let guard = lock(&relays).take_replay(REPLAY_SID, epoch, "c_replay");
    drop(guard);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.subscribe_replay", 0);
}

#[test]
fn a_failed_transfer_is_stamped_for_the_next_drain() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    // A live entry whose session is gone: the write fails at once.
    let (dead, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let epoch = lock(&relays).mint();
    lock(&relays).entries.push(Relay {
        session: REPLAY_SID.to_owned(),
        epoch,
        writer: dead,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    });
    assert!(lock(&relays).transfer(REPLAY_SID, &subscribe_line("c_old", "full")));
    assert!(
        lock(&relays).pending_replay.is_empty(),
        "no pending slot survives a failed write"
    );
    assert_eq!(
        lock(&relays).completed_replay.len(),
        1,
        "the failure waits in the completed queue"
    );
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "a queued failure emits nothing yet"
    );
    // Deferred cleanup past the failure adds no duration: the stamp was
    // taken at the write, not at the drain.
    // No code under test waits on the clock here; no thread waits: take_completed_replays is a synchronous call on the test's thread (ruling 6179406086).
    temp.clock.advance(Duration::from_secs(5));
    let failed = lock(&relays).take_completed_replays();
    drop(failed);
    assert_op_line(&op_timings(&temp.hub_log()), "hub.subscribe_replay", 0);
}

#[test]
fn a_transfer_with_no_live_relay_starts_no_timer() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    // Nothing is relayed: the level is only kept, for the next open to
    // replay, which starts its own timer then.
    assert!(!lock(&relays).transfer(REPLAY_SID, &subscribe_line("c_old", "full")));
    assert_eq!(
        lock(&relays).subscription(REPLAY_SID),
        Some(subscribe_line("c_old", "full"))
    );
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(lock(&relays).completed_replay.is_empty());
    assert!(op_timings(&temp.hub_log()).is_empty());
}

#[test]
fn a_transfer_onto_a_held_subscription_starts_no_timer() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    lock(&relays)
        .subscribed
        .push((REPLAY_SID.to_owned(), subscribe_line("c_kept", "full")));
    // Already held: nothing is written and nothing is timed.
    assert!(lock(&relays).transfer(REPLAY_SID, &subscribe_line("c_old", "full")));
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(lock(&relays).completed_replay.is_empty());
    assert!(op_timings(&temp.hub_log()).is_empty());
}

#[test]
fn a_refused_admission_releases_the_replay_guard() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    // A live relay for the session: the exclusive attach below admits
    // nothing, through the existing admission seam.
    let (live_hub_end, _live_session_end) = UnixStream::pair().unwrap();
    let epoch = lock(&relays).mint();
    lock(&relays).entries.push(Relay {
        session: REPLAY_SID.to_owned(),
        epoch,
        writer: live_hub_end,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    });
    let (hub_end, _session_end) = UnixStream::pair().unwrap();
    crate::relay::attach(
        REPLAY_SID,
        hub_end,
        &hub,
        &writer,
        &relays,
        Some(subscribe_line("c_kept", "full")),
        None,
        true,
    );
    // The replay was written, then admission refused it: the guard is
    // gone with the slot, emitted once, and no entry was added.
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    assert_eq!(lock(&relays).entries.len(), 1);
}

#[test]
fn an_old_worker_ending_keeps_the_replacement_slot() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    let (first_end, first_id) = replaying(&hub, &writer, &relays, REPLAY_SID, &temp, 1);
    let first_epoch = lock(&relays).entries[0].epoch;
    let (second_end, second_id) = replaying(&hub, &writer, &relays, REPLAY_SID, &temp, 2);
    // Two epochs, two outstanding replays on one session.
    assert_eq!(lock(&relays).pending_replay.len(), 2);
    // The old worker ends before any acknowledgement: only its own
    // epoch's slot completes, with a zero wait.
    let first_thread = take_thread(&relays, REPLAY_SID, first_epoch);
    drop(first_end);
    join_relay(first_thread);
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert_eq!(
        lock(&relays).pending_replay.len(),
        1,
        "the replacement slot survives"
    );
    // The replacement's acknowledgement completes its own slot.
    let mut second_end = second_end;
    second_end.write_all(&ack_bytes(&second_id, true)).unwrap();
    second_end.flush().unwrap();
    assert_eq!(await_op_lines(&temp, 2).len(), 2);
    assert!(lock(&relays).pending_replay.is_empty());
    let second_epoch = lock(&relays)
        .entries
        .iter()
        .find(|entry| entry.session == REPLAY_SID && entry.epoch != first_epoch)
        .map(|entry| entry.epoch)
        .unwrap();
    let second_thread = take_thread(&relays, REPLAY_SID, second_epoch);
    drop(second_end);
    join_relay(second_thread);
    assert_eq!(
        await_op_lines(&temp, 2).len(),
        2,
        "no second completion on disconnect"
    );
    let _ = first_id;
}

/// A transfer that answers while still holding the relays lock: the
/// relay thread observes its acknowledgement through the existing
/// after-write and after-filter hooks, and still completes its timer
/// without leaking the acknowledgement or deadlocking.
#[test]
fn a_transfer_that_answers_mid_write_completes_its_timer() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (written_tx, written_rx) = mpsc::channel();
    let (release_transfer_tx, release_transfer_rx) = mpsc::channel();
    lock(&relays).after_transfer_write = Some(Box::new(move || {
        written_tx.send(()).unwrap_or(());
        Deadline::start()
            .recv_or_fail(&release_transfer_rx, "the test releases the transfer pause");
    }));
    let (filter_tx, filter_rx) = mpsc::channel();
    let (release_filter_tx, release_filter_rx) = mpsc::channel();
    *lock(&hub.after_replay_filter) = Some(Box::new(move |_line, muted| {
        filter_tx.send(muted).unwrap_or(());
        Deadline::start().recv_or_fail(&release_filter_rx, "the test releases the relay pause");
    }));
    let (writer, client_read) = client();
    // A live relay with no replay of its own: the transfer below
    // carries the subscription onto it.
    let (attach_end, session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    attach(
        REPLAY_SID, attach_end, &hub, &writer, &relays, None, None, false,
    );
    let epoch = lock(&relays).entries[0].epoch;
    let line = subscribe_line("c_old", "full");
    let (transferred_tx, transferred_rx) = mpsc::channel();
    let transferring = Arc::clone(&relays);
    thread::Builder::new()
        .name("hub-test-transfer".to_owned())
        .spawn(move || {
            transferred_tx
                .send(lock(&transferring).transfer(REPLAY_SID, &line))
                .unwrap_or(());
        })
        .unwrap();
    // The transfer wrote and paused holding the relays lock: answer
    // now, so the acknowledgement lands mid-write.
    assert!(
        Deadline::after(DEADLINE).recv(&written_rx).is_ok(),
        "transfer reaches its pause"
    );
    let mut buf = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut buf)
        .expect("the transfer writes the level");
    let sent: Value = serde_json::from_str(buf.trim_end()).unwrap();
    let id = sent.get("id").cloned().unwrap();
    assert_ne!(
        id,
        json!("c_old"),
        "the transfer carries an id of the hub's own"
    );
    let mut session_end = session_end;
    session_end.write_all(&ack_bytes(&id, true)).unwrap();
    session_end.flush().unwrap();
    // The relay filtered the acknowledgement while transfer still holds
    // the lock: the timer cannot complete until the transfer returns.
    // This is the bad interleaving if the worker waited on the lock
    // before running the filter hook.
    assert_eq!(
        Deadline::after(DEADLINE).recv(&filter_rx).ok(),
        Some(true),
        "the acknowledgement is filtered before transfer resumes"
    );
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "no completed timer while transfer holds the lock"
    );
    release_transfer_tx.send(()).unwrap_or(());
    assert_eq!(
        Deadline::after(DEADLINE).recv(&transferred_rx).ok(),
        Some(true),
        "the transfer completed"
    );
    release_filter_tx.send(()).unwrap_or(());
    // The worker completed its slot after the transfer returned, and
    // dropped the hub-only acknowledgement: a live line behind it is
    // what the client reads first.
    session_end
        .write_all(b"{\"kind\":\"session_status\",\"ts\":1,\"schema_version\":1,\"payload\":{}}\n")
        .unwrap();
    session_end.flush().unwrap();
    assert_eq!(client_line(&client_read)["kind"], json!("session_status"));
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
}

/// A rewind follow whose transfer fails: the failed write is stamped
/// into the completed queue, and the calling worker emits it after
/// follow returns, without waiting for the target reader.
fn follow_with_dead_target(temp: &Temp, old: &str, next: &str) -> (Arc<Hub>, Arc<Mutex<Relays>>) {
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    // The target relay: a parked reader that stays alive, and a writer
    // whose session is gone, so the transfer fails at once. A drain
    // that waited for the reader would hang here instead of emitting.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let parked = thread::Builder::new()
        .name("hub-test-parked-reader".to_owned())
        .spawn(move || {
            stop_rx.recv().unwrap_or(());
        })
        .unwrap();
    let (dead, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let epoch = lock(&relays).mint();
    lock(&relays).entries.push(Relay {
        session: next.to_owned(),
        epoch,
        writer: dead,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: Some(parked),
        retiring: None,
    });
    // The old session's log continues into the next one, at the kept
    // level the follow transfers.
    let workspace = temp.workspace();
    temp.recorded_for(old, &workspace);
    temp.append_line(
        old,
        &json!({
            "kind": "rewound", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
            "payload": {"new_session_id": next, "seq": 3, "jobs": []},
        }),
    );
    lock(&relays)
        .subscribed
        .push((old.to_owned(), subscribe_line("c_old", "full")));
    // The old worker, with no replay of its own: ending it runs the
    // follow that fails its transfer onto the dead target.
    let (hub_end, session_end) = UnixStream::pair().unwrap();
    attach(old, hub_end, &hub, &writer, &relays, None, None, false);
    let old_epoch = lock(&relays)
        .entries
        .iter()
        .find(|entry| entry.session == old)
        .map(|entry| entry.epoch)
        .unwrap();
    let old_thread = take_thread(&relays, old, old_epoch);
    drop(session_end);
    join_relay(old_thread);
    drop(stop_tx);
    (hub, relays)
}

#[test]
fn a_failed_follow_transfer_emits_after_follow_returns() {
    let temp = Temp::new();
    let next = "s_ffffffffffffffff";
    let old = "s_1111111111111111";
    let (_hub, relays) = follow_with_dead_target(&temp, old, next);
    // One stamped failure, emitted by the calling worker's post-follow
    // drain even though the target reader never exited.
    assert_op_line(&await_op_lines(&temp, 1), "hub.subscribe_replay", 0);
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(lock(&relays).completed_replay.is_empty());
    assert!(
        lock(&relays)
            .entries
            .iter()
            .any(|entry| entry.session == next),
        "the dead target entry stays for its reader"
    );
}

#[test]
fn concurrent_failed_follows_emit_each_failure_once() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let relays = timing_relays(&hub);
    let (writer, _client_read) = client();
    // Two old sessions rewound into two dead targets: each follow
    // transfers onto its own live entry, fails its write, and races
    // the other through the shared completed queue. One shared target
    // would hold the level after the first transfer, so the second
    // follow would correctly transfer nothing.
    let workspace = temp.workspace();
    let mut stops = Vec::new();
    for (old, next) in [
        ("s_1111111111111111", "s_ffffffffffffffff"),
        ("s_2222222222222222", "s_eeeeeeeeeeeeeeee"),
    ] {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let parked = thread::Builder::new()
            .name("hub-test-parked-reader".to_owned())
            .spawn(move || {
                stop_rx.recv().unwrap_or(());
            })
            .unwrap();
        let (dead, peer) = UnixStream::pair().unwrap();
        drop(peer);
        let epoch = lock(&relays).mint();
        lock(&relays).entries.push(Relay {
            session: next.to_owned(),
            epoch,
            writer: dead,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: Some(parked),
            retiring: None,
        });
        temp.recorded_for(old, &workspace);
        temp.append_line(
            old,
            &json!({
                "kind": "rewound", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
                "payload": {"new_session_id": next, "seq": 3, "jobs": []},
            }),
        );
        lock(&relays)
            .subscribed
            .push((old.to_owned(), subscribe_line("c_old", "full")));
        let (hub_end, session_end) = UnixStream::pair().unwrap();
        attach(old, hub_end, &hub, &writer, &relays, None, None, false);
        drop(session_end);
        stops.push(stop_tx);
    }
    // Both workers end on their own: whatever interleaving the two
    // post-follow drains take, each failure emits exactly once.
    let lines = await_op_lines(&temp, 2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    for line in &lines {
        assert_eq!(line["data"]["op"], json!("hub.subscribe_replay"), "{line}");
        assert!(line["data"]["ms"].is_u64(), "{line}");
    }
    // Any guard that landed after both drains still waits: drain it on
    // the test thread and prove the total stays two.
    let leftovers = lock(&relays).take_completed_replays();
    drop(leftovers);
    drop(stops);
    // The workers ended; their handles join cleanly.
    for old in ["s_1111111111111111", "s_2222222222222222"] {
        let ended = lock(&relays)
            .entries
            .iter_mut()
            .find(|entry| entry.session == old)
            .and_then(|entry| entry.thread.take());
        if let Some(thread) = ended {
            join_relay(thread);
        }
    }
    assert_eq!(
        await_op_lines(&temp, 2).len(),
        2,
        "each failure emitted exactly once"
    );
}

#[test]
fn info_connections_time_no_replay() {
    let temp = Temp::new();
    let hub = temp.hub_info(FakeStarter::hang(&temp.dir));
    // An info connection keeps the fixture default: no timing hub, so
    // neither the attach replay nor the transfer below starts a timer.
    let relays = Arc::new(Mutex::new(Relays::default()));
    let (writer, client_read) = client();
    let (hub_end, mut session_end) = UnixStream::pair().unwrap();
    session_end.set_read_timeout(Some(DEADLINE)).unwrap();
    attach(
        REPLAY_SID,
        hub_end,
        &hub,
        &writer,
        &relays,
        Some(subscribe_line("c_kept", "full")),
        None,
        false,
    );
    let mut line = String::new();
    BufReader::new(session_end.try_clone().unwrap())
        .read_line(&mut line)
        .expect("the hub replays the kept subscription");
    let replay: Value = serde_json::from_str(line.trim_end()).unwrap();
    session_end
        .write_all(&ack_bytes(&replay["id"], true))
        .unwrap();
    session_end.flush().unwrap();
    session_end
        .write_all(b"{\"kind\":\"session_status\",\"ts\":1,\"schema_version\":1,\"payload\":{}}\n")
        .unwrap();
    session_end.flush().unwrap();
    assert_eq!(client_line(&client_read)["kind"], json!("session_status"));
    assert!(lock(&relays).pending_replay.is_empty());
    assert!(lock(&relays).completed_replay.is_empty());
    assert!(
        op_timings(&temp.hub_log()).is_empty(),
        "info writes no replay timer"
    );
    let epoch = lock(&relays).entries[0].epoch;
    let thread = take_thread(&relays, REPLAY_SID, epoch);
    drop(session_end);
    join_relay(thread);
}
