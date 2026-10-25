//! Tests for starting the session a `rewound` line names: reading the
//! pointer, starting it from the relay before the acknowledgement, the
//! redirect after the old socket closes, the per-session start lock, and a
//! failed start's diagnostic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::{CommandId, ErrorCode};
use serde_json::{Map, Value, json};

use super::*;
use crate::Started;
use crate::Starter;
use crate::connection::serve_connection;
use crate::fake::{FakeStarter, Handshake, failure};
use crate::start::START_DEADLINE;
use crate::testkit::{id, write_log};
use fakes::Deadline;

/// One named deadline per wait: every start and acknowledgement lands
/// before it.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long the negative wait in `race_openers` holds the second opener's
/// pause unreached: the same 200 ms bound the fakes hold tests use for
/// the same kind of wait.
const GATE_QUIET: Duration = Duration::from_millis(200);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<fakes::clock::FakeClock>,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hr");
        Self {
            dir,
            held,
            clock: fakes::clock::FakeClock::new(),
        }
    }

    fn hub(&self, starter: impl Starter + 'static) -> Arc<Hub> {
        let clock = Arc::clone(&self.clock);
        let timed: Arc<dyn contract::clock::Clock> = clock;
        Arc::new(crate::testkit::hub(&self.dir, Arc::new(starter), timed))
    }

    fn workspace(&self) -> String {
        let workspace = self.dir.join("w");
        fs::create_dir_all(&workspace).unwrap();
        workspace.to_string_lossy().into_owned()
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }
}

fn rewound_line(old: &str, next: &str) -> Value {
    json!({
        "kind": "rewound", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
        "payload": {"new_session_id": next, "seq": 3, "jobs": []},
    })
}

fn exited_line(old: &str) -> Value {
    json!({
        "kind": "fiber_exited", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
        "payload": {},
    })
}

#[test]
fn a_next_session_has_one_start_lock_and_each_session_its_own() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let first = SessionId(id(2));
    let other = SessionId(id(3));
    let lock = guard_for(&hub, &first);
    assert!(Arc::ptr_eq(&lock, &guard_for(&hub, &first)));
    assert!(!Arc::ptr_eq(&lock, &guard_for(&hub, &other)));
}

#[test]
fn continued_names_the_rewound_session() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    assert_eq!(continued(&temp.dir, &SessionId(old)), Some(SessionId(next)));
}

#[test]
fn continued_ignores_anything_but_a_well_formed_rewound() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    // An exit starts nothing.
    let exited = id(1);
    write_log(
        &temp.dir,
        &exited,
        &workspace,
        None,
        &[exited_line(&exited)],
    );
    assert_eq!(continued(&temp.dir, &SessionId(exited)), None);
    // A missing log starts nothing.
    assert_eq!(continued(&temp.dir, &SessionId(id(2))), None);
    // A `rewound` naming no minted session starts nothing.
    let odd = id(3);
    write_log(
        &temp.dir,
        &odd,
        &workspace,
        None,
        &[json!({
            "kind": "rewound", "session_id": odd.clone(), "ts": 2, "schema_version": 1, "seq": 5,
            "payload": {"new_session_id": "nope", "seq": 3, "jobs": []},
        })],
    );
    assert_eq!(continued(&temp.dir, &SessionId(odd)), None);
}

#[test]
fn start_connects_when_the_next_session_runs() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let next = id(2);
    let bound = crate::fake::FakeSession::bind(&temp.dir, &next);
    assert!(reach(&hub, &SessionId(id(1)), &SessionId(next.clone())).is_ok());
    assert!(
        starter.rewound().is_empty(),
        "a running session is attached to"
    );
    assert!(
        starter.resumed().is_empty(),
        "no resume for a running session"
    );
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&next)).is_ok(),
        "the socket accepts"
    );
    bound.close();
}

#[test]
fn start_resumes_a_next_session_with_a_log() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    write_log(&temp.dir, &next, &workspace, None, &[exited_line(&next)]);
    assert!(reach(&hub, &SessionId(from), &SessionId(next.clone())).is_ok());
    assert!(starter.rewound().is_empty(), "a logged session is resumed");
    assert_eq!(
        starter.resumed(),
        vec![(SessionId(next), PathBuf::from(workspace))],
        "resumed in the workspace its own log recorded"
    );
}

#[test]
fn start_starts_a_next_session_without_a_log() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    assert!(reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).is_ok());
    assert_eq!(
        starter.rewound(),
        vec![(
            SessionId(next.clone()),
            PathBuf::from(workspace),
            SessionId(from)
        )],
        "started in the old session's workspace, for its rewind"
    );
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&next)).is_ok(),
        "the new session's socket accepts"
    );
}

#[test]
fn two_starts_of_one_session_start_once() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let other = thread::Builder::new()
        .name("second-start".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, next) = (from.clone(), next.clone());
            move || reach(&hub, &SessionId(from), &SessionId(next))
        })
        .unwrap();
    assert!(reach(&hub, &SessionId(from), &SessionId(next)).is_ok());
    assert!(other.join().unwrap().is_ok());
    assert_eq!(starter.rewound().len(), 1, "one starter call");
}

/// A starter whose `rewind` of one session waits for a release: every
/// other session still starts.
struct GateStarter {
    inner: FakeStarter,
    held: SessionId,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Starter for GateStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
        continues: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        self.inner
            .start(id, workspace, model, overrides, worktree, continues)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        self.inner.resume(id, workspace, credential)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &std::path::Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        if *id == self.held {
            self.entered.send(()).unwrap_or(());
            if let Some(release) = lock(&self.release).take() {
                release.recv().unwrap_or(());
            }
        }
        self.inner.rewind(id, workspace, from)
    }
}

#[test]
fn a_slow_start_of_one_session_does_not_block_another() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let held = id(2);
    let other = id(3);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let starter = GateStarter {
        inner: FakeStarter::bind_and_hold(&temp.dir),
        held: SessionId(held.clone()),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    };
    let hub = temp.hub(starter);
    let slow = thread::Builder::new()
        .name("slow-rewind".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let from = from.clone();
            move || drop(reach(&hub, &SessionId(from), &SessionId(held)))
        })
        .unwrap();
    // The slow start is inside `rewind`, holding only its own session's
    // lock: no sleep, the channel says so.
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the slow start reached the starter"
    );
    // On a thread with a named deadline: a shared-lock regression would
    // block it past the deadline instead of hanging the test.
    let (done_tx, done) = mpsc::channel();
    thread::Builder::new()
        .name("rival-start".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, other) = (from.clone(), other.clone());
            move || {
                done_tx
                    .send(reach(&hub, &SessionId(from), &SessionId(other)))
                    .unwrap_or(());
            }
        })
        .unwrap();
    let stream = Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the other session starts while the slow one waits");
    assert!(stream.is_ok());
    assert!(
        UnixStream::connect(temp.dir.join("run").join(&other)).is_ok(),
        "the other session's socket accepts"
    );
    drop(release_tx);
    slow.join().unwrap();
}

#[test]
fn a_rewind_in_flight_is_stopped_and_refused_when_the_store_lands() {
    // The sessions stop lands while the new session's start is held at
    // the starter: the refuse kills the started session, exactly once.
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let inner = FakeStarter::hang(&temp.dir);
    let starter = GateStarter {
        inner: inner.clone(),
        held: SessionId(next.clone()),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    };
    let hub = temp.hub(starter);
    let finished = thread::Builder::new()
        .name("hub-test-rewind-stop".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, next) = (from.clone(), next.clone());
            move || reach(&hub, &SessionId(from), &SessionId(next))
        })
        .unwrap();
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the rewind reaches the starter"
    );
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 1);
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    drop(release_tx);
    let Err(refused) = finished.join().expect("the rewind answers") else {
        panic!("the rewind in flight is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert_eq!(temp.clock.now(), temp.clock.origin());
    // The refused bind kills the session it started.
    assert_eq!(inner.kills(), [SessionId(next)]);
}

#[test]
fn a_rewind_after_the_store_is_refused_at_entry() {
    let temp = Temp::new();
    let inner = FakeStarter::hang(&temp.dir);
    let hub = temp.hub(inner.clone());
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    // Admitted after the store: refused at once, the starter never run.
    let Err(refused) = reach(&hub, &SessionId(id(1)), &SessionId(id(2))) else {
        panic!("the rewind after the store is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert!(inner.rewound().is_empty(), "the starter never ran");
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_rewind_passes_the_nested_stopping_refusal_through() {
    // The new session is logged, so the reach resumes it: the nested
    // resume admits while the test holds the resume gate, then refuses
    // once the store lands, and the refusal passes through unchanged.
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &next, &workspace, None, &[]);
    let inner = FakeStarter::hang(&temp.dir);
    let hub = temp.hub(inner.clone());
    let _gate = support::lock(&hub.resume_gate);
    let finished = thread::Builder::new()
        .name("hub-test-rewind-nested".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let (from, next) = (from.clone(), next.clone());
            move || reach(&hub, &SessionId(from), &SessionId(next))
        })
        .unwrap();
    // The nested resume admitted behind the reach: two binds in flight.
    crate::testkit::await_true("the nested resume admits", {
        let hub = Arc::clone(&hub);
        move || hub.inflight_binds.load(Ordering::SeqCst) == 2
    });
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    drop(_gate);
    let Err(refused) = finished.join().expect("the rewind answers") else {
        panic!("the nested refusal passes through");
    };
    assert_eq!(refused, crate::connection::stopping());
    // The nested resume refused on its loop re-check, before any
    // starter call: neither resume nor rewind ran, and the base
    // wording never appears.
    assert!(inner.resumed().is_empty(), "the nested resume never binds");
    assert!(inner.rewound().is_empty(), "the reach resumed, not rewound");
    assert!(inner.kills().is_empty(), "nothing started, nothing killed");
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
}

/// A starter that fails every `rewind` with `message` as its io error text.
struct FailRewind {
    message: String,
}

impl Starter for FailRewind {
    fn start(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _model: Option<&str>,
        _overrides: &[&str],
        _worktree: bool,
        _continues: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        Err(std::io::Error::other("unused"))
    }

    fn resume(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        Err(std::io::Error::other("unused"))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &std::path::Path,
        _from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        Err(std::io::Error::other(self.message.clone()))
    }
}

#[test]
fn a_failed_start_writes_a_diagnostic_without_the_detail() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let workspace = temp.workspace();
    let hub = temp.hub(FailRewind {
        message: format!("starter blew up on {SECRET}."),
    });
    let from = id(1);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let refused = reach(&hub, &SessionId(from), &SessionId(id(2))).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(refused.message, "The new session could not start.");
    let log = temp.hub_log();
    assert!(log.contains("\"code\":\"io_failed\""), "{log}");
    assert!(log.contains("could not start."), "{log}");
    assert!(!log.contains(SECRET), "no starter text in the log: {log}");
}

/// Holds `from`'s session lock, as its running process does: the test
/// first creates the file, because `write_log` creates only
/// `events.jsonl`, then locks it and keeps the guard to the end.
fn hold_old_lock(temp: &Temp, from: &str) -> File {
    let dir = temp
        .dir
        .join("projects")
        .join("-w")
        .join("sessions")
        .join(from);
    fs::write(dir.join("session.lock"), b"").unwrap();
    let file = File::open(dir.join("session.lock")).unwrap();
    file.try_lock().unwrap();
    file
}

#[test]
fn a_start_the_new_process_refuses_is_refused_with_its_own_code() {
    let codes = [
        ErrorCode::SessionHeld,
        ErrorCode::ExtensionRequiredFailed,
        ErrorCode::McpRequiredServerFailed,
        ErrorCode::AuthenticationFailed,
        ErrorCode::ConnectionFailed,
    ];
    for code in codes {
        let temp = Temp::new();
        let workspace = temp.workspace();
        let from = id(1);
        let next = id(2);
        write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
        let _held = hold_old_lock(&temp, &from);
        let origin = temp.clock.origin();
        let starter = FakeStarter::exit_with(&temp.dir, failure(code.clone(), "msg"));
        let hub = temp.hub(starter.clone());
        let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
        assert_eq!(refused.code, code);
        assert_eq!(refused.message, "msg");
        assert_eq!(starter.rewound().len(), 1, "the start was attempted once");
        assert_eq!(temp.clock.now(), origin, "the refusal waits for nothing");
        let dir = temp
            .dir
            .join("projects")
            .join("-w")
            .join("sessions")
            .join(&from);
        let probe = File::open(dir.join("session.lock")).unwrap();
        assert!(
            probe.try_lock().is_err(),
            "the refusal neither reads nor releases the old lock"
        );
    }
}

#[test]
fn a_start_message_that_names_the_new_session_is_not_forwarded() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let _held = hold_old_lock(&temp, &from);
    let message = format!("Session `{from}` was not rewound for session `{next}`.");
    let starter = FakeStarter::exit_with(&temp.dir, failure(ErrorCode::InvalidArguments, &message));
    let hub = temp.hub(starter.clone());
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "The new session could not start.");
    assert!(
        !refused.message.contains(&next),
        "no new session id reaches the client"
    );
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let _held = hold_old_lock(&temp, &from);
    let starter = FakeStarter::exit_with(
        &temp.dir,
        failure(ErrorCode::NoModel, "No model is configured."),
    );
    let hub = temp.hub(starter.clone());
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::NoModel);
    assert_eq!(refused.message, "No model is configured.");
}

#[test]
fn a_start_that_exits_io_failed_is_refused_with_the_fixed_sentence() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let _held = hold_old_lock(&temp, &from);
    let starter = FakeStarter::exit_with(&temp.dir, failure(ErrorCode::IoFailed, "detail text"));
    let hub = temp.hub(starter.clone());
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(refused.message, "The new session could not start.");
}

#[test]
fn a_start_that_never_binds_is_refused_io_failed() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let _held = hold_old_lock(&temp, &from);
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(refused.message, "The new session could not start.");
    assert!(
        temp.clock.now() >= temp.clock.origin() + START_DEADLINE,
        "the wait lasts the whole deadline"
    );
}

#[test]
fn an_unreachable_run_directory_is_refused_io_failed() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    let _held = hold_old_lock(&temp, &from);
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(refused.message, "The new session could not start.");
}

#[test]
fn a_refused_resume_of_the_new_session_keeps_its_code() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let from = id(1);
    let next = id(2);
    write_log(&temp.dir, &from, &workspace, None, &[exited_line(&from)]);
    write_log(&temp.dir, &next, &workspace, None, &[]);
    let _held = hold_old_lock(&temp, &from);
    let origin = temp.clock.origin();
    let starter = FakeStarter::held_then_bind(&temp.dir, usize::MAX);
    let hub = temp.hub(starter.clone());
    let refused = reach(&hub, &SessionId(from.clone()), &SessionId(next.clone())).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert_eq!(refused.message, "The new session could not be resumed.");
    assert!(
        !refused.message.contains(&next) && !refused.message.contains(&from),
        "no session id reaches the client"
    );
    assert!(starter.rewound().is_empty(), "a logged session is resumed");
    assert_eq!(starter.resumed().len(), 1, "resumed once");
    assert_eq!(temp.clock.now(), origin, "the refusal waits for nothing");
}

/// One side of a client connection: the hub serves the other end.
struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Client {
    fn connect(hub: &Arc<Hub>) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        let hub = Arc::clone(hub);
        thread::Builder::new()
            .name("hub-conn".to_owned())
            .spawn(move || serve_connection(a, hub))
            .unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let reader = b.try_clone().unwrap();
        Self {
            write: b,
            read: BufReader::new(reader),
        }
    }

    fn send(&mut self, line: &Value) {
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    fn next(&mut self, what: &str) -> Value {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the hub closed before {what}");
        serde_json::from_str(&text).unwrap()
    }
}

/// Serves one old session: answers `subscribe`, writes the `rewound`
/// line naming `next`, then answers `rewind` naming `next`, then holds
/// the socket open until `release` before closing, as a session closing
/// with `rewound` does. The listener binds before the thread starts, so
/// a client routing first always finds it.
fn serve_old(
    listener: UnixListener,
    old: String,
    next: String,
    release: mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("old-session".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut read = BufReader::new(stream.try_clone().unwrap());
            let mut write = stream;
            let mut buf = String::new();
            read.read_line(&mut buf).unwrap();
            let id: Value = serde_json::from_str(&buf).unwrap();
            accept(&mut write, &id);
            buf.clear();
            read.read_line(&mut buf).unwrap();
            let rewind: Value = serde_json::from_str(&buf).unwrap();
            let id = rewind.get("id").cloned().unwrap();
            let event = rewound_line(&old, &next);
            let mut bytes = serde_json::to_vec(&event).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).unwrap();
            write.flush().unwrap();
            let ack = json!({
                "kind": "command_accepted", "ts": 1, "schema_version": 1,
                "payload": {"command_id": id, "result": {"new_session_id": next}},
            });
            let mut bytes = serde_json::to_vec(&ack).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).unwrap();
            write.flush().unwrap();
            // Hold the socket open: the client commands the new session
            // while this relay still waits for EOF.
            release.recv().unwrap_or(());
        })
        .unwrap()
}

fn accept(write: &mut UnixStream, command: &Value) {
    let id = command.get("id").cloned().unwrap();
    let ack = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": id},
    });
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    write.write_all(&bytes).unwrap();
    write.flush().unwrap();
}

#[test]
fn a_relayed_rewind_starts_and_redirects_before_the_next_command() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: true,
            code: String::new(),
            message: String::new(),
        },
    );
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    // The old session's log already ends `rewound`: the redirect reads it
    // once the old socket closes.
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&old)).unwrap();
    let (release_tx, release) = mpsc::channel::<()>();
    let old_session = serve_old(listener, old.clone(), next.clone(), release);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub1", "session_id": old, "command": "subscribe", "args": {"level": "full"},
    }));
    let subscribed = client.next("the subscribe acknowledgement");
    assert_eq!(subscribed["kind"], "command_accepted");
    client.send(&json!({
        "id": "c_rw1", "session_id": old, "command": "rewind", "args": {},
    }));
    let event = client.next("the old session's rewound event");
    assert_eq!(event["kind"], "rewound");
    assert_eq!(event["payload"]["new_session_id"], next);
    let accepted = client.next("the rewind acknowledgement");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(
        accepted["payload"]["result"]["new_session_id"], next,
        "the client learns the new session"
    );
    assert_eq!(
        starter.rewound(),
        vec![(
            SessionId(next.clone()),
            PathBuf::from(workspace),
            SessionId(old.clone())
        )],
        "started once, in the old workspace, for its rewind"
    );
    drop(release_tx);
    old_session.join().unwrap();
    // The old socket closed: the connection is subscribed to the new
    // session at its kept level, under a hub-minted id it never sees.
    client.send(&json!({
        "id": "c_p1", "session_id": next, "command": "prompt",
        "args": {"content": [{"type": "text", "text": "three"}]},
    }));
    let prompted = client.next("the prompt acknowledgement");
    assert_eq!(prompted["kind"], "command_accepted");
    assert_eq!(prompted["payload"]["command_id"], "c_p1");
}

#[test]
fn a_command_for_the_new_session_first_still_gets_its_level() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&old)).unwrap();
    let (release_tx, release) = mpsc::channel::<()>();
    let old_session = serve_old(listener, old.clone(), next.clone(), release);
    let mut client = Client::connect(&hub);
    assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
    client.send(&json!({
        "id": "c_sub1", "session_id": old, "command": "subscribe", "args": {"level": "full"},
    }));
    assert_eq!(
        client.next("the subscribe acknowledgement")["kind"],
        "command_accepted"
    );
    client.send(&json!({
        "id": "c_rw1", "session_id": old, "command": "rewind", "args": {},
    }));
    let event = client.next("the old session's rewound event");
    assert_eq!(event["kind"], "rewound");
    assert_eq!(event["payload"]["new_session_id"], next);
    let accepted = client.next("the rewind acknowledgement");
    assert_eq!(accepted["payload"]["result"]["new_session_id"], next);
    // The interleaving: the client commands the new session while the old
    // relay still waits for EOF, so it relays unsubscribed. Any other
    // command is accepted and held open, as a session that takes it does.
    client.send(&json!({
        "id": "c_t1", "session_id": next, "command": "tools", "args": {},
    }));
    let tools = client.next("the tools acknowledgement");
    assert_eq!(tools["kind"], "command_accepted");
    assert_eq!(tools["payload"]["command_id"], "c_t1");
    // Only now does the old socket close: the redirect transfers the kept
    // level onto that relay instead of starting a second one.
    drop(release_tx);
    old_session.join().unwrap();
    // Both the tools command and the transfer's subscribe reached the new
    // session before this returns.
    assert!(
        starter.await_received(2, DEADLINE),
        "the tools command and the transfer arrive"
    );
    let sent: Vec<String> = starter
        .received()
        .into_iter()
        .filter(|line| line.contains("\"subscribe\""))
        .collect();
    assert_eq!(sent.len(), 1, "the transfer subscribes once: {sent:?}");
    let replay: Value = serde_json::from_str(&sent[0]).unwrap();
    assert_eq!(replay["args"], json!({"level": "full"}));
    assert_ne!(
        replay["id"], "c_sub1",
        "under a hub-minted id the client never sent"
    );
    // The transfer's acknowledgement never reaches the client: every
    // acknowledgement it reads names a command it sent.
    client.send(&json!({
        "id": "c_t2", "session_id": next, "command": "tools", "args": {},
    }));
    let mut seen = Vec::new();
    let acknowledged = loop {
        let line = client.next("the second tools acknowledgement");
        if line
            .get("payload")
            .and_then(|payload| payload.get("command_id"))
            == Some(&Value::String("c_t2".to_owned()))
        {
            break line;
        }
        seen.push(line);
    };
    assert_eq!(acknowledged["kind"], "command_accepted");
    assert!(
        seen.iter().all(|line| line["kind"] != "command_accepted"),
        "no other acknowledgement in between: {seen:?}"
    );
}

#[test]
fn a_relayed_rewind_whose_start_is_refused_answers_that_code() {
    for code in [ErrorCode::SessionHeld, ErrorCode::ExtensionRequiredFailed] {
        let temp = Temp::new();
        let workspace = temp.workspace();
        let code_name: String =
            serde_json::from_value(serde_json::to_value(&code).unwrap()).unwrap();
        let starter =
            FakeStarter::exit_with(&temp.dir, failure(code.clone(), "the new process refused."));
        let hub = temp.hub(starter.clone());
        let old = id(1);
        let next = id(2);
        write_log(
            &temp.dir,
            &old,
            &workspace,
            None,
            &[rewound_line(&old, &next)],
        );
        fs::create_dir_all(temp.dir.join("run")).unwrap();
        let listener = UnixListener::bind(temp.dir.join("run").join(&old)).unwrap();
        let (release_tx, release) = mpsc::channel::<()>();
        let old_session = serve_old(listener, old.clone(), next.clone(), release);
        let mut client = Client::connect(&hub);
        assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
        client.send(&json!({
            "id": "c_sub1", "session_id": old, "command": "subscribe", "args": {"level": "full"},
        }));
        assert_eq!(
            client.next("the subscribe acknowledgement")["kind"],
            "command_accepted"
        );
        client.send(&json!({
            "id": "c_rw1", "session_id": old, "command": "rewind", "args": {},
        }));
        let event = client.next("the old session's rewound event");
        assert_eq!(event["kind"], "rewound");
        assert_eq!(event["payload"]["new_session_id"], next);
        let rejection = client.next("the rewind rejection");
        assert_eq!(rejection["kind"], "command_rejected", "{rejection}");
        assert_eq!(rejection["payload"]["command_id"], "c_rw1", "{rejection}");
        assert_eq!(rejection["payload"]["code"], code_name, "{rejection}");
        assert!(
            !serde_json::to_string(&rejection).unwrap().contains(&next),
            "the rejection carries no new session id: {rejection}"
        );
        assert_eq!(starter.rewound().len(), 1, "the start was attempted once");
        // Before the old socket is released, a command for the new
        // session names a session with no log: it is rejected
        // `session_not_found`.
        client.send(&json!({
            "id": "c_new1", "session_id": next, "command": "tools", "args": {},
        }));
        let refused = client.next("the new session's rejection");
        assert_eq!(refused["kind"], "command_rejected", "{refused}");
        assert_eq!(refused["payload"]["command_id"], "c_new1", "{refused}");
        assert_eq!(refused["payload"]["code"], "session_not_found", "{refused}");
        // The old socket closes: the redirect follows nothing, so no
        // second start runs. A later command for the old session is
        // answered, proving the relay thread ended and its `follow` ran.
        drop(release_tx);
        old_session.join().unwrap();
        client.send(&json!({
            "id": "c_old2", "session_id": old, "command": "tools", "args": {},
        }));
        let answered = client.next("the old session's later answer");
        assert_eq!(answered["payload"]["command_id"], "c_old2", "{answered}");
        assert_eq!(
            starter.rewound().len(),
            1,
            "the relay's follow started nothing"
        );
    }
}

#[test]
fn a_refused_rewind_drops_only_the_old_sessions_kept_level() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::exit_with(
        &temp.dir,
        failure(ErrorCode::SessionHeld, "the new process refused."),
    );
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    let other = id(3);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let epoch = lock(&relays).mint();
    // The connection holds a kept level for the rewound session and for
    // an untouched one: the refusal must drop only the rewound one.
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let other_level = json!({"id": "c_sub9", "command": "subscribe", "args": {"level": "summary"}})
        .as_object()
        .unwrap()
        .clone();
    lock(&relays).subscribed.push((old.clone(), level));
    lock(&relays)
        .subscribed
        .push((other.clone(), other_level.clone()));
    let kept: crate::relay::Kept = Arc::new(Mutex::new(vec![(
        "c_rw".to_owned(),
        json!({"id": "c_rw", "command": "rewind", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        true,
    )]));
    let (hub_session, mut session_peer) = UnixStream::pair().unwrap();
    let reader = hub_session.try_clone().unwrap();
    lock(&relays).entries.push(crate::relay::Relay {
        session: old.clone(),
        epoch,
        writer: hub_session,
        kept: Arc::clone(&kept),
        replayed: Default::default(),
        thread: None,
        retiring: None,
        session_pid: None,
    });
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
    let thread = crate::relay::spawn_for_test(
        old.clone(),
        epoch,
        reader,
        Arc::clone(&hub),
        Arc::clone(&client_writer),
        Arc::clone(&relays),
        kept,
    );
    // The session accepts the rewind, naming the new session: its start
    // is refused, so the relay answers the start's code instead.
    let ack = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_rw", "result": {"new_session_id": next}},
    });
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    session_peer.write_all(&bytes).unwrap();
    session_peer.flush().unwrap();
    let mut read = BufReader::new(client_read);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the client reads the refusal");
    assert!(!text.is_empty(), "the hub closed before the refusal");
    let rejection: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(rejection["kind"], "command_rejected", "{rejection}");
    assert_eq!(rejection["payload"]["command_id"], "c_rw", "{rejection}");
    assert_eq!(rejection["payload"]["code"], "session_held", "{rejection}");
    // The session socket closes: the relay ends and its redirect runs.
    // The blocking join runs on a thread of its own, so the wait carries
    // the deadline.
    drop(session_peer);
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || {
        thread.join().unwrap();
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&done).is_ok(),
        "the relay ends after its session socket closes"
    );
    // The refusal dropped the rewound session's kept level and kept the
    // untouched one, so the redirect follows nothing and starts nothing
    // more: both sides of the kept-level check.
    assert_eq!(starter.rewound().len(), 1, "the start was attempted once");
    let held = lock(&relays);
    assert!(
        held.subscription(&old).is_none(),
        "the refused rewind keeps no subscription for the old session"
    );
    assert_eq!(
        held.subscription(&other),
        Some(other_level),
        "the refusal keeps every other session's level"
    );
}

#[test]
fn a_refusal_from_a_retiring_relay_waits_for_the_acknowledgements_it_rerouted() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::exit_with(
        &temp.dir,
        failure(ErrorCode::SessionHeld, "the new process refused."),
    );
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let epoch = lock(&relays).mint();
    let kept: crate::relay::Kept = Arc::new(Mutex::new(vec![
        (
            "c_earlier".to_owned(),
            json!({"id": "c_earlier", "command": "prompt", "args": {}})
                .as_object()
                .unwrap()
                .clone(),
            true,
        ),
        (
            "c_rw".to_owned(),
            json!({"id": "c_rw", "command": "rewind", "args": {}})
                .as_object()
                .unwrap()
                .clone(),
            true,
        ),
    ]));
    let order = lock(&relays).order.clone();
    order.enqueue(&old, "c_earlier");
    order.enqueue(&old, "c_rw");
    order.mark_rerouted("c_earlier", &old, epoch);
    let (waited_tx, waited_rx) = mpsc::channel();
    *lock(&order.before_wait_rerouted) = Some(Box::new(move || {
        waited_tx.send(()).unwrap_or(());
    }));
    let (hub_session, mut session_peer) = UnixStream::pair().unwrap();
    let reader = hub_session.try_clone().unwrap();
    lock(&relays).entries.push(crate::relay::Relay {
        session: old.clone(),
        epoch,
        writer: hub_session,
        kept: Arc::clone(&kept),
        replayed: Default::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
        session_pid: None,
    });
    // A live relay for the same session answers the re-routed command.
    // It starts after the retiring relay below has read, so the
    // retiring relay attempts its forward first.
    let live = lock(&relays).mint();
    let kept_live: crate::relay::Kept = Arc::new(Mutex::new(vec![(
        "c_earlier".to_owned(),
        json!({"id": "c_earlier", "command": "prompt", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        true,
    )]));
    let (live_session, mut live_peer) = UnixStream::pair().unwrap();
    let live_reader = live_session.try_clone().unwrap();
    lock(&relays).entries.push(crate::relay::Relay {
        session: old.clone(),
        epoch: live,
        writer: live_session,
        kept: Arc::clone(&kept_live),
        replayed: Default::default(),
        thread: None,
        retiring: None,
        session_pid: None,
    });
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
    let (read_tx, read_rx) = mpsc::channel();
    *lock(&hub.after_replay_filter) = Some(Box::new(move |_, _| {
        read_tx.send(()).unwrap_or(());
    }));
    // The first acknowledgement through the ordered forward is the
    // re-routed one: a bypassed wait would forward the rejection first.
    let (first_tx, first_rx) = mpsc::channel();
    *lock(&hub.before_forward) = Some(Box::new(move |line, _| {
        first_tx
            .send(crate::relay::acknowledges(line, "c_earlier"))
            .unwrap_or(());
    }));
    let retiring_thread = crate::relay::spawn_for_test(
        old.clone(),
        epoch,
        reader,
        Arc::clone(&hub),
        Arc::clone(&client_writer),
        Arc::clone(&relays),
        kept,
    );
    let ack = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_rw", "result": {"new_session_id": next}},
    });
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    session_peer.write_all(&bytes).unwrap();
    session_peer.flush().unwrap();
    assert!(
        Deadline::after(DEADLINE).recv(&read_rx).is_ok(),
        "the relay read the rewind acknowledgement"
    );
    // The live relay starts now, past the retiring relay's first read,
    // and parks before its own first read, holding no lock. The park
    // holds the order hook's lock until released, so it arms only here,
    // once the retiring relay is past its own first read.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&order.before_read) = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    let live_thread = crate::relay::spawn_for_test(
        old.clone(),
        live,
        live_reader,
        Arc::clone(&hub),
        Arc::clone(&client_writer),
        Arc::clone(&relays),
        kept_live,
    );
    assert!(
        Deadline::after(DEADLINE).recv(&parked_rx).is_ok(),
        "the live relay parks before its first read"
    );
    // Buffer the re-routed answer where the parked live relay will
    // read it: it cannot move before the test releases it.
    let earlier = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_earlier"},
    });
    let mut bytes = serde_json::to_vec(&earlier).unwrap();
    bytes.push(b'\n');
    live_peer.write_all(&bytes).unwrap();
    live_peer.flush().unwrap();
    // Hold the live relay until the retiring relay's refusal reaches
    // the hub log, the last step before its forward attempt, so the
    // attempt comes while the handover is still unanswered. Both orders
    // below then prove the wait held: a bypassed wait forwards the
    // rejection first and fails.
    let refused = Deadline::after(DEADLINE);
    let logged = format!("Session {next} could not start");
    while !temp.hub_log().contains(&logged) {
        assert!(
            !refused.left().is_zero(),
            "the retiring relay to refuse the start"
        );
        std::thread::yield_now();
    }
    // The retiring relay has reached its ordering wait while the
    // earlier acknowledgement is still parked: only then release it,
    // so the losing order (the rejection first) cannot slip through.
    assert!(
        Deadline::after(DEADLINE).recv(&waited_rx).is_ok(),
        "the retiring relay reaches its ordering wait"
    );
    release_tx.send(()).unwrap_or(());
    assert!(
        Deadline::after(DEADLINE)
            .recv(&first_rx)
            .expect("the first acknowledgement forwards"),
        "the re-routed acknowledgement forwards first"
    );
    let mut read = BufReader::new(client_read);
    let mut text = String::new();
    read.read_line(&mut text).unwrap();
    let first: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(first["payload"]["command_id"], "c_earlier", "{first}");
    text.clear();
    read.read_line(&mut text).unwrap();
    let rejection: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(rejection["kind"], "command_rejected", "{rejection}");
    assert_eq!(rejection["payload"]["command_id"], "c_rw", "{rejection}");
    assert_eq!(rejection["payload"]["code"], "session_held", "{rejection}");
    assert!(
        !serde_json::to_string(&rejection).unwrap().contains(&next),
        "the rejection carries no new session id: {rejection}"
    );
    drop(session_peer);
    drop(live_peer);
    retiring_thread.join().unwrap();
    live_thread.join().unwrap();
}

#[test]
fn follow_leaves_a_relay_already_holding_the_level_alone() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays);
        held.subscribed.push((old.clone(), level.clone()));
        held.subscribed.push((next.clone(), level.clone()));
        let (writer, _) = UnixStream::pair().unwrap();
        let epoch = held.mint();
        held.entries.push(crate::relay::Relay {
            session: next.clone(),
            epoch,
            writer,
            kept: crate::relay::Kept::default(),
            replayed: crate::relay::Replayed::default(),
            thread: None,
            retiring: None,
            session_pid: None,
        });
    }
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    let held = lock(&relays);
    assert!(starter.rewound().is_empty(), "no second start");
    assert!(starter.resumed().is_empty(), "no resume either");
    assert_eq!(held.entries.len(), 1, "no second relay");
    assert!(
        lock(&held.entries.first().unwrap().replayed).is_empty(),
        "nothing written to the relay"
    );
    assert_eq!(held.subscription(&next), Some(level), "the level stands");
}

#[test]
fn follow_ignores_a_log_that_continues_nowhere() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    write_log(&temp.dir, &old, &workspace, None, &[exited_line(&old)]);
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    lock(&relays).subscribed.push((
        old.clone(),
        json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone(),
    ));
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    assert!(starter.rewound().is_empty(), "an exit starts nothing");
    assert!(starter.resumed().is_empty(), "an exit resumes nothing");
}

#[test]
fn follow_without_a_kept_level_starts_nothing() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    assert!(starter.rewound().is_empty(), "no level to keep");
    assert!(starter.resumed().is_empty(), "no level to keep");
}

#[test]
fn follow_transfers_the_kept_level_onto_an_unsubscribed_relay() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    // The new session's socket, bound before the test: the transfer
    // writes to the relay's connection, never the starter.
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = UnixListener::bind(temp.dir.join("run").join(&next)).unwrap();
    let (heard_tx, heard) = mpsc::channel();
    thread::Builder::new()
        .name("next-session".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(DEADLINE)).unwrap();
            let mut read = BufReader::new(stream);
            let mut line = String::new();
            let got = read.read_line(&mut line).unwrap_or(0);
            heard_tx.send((got, line)).unwrap_or(());
        })
        .unwrap();
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    {
        let mut held = lock(&relays);
        held.subscribed.push((old.clone(), level.clone()));
        // The interleaving: the client commanded the new session while the
        // old relay still waited for EOF, so this relay is unsubscribed.
        let writer = UnixStream::connect(temp.dir.join("run").join(&next)).unwrap();
        let epoch = held.mint();
        held.entries.push(crate::relay::Relay {
            session: next.clone(),
            epoch,
            writer,
            kept: crate::relay::Kept::default(),
            replayed: crate::relay::Replayed::default(),
            thread: None,
            retiring: None,
            session_pid: None,
        });
    }
    let (write, _) = UnixStream::pair().unwrap();
    follow(&hub, &Arc::new(Mutex::new(write)), &relays, &old);
    let held = lock(&relays);
    assert_eq!(held.entries.len(), 1, "no second relay");
    assert_eq!(
        held.subscription(&next),
        Some(level.clone()),
        "the level is kept for the new session"
    );
    assert!(
        starter.rewound().is_empty(),
        "no start for a running session"
    );
    let (got, line) = Deadline::after(DEADLINE)
        .recv(&heard)
        .expect("the transfer sends the level");
    assert!(got > 0, "the relay's connection carries it");
    let sent: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(sent["command"], "subscribe");
    assert_eq!(sent["args"], json!({"level": "full"}));
    let minted = sent["id"].as_str().unwrap().to_owned();
    assert_ne!(minted, "c_sub1", "under a hub-minted id");
    assert_eq!(
        lock(&held.entries.first().unwrap().replayed).clone(),
        vec![minted],
        "the relay thread drops its acknowledgement"
    );
}

/// A pre-bound session socket for the open race: counts the connections
/// it accepted, answers every command `command_accepted`, records every
/// line per connection, in accept order, and tells `heard` after every
/// line. Stays bound until `close`: closing it earlier would read as EOF
/// on the relay threads.
struct OpenSession {
    socket: PathBuf,
    accepts: Arc<Mutex<usize>>,
    accepted: Mutex<Option<mpsc::Receiver<()>>>,
    received: Arc<Mutex<Vec<Vec<String>>>>,
    heard: Mutex<Option<mpsc::Receiver<()>>>,
    closed: Arc<Mutex<bool>>,
    accept: Mutex<Option<thread::JoinHandle<()>>>,
}

impl OpenSession {
    fn bind(home: &std::path::Path, id: &str) -> Self {
        fs::create_dir_all(home.join("run")).unwrap();
        let socket = home.join("run").join(id);
        let accepts = Arc::new(Mutex::new(0));
        let (accepted_tx, accepted) = mpsc::channel();
        let received = Arc::new(Mutex::new(Vec::new()));
        let (heard_tx, heard) = mpsc::channel();
        let closed = Arc::new(Mutex::new(false));
        let listener = UnixListener::bind(&socket).unwrap();
        let accept = {
            let accepts = Arc::clone(&accepts);
            let received = Arc::clone(&received);
            let closed = Arc::clone(&closed);
            thread::Builder::new()
                .name("open-session".to_owned())
                .spawn(move || {
                    let mut index = 0;
                    while let Ok((stream, _)) = listener.accept() {
                        if *lock(&closed) {
                            return;
                        }
                        *lock(&accepts) += 1;
                        accepted_tx.send(()).unwrap_or(());
                        lock(&received).push(Vec::new());
                        let received = Arc::clone(&received);
                        let heard = heard_tx.clone();
                        thread::Builder::new()
                            .name("open-session-conn".to_owned())
                            .spawn(move || serve_open(stream, index, &received, heard))
                            .unwrap();
                        index += 1;
                    }
                })
                .unwrap()
        };
        Self {
            socket,
            accepts,
            accepted: Mutex::new(Some(accepted)),
            received,
            heard: Mutex::new(Some(heard)),
            closed,
            accept: Mutex::new(Some(accept)),
        }
    }

    fn accepts(&self) -> usize {
        *lock(&self.accepts)
    }

    /// Waits until the session has accepted `count` connections in total,
    /// so the accept loop lagging behind the openers cannot hide one.
    /// One deadline bounds the whole wait: a worker collects the accepts
    /// while the test takes its result once.
    #[track_caller]
    fn await_accepts(&self, count: usize, what: &str) {
        let accepted = lock(&self.accepted).take().expect("one wait per session");
        let (done_tx, done) = mpsc::channel();
        thread::Builder::new()
            .name("open-session-accept-wait".to_owned())
            .spawn(move || {
                let mut missing = count;
                while missing > 0 {
                    if accepted.recv().is_err() {
                        break;
                    }
                    missing -= 1;
                }
                done_tx.send(missing).unwrap_or(());
            })
            .expect("the waiter spawns");
        assert_eq!(
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&done)
                .unwrap_or(count),
            0,
            "{what} reached the session"
        );
    }

    /// Waits until the session has received `count` lines in total, so a
    /// serve thread lagging behind the openers cannot hide one. One
    /// deadline bounds the whole wait: a worker collects the lines while
    /// the test takes its result once.
    #[track_caller]
    fn await_lines(&self, count: usize, what: &str) {
        let heard = lock(&self.heard).take().expect("one wait per session");
        let (done_tx, done) = mpsc::channel();
        thread::Builder::new()
            .name("open-session-wait".to_owned())
            .spawn(move || {
                let mut missing = count;
                while missing > 0 {
                    if heard.recv().is_err() {
                        break;
                    }
                    missing -= 1;
                }
                done_tx.send(missing).unwrap_or(());
            })
            .expect("the waiter spawns");
        assert_eq!(
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&done)
                .unwrap_or(count),
            0,
            "{what} reached the session"
        );
    }

    /// Stops accepting: the probe connection wakes the accept loop, which
    /// sees the flag before counting it. Served connections stay open.
    fn close(&self) {
        *lock(&self.closed) = true;
        drop(UnixStream::connect(&self.socket));
        if let Some(accept) = lock(&self.accept).take() {
            accept.join().unwrap_or(());
        }
    }
}

/// Serves one open-race connection: records every line, answers each
/// `command_accepted` under its own id, tells `heard` after every line,
/// and holds the socket open.
fn serve_open(
    stream: UnixStream,
    index: usize,
    received: &Arc<Mutex<Vec<Vec<String>>>>,
    heard: mpsc::Sender<()>,
) {
    let mut write = stream.try_clone().unwrap();
    let mut read = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match read.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                lock(received)[index].push(line.clone());
                heard.send(()).unwrap_or(());
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|line| line.get("id").cloned())
                    .unwrap_or(Value::Null);
                let ack = json!({
                    "kind": "command_accepted", "ts": 1, "schema_version": 1,
                    "payload": {"command_id": id},
                });
                let mut bytes = serde_json::to_vec(&ack).unwrap();
                bytes.push(b'\n');
                if write
                    .write_all(&bytes)
                    .and_then(|()| write.flush())
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// The race setup both open tests share: the connection holds the level
/// on the old session, and returns the client's read end with its writer
/// and relays. The read end stays open in the test scope: every relay
/// thread forwards onto the writer, and a closed reader would fail those
/// forwards and drain the entries under test.
fn open_race(
    level: &Map<String, Value>,
    old: &str,
) -> (
    UnixStream,
    Arc<Mutex<UnixStream>>,
    Arc<Mutex<crate::relay::Relays>>,
) {
    let relays: Arc<Mutex<crate::relay::Relays>> =
        Arc::new(Mutex::new(crate::relay::Relays::default()));
    lock(&relays)
        .subscribed
        .push((old.to_owned(), level.clone()));
    let (write, read) = UnixStream::pair().unwrap();
    (read, Arc::new(Mutex::new(write)), relays)
}

/// Arms the one-shot open pause on `hub`: the opener reaching it signals
/// `paused` and waits for `release` before connecting.
fn arm_before_open(hub: &Arc<Hub>) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (paused_tx, paused) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    *lock(&hub.before_open) = Some(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release.recv().unwrap_or(());
    }));
    (paused, release_tx)
}

/// Spawns `job` on a worker and reports when it returns.
fn spawn_opener(job: impl FnOnce() + Send + 'static, name: &str) -> mpsc::Receiver<()> {
    let (done_tx, done) = mpsc::channel();
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            job();
            done_tx.send(()).unwrap_or(());
        })
        .unwrap();
    done
}

/// Parks the first opener in the open hook, then the second in the
/// re-armed hook, releases them, and joins both: the two opener orders
/// share this choreography. Without the gate both wait past their checks,
/// each about to publish; with it the second queues behind the first on
/// the gate instead of pausing, and the wait below times out. When the
/// waiter is the redirect it pauses in the hook once the gate frees it;
/// when it is the command it takes the existing relay outright.
#[track_caller]
fn race_openers(
    hub: &Arc<Hub>,
    relays: &Arc<Mutex<crate::relay::Relays>>,
    first: impl FnOnce() + Send + 'static,
    second: impl FnOnce() + Send + 'static,
    second_is_follow: bool,
) {
    let (paused_first, release_first) = arm_before_open(hub);
    let done_first = spawn_opener(first, "race-first");
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&paused_first)
            .is_ok(),
        "the first opener parked after its opening"
    );
    // Arm the gate seam now: the first opener already passed it (it is
    // parked past the gate), so only the contender signals.
    let (reached_tx, reached) = mpsc::channel();
    lock(relays).at_gate = Some(Box::new(move || {
        reached_tx.send(()).unwrap_or(());
    }));
    let (paused_second, release_second) = arm_before_open(hub);
    let done_second = spawn_opener(second, "race-second");
    // The first opener stays parked until the contender reached the
    // gate: releasing it on a thread start alone could release it
    // before any contention.
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&reached)
            .is_ok(),
        "the second opener reached the gate"
    );
    // The second opener queues behind the first on the gate instead of
    // pausing: the 200 ms bound holds the pause unreached with no signal,
    // and a late signal only takes the gated branch, where the test still
    // fails on the two relays it then finds.
    if Deadline::after(GATE_QUIET).recv(&paused_second).is_err() {
        drop(release_first);
        assert!(
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&done_first)
                .is_ok(),
            "the first opener finished"
        );
        if second_is_follow {
            assert!(
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&paused_second)
                    .is_ok(),
                "the waiter parked after the first published"
            );
            drop(release_second);
        }
        assert!(
            Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                .recv(&done_second)
                .is_ok(),
            "the second opener finished"
        );
    } else {
        // No gate: both openers are parked before publishing. Release
        // the redirect first: it is still before its check in one order
        // and publishes while the command is still parked; the command
        // is already past its check, so it publishes too.
        if second_is_follow {
            drop(release_second);
            assert!(
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_second)
                    .is_ok(),
                "the second opener finished"
            );
            drop(release_first);
            assert!(
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_first)
                    .is_ok(),
                "the first opener finished"
            );
        } else {
            drop(release_first);
            assert!(
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_first)
                    .is_ok(),
                "the first opener finished"
            );
            drop(release_second);
            assert!(
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&done_second)
                    .is_ok(),
                "the second opener finished"
            );
        }
    }
}

/// The client command for `next` as a worker job: a `tools` command, so
/// the kept level can only come from the redirect's transfer.
fn route_job(
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<crate::relay::Relays>>,
    next: &str,
) -> impl FnOnce() + Send + 'static {
    let hub = Arc::clone(hub);
    let writer = Arc::clone(writer);
    let relays = Arc::clone(relays);
    let next = next.to_owned();
    move || {
        let stripped = json!({"id": "c_cmd", "command": "tools", "args": {}})
            .as_object()
            .unwrap()
            .clone();
        crate::relay::route(
            &CommandId("c_cmd".to_owned()),
            &next,
            stripped,
            &hub,
            &writer,
            &relays,
            None,
            false,
            None,
        );
    }
}

/// The redirect from `old` as a worker job.
fn follow_job(
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<crate::relay::Relays>>,
    old: &str,
) -> impl FnOnce() + Send + 'static {
    let hub = Arc::clone(hub);
    let writer = Arc::clone(writer);
    let relays = Arc::clone(relays);
    let old = old.to_owned();
    move || follow(&hub, &writer, &relays, &old)
}

/// Asserts the race published exactly one live relay for `next`: one
/// entry, one accept, the kept level, and both lines on the one
/// connection — the client's command once, and the transfer's subscribe.
#[track_caller]
fn assert_single_relay(
    relays: &Arc<Mutex<crate::relay::Relays>>,
    session: &OpenSession,
    next: &str,
    level: &Map<String, Value>,
) {
    let held = lock(relays);
    let live = held
        .entries
        .iter()
        .filter(|entry| entry.session == next && entry.retiring.is_none())
        .count();
    assert_eq!(live, 1, "one relay for the session, not one per opener");
    assert_eq!(held.entries.len(), 1, "nothing else published");
    // The accept loop can lag behind the openers: the wait holds until
    // the connection it counted arrived.
    session.await_accepts(1, "the session's connection");
    assert_eq!(session.accepts(), 1, "the session saw one connection");
    assert_eq!(
        held.subscription(next),
        Some(level.clone()),
        "the kept level reached the relay"
    );
    drop(held);
    // A serve thread lagging behind the openers cannot hide a line: the
    // wait holds until both arrived.
    session.await_lines(2, "the command and the transfer");
    let received = lock(&session.received);
    assert_eq!(received.len(), 1, "one connection carried every line");
    assert!(
        received[0].iter().any(|line| line.contains("\"c_cmd\"")),
        "the client command reached it: {:?}",
        received[0]
    );
    assert!(
        received[0]
            .iter()
            .any(|line| line.contains("\"subscribe\"")),
        "the transfer's subscribe reached it: {:?}",
        received[0]
    );
    let commands = received
        .iter()
        .flatten()
        .filter(|line| line.contains("\"c_cmd\""))
        .count();
    assert_eq!(commands, 1, "the client command reached the session once");
}

#[test]
fn route_paused_in_before_open_shares_one_relay_with_follow() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let session = OpenSession::bind(&temp.dir, &next);
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let (_read, writer, relays) = open_race(&level, &old);
    // The client command parks first; the redirect parks in the re-armed
    // hook once it too is about to open, still having published nothing.
    race_openers(
        &hub,
        &relays,
        route_job(&hub, &writer, &relays, &next),
        follow_job(&hub, &writer, &relays, &old),
        true,
    );
    assert_single_relay(&relays, &session, &next, &level);
    session.close();
}

#[test]
fn follow_paused_in_before_open_shares_one_relay_with_route() {
    let temp = Temp::new();
    let workspace = temp.workspace();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let old = id(1);
    let next = id(2);
    write_log(
        &temp.dir,
        &old,
        &workspace,
        None,
        &[rewound_line(&old, &next)],
    );
    let session = OpenSession::bind(&temp.dir, &next);
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let (_read, writer, relays) = open_race(&level, &old);
    // The redirect parks first; the client command parks in the re-armed
    // hook once it too is about to open, still having published nothing.
    race_openers(
        &hub,
        &relays,
        follow_job(&hub, &writer, &relays, &old),
        route_job(&hub, &writer, &relays, &next),
        false,
    );
    assert_single_relay(&relays, &session, &next, &level);
    session.close();
}
