//! Tests for resuming the session a relayed command names: finding its log
//! and workspace, attaching to a running session, one resume at a time,
//! the outcomes of the resumed process, and the connection's subscription
//! sent again on a reconnect.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::{Value, json};

use super::*;
use crate::connection::serve_connection;
use crate::fake::{FakeStarter, Handshake, failure};
use crate::start::START_DEADLINE;
use crate::{Started, Starter};
use fakes::Deadline;

/// One named deadline per wait: a resume answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

const SID: &str = "s_0123456789abcdef";

/// A path secret the hub's log must never hold.
const SECRET: &str = "the-volume-of-the-meeting-room";

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

    fn hub(&self, starter: FakeStarter) -> Arc<Hub> {
        self.hub_with(Arc::new(starter))
    }

    fn hub_with(&self, starter: Arc<dyn Starter>) -> Arc<Hub> {
        let clock = Arc::clone(&self.clock);
        let timed: Arc<dyn Clock> = clock;
        Arc::new(crate::testkit::hub(&self.dir, starter, timed))
    }

    /// The workspace a session's log records: its name holds [`SECRET`].
    fn workspace(&self) -> PathBuf {
        let workspace = self.dir.join(format!("w-{SECRET}"));
        fs::create_dir_all(&workspace).unwrap();
        workspace
    }

    /// `id`'s log in a project: `projects/-p/sessions/<id>/events.jsonl`,
    /// created with it.
    fn events(&self, id: &str) -> PathBuf {
        let dir = self
            .dir
            .join("projects")
            .join("-p")
            .join("sessions")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    /// Writes `lines` as the whole of `id`'s log.
    fn write_lines(&self, id: &str, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        fs::write(self.events(id), text).unwrap();
    }

    /// Writes `first` as the first line of `SID`'s log in a project.
    fn log(&self, first: &str) {
        self.write_lines(SID, &[first.to_owned(), "{}".to_owned()]);
    }

    /// Writes `bytes` as the whole of `SID`'s log, with no newline added:
    /// for a first line still being written or torn mid-character.
    fn log_bytes(&self, bytes: &[u8]) {
        fs::write(self.events(SID), bytes).unwrap();
    }

    /// `SID`'s log recording [`Temp::workspace`].
    fn recorded(&self) -> PathBuf {
        let workspace = self.workspace();
        self.log(
            &json!({
                "kind": "session_started",
                "seq": 0,
                "session_id": SID,
                "payload": {"workspace": workspace},
            })
            .to_string(),
        );
        workspace
    }

    /// `SID`'s log recording [`Temp::workspace`] and naming `parent`: a
    /// delegate's, when `parent` is anything but `null`.
    fn delegate_log(&self, parent: Value) -> PathBuf {
        let workspace = self.workspace();
        self.log(
            &json!({
                "kind": "session_started",
                "seq": 0,
                "session_id": SID,
                "payload": {"workspace": workspace, "parent": parent},
            })
            .to_string(),
        );
        workspace
    }

    /// Appends `kind` as the last line of `SID`'s log.
    fn append(&self, kind: &str) {
        let log = self.events(SID);
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(&json!({"kind": kind, "session_id": SID, "payload": {}}).to_string());
        text.push('\n');
        fs::write(log, text).unwrap();
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.dir.join("logs").join("hub.log")).unwrap_or_default()
    }

    /// `SID`'s log recording the workspace `w-SECRET` and `worktree` as
    /// given: the repository path is `r-SECRET/.git` under the home,
    /// never created. Returns the workspace and the repository.
    fn worktree_log(&self, workspace_present: bool, worktree: Value) -> (PathBuf, PathBuf) {
        let workspace = self.dir.join(format!("w-{SECRET}"));
        if workspace_present {
            fs::create_dir_all(&workspace).unwrap();
        }
        let repository = self.dir.join(format!("r-{SECRET}")).join(".git");
        self.log(
            &json!({
                "kind": "session_started",
                "seq": 0,
                "session_id": SID,
                "payload": {"workspace": workspace, "worktree": worktree},
            })
            .to_string(),
        );
        (workspace, repository)
    }
}

/// Takes the relay thread's drain notice with the deadline, failing at
/// the caller's line: the `before_command_write` hook below is a closure,
/// which `#[track_caller]` cannot cross.
#[track_caller]
fn await_drain(draining: &mpsc::Receiver<()>) {
    Deadline::after(DEADLINE)
        .recv(draining)
        .expect("the relay thread drains");
}

fn sid() -> SessionId {
    SessionId(SID.to_owned())
}

/// Runs `resume` on a thread: code that blocks is a wait, so the result is
/// received with a wall-clock deadline.
#[track_caller]
fn resumed(hub: &Arc<Hub>) -> Result<UnixStream, Refused> {
    resumed_with(hub, None)
}

/// Runs `resume` with an asked credential label on a thread, received
/// with a wall-clock deadline.
#[track_caller]
fn resumed_with(hub: &Arc<Hub>, credential: Option<&str>) -> Result<UnixStream, Refused> {
    let (done, finished) = mpsc::channel();
    let hub = Arc::clone(hub);
    let credential = credential.map(str::to_owned);
    thread::spawn(move || {
        done.send(resume(&hub, &sid(), credential.as_deref()))
            .unwrap_or(())
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers before its deadline")
}

/// Runs `resume_exited` on a thread, received with a wall-clock deadline.
#[track_caller]
fn resumed_exited(hub: &Arc<Hub>) -> Result<UnixStream, Refused> {
    resumed_exited_with(hub, None)
}

/// Runs `resume_exited` with an asked credential label on a thread,
/// received with a wall-clock deadline.
#[track_caller]
fn resumed_exited_with(hub: &Arc<Hub>, credential: Option<&str>) -> Result<UnixStream, Refused> {
    let (done, finished) = mpsc::channel();
    let hub = Arc::clone(hub);
    let credential = credential.map(str::to_owned);
    thread::spawn(move || {
        done.send(resume_exited(&hub, &sid(), credential.as_deref()))
            .unwrap_or(())
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers before its deadline")
}

/// Runs `resume` on a thread and hands back where its result arrives: the
/// test drives the fake clock while it waits.
fn resuming(hub: &Arc<Hub>) -> mpsc::Receiver<Result<UnixStream, Refused>> {
    let (done, finished) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::spawn(move || done.send(resume(&hub, &sid(), None)).unwrap_or(()));
    finished
}

/// How many held polls fit in the shutdown bound.
fn polls() -> u32 {
    u32::try_from(SHUTDOWN_BOUND.as_millis() / HELD_POLL.as_millis()).unwrap()
}

fn refused(result: Result<UnixStream, Refused>) -> Refused {
    match result {
        Ok(_) => panic!("the resume was refused"),
        Err(refused) => refused,
    }
}

#[test]
fn an_exited_session_resumes_in_its_recorded_workspace() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    let log = temp.hub_log();
    assert!(log.contains("\"code\":\"session_resumed\""), "{log}");
    assert!(log.contains("Session resumed for local."), "{log}");
    assert!(log.contains(SID), "{log}");
    assert!(!log.contains(SECRET), "no workspace path in the log");
}

#[test]
fn a_running_session_is_attached_to_without_a_resume() {
    let temp = Temp::new();
    temp.recorded();
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let _running = UnixListener::bind(run.join(SID)).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert!(starter.resumed().is_empty());
    assert!(!temp.hub_log().contains("session_resumed"));
}

#[test]
fn a_non_trusted_attempt_with_a_live_socket_looks_up_once() {
    let temp = Temp::new();
    temp.recorded();
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let _running = UnixListener::bind(run.join(SID)).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed_exited(&hub).is_ok());
    assert!(starter.resumed().is_empty());
    assert_eq!(
        crate::recent::LOGGED_LOOKUPS.load(Ordering::SeqCst),
        1,
        "the connect-succeeded branch shares one lookup between its predicates"
    );
}

#[test]
fn a_refused_connect_then_resume_looks_up_once() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert_eq!(
        crate::recent::LOGGED_LOOKUPS.load(Ordering::SeqCst),
        1,
        "the connect-failed branch looks up once per attempt"
    );
}

#[test]
fn a_directory_without_a_log_does_not_mask_a_later_log() {
    let temp = Temp::new();
    // `-a` holds the session directory with no log; `-p` holds the log.
    // The lookup must skip the earlier directory for the later log.
    fs::create_dir_all(
        temp.dir
            .join("projects")
            .join("-a")
            .join("sessions")
            .join(SID),
    )
    .unwrap();
    let workspace = temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
}

#[test]
fn an_exited_session_resume_carries_the_asked_label_to_the_starter() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed_with(&hub, Some("work")).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert_eq!(starter.asked(), [Some("work".to_owned())]);
}

#[test]
fn a_resume_without_a_label_asks_for_none() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(starter.asked(), [None]);
}

#[test]
fn a_running_session_resume_starts_nothing_and_asks_for_nothing() {
    let temp = Temp::new();
    temp.recorded();
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let _running = UnixListener::bind(run.join(SID)).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed_with(&hub, Some("work")).is_ok());
    assert!(starter.resumed().is_empty());
    assert!(starter.asked().is_empty());
}

#[test]
fn an_exited_session_resume_exited_carries_the_asked_label_to_the_starter() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed_exited_with(&hub, Some("work")).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert_eq!(starter.asked(), [Some("work".to_owned())]);
}

#[test]
fn an_opening_subscribe_with_a_label_starts_the_session_on_it() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_with("c_1", "full", Some("work"));
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.asked(), [Some("work".to_owned())]);
    // The original command is forwarded as sent, label included.
    let received = starter.received();
    assert_eq!(received.len(), 1, "{received:?}");
    let line: Value = serde_json::from_str(&received[0]).unwrap();
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["args"]["level"], "full");
    assert_eq!(line["args"]["credential"], "work");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
}

#[test]
fn an_opening_subscribe_without_a_label_starts_the_session_without_one() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "full");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(starter.asked(), [None]);
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
}

#[test]
fn an_opening_subscribe_with_a_wrong_typed_level_and_a_label_starts_nothing() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_raw("c_1", json!({"level": 1, "credential": "work"}));
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(starter.asked().is_empty(), "no label is used");
}

#[test]
fn an_opening_subscribe_with_an_unknown_arg_and_a_label_starts_nothing() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_raw(
        "c_1",
        json!({"level": "full", "credential": "work", "future": 1}),
    );
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(starter.asked().is_empty(), "no label is used");
}

#[test]
fn an_opening_subscribe_with_a_null_credential_starts_nothing() {
    // An optional key is absent, never null: an explicit null is
    // refused before any process starts, not read as absent.
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_raw("c_1", json!({"level": "full", "credential": Value::Null}));
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(starter.asked().is_empty(), "no label is used");
}

#[test]
fn an_opening_subscribe_with_a_wrong_typed_credential_starts_nothing() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_raw("c_1", json!({"level": "full", "credential": 1}));
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(starter.asked().is_empty(), "no label is used");
}

#[test]
fn a_label_bearing_subscribe_past_an_exited_process_starts_the_session_on_it() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    // A session whose log is gone answers `subscribe` `closing` too.
    let dying = FakeStarter::closing_every_command(&temp.dir);
    let started = crate::Starter::start(&dying, &sid(), &temp.workspace(), None, &[], false, None);
    assert!(started.is_ok(), "the closing session binds");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_with("c_1", "full", Some("work"));
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    // One acknowledgement, the resumed session's.
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.asked(), [Some("work".to_owned())]);
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
}

#[test]
fn a_trusted_resume_returns_an_accepting_socket_past_fiber_exited_at_once() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let _running = UnixListener::bind(run.join(SID)).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    // `resume` passes trusted=true: the accepting socket is the answer,
    // even while the log ends in `fiber_exited`.
    assert!(resumed(&hub).is_ok());
    assert!(starter.resumed().is_empty());
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_session_with_no_log_is_session_not_found() {
    let temp = Temp::new();
    // Another session's log does not answer for this one.
    let other = temp.dir.join("projects/-p/sessions/s_1111111111111111");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("events.jsonl"), "{}\n").unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_eq!(refused.message, format!("No session `{SID}`."));
    assert!(starter.resumed().is_empty());
}

#[test]
fn a_log_whose_first_line_names_no_workspace_is_log_corrupt() {
    for first in [
        "not json".to_owned(),
        json!({"kind": "session_started", "payload": {}}).to_string(),
        json!({"kind": "session_started", "payload": {"workspace": 7}}).to_string(),
        json!({"kind": "fiber_started", "payload": {"workspace": "/w"}}).to_string(),
        // A delegate's first line with no workspace is corrupt first.
        json!({
            "kind": "session_started",
            "payload": {"parent": {"session_id": "s", "delegate_id": "j"}},
        })
        .to_string(),
    ] {
        let temp = Temp::new();
        temp.log(&first);
        let starter = FakeStarter::bind_and_hold(&temp.dir);
        let hub = temp.hub(starter.clone());
        let refused = refused(resumed(&hub));
        assert_eq!(refused.code, ErrorCode::LogCorrupt, "{first}");
        assert!(starter.resumed().is_empty(), "{first}");
        assert!(temp.hub_log().contains("\"code\":\"log_corrupt\""));
    }
}

#[test]
fn a_log_with_no_complete_first_line_is_session_not_found() {
    // A session's process creates its log before it binds its socket, so
    // a client can open the session while the log is empty or its first
    // line is still being written. Either way the hub answers as for a
    // missing log, and writes no corrupt-log diagnostic.
    for bytes in [Vec::new(), b"{\"kind\":\"session_started\"".to_vec()] {
        let temp = Temp::new();
        temp.log_bytes(&bytes);
        let starter = FakeStarter::bind_and_hold(&temp.dir);
        let hub = temp.hub(starter.clone());
        let refused = refused(resumed(&hub));
        assert_eq!(refused.code, ErrorCode::SessionNotFound, "{bytes:?}");
        assert_eq!(refused.message, format!("No session `{SID}`."), "{bytes:?}");
        assert!(starter.resumed().is_empty(), "{bytes:?}");
        assert!(
            !temp.hub_log().contains("log_corrupt"),
            "{}",
            temp.hub_log()
        );
    }
}

#[test]
fn a_first_line_torn_inside_a_character_is_incomplete_until_its_newline() {
    // Completeness is decided on bytes before any decoding: these bytes
    // end inside a multi-byte character, so without a newline they answer
    // as a missing log, while the same bytes with a newline form a
    // complete line that is not UTF-8 and stay corrupt.
    let mut torn = br#"{"kind":"session_started","payload":{"workspace":"/w"#.to_vec();
    torn.extend_from_slice(&[0xC3]);
    let temp = Temp::new();
    temp.log_bytes(&torn);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let torn_refused = refused(resumed(&hub));
    assert_eq!(torn_refused.code, ErrorCode::SessionNotFound);
    assert_eq!(torn_refused.message, format!("No session `{SID}`."));
    assert!(starter.resumed().is_empty());
    assert!(!temp.hub_log().contains("log_corrupt"));

    let mut complete = torn.clone();
    complete.push(b'\n');
    let temp = Temp::new();
    temp.log_bytes(&complete);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let complete_refused = refused(resumed(&hub));
    assert_eq!(complete_refused.code, ErrorCode::LogCorrupt);
    assert!(starter.resumed().is_empty());
    assert!(temp.hub_log().contains("\"code\":\"log_corrupt\""));
}

#[test]
fn a_resume_exited_of_an_empty_log_is_session_not_found() {
    let temp = Temp::new();
    temp.log_bytes(&[]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed_exited(&hub));
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_eq!(refused.message, format!("No session `{SID}`."));
    assert!(starter.resumed().is_empty());
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_resumed_process_that_exits_first_is_refused_with_its_failure() {
    let temp = Temp::new();
    temp.recorded();
    let exited = failure(ErrorCode::IoFailed, "The workspace is gone.");
    let starter = FakeStarter::exit_with(&temp.dir, exited);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(refused.message, "The workspace is gone.");
    assert_eq!(starter.resumed().len(), 1);
    let log = temp.hub_log();
    assert!(log.contains("exited before it resumed."), "{log}");
    assert!(!log.contains("The workspace is gone."), "{log}");
}

#[test]
fn a_resumed_process_that_exits_after_another_bound_attaches() {
    let temp = Temp::new();
    temp.recorded();
    let exited = failure(ErrorCode::SessionHeld, "Another process holds it.");
    let starter = FakeStarter::bind_and_exit(&temp.dir, exited);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed().len(), 1);
}

#[test]
fn a_resumed_process_that_never_binds_fails_at_the_start_deadline() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::hang(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert!(
        refused.message.contains("did not bind"),
        "{}",
        refused.message
    );
    // The wait lasts exactly the deadline, polled on the fake clock.
    assert_eq!(temp.clock.now(), temp.clock.origin() + START_DEADLINE);
    let log = temp.hub_log();
    assert!(log.contains("could not resume."), "{log}");
    assert!(!log.contains(SECRET), "no workspace path in the log");
}

#[test]
fn a_resume_in_flight_is_refused_within_one_pass_of_the_store() {
    // No socket bound: the attempt reaches the starter, which holds the
    // resume until the test releases it, so the store lands mid-flight.
    struct GateStarter {
        inner: FakeStarter,
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
        ) -> std::io::Result<Box<dyn Started>> {
            self.inner
                .start(id, workspace, model, overrides, worktree, continues)
        }
        fn resume(
            &self,
            id: &SessionId,
            workspace: &std::path::Path,
            credential: Option<&str>,
        ) -> std::io::Result<Box<dyn Started>> {
            self.entered.send(()).unwrap_or(());
            if let Some(release) = support::lock(&self.release).take() {
                release.recv().unwrap_or(());
            }
            self.inner.resume(id, workspace, credential)
        }
        fn rewind(
            &self,
            id: &SessionId,
            workspace: &std::path::Path,
            from: &SessionId,
        ) -> std::io::Result<Box<dyn Started>> {
            self.inner.rewind(id, workspace, from)
        }
    }
    let temp = Temp::new();
    temp.recorded();
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let inner = FakeStarter::hang(&temp.dir);
    let hub = temp.hub_with(Arc::new(GateStarter {
        inner: inner.clone(),
        entered: entered_tx,
        release: Mutex::new(Some(release)),
    }));
    let finished = resuming(&hub);
    assert!(
        Deadline::after(DEADLINE).recv(&entered).is_ok(),
        "the resume reaches the starter"
    );
    // Admitted before the store: one bind in flight.
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 1);
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    let Err(refused) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers")
    else {
        panic!("the resume in flight is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    // The guard dropped with the refusal; the wait never advanced the clock.
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert_eq!(temp.clock.now(), temp.clock.origin());
    // The bind the store interrupted is killed.
    assert_eq!(inner.kills(), [sid()]);
}

#[test]
fn a_resume_parked_on_its_held_wait_leaves_within_one_pass() {
    // The session exited but still holds its socket: no socket bound,
    // so the resume waits the exiting process out instead of binding.
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let hub = temp.hub(FakeStarter::held_then_bind(&temp.dir, 1));
    let finished = resuming(&hub);
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the resume waits out the exiting process"
    );
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 1);
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    let Err(refused) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers")
    else {
        panic!("the parked resume is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert_eq!(temp.clock.now(), temp.clock.origin() + HELD_POLL);
}

#[test]
fn a_resume_after_the_store_is_refused_at_entry() {
    let temp = Temp::new();
    temp.recorded();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    // Admitted after the store: refused at once, the starter never run.
    let finished = resuming(&hub);
    let Err(refused) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers")
    else {
        panic!("the resume after the store is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_resume_exited_after_the_store_is_refused_at_entry() {
    let temp = Temp::new();
    temp.recorded();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    let Err(refused) = resume_exited(&hub, &sid(), None) else {
        panic!("the exited resume after the store is refused");
    };
    assert_eq!(refused, crate::connection::stopping());
    assert_eq!(hub.inflight_binds.load(Ordering::SeqCst), 0);
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_held_resume_of_an_exited_session_retries_until_the_lock_is_released() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let starter = FakeStarter::held_then_bind(&temp.dir, 2);
    let hub = temp.hub(starter.clone());
    let finished = resuming(&hub);
    for k in 1..=2 {
        let until = temp.clock.origin() + HELD_POLL * k;
        assert!(
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits a poll after held failure {k}"
        );
        assert_eq!(starter.resumed().len(), usize::try_from(k).unwrap());
        assert!(finished.try_recv().is_err(), "no answer while it waits");
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    }
    let result = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the resume answers once the lock is released");
    assert!(result.is_ok());
    assert_eq!(starter.resumed().len(), 3);
}

#[test]
fn a_resume_held_past_the_shutdown_bound_is_session_held() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let starter = FakeStarter::held_then_bind(&temp.dir, usize::MAX);
    let hub = temp.hub(starter.clone());
    let finished = resuming(&hub);
    for k in 1..=polls() {
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
    let refused = refused(
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the resume answers at the bound"),
    );
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert_eq!(refused.message, "Another process holds this session.");
    assert_eq!(temp.clock.now(), temp.clock.origin() + SHUTDOWN_BOUND);
    assert_eq!(
        starter.resumed().len(),
        usize::try_from(polls()).unwrap() + 1
    );
}

#[test]
fn a_held_resume_of_a_session_that_has_not_exited_is_session_held_at_once() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("turn_started");
    let starter = FakeStarter::held_then_bind(&temp.dir, usize::MAX);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn two_commands_for_one_exited_session_start_one_process() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let (done, finished) = mpsc::channel();
    for _ in 0..2 {
        let hub = Arc::clone(&hub);
        let done = done.clone();
        thread::spawn(move || done.send(resume(&hub, &sid(), None).is_ok()).unwrap_or(()));
    }
    let wait = Deadline::after(DEADLINE);
    for _ in 0..2 {
        assert!(
            wait.recv(&finished)
                .expect("both resumes answer before the deadline"),
            "both commands reach the session"
        );
    }
    assert_eq!(starter.resumed().len(), 1);
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
        thread::spawn(move || serve_connection(a, hub));
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut client = Self {
            read: BufReader::new(b.try_clone().unwrap()),
            write: b,
        };
        assert_eq!(client.next("hub_hello")["kind"], "hub_hello");
        client
    }

    fn send(&mut self, id: &str, command: &str) {
        let line = json!({
            "id": id,
            "session_id": SID,
            "command": command,
            "args": {"level": "full"},
        });
        let mut bytes = serde_json::to_vec(&line).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    /// Sends a `subscribe` for the session at `level`.
    fn subscribe(&mut self, id: &str, level: &str) {
        self.subscribe_with(id, level, None);
    }

    /// Sends a `subscribe` for the session at `level`, asking for
    /// `credential` when one is named.
    fn subscribe_with(&mut self, id: &str, level: &str, credential: Option<&str>) {
        let mut args = serde_json::Map::new();
        args.insert("level".into(), Value::String(level.to_owned()));
        if let Some(credential) = credential {
            args.insert("credential".into(), Value::String(credential.to_owned()));
        }
        let line = json!({
            "id": id,
            "session_id": SID,
            "command": "subscribe",
            "args": Value::Object(args),
        });
        let mut bytes = serde_json::to_vec(&line).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    /// Sends `args` as the subscribe's args verbatim.
    fn subscribe_raw(&mut self, id: &str, args: Value) {
        let line = json!({
            "id": id,
            "session_id": SID,
            "command": "subscribe",
            "args": args,
        });
        let mut bytes = serde_json::to_vec(&line).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
        self.write.flush().unwrap();
    }

    /// The next line. Panics past the deadline naming the wait.
    fn next(&mut self, what: &str) -> Value {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the hub closed before {what}");
        serde_json::from_str(&text).unwrap()
    }

    /// The `command_id` of the next line, an acknowledgement.
    fn acknowledged(&mut self, what: &str) -> String {
        let line = self.next(what);
        assert_eq!(line["kind"], "command_accepted", "{line}");
        line["payload"]["command_id"].as_str().unwrap().to_owned()
    }
}

/// The hub's replacement for a muted full replay acknowledgement.
fn assert_reopened(line: &Value, last_seq: Option<u64>) {
    assert_eq!(line["kind"], "stream_reopened", "{line}");
    assert_eq!(line["payload"]["session_id"], SID);
    assert_eq!(
        line["payload"].get("last_seq"),
        last_seq.map(Value::from).as_ref()
    );
    assert!(line.get("seq").is_none());
    assert!(line.get("session_id").is_none());
}

/// What the fake sessions received, as `(id, command)`.
fn received(starter: &FakeStarter) -> Vec<(String, String)> {
    starter
        .received()
        .iter()
        .map(|text| received_line(text))
        .collect()
}

/// One received line as `(id, command)`.
fn received_line(text: &str) -> (String, String) {
    let line: Value = serde_json::from_str(text).unwrap();
    (
        line["id"].as_str().unwrap().to_owned(),
        line["command"].as_str().unwrap().to_owned(),
    )
}

/// What the fake sessions received on each connection, as `(id, command)`.
fn received_by_connection(starter: &FakeStarter) -> Vec<Vec<(String, String)>> {
    starter
        .received_by_connection()
        .iter()
        .map(|conn| conn.iter().map(|text| received_line(text)).collect())
        .collect()
}

#[test]
fn a_command_for_an_exited_session_resumes_it_and_is_relayed() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "subscribe");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(received(&starter), [("c_1".into(), "subscribe".into())]);
}

#[test]
fn a_command_for_a_session_with_no_log_is_session_not_found() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let mut client = Client::connect(&hub);
    client.send("c_1", "subscribe");
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected");
    assert_eq!(line["payload"]["code"], "session_not_found");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["message"], format!("No session `{SID}`."));
}

/// A resumed session binds its socket for the test to play by hand.
struct HandResume {
    home: PathBuf,
    bound: mpsc::Sender<UnixListener>,
}

struct HandAlive;
impl crate::Started for HandAlive {
    fn exited(&self) -> Option<crate::Failure> {
        None
    }
}
impl crate::Starter for HandResume {
    fn start(
        &self,
        _: &SessionId,
        _: &std::path::Path,
        _: Option<&str>,
        _: &[&str],
        _: bool,
        _: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        Err(std::io::Error::other("only resumes are expected"))
    }
    fn rewind(
        &self,
        _: &SessionId,
        _: &std::path::Path,
        _: &SessionId,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        Err(std::io::Error::other("only resumes are expected"))
    }
    fn resume(
        &self,
        id: &SessionId,
        _: &std::path::Path,
        _: Option<&str>,
    ) -> std::io::Result<Box<dyn crate::Started>> {
        let listener = UnixListener::bind(self.home.join("run").join(&id.0))?;
        self.bound.send(listener).map_err(std::io::Error::other)?;
        Ok(Box::new(HandAlive))
    }
}

/// Replays a kept level through a route reconnect or a sweep rejoin.
fn scripted_reopening(level: &str, accepted: bool, last_seq: Option<u64>, path: &str) {
    use crate::connection::lock;
    use crate::relay::Relays;
    use std::sync::Mutex;
    let temp = Temp::new();
    temp.recorded();
    let (bound_tx, bound) = mpsc::channel();
    let timed: Arc<dyn Clock> = temp.clock.clone();
    let hub = Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(HandResume {
            home: temp.dir.clone(),
            bound: bound_tx,
        }),
        timed,
    ));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let kept = json!({"id": "c_sub", "command": "subscribe", "args": {"level": level}})
        .as_object()
        .unwrap()
        .clone();
    lock(&relays).subscribed.push((SID.into(), kept));
    let (writer, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let listener = match path {
        "resume" => {
            temp.append("fiber_exited");
            None
        }
        _ => Some(UnixListener::bind(temp.dir.join("run").join(SID)).unwrap()),
    };
    let (played_tx, played) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let player = thread::spawn(move || {
        let listener = listener.unwrap_or_else(|| {
            Deadline::after(DEADLINE).recv_or_fail(&bound, "the resumed session to bind")
        });
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut text = String::new();
        reader.read_line(&mut text).unwrap();
        let replay: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(replay["command"], "subscribe");
        assert_ne!(replay["id"], "c_sub");
        let mut payload = json!({"command_id": replay["id"]});
        if let Some(last_seq) = last_seq {
            payload["result"] = json!({"last_seq": last_seq});
        }
        if !accepted {
            payload["code"] = json!("closing");
        }
        let kind = if accepted {
            "command_accepted"
        } else {
            "command_rejected"
        };
        writeln!(
            stream,
            "{}",
            json!({"kind": kind, "ts": 0, "schema_version": 1, "payload": payload})
        )
        .unwrap();
        // A marker after the replay pins the event's position and count.
        writeln!(stream, "{}", json!({"kind": "fold_marker", "session_id": SID, "ts": 0, "schema_version": 1, "payload": {}})).unwrap();
        played_tx.send(()).unwrap();
        match Deadline::after(DEADLINE).recv(&release) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the scripted replay was not released"),
        }
    });
    if path == "rejoin" {
        let mark = crate::rejoin::Mark::now(&temp.dir, SID);
        let epoch = {
            let mut held = lock(&relays);
            let epoch = held.mint();
            assert!(crate::rejoin::admit(&mut held, SID, false, mark, epoch));
            epoch
        };
        crate::relay::attach_rejoin(
            SID,
            UnixStream::connect(temp.dir.join("run").join(SID)).unwrap(),
            &hub,
            &writer,
            &relays,
            epoch,
        );
    } else {
        let command = json!({"id": "c_next", "command": "reply"})
            .as_object()
            .unwrap()
            .clone();
        crate::relay::route(
            &contract::CommandId("c_next".into()),
            SID,
            command,
            &hub,
            &writer,
            &relays,
            None,
            false,
        );
    }
    Deadline::after(DEADLINE).recv_or_fail(&played, "the scripted replay");
    let mut reader = BufReader::new(client_read);
    let mut text = String::new();
    reader.read_line(&mut text).unwrap();
    let first: Value = serde_json::from_str(&text).unwrap();
    if level == "full" && accepted {
        assert_reopened(&first, last_seq);
        text.clear();
        reader.read_line(&mut text).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["kind"],
            "fold_marker"
        );
    } else {
        assert_eq!(
            first["kind"], "fold_marker",
            "no replay acknowledgement or reopening at summary or rejection"
        );
    }
    // Drop the release before waiting for the player and relay to end.
    drop(release_tx);
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || {
        player.join().unwrap();
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE).recv_or_fail(&done, "the scripted session to end");
    crate::testkit::await_true("the relay to leave", move || {
        lock(&relays).entries.is_empty()
    });
}

/// A relay whose session closed the connection while its listener is
/// still bound refuses the unanswered command `closing` and leaves: the
/// drain never routes into the exiting process's shutdown wait. The
/// scripted session holds its listener past the connection's close, the
/// teardown a descheduled session leaves behind for the relay's end to
/// observe.
#[test]
fn a_drain_that_sees_the_exiting_socket_refuses_closing_and_leaves() {
    use crate::connection::lock;
    use crate::relay::Relays;
    use std::sync::Mutex;
    let temp = Temp::new();
    temp.recorded();
    let (bound_tx, bound) = mpsc::channel();
    let timed: Arc<dyn Clock> = temp.clock.clone();
    let hub = Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(HandResume {
            home: temp.dir.clone(),
            bound: bound_tx,
        }),
        timed,
    ));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let kept = json!({"id": "c_sub", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    lock(&relays).subscribed.push((SID.into(), kept));
    let (writer, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    temp.append("fiber_exited");
    let (played_tx, played) = mpsc::channel();
    let player = thread::spawn(move || {
        let listener =
            Deadline::after(DEADLINE).recv_or_fail(&bound, "the resumed session to bind");
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut text = String::new();
        reader.read_line(&mut text).unwrap();
        let replay: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(replay["command"], "subscribe");
        let payload = json!({"command_id": replay["id"], "result": {"last_seq": 41}});
        writeln!(
            stream,
            "{}",
            json!({"kind": "command_accepted", "ts": 0, "schema_version": 1, "payload": payload})
        )
        .unwrap();
        writeln!(stream, "{}", json!({"kind": "fold_marker", "session_id": SID, "ts": 0, "schema_version": 1, "payload": {}})).unwrap();
        played_tx.send(()).unwrap();
        // Close the relayed connection while the listener stays bound, as
        // a teardown descheduled mid-close leaves it. The relay's end then
        // connects back while the log still ends in `fiber_exited`, either
        // for the drain's probe or for the resume it would wait out:
        // accepting here holds the window open exactly until the relay's
        // end has observed the exiting socket, with no sleep.
        drop(reader);
        drop(stream);
        let (probe, _) = listener.accept().unwrap();
        drop(probe);
    });
    let command = json!({"id": "c_next", "command": "reply"})
        .as_object()
        .unwrap()
        .clone();
    crate::relay::route(
        &contract::CommandId("c_next".into()),
        SID,
        command,
        &hub,
        &writer,
        &relays,
        None,
        false,
    );
    Deadline::after(DEADLINE).recv_or_fail(&played, "the scripted replay");
    let mut reader = BufReader::new(client_read);
    let mut text = String::new();
    reader.read_line(&mut text).unwrap();
    assert_reopened(&serde_json::from_str(&text).unwrap(), Some(41));
    text.clear();
    reader.read_line(&mut text).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["kind"],
        "fold_marker"
    );
    crate::testkit::await_true("the relay to leave", move || {
        lock(&relays).entries.is_empty()
    });
    text.clear();
    reader.read_line(&mut text).unwrap();
    let refused: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(refused["kind"], "command_rejected");
    assert_eq!(refused["payload"]["code"], "closing");
    assert_eq!(refused["payload"]["command_id"], "c_next");
    player.join().unwrap();
}

#[test]
fn reconnect_and_rejoin_replace_full_replay_acknowledgements_with_boundaries() {
    for path in ["reconnect", "resume", "rejoin"] {
        scripted_reopening("full", true, Some(41), path);
        scripted_reopening("full", true, None, path);
        scripted_reopening("summary", true, Some(41), path);
        scripted_reopening("full", false, Some(41), path);
    }
}

#[test]
fn a_reconnect_sends_the_subscription_again_before_the_command() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "subscribe");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    // The session exits: its socket is gone and its connection closed.
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");

    client.send("c_2", "reply");
    // The session acknowledges the replayed subscribe first; the client
    // sees the reopening boundary, then its own command's acknowledgement.
    assert_reopened(&client.next("the reopened stream"), None);
    assert_eq!(client.acknowledged("the reply"), "c_2");
    assert_eq!(starter.resumed().len(), 2);
    let got = received(&starter);
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0], ("c_1".into(), "subscribe".into()));
    assert_eq!(got[1].1, "subscribe");
    assert!(got[1].0.starts_with("c_"), "{got:?}");
    assert_ne!(got[1].0, "c_1", "the replay carries an id of the hub's own");
    assert_eq!(got[2], ("c_2".into(), "reply".into()));

    // A client's own second subscribe still reaches the session.
    client.send("c_3", "subscribe");
    assert_eq!(client.acknowledged("the second subscribe"), "c_3");
    assert_eq!(received(&starter)[3], ("c_3".into(), "subscribe".into()));
}

#[test]
fn a_connection_that_never_subscribed_gets_no_replay() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_2", "reply");
    assert_eq!(client.acknowledged("the second reply"), "c_2");
    assert_eq!(starter.resumed().len(), 2);
    assert_eq!(
        received(&starter),
        [
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "reply".into())
        ]
    );
}

/// A session after `close` at `run/<SID>`: it answers every command but
/// `subscribe` with `closing` until the test stops it.
fn closing_session(temp: &Temp) -> FakeStarter {
    let dying = FakeStarter::closing(&temp.dir);
    let started = crate::Starter::start(&dying, &sid(), &temp.workspace(), None, &[], false, None);
    assert!(started.is_ok(), "the closing session binds");
    dying
}

#[test]
fn a_command_answered_closing_after_fiber_exited_reaches_the_resumed_session() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    // The exiting process still accepts on its socket: the hub waits.
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    assert!(starter.resumed().is_empty());
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    // One acknowledgement, the resumed session's.
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(received(&starter), [("c_1".into(), "reply".into())]);
    client.send("c_2", "steer");
    assert_eq!(client.acknowledged("the steer"), "c_2");
}

#[test]
fn two_commands_answered_closing_reach_one_resumed_session() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    // The resumed process writes its durable `fiber_started` while the
    // relay thread is still draining the dying connection's answers, so
    // the second `closing` re-routes only when the thread keeps what its
    // first answer detected.
    let starter = FakeStarter::bind_hold_and_append_started(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    // Holds the relay thread on its first answer until both commands
    // reached the dying process, so the second answer re-routes while the
    // first re-route is still opening: the interleaving that reorders.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_replay_filter) = Some(Box::new(move |_, _| {
        parked_tx.send(()).unwrap_or(());
        let _released = release_rx.recv();
    }));
    client.send("c_1", "reply");
    client.send("c_2", "steer");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the relay reads the first closing answer");
    assert!(
        dying.await_received(2, DEADLINE),
        "both commands were answered closing"
    );
    release_tx.send(()).unwrap_or(());
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert_eq!(starter.resumed().len(), 1, "one resume for both");
    assert_eq!(
        received(&starter),
        [
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into())
        ]
    );
    assert_eq!(
        received_by_connection(&starter),
        [[
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into())
        ]],
        "one connection for both"
    );
}

#[test]
fn a_command_read_while_a_reroute_opens_follows_it_on_one_connection() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_hold_and_append_started(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    // The first answer re-routed: its entry is gone at base, so the next
    // lookup finds nothing and opens a second relay.
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    let (parked_tx, parked_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
    }));
    client.send("c_2", "steer");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the second command is routed");
    let (parked_tx, parked_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        parked_tx.send(()).unwrap_or(());
    }));
    client.subscribe("c_3", "full");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the third command is routed");
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert_eq!(client.acknowledged("the subscribe"), "c_3");
    assert_eq!(starter.resumed().len(), 1, "one resume for all three");
    assert_eq!(
        received_by_connection(&starter),
        [[
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into()),
            ("c_3".into(), "subscribe".into()),
        ]],
        "one connection for all three"
    );
}

#[test]
fn a_command_whose_write_to_the_exiting_relay_fails_follows_the_rerouted_one() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    // Parks the relay thread on the first answer, before it settles, so
    // the next command's write lands on the exiting relay.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_replay_filter) = Some(Box::new(move |_, _| {
        parked_tx.send(()).unwrap_or(());
        let _released = release_rx.recv();
    }));
    client.send("c_1", "reply");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the relay reads the closing answer");
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_2", "steer");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the second command is queued");
    release_tx.send(()).unwrap_or(());
    // No code under test waits on the clock here; no reach wait at the move: crates/hub/src/resume.rs:132 is not reached, so the advance is dead (ruling 6179406086).
    temp.clock.advance(HELD_POLL);
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert_eq!(starter.resumed().len(), 1, "one resume for both");
    assert_eq!(
        received_by_connection(&starter),
        [[
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into())
        ]],
        "one connection for both"
    );
}

#[test]
fn answers_buffered_before_a_failed_write_are_still_read() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    // Parks the relay thread on its first line: all three commands are
    // written to the exiting relay, and their answers wait in its kernel
    // buffer.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_replay_filter) = Some(Box::new(move |_, _| {
        parked_tx.send(()).unwrap_or(());
        let _released = release_rx.recv();
    }));
    client.send("c_1", "reply");
    client.send("c_2", "steer");
    client.subscribe("c_3", "full");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the relay reads the first answer");
    assert!(
        dying.await_received(3, DEADLINE),
        "the dying process answered all three"
    );
    assert!(
        dying.await_accepted(1, DEADLINE),
        "the dying process wrote the acceptance"
    );
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_4", "reply");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the fourth command is queued");
    release_tx.send(()).unwrap_or(());
    // Every command was answered, so each gets an acknowledgement in the
    // order it was read: the third's is the dying process's acceptance,
    // forwarded once the resumed session answered the two commands ahead
    // of it.
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert_eq!(client.acknowledged("the subscribe"), "c_3");
    assert_eq!(client.acknowledged("the second reply"), "c_4");
    assert_eq!(starter.resumed().len(), 1, "one resume");
    assert_eq!(
        received_by_connection(&starter),
        [[
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into()),
            ("c_4".into(), "reply".into()),
        ]],
        "one connection, without the accepted subscribe"
    );
}

#[test]
fn commands_held_behind_a_failed_reroute_are_each_refused_in_order() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_2", "steer");
    // The exiting process never ends: each command is refused with its
    // own shutdown bound, in the order it was read.
    for k in 1..=polls() {
        let until = temp.clock.origin() + HELD_POLL * k;
        assert!(
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits poll {k} inside the bound"
        );
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    }
    let line = client.next("the first refusal");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["code"], "session_held");
    for k in 1..=polls() {
        let until = temp.clock.origin() + SHUTDOWN_BOUND + HELD_POLL * k;
        assert!(
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits poll {k} inside the second bound"
        );
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    }
    let line = client.next("the second refusal");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_2");
    assert_eq!(line["payload"]["code"], "session_held");
    assert!(
        Deadline::after(DEADLINE).recv(&routed_rx).is_ok(),
        "the second command was routed"
    );
    assert!(starter.resumed().is_empty(), "no resume while held");
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    client.send("c_3", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_3");
    assert_eq!(received(&starter), [("c_3".into(), "reply".into())]);
}

#[test]
fn closing_before_fiber_exited_is_passed_on() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("turn_ended");
    let _dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["code"], "closing");
    assert_eq!(line["payload"]["message"], "The session is closing.");
    assert!(starter.resumed().is_empty());
}

#[test]
fn a_rejection_that_is_not_closing_after_fiber_exited_is_passed_on() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: false,
            code: "prompt_rejected".to_owned(),
            message: "The prompt was rejected.".to_owned(),
        },
    );
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "prompt");
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["code"], "prompt_rejected");
    assert_eq!(line["payload"]["message"], "The prompt was rejected.");
    // Passed on, not routed again: one resume, the first connection's.
    assert_eq!(starter.resumed().len(), 1);
}

#[test]
fn an_exiting_process_that_never_ends_is_session_held_past_the_bound() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let _dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    for k in 1..=polls() {
        let until = temp.clock.origin() + HELD_POLL * k;
        assert!(
            temp.clock
                .await_parked(until, fakes::Deadline::after(DEADLINE)),
            "the hub waits poll {k} inside the bound"
        );
        temp.clock
            .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    }
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["code"], "session_held");
    assert_eq!(
        line["payload"]["message"],
        format!("Session {SID} is still held by its exiting process.")
    );
    assert_eq!(temp.clock.now(), temp.clock.origin() + SHUTDOWN_BOUND);
    assert!(starter.resumed().is_empty());
}

/// What the fake sessions received, as `(id, command, args.level)`: a
/// `subscribe` carries its level, any other command the `send` helper's.
fn received_levels(starter: &FakeStarter) -> Vec<(String, String, Option<String>)> {
    starter
        .received()
        .iter()
        .map(|text| {
            let line: Value = serde_json::from_str(text).unwrap();
            (
                line["id"].as_str().unwrap().to_owned(),
                line["command"].as_str().unwrap().to_owned(),
                line.get("args")
                    .and_then(|args| args.get("level"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn a_resume_replays_the_level_the_connection_last_changed_to() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    client.subscribe("c_2", "full");
    assert_eq!(client.acknowledged("the change"), "c_2");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_3", "reply");
    // The replay's acknowledgement is replaced by the reopening boundary.
    assert_reopened(&client.next("the reopened stream"), None);
    assert_eq!(client.acknowledged("the reply"), "c_3");
    let got = received_levels(&starter);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        got[0],
        ("c_1".into(), "subscribe".into(), Some("summary".into()))
    );
    assert_eq!(
        got[1],
        ("c_2".into(), "subscribe".into(), Some("full".into()))
    );
    assert_eq!(got[2].1, "subscribe");
    assert!(got[2].0.starts_with("c_"), "{got:?}");
    assert_ne!(got[2].0, "c_1", "the replay carries an id of the hub's own");
    assert_ne!(got[2].0, "c_2", "the replay carries an id of the hub's own");
    assert_eq!(got[2].2, Some("full".into()), "{got:?}");
    assert_eq!(got[3].0, "c_3");
    assert_eq!(got[3].1, "reply");
}

#[test]
fn the_level_is_recorded_before_its_acknowledgement_is_forwarded() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let (done, finished) = mpsc::channel();
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    // The one-shot hook runs inside the forward of the next session line,
    // the acknowledgement of `c_2`: it reads the kept subscription, which
    // the relay records before it forwards.
    *crate::connection::lock(&hub.before_forward) = Some(Box::new(move |line, relays| {
        assert!(crate::relay::acknowledges(line, "c_2"), "{line:?}");
        let level = crate::connection::lock(relays)
            .subscribed
            .iter()
            .find(|(session, _)| session == SID)
            .and_then(|(_, kept)| kept.get("args"))
            .and_then(|args| args.get("level"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Ok(()) = done.send(level) {}
    }));
    client.subscribe("c_2", "full");
    assert_eq!(client.acknowledged("the change"), "c_2");
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the hook ran"),
        Some("full".into()),
        "the recording runs before the forward"
    );
}

#[test]
fn a_rejected_change_leaves_the_replayed_level_unchanged() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    client.subscribe("c_2", "bogus");
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_2");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_3", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_3");
    let got = received_levels(&starter);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        got[0],
        ("c_1".into(), "subscribe".into(), Some("summary".into()))
    );
    assert_eq!(got[1].1, "subscribe");
    assert_eq!(got[1].2, Some("bogus".into()), "{got:?}");
    assert_eq!(got[2].1, "subscribe");
    assert!(got[2].0.starts_with("c_"), "{got:?}");
    assert_eq!(got[2].2, Some("summary".into()), "{got:?}");
    assert_eq!(got[3].0, "c_3");
    assert_eq!(got[3].1, "reply");
}

#[test]
fn a_rejected_first_subscribe_is_not_replayed_on_resume() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "bogus");
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_2", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_2");
    assert_eq!(
        received_levels(&starter),
        [
            ("c_1".into(), "subscribe".into(), Some("bogus".into())),
            ("c_2".into(), "reply".into(), Some("full".into())),
        ]
    );
}

#[test]
fn a_subscribe_accepted_after_a_rejected_one_is_replayed() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "bogus");
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    client.subscribe("c_2", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_2");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_3", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_3");
    let got = received_levels(&starter);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        got[0],
        ("c_1".into(), "subscribe".into(), Some("bogus".into()))
    );
    assert_eq!(
        got[1],
        ("c_2".into(), "subscribe".into(), Some("summary".into()))
    );
    assert_eq!(got[2].1, "subscribe");
    assert!(got[2].0.starts_with("c_"), "{got:?}");
    assert_ne!(got[2].0, "c_1", "the replay carries an id of the hub's own");
    assert_ne!(got[2].0, "c_2", "the replay carries an id of the hub's own");
    assert_eq!(got[2].2, Some("summary".into()), "{got:?}");
    assert_eq!(got[3].0, "c_3");
    assert_eq!(got[3].1, "reply");
}

#[test]
fn a_subscribe_answered_closing_after_fiber_exited_is_sent_once() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    // A session whose log is gone answers `subscribe` `closing` too.
    let dying = FakeStarter::closing_every_command(&temp.dir);
    let started = crate::Starter::start(&dying, &sid(), &temp.workspace(), None, &[], false, None);
    assert!(started.is_ok(), "the closing session binds");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "full");
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    // One acknowledgement, the resumed session's.
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.resumed().len(), 1);
    assert_eq!(received(&starter), [("c_1".into(), "subscribe".into())]);
}

#[test]
fn a_lowered_level_is_replayed() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "full");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    client.subscribe("c_2", "summary");
    assert_eq!(client.acknowledged("the change"), "c_2");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    client.send("c_3", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_3");
    let got = received_levels(&starter);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        got[0],
        ("c_1".into(), "subscribe".into(), Some("full".into()))
    );
    assert_eq!(
        got[1],
        ("c_2".into(), "subscribe".into(), Some("summary".into()))
    );
    assert_eq!(got[2].1, "subscribe");
    assert!(got[2].0.starts_with("c_"), "{got:?}");
    assert_eq!(got[2].2, Some("summary".into()), "{got:?}");
    assert_eq!(got[3].0, "c_3");
    assert_eq!(got[3].1, "reply");
}

/// What a client is told for a delegate the hub does not resume.
const DELEGATE_REFUSED: &str = "A delegate resumes only through its parent.";

/// The `parent` a delegate's `session_started` names, in the contract's
/// wire shape.
fn parent() -> Value {
    serde_json::to_value(contract::events::Parent {
        session_id: SessionId("s_1111111111111111".to_owned()),
        delegate_id: contract::JobId("j_1".to_owned()),
    })
    .unwrap()
}

#[test]
fn an_exited_delegate_is_refused_without_a_resume() {
    // Any `parent` but `null` is a delegate's, even one of the wrong shape.
    for parent in [parent(), json!("s0"), json!({})] {
        let temp = Temp::new();
        temp.delegate_log(parent.clone());
        let starter = FakeStarter::bind_and_hold(&temp.dir);
        let hub = temp.hub(starter.clone());
        let refused = refused(resumed(&hub));
        assert_eq!(refused.code, ErrorCode::SessionNotFound, "{parent}");
        assert_eq!(refused.message, DELEGATE_REFUSED, "{parent}");
        assert!(starter.resumed().is_empty(), "{parent}");
        assert!(!temp.hub_log().contains("session_resumed"), "{parent}");
    }
    let temp = Temp::new();
    let workspace = temp.delegate_log(Value::Null);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(
        resumed(&hub).is_ok(),
        "a null parent is a top-level session"
    );
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
}

#[test]
fn an_exited_delegate_ending_fiber_exited_is_refused_on_resume_exited() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    temp.append("fiber_exited");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed_exited(&hub));
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_eq!(refused.message, DELEGATE_REFUSED);
    assert!(starter.resumed().is_empty());
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_running_delegate_is_attached_to() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    let run = temp.dir.join("run");
    fs::create_dir_all(&run).unwrap();
    let _running = UnixListener::bind(run.join(SID)).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    // The trusted first pass attaches past `fiber_exited` too.
    temp.append("fiber_exited");
    assert!(resumed(&hub).is_ok());
    assert!(starter.resumed().is_empty());
    assert_eq!(temp.clock.now(), temp.clock.origin());
}

#[test]
fn a_subscribe_through_the_hub_to_a_running_delegate_is_relayed() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    let running = FakeStarter::bind_and_hold(&temp.dir);
    let started =
        crate::Starter::start(&running, &sid(), &temp.workspace(), None, &[], false, None);
    assert!(started.is_ok(), "the delegate binds");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert!(starter.resumed().is_empty());
}

#[test]
fn a_subscribe_through_the_hub_to_an_exited_delegate_is_refused() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["code"], "session_not_found");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["message"], DELEGATE_REFUSED);
    assert!(starter.resumed().is_empty());
}

#[test]
fn a_command_for_an_exiting_delegate_is_refused_at_once() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    temp.append("fiber_exited");
    let _dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    // No shutdown wait: the refusal comes before any poll on the clock.
    let line = client.next("the rejection");
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["code"], "session_not_found");
    assert_eq!(line["payload"]["command_id"], "c_1");
    assert_eq!(line["payload"]["message"], DELEGATE_REFUSED);
    assert_eq!(temp.clock.now(), temp.clock.origin());
    assert!(starter.resumed().is_empty());
}

/// A `subscribe` accepted as the session dies is replayed to the resumed
/// session. With `prior`, the client first sets that level as `c_1`, and
/// the accepted subscribe is `c_2`; without it, the accepted subscribe is
/// `c_1`.
///
/// Receives with the deadline, failing at the caller's line.
#[track_caller]
fn an_accepted_subscribe_is_replayed(prior: Option<&str>) {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    let (accepted, reply) = if let Some(level) = prior {
        client.subscribe("c_1", level);
        assert_eq!(client.acknowledged("the subscribe"), "c_1");
        ("c_2", "c_3")
    } else {
        ("c_1", "c_2")
    };
    let before = usize::from(prior.is_some());
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // Parks the old relay thread after it reads the acknowledgement and
    // before it keeps the level, so the reconnect below runs while the
    // acceptance is still unrecorded.
    *crate::connection::lock(&hub.before_accepted) = Some(Box::new(move |line: &[u8], _| {
        assert!(crate::relay::acknowledges(line, accepted), "{line:?}");
        parked_tx.send(()).unwrap_or(());
        release_rx
            .recv()
            .expect("the reconnect releases the parked acknowledgement");
    }));
    client.subscribe(accepted, "full");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the relay parks before keeping the subscribe");
    assert!(starter.stop(&sid(), DEADLINE), "the fake session ended");
    // The reconnect queues behind the parked acknowledgement: once it is
    // routed, the parked thread is released to keep the level it read.
    // The hook only reports routing; the release sender stays test-owned
    // so unwinding drops it and disconnects the parked acknowledgement.
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send(reply, "reply");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the reconnect routes behind the parked acknowledgement");
    release_tx.send(()).unwrap_or(());
    let lines: Vec<_> = (0..3)
        .map(|_| client.next("the acknowledgements and reopening boundary"))
        .collect();
    let mut ids = Vec::new();
    let mut reopened = 0;
    for line in &lines {
        if line["kind"] == "stream_reopened" {
            assert_reopened(line, None);
            reopened += 1;
        } else {
            assert_eq!(line["kind"], "command_accepted");
            ids.push(line["payload"]["command_id"].as_str().unwrap().to_owned());
        }
    }
    ids.sort();
    assert_eq!(ids, [accepted.to_owned(), reply.to_owned()]);
    assert_eq!(reopened, 1);
    assert_eq!(starter.resumed().len(), 2);
    let got = received_levels(&starter);
    assert_eq!(got.len(), before + 3, "{got:?}");
    if let Some(level) = prior {
        assert_eq!(
            got[0],
            ("c_1".into(), "subscribe".into(), Some(level.into()))
        );
    }
    assert_eq!(
        got[before],
        (accepted.into(), "subscribe".into(), Some("full".into()))
    );
    let replay = &got[before + 1];
    assert_eq!(replay.1, "subscribe");
    assert!(replay.0.starts_with("c_"), "{got:?}");
    assert_ne!(replay.0, "c_1", "the replay carries an id of the hub's own");
    assert_ne!(
        replay.0, accepted,
        "the replay carries an id of the hub's own"
    );
    assert_eq!(replay.2, Some("full".into()), "{got:?}");
    assert_eq!(
        got[before + 2],
        (reply.into(), "reply".into(), Some("full".into()))
    );
}

#[test]
fn a_subscribe_accepted_as_the_session_dies_is_replayed() {
    an_accepted_subscribe_is_replayed(None);
}

#[test]
fn a_level_change_accepted_as_the_session_dies_is_replayed() {
    an_accepted_subscribe_is_replayed(Some("summary"));
}

#[test]
fn the_last_queued_command_holds_the_relay_until_its_queue_is_empty() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_2", "steer");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the second command is queued");
    // Holds the relay thread after it pops its last queued command and
    // before it routes it: a command read meanwhile still queues on it.
    let (popped_tx, popped_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
        let crate::retire::PassOn::Popped(id) = passed else {
            panic!("the queue is passed on, not dropped");
        };
        popped_tx.send(id).unwrap_or(());
        let _released = release_rx.recv();
    }));
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&popped_rx)
            .expect("the queue is passed on"),
        "c_2"
    );
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_3", "reply");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the third command queues on the held relay");
    release_tx.send(()).unwrap_or(());
    assert_eq!(client.acknowledged("the reply"), "c_1");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert_eq!(client.acknowledged("the second reply"), "c_3");
    assert_eq!(starter.resumed().len(), 1, "one resume for all three");
    assert_eq!(
        received_by_connection(&starter),
        [[
            ("c_1".into(), "reply".into()),
            ("c_2".into(), "steer".into()),
            ("c_3".into(), "reply".into()),
        ]],
        "one connection for all three"
    );
}

#[test]
fn a_client_that_leaves_drops_the_queue_and_passes_nothing_on() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "reply");
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_2", "steer");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the second command is queued");
    let (dropped_tx, dropped_rx) = mpsc::channel();
    *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
        let crate::retire::PassOn::Dropped(queued) = passed else {
            panic!("the queue is dropped, not passed on");
        };
        dropped_tx.send(queued).unwrap_or(());
    }));
    drop(client);
    let (gone_tx, gone_rx) = mpsc::channel();
    let gone_hub = Arc::clone(&hub);
    thread::spawn(move || {
        while gone_hub.clients() != 0 {
            thread::yield_now();
        }
        gone_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&gone_rx)
        .expect("the hub drops the disconnected client");
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&dropped_rx)
            .expect("the queue is cleared"),
        1,
        "the queued command is dropped"
    );
    assert!(
        starter.await_received(1, DEADLINE),
        "the in-flight re-route finished"
    );
    assert_eq!(received(&starter), [("c_1".into(), "reply".into())]);
}

#[test]
fn an_acknowledgement_is_forwarded_before_the_next_queued_command_passes_on() {
    let temp = Temp::new();
    temp.recorded();
    temp.append("fiber_exited");
    let dying = closing_session(&temp);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    // Parks the relay thread on its first line: both commands are written
    // to the exiting relay, and their answers wait in its kernel buffer.
    let (parked_tx, parked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_replay_filter) = Some(Box::new(move |_, _| {
        parked_tx.send(()).unwrap_or(());
        let _released = release_rx.recv();
    }));
    client.send("c_1", "reply");
    client.subscribe("c_2", "full");
    Deadline::after(DEADLINE)
        .recv(&parked_rx)
        .expect("the relay reads the first answer");
    assert!(
        dying.await_received(2, DEADLINE),
        "the dying process answered both"
    );
    assert!(
        dying.await_accepted(1, DEADLINE),
        "the dying process wrote the acceptance"
    );
    release_tx.send(()).unwrap_or(());
    let until = temp.clock.origin() + HELD_POLL;
    assert!(
        temp.clock
            .await_parked(until, fakes::Deadline::after(DEADLINE)),
        "the hub waits out the exiting process"
    );
    let (routed_tx, routed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_relay) = Some(Box::new(move || {
        routed_tx.send(()).unwrap_or(());
    }));
    client.send("c_3", "steer");
    Deadline::after(DEADLINE)
        .recv(&routed_rx)
        .expect("the third command is queued");
    // The forwarded acknowledgement and the pass-on, in order: a pass-on
    // before the forward logs `Popped` first.
    let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let (go_tx, go_rx) = mpsc::channel();
    *crate::connection::lock(&hub.before_accepted) = Some(Box::new(move |line, _| {
        assert!(crate::relay::acknowledges(line, "c_2"), "{line:?}");
        go_rx.recv().expect("the test releases the forward");
    }));
    let logging = Arc::clone(&log);
    *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
        let crate::retire::PassOn::Popped(id) = passed else {
            panic!("the queue is passed on, not dropped");
        };
        logging.lock().unwrap().push(format!("Popped({id})"));
    }));
    assert!(dying.stop(&sid(), DEADLINE), "the exiting process ended");
    temp.clock
        .advance_when_parked(Some(until), None, HELD_POLL, &Deadline::after(DEADLINE));
    assert_eq!(client.acknowledged("the reply"), "c_1");
    // Armed once the first acknowledgement is read: the next forward is
    // the held acceptance, so the one-shot hook cannot be consumed first
    // by the resumed session's answer.
    let logging = Arc::clone(&log);
    *crate::connection::lock(&hub.before_forward) = Some(Box::new(move |line, _| {
        if crate::relay::acknowledges(line, "c_2") {
            logging.lock().unwrap().push("Forward(c_2)".to_owned());
        }
    }));
    go_tx.send(()).unwrap_or(());
    assert_eq!(client.acknowledged("the subscribe"), "c_2");
    assert_eq!(client.acknowledged("the steer"), "c_3");
    assert_eq!(*log.lock().unwrap(), ["Forward(c_2)", "Popped(c_3)"]);
    assert_eq!(starter.resumed().len(), 1, "one resume");
}

#[test]
fn a_command_routed_while_its_relay_drains_is_passed_on() {
    let temp = Temp::new();
    temp.recorded();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    // In the forward of c_1's acknowledgement, which runs on the relay
    // thread: hand the test the relays and arm a signal for the top of
    // the thread's next read, past every relays lock after a forward.
    let (relays_tx, relays_rx) = mpsc::channel();
    let (reading_tx, reading_rx) = mpsc::channel();
    *crate::connection::lock(&hub.before_forward) = Some(Box::new(move |_, relays| {
        *crate::connection::lock(&crate::connection::lock(relays).order.before_read) =
            Some(Box::new(move || reading_tx.send(()).unwrap_or(())));
        relays_tx.send(Arc::clone(relays)).unwrap_or(());
    }));
    client.subscribe("c_1", "summary");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    let relays = Deadline::after(DEADLINE)
        .recv(&relays_rx)
        .expect("the forward ran");
    Deadline::after(DEADLINE)
        .recv(&reading_rx)
        .expect("the relay thread waits on its next read");
    let (draining_tx, draining_rx) = mpsc::channel();
    let order = crate::connection::lock(&relays).order.clone();
    *crate::connection::lock(&order.before_drain) =
        Some(Box::new(move || draining_tx.send(()).unwrap_or(())));
    // In route, with c_3 kept as sent and not yet written: end the
    // session, then wait until the relay thread reaches its drain.
    let stopping = starter.clone();
    crate::connection::lock(&relays).before_command_write = Some(Box::new(move || {
        assert!(stopping.stop(&sid(), DEADLINE), "the fake session ended");
        await_drain(&draining_rx);
    }));
    client.send("c_3", "reply");
    assert_eq!(client.acknowledged("the reply"), "c_3");
    assert_eq!(starter.resumed().len(), 2);
    assert_eq!(
        received(&starter).last().unwrap(),
        &("c_3".into(), "reply".into())
    );
}

#[test]
fn a_removed_workspace_resumes_in_its_recorded_repository() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    temp.worktree_log(
        false,
        json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
    );
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), repository)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_present_workspace_resumes_in_the_workspace_whatever_repository_is_recorded() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    let (workspace, _) = temp.worktree_log(
        true,
        json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
    );
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_removed_workspace_without_a_repository_is_refused_before_any_start() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let (workspace, _) = temp.worktree_log(false, json!({"path": workspace, "branch": "fiber/s"}));
    // The workspace is gone, and with no recorded repository there is
    // nowhere to make it from: the hub refuses before starting a process.
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        refused.message,
        format!(
            "Session {SID} could not resume: {} no longer exists",
            workspace.display()
        )
    );
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

/// A starter whose resume always fails missing, naming its target: the
/// race and the confirmed case behind the hub's wrap.
struct MissingStarter {
    tried: Mutex<Vec<(SessionId, PathBuf)>>,
}

impl MissingStarter {
    fn new() -> Self {
        Self {
            tried: Mutex::new(Vec::new()),
        }
    }

    fn tried(&self) -> Vec<(SessionId, PathBuf)> {
        self.tried.lock().unwrap().clone()
    }
}

impl Starter for MissingStarter {
    fn start(
        &self,
        _id: &SessionId,
        _workspace: &Path,
        _model: Option<&str>,
        _overrides: &[&str],
        _worktree: bool,
        _continues: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn Started>> {
        panic!("no start in the missing-target tests")
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        _credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        self.tried
            .lock()
            .unwrap()
            .push((id.clone(), workspace.to_path_buf()));
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} no longer exists", workspace.display()),
        ))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &Path,
        _from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        panic!("no rewind in the missing-target tests")
    }
}

#[test]
fn a_starter_failure_on_an_already_missing_target_keeps_the_ruled_shape() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    temp.worktree_log(
        false,
        json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
    );
    // The workspace and its repository are both gone, so the launch
    // target was already missing when the starter failed: the confirmed
    // case, refused with the ruled shape and no wrapper period.
    let starter = Arc::new(MissingStarter::new());
    let hub = temp.hub_with(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        refused.message,
        format!(
            "Session {SID} could not resume: {} no longer exists",
            repository.display()
        )
    );
    assert_eq!(starter.tried(), [(sid(), repository)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_starter_failure_on_a_present_target_keeps_the_wrapper_period() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    // The workspace was present at the check, so the starter failure is
    // the race: the refusal keeps the wrapper's period and renders like
    // any other refusal.
    let starter = Arc::new(MissingStarter::new());
    let hub = temp.hub_with(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        refused.message,
        format!(
            "Session {SID} could not resume: {} no longer exists.",
            workspace.display()
        )
    );
    assert_eq!(starter.tried(), [(sid(), workspace)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_removed_plain_workspace_is_refused_before_any_start() {
    let temp = Temp::new();
    let workspace = temp.recorded();
    fs::remove_dir_all(&workspace).unwrap();
    // A plain directory records no worktree, so the missing workspace
    // cannot be made again: the hub refuses before starting a process.
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        refused.message,
        format!(
            "Session {SID} could not resume: {} no longer exists",
            workspace.display()
        )
    );
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_repository_that_is_not_a_string_is_ignored_and_the_log_is_not_corrupt() {
    for repository in [json!(7), Value::Null, json!(["x"])] {
        let temp = Temp::new();
        let workspace = temp.dir.join(format!("w-{SECRET}"));
        let (workspace, _) = temp.worktree_log(
            false,
            json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
        );
        // A repository that is not a string is no repository: the missing
        // workspace cannot be made again, so the resume is refused before
        // any start, and the log is still not corrupt.
        let starter = FakeStarter::bind_and_hold(&temp.dir);
        let hub = temp.hub(starter.clone());
        let refused = refused(resumed(&hub));
        assert_eq!(refused.code, contract::ErrorCode::IoFailed, "{repository}");
        assert_eq!(
            refused.message,
            format!(
                "Session {SID} could not resume: {} no longer exists",
                workspace.display()
            ),
            "{repository}"
        );
        assert!(starter.resumed().is_empty(), "{repository}");
        assert!(!temp.hub_log().contains(SECRET), "no path in the log");
    }
    // A `worktree` that is not an object holds no repository either.
    let temp = Temp::new();
    let (workspace, _) = temp.worktree_log(false, json!("x"));
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let refused = refused(resumed(&hub));
    assert_eq!(refused.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        refused.message,
        format!(
            "Session {SID} could not resume: {} no longer exists",
            workspace.display()
        )
    );
    assert!(starter.resumed().is_empty(), "no process starts");
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_dangling_symlink_workspace_is_present() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    let (workspace, _) = temp.worktree_log(
        false,
        json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
    );
    std::os::unix::fs::symlink(temp.dir.join("no-such-target"), &workspace).unwrap();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_lookup_error_other_than_not_found_keeps_the_workspace() {
    // The workspace is `<regular file>/inner`: its metadata fails with
    // `ENOTDIR`, never `NotFound`.
    let temp = Temp::new();
    let base = temp.dir.join(format!("w-{SECRET}"));
    fs::write(&base, "x").unwrap();
    let workspace = base.join("inner");
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    temp.log(
        &json!({
            "kind": "session_started",
            "seq": 0,
            "session_id": SID,
            "payload": {
                "workspace": workspace,
                "worktree": {"path": workspace, "branch": "fiber/s", "repository": repository},
            },
        })
        .to_string(),
    );
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    assert!(resumed(&hub).is_ok());
    assert_eq!(starter.resumed(), [(sid(), workspace)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_removed_workspace_resumes_in_its_recorded_repository_through_serve() {
    let temp = Temp::new();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    let repository = temp.dir.join(format!("r-{SECRET}")).join(".git");
    let (_, repository) = temp.worktree_log(
        false,
        json!({"path": workspace, "branch": "fiber/s", "repository": repository}),
    );
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.send("c_1", "subscribe");
    assert_eq!(client.acknowledged("the subscribe"), "c_1");
    assert_eq!(starter.resumed(), [(sid(), repository)]);
    assert!(!temp.hub_log().contains(SECRET), "no path in the log");
}

#[test]
fn a_refused_labelled_subscribe_asks_once() {
    // The guard for the terminal's refused open: the hub tries the
    // asked label once, so `asked()` stays `[Some("nope")]`.
    let temp = Temp::new();
    temp.recorded();
    let exited = failure(ErrorCode::CredentialMissing, "no credential label `nope`");
    let starter = FakeStarter::exit_with(&temp.dir, exited);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_with("c_1", "full", Some("nope"));
    let rejected = client.next("the rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["command_id"], "c_1");
    assert_eq!(starter.asked(), [Some("nope".to_owned())]);
}

/// Writes `SID`'s log as a finished delegate's: `session_started` naming
/// the parent, then `fiber_started` and `fiber_exited`, each with its
/// `seq`, so a replay carries `last_seq` and a verbatim fold.
fn finished_delegate_log(temp: &Temp) {
    let workspace = temp.workspace();
    let started = json!({"kind": "session_started", "seq": 0, "session_id": SID,
        "ts": 1, "schema_version": 1,
        "payload": {"workspace": workspace, "parent": parent()}});
    let middle = json!({"kind": "fiber_started", "seq": 1, "session_id": SID,
        "ts": 2, "schema_version": 1, "payload": {}});
    let exited = json!({"kind": "fiber_exited", "seq": 2, "session_id": SID,
        "ts": 3, "schema_version": 1, "payload": {}});
    temp.write_lines(
        SID,
        &[started.to_string(), middle.to_string(), exited.to_string()],
    );
}

/// Writes the parent's log with `session_started` then one job line per
/// `kinds`, each naming `j_1` with the next `seq`.
fn parent_log(temp: &Temp, kinds: &[&str]) {
    let workspace = temp.workspace();
    let started = json!({"kind": "session_started", "seq": 0,
        "session_id": "s_1111111111111111", "ts": 1, "schema_version": 1,
        "payload": {"workspace": workspace}})
    .to_string();
    let mut lines = vec![started];
    for (n, kind) in kinds.iter().enumerate() {
        let line = json!({"kind": kind, "seq": n + 1, "session_id": "s_1111111111111111",
            "ts": 2, "schema_version": 1, "payload": {"job_id": "j_1"}});
        lines.push(line.to_string());
    }
    temp.write_lines("s_1111111111111111", &lines);
}

#[test]
fn an_exited_delegate_with_a_completed_job_replays() {
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started", "job_completed"]);
    assert!(super::exited_delegate(&temp.dir, &sid()));
}

#[test]
fn an_exited_delegate_with_an_open_job_started_does_not_replay() {
    let temp = Temp::new();
    finished_delegate_log(&temp);
    for jobs in [
        vec!["job_started"],
        vec!["job_started", "job_completed", "job_started"],
        // A delivery naming the job never closes its run: the latest
        // lifecycle line is still the start.
        vec!["job_started", "job_line"],
    ] {
        parent_log(&temp, &jobs);
        assert!(!super::exited_delegate(&temp.dir, &sid()), "{jobs:?}");
    }
}

#[test]
fn an_exited_delegate_without_a_parent_log_replays() {
    // No parent log can hold an open `job_started`, so the window is shut.
    let temp = Temp::new();
    finished_delegate_log(&temp);
    assert!(super::exited_delegate(&temp.dir, &sid()));
}

#[test]
fn what_is_not_a_finished_delegate_does_not_replay() {
    for shape in [
        // Running: the tail is not `fiber_exited`.
        (false, true),
        // Not a delegate: no parent names a job.
        (true, false),
    ] {
        let temp = Temp::new();
        if shape.1 {
            temp.delegate_log(parent());
        } else {
            temp.recorded();
        }
        if shape.0 {
            temp.append("fiber_exited");
        }
        assert!(!super::exited_delegate(&temp.dir, &sid()), "{shape:?}");
    }
    // A malformed parent names no job to check the window for.
    for parent in [json!("s0"), Value::Null] {
        let temp = Temp::new();
        temp.delegate_log(parent);
        temp.append("fiber_exited");
        assert!(!super::exited_delegate(&temp.dir, &sid()));
    }
}

#[test]
fn a_full_subscribe_to_an_exited_delegate_replays_its_log() {
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started", "job_completed"]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "full");
    let accepted = client.next("the replayed acknowledgement");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(accepted["payload"]["command_id"], "c_1");
    assert_eq!(accepted["payload"]["result"]["last_seq"], 2, "{accepted}");
    let kinds: Vec<String> = (0..3)
        .map(|n| {
            let line = client.next("the next folded line");
            assert_eq!(line["session_id"], SID, "{line}");
            assert_eq!(line["seq"], n, "{line}");
            line["kind"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(kinds, ["session_started", "fiber_started", "fiber_exited"]);
    // Every other command is refused exactly as today: the replay kept no
    // live tail, and the entry already drained.
    client.send("c_2", "steer");
    let refused = client.next("the refusal");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["command_id"], "c_2");
    assert_eq!(refused["payload"]["code"], "session_not_found");
    assert_eq!(refused["payload"]["message"], DELEGATE_REFUSED);
    assert!(starter.resumed().is_empty(), "no process starts");
}

#[test]
fn a_summary_subscribe_to_an_exited_delegate_is_refused_without_a_replay() {
    // A summary (non-`full`) subscribe never enters the replay branch:
    // only a `full` subscribe replays the exited run's log, so the `&&`
    // joining the command and the level in `route` keeps this a delegate
    // refusal with no accepted fold.
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started", "job_completed"]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "summary");
    let refused = client.next("the refusal");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["command_id"], "c_1");
    assert_eq!(refused["payload"]["code"], "session_not_found");
    assert_eq!(refused["payload"]["message"], DELEGATE_REFUSED);
    assert!(starter.resumed().is_empty(), "no process starts");
}

#[test]
fn a_full_subscribe_bypasses_a_live_relay_for_an_exited_delegate() {
    let temp = Temp::new();
    temp.delegate_log(parent());
    let running = FakeStarter::bind_and_hold(&temp.dir);
    let started =
        crate::Starter::start(&running, &sid(), &temp.workspace(), None, &[], false, None);
    assert!(started.is_ok(), "the delegate binds");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    let (relays_tx, relays_rx) = mpsc::channel();
    *crate::connection::lock(&hub.before_forward) = Some(Box::new(move |_, relays| {
        relays_tx.send(Arc::clone(relays)).unwrap_or(());
    }));
    client.subscribe("c_0", "summary");
    assert_eq!(client.acknowledged("the live subscribe"), "c_0");
    let relays = Deadline::after(DEADLINE)
        .recv(&relays_rx)
        .expect("the forward ran");
    // The run ends after the live relay publishes.
    finished_delegate_log(&temp);
    crate::connection::lock(&relays).before_command_write = Some(Box::new(|| {
        panic!("the replay writes nothing to the session");
    }));
    crate::connection::lock(&relays).at_gate = Some(Box::new(|| {
        panic!("the replay opens nothing");
    }));
    *crate::connection::lock(&hub.before_open) = Some(Box::new(|| {
        panic!("the replay resumes nothing");
    }));
    client.subscribe("c_1", "full");
    let accepted = client.next("the replayed acknowledgement");
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(accepted["payload"]["command_id"], "c_1");
    assert_eq!(accepted["payload"]["result"]["last_seq"], 2, "{accepted}");
    let kinds: Vec<String> = (0..3)
        .map(|_| {
            let line = client.next("the next folded line");
            line["kind"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(kinds, ["session_started", "fiber_started", "fiber_exited"]);
    assert_eq!(
        received(&running),
        [("c_0".to_owned(), "subscribe".to_owned())],
        "the dying session receives nothing more"
    );
    assert!(starter.resumed().is_empty(), "no process starts");
    // The tripwires above only guard the replay: the steer that follows
    // is meant to reach the live relay.
    crate::connection::lock(&relays).before_command_write.take();
    crate::connection::lock(&relays).at_gate.take();
    crate::connection::lock(&hub.before_open).take();
    // A steer still writes to the live relay as today.
    client.send("c_2", "steer");
    assert_eq!(client.acknowledged("the steer"), "c_2");
    assert!(
        received(&running).contains(&("c_2".to_owned(), "steer".to_owned())),
        "the steer reached the live relay"
    );
}

#[test]
fn a_full_subscribe_waits_out_an_open_delegate_job() {
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started"]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe("c_1", "full");
    let refused = client.next("the refusal");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["command_id"], "c_1");
    assert_eq!(refused["payload"]["code"], "session_not_found");
    assert_eq!(refused["payload"]["message"], DELEGATE_REFUSED);
    assert!(starter.resumed().is_empty(), "no process starts");
    // The job completing replays the same log: the window shut.
    parent_log(&temp, &["job_started", "job_completed"]);
    client.subscribe("c_2", "full");
    assert_eq!(client.acknowledged("the replayed subscribe"), "c_2");
}

#[test]
fn a_malformed_full_subscribe_to_an_exited_delegate_is_refused() {
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started", "job_completed"]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let mut client = Client::connect(&hub);
    client.subscribe_raw("c_1", json!({"level": "full", "credential": 7}));
    let refused = client.next("the refusal");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["command_id"], "c_1");
    assert_eq!(refused["payload"]["code"], "invalid_arguments");
    assert!(starter.resumed().is_empty(), "no process starts");
}

#[test]
fn a_rerouted_full_subscribe_to_an_exited_delegate_replays_and_releases_the_queue() {
    // The `Reroute` arm routes with `from` and `exited` set: the replay
    // answers in command order, releasing the parked waiter.
    let temp = Temp::new();
    finished_delegate_log(&temp);
    parent_log(&temp, &["job_started", "job_completed"]);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter.clone());
    let relays = Arc::new(Mutex::new(crate::relay::Relays::default()));
    let epoch = crate::connection::lock(&relays).mint();
    let (writer, peer) = UnixStream::pair().unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let stripped = json!({"id": "c_1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let order = crate::connection::lock(&relays).order.clone();
    crate::retire::enqueue_new(
        &order,
        SID,
        &contract::CommandId("c_1".to_owned()),
        &stripped,
    );
    order.mark_rerouted("c_1", SID, epoch);
    crate::relay::route(
        &contract::CommandId("c_1".to_owned()),
        SID,
        stripped,
        &hub,
        &writer,
        &relays,
        Some(epoch),
        true,
    );
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut read = BufReader::new(peer);
    let mut text = String::new();
    read.read_line(&mut text).unwrap();
    let accepted: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
    assert_eq!(accepted["payload"]["command_id"], "c_1");
    assert_eq!(accepted["payload"]["result"]["last_seq"], 2, "{accepted}");
    // The replay's acknowledgement released the queue: nothing is still
    // parked behind the re-routed command.
    let (done_tx, done) = mpsc::channel();
    let waiting = order.clone();
    thread::spawn(move || {
        waiting.wait_rerouted(SID, epoch, "c_2");
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the replayed acknowledgement released the queue");
    assert!(starter.resumed().is_empty(), "no process starts");
}
