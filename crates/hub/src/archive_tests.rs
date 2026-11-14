//! Tests for hub `archive`: validation, refusal, publication order,
//! idempotence, failure retention and the `on_left` race. Every log here
//! carries `seq` on each line, since [`log::Log::open`] derives the next
//! `seq` from the last complete line.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::clock::Clock;
use contract::{ErrorCode, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value};

use super::{archive, pause_before_commit};
use crate::connection::{Hub, Refused};
use crate::fake::{FakeSession, FakeStarter, status, status_line};
use crate::feed::{self, Closer, Feed, pause_before_recent_mutex};
use crate::testkit::{self, DEADLINE};

/// What an archive test owns: the home, its hub with a shared fake
/// clock, and every socket and worker it starts. Children are stopped
/// and workers joined before the home is released.
struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<FakeClock>,
    sessions: Vec<FakeSession>,
    feed: Option<Arc<Feed>>,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = testkit::home("ha");
        Self {
            dir,
            held,
            clock: FakeClock::new(),
            sessions: Vec::new(),
            feed: None,
        }
    }

    fn hub(&self) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = Arc::clone(&self.clock) as Arc<dyn Clock>;
        Arc::new(testkit::hub(
            &self.dir,
            Arc::new(FakeStarter::bind_and_hold(&self.dir)),
            timed,
        ))
    }

    fn dir_of(&self, n: u64) -> PathBuf {
        log::session_dir(&self.dir, "-w", &SessionId(testkit::id(n)))
    }

    /// Writes session `n`'s log with `kinds` after `session_started`,
    /// each line carrying its `seq`; the lock file is left unheld. A
    /// `fiber_exited` last line needs no further payload: open only
    /// parses folded kinds, and neither started nor exited is one.
    fn logged(&self, n: u64, kinds: &[&str]) -> PathBuf {
        let dir = self.dir_of(n);
        fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        let mut seq = 0u64;
        let mut push = |kind: &str, payload: Value| {
            text.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "kind": kind,
                    "session_id": testkit::id(n),
                    "ts": 1_700_000_000_000 + seq,
                    "schema_version": 1,
                    "seq": seq,
                    "payload": payload,
                }))
                .unwrap(),
            );
            text.push('\n');
            seq += 1;
        };
        push(
            "session_started",
            serde_json::json!({
                "workspace": "/w",
                "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            }),
        );
        for kind in kinds {
            push(kind, Value::Object(Default::default()));
        }
        fs::write(dir.join("events.jsonl"), &text).unwrap();
        fs::write(dir.join("session.lock"), b"").unwrap();
        dir
    }

    /// Appends an exited row for session `n`, its directory made.
    fn exited_row(&self, n: u64) {
        self.indexed(n, "/w", None);
    }

    fn indexed(&self, n: u64, workspace: &str, parent: Option<&str>) {
        let status = crate::fake::status("session", workspace, "idle", parent);
        let row = crate::recent::RecentRow {
            session_id: SessionId(testkit::id(n)),
            ts: n,
            project: "-w".to_owned(),
            workspace: workspace.to_owned(),
            name: format!("session {n}"),
            how: crate::recent::Left::Exited,
            status: Some(serde_json::from_value(status).unwrap()),
            archived: false,
        };
        crate::recent::append(&self.dir, &row).unwrap();
    }

    fn hub_with_keep(&self, keep: usize) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = Arc::clone(&self.clock) as Arc<dyn Clock>;
        Arc::new(
            testkit::hub(
                &self.dir,
                Arc::new(FakeStarter::bind_and_hold(&self.dir)),
                timed,
            )
            .with_keep_exited(keep),
        )
    }

    /// Starts the hub's feed on the shared clock: a stop that hangs
    /// fails the test, so every started feed is stopped by [`Temp`].
    fn start_feed(&mut self, hub: &Hub) {
        testkit::start(&hub.feed, &self.clock);
        self.feed = Some(Arc::clone(&hub.feed));
    }

    /// Binds session `n`'s socket and says a running status: the feed
    /// follows it on its first scan after starting, replaying the line.
    fn running(&mut self, n: u64) {
        let session = FakeSession::bind(&self.dir, &testkit::id(n));
        session.say(&running_line(n));
        self.sessions.push(session);
    }

    /// Waits until the feed follows session `n` as running.
    fn await_running(&self, hub: &Hub, n: u64) {
        testkit::await_true("the session runs", {
            let feed = Arc::clone(&hub.feed);
            let id = testkit::id(n);
            move || feed.live().iter().any(|(live, _)| live == &id)
        });
    }

    fn log_text(&self, n: u64) -> String {
        fs::read_to_string(self.dir_of(n).join("events.jsonl")).unwrap()
    }

    fn recent_text(&self) -> String {
        fs::read_to_string(self.dir.join("recent.jsonl")).unwrap_or_default()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        // Sockets close before the feed stops, so no follow outlives the
        // home; the owning fixture stops the feed directly, joining its
        // workers on this thread.
        for session in self.sessions.drain(..) {
            session.close();
        }
        if let Some(feed) = self.feed.take() {
            feed.stop();
        }
    }
}

/// A running `session_status` line for session `n`, as the session sends it.
fn running_line(n: u64) -> String {
    status_line(&testkit::id(n), &status("n", "/w", "idle", None))
}

fn args(value: Value) -> Map<String, Value> {
    testkit::args(value)
}

fn call(hub: &Hub, value: Value) -> Result<Option<Value>, Refused> {
    archive(hub, &args(value))
}

fn archived_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| line["kind"] == "archived")
        .collect()
}

#[test]
fn arguments_that_do_not_fit_are_invalid_arguments() {
    let temp = Temp::new();
    let hub = temp.hub();
    for bad in [
        serde_json::json!({}),
        serde_json::json!({"session": null}),
        serde_json::json!({"session": 1}),
        serde_json::json!({"session": ""}),
        serde_json::json!({"session": "nope"}),
        serde_json::json!({"session": "s_1", "extra": 1}),
        serde_json::json!({"future": 1}),
    ] {
        let refused = call(&hub, bad.clone()).unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidArguments, "{bad}");
    }
    // A valid shape on a missing session parses, then misses the index.
    let missing = serde_json::json!({"session": testkit::id(1)});
    let refused = call(&hub, missing.clone()).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionNotFound, "{missing}");
}

#[test]
fn missing_nonplain_and_logless_sessions_are_not_found() {
    let temp = Temp::new();
    let hub = temp.hub();
    // Missing.
    let refused = call(&hub, serde_json::json!({"session": testkit::id(1)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    // A directory without a log never resolves.
    fs::create_dir_all(temp.dir_of(2)).unwrap();
    let refused = call(&hub, serde_json::json!({"session": testkit::id(2)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    // A link to a session directory is not a plain session directory.
    let dir = temp.logged(3, &["fiber_exited"]);
    temp.exited_row(3);
    let link = temp.dir_of(4);
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let refused = call(&hub, serde_json::json!({"session": testkit::id(4)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    // The link target itself still archives: the refusal is the shape,
    // not the session.
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(3)}))
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_exited_session_archives_with_the_next_seq_and_empty_payload() {
    let temp = Temp::new();
    let hub = temp.hub();
    let dir = temp.logged(1, &["fiber_exited"]);
    temp.exited_row(1);
    let before_log = temp.log_text(1);
    let before_recent = temp.recent_text();
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(1)}))
            .unwrap()
            .is_none()
    );
    // The log gains exactly one line: the marker with the next seq
    // (`docs/events.md`, "`archived`": appended "with the next `seq`").
    let after = temp.log_text(1);
    assert!(after.starts_with(&before_log), "{after}");
    let added = &after[before_log.len()..];
    let line: Value = serde_json::from_str(added.trim_end()).unwrap();
    assert_eq!(line["kind"], "archived");
    assert_eq!(line["seq"], 2);
    assert_eq!(line["payload"], serde_json::json!({}));
    assert_eq!(line["session_id"], testkit::id(1));
    // The index gains exactly one row: the exit row marked archived.
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{}", temp.recent_text());
    assert_eq!(rows[0]["archived"], Value::Null);
    assert_eq!(rows[1]["archived"], true);
    for key in [
        "session_id",
        "ts",
        "project",
        "workspace",
        "name",
        "how",
        "status",
    ] {
        assert_eq!(rows[1][key], rows[0][key], "{key}");
    }
    // Nothing else in the home changed: artifacts, workspace and the
    // original rows are untouched.
    assert!(dir.join("session.lock").is_file());
    assert_eq!(temp.recent_text()[..before_recent.len()], before_recent);
}

#[test]
fn a_crashed_session_without_a_row_archives_from_its_opening() {
    let temp = Temp::new();
    let hub = temp.hub();
    // Crashed: the last line is not an exit, and no row was indexed.
    temp.logged(2, &["turn_completed"]);
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(2)}))
            .unwrap()
            .is_none()
    );
    let lines: Vec<Value> = temp
        .log_text(2)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2]["kind"], "archived");
    assert_eq!(lines[2]["seq"], 2);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{}", temp.recent_text());
    assert_eq!(rows[0]["session_id"], testkit::id(2));
    assert_eq!(rows[0]["how"], "crashed");
    assert_eq!(rows[0]["archived"], true);
    assert_eq!(rows[0]["project"], "-w");
    assert_eq!(rows[0]["workspace"], "/w");
    assert_eq!(rows[0]["name"], "");
    assert_eq!(rows[0]["status"], Value::Null);
}

#[test]
fn an_exited_waiting_session_without_a_row_archives_with_the_feed_status() {
    // Pins the feed-status path of `missing_row`: with a waiting `Left`
    // entry and no derived row, the archived row copies the feed's status
    // instead of the opening. Fails when `status_of` reads as `None`.
    let mut temp = Temp::new();
    let hub = temp.hub();
    temp.logged(11, &["fiber_exited"]);
    let session = FakeSession::bind(&temp.dir, &testkit::id(11));
    session.say(&status_line(
        &testkit::id(11),
        &status("feed-name", "/feed-ws", "waiting", None),
    ));
    temp.sessions.push(session);
    temp.start_feed(&hub);
    temp.await_running(&hub, 11);
    temp.sessions.pop().unwrap().close();
    let feed = Arc::clone(&hub.feed);
    let id = testkit::id(11);
    testkit::await_true("the waiting session to leave", move || {
        feed.live().iter().all(|(live, _)| live != &id) && feed.status_of(&id).is_some()
    });
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(11)}))
            .unwrap()
            .is_none()
    );
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{}", temp.recent_text());
    assert_eq!(rows[0]["how"], "crashed");
    assert_eq!(rows[0]["archived"], true);
    assert_eq!(rows[0]["workspace"], "/feed-ws");
    assert_eq!(rows[0]["name"], "feed-name");
    assert_eq!(rows[0]["status"]["workspace"], "/feed-ws");
    assert_eq!(rows[0]["status"]["name"], "feed-name");
}

#[test]
fn archive_refuses_a_running_session_without_appending() {
    let mut temp = Temp::new();
    let hub = temp.hub();
    // Running: the log has no exit line, and the feed follows the
    // session while its lock file sits unheld, so only the live set
    // refuses it.
    temp.logged(3, &[]);
    temp.running(3);
    temp.start_feed(&hub);
    temp.await_running(&hub, 3);
    let before_log = temp.log_text(3);
    let refused = call(&hub, serde_json::json!({"session": testkit::id(3)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert_eq!(temp.log_text(3), before_log);
    assert!(temp.recent_text().is_empty());
}

#[test]
fn archive_refuses_a_held_lock_without_appending() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(4, &["fiber_exited"]);
    temp.exited_row(4);
    // Held on this thread, as a running session process holds it: the
    // label names this process, and the lock releases at scope end.
    let dir = temp.dir_of(4);
    fs::write(
        dir.join("session.lock"),
        format!("keeper {}", std::process::id()),
    )
    .unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("session.lock"))
        .unwrap();
    held.try_lock().unwrap();
    let before_log = temp.log_text(4);
    let before_recent = temp.recent_text();
    let refused = call(&hub, serde_json::json!({"session": testkit::id(4)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert_eq!(temp.log_text(4), before_log);
    assert_eq!(temp.recent_text(), before_recent);
}

#[test]
fn a_retry_writes_nothing_when_the_row_is_already_archived() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(5, &["fiber_exited"]);
    temp.exited_row(5);
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(5)}))
            .unwrap()
            .is_none()
    );
    let after_first_log = temp.log_text(5);
    let after_first_recent = temp.recent_text();
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(5)}))
            .unwrap()
            .is_none()
    );
    // A converged retry is byte-identical in both files.
    assert_eq!(temp.log_text(5), after_first_log);
    assert_eq!(temp.recent_text(), after_first_recent);
    assert_eq!(archived_lines(&after_first_log).len(), 1);
    let rows: Vec<Value> = after_first_recent
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["archived"], true);
}

#[test]
fn a_corrupt_log_refuses_io_failed_and_writes_no_row() {
    let temp = Temp::new();
    let hub = temp.hub();
    let dir = temp.logged(6, &["fiber_exited"]);
    temp.exited_row(6);
    // A complete line no open parses: the log's marker check skips it,
    // but the exclusive open refuses it, before any row is appended.
    fs::write(
        dir.join("events.jsonl"),
        format!("{}not json at all\n", temp.log_text(6)),
    )
    .unwrap();
    let before_recent = temp.recent_text();
    let refused = call(&hub, serde_json::json!({"session": testkit::id(6)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert_eq!(temp.recent_text(), before_recent);
}

#[test]
fn a_failed_index_append_keeps_the_event_and_reports_io_failed() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(7, &["fiber_exited"]);
    temp.exited_row(7);
    // An actual file error: the index path is a directory.
    let index = temp.dir.join("recent.jsonl");
    fs::remove_file(&index).unwrap();
    fs::create_dir(&index).unwrap();
    let refused = call(&hub, serde_json::json!({"session": testkit::id(7)})).unwrap_err();
    assert_eq!(refused.code, ErrorCode::IoFailed);
    // The event is retained: one archived line, no new row.
    assert_eq!(archived_lines(&temp.log_text(7)).len(), 1);
}

#[test]
fn a_retry_after_a_failed_index_repairs_the_row_without_a_new_event() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(8, &["fiber_exited"]);
    temp.exited_row(8);
    let index = temp.dir.join("recent.jsonl");
    let before = temp.recent_text();
    fs::remove_file(&index).unwrap();
    fs::create_dir(&index).unwrap();
    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(8)}))
            .unwrap_err()
            .code,
        ErrorCode::IoFailed
    );
    fs::remove_dir(&index).unwrap();
    fs::write(&index, before).unwrap();
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(8)}))
            .unwrap()
            .is_none()
    );
    // One archived line, and the repaired row is the exit row archived.
    assert_eq!(archived_lines(&temp.log_text(8)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{}", temp.recent_text());
    assert_eq!(rows[1]["archived"], true);
    // Resolution skips it for continue but still finds it explicitly.
    assert_eq!(
        log::resolve(
            &temp.dir.join("projects").join("-w").join("sessions"),
            &testkit::id(8),
            &|_| true
        )
        .unwrap()
        .0,
        testkit::id(8)
    );
    assert!(
        log::most_recent(
            &temp.dir.join("projects").join("-w").join("sessions"),
            &|_| true
        )
        .is_none_or(|id| id.0 != testkit::id(8))
    );
}

/// A feed subscriber's far end: lines read under [`DEADLINE`].
struct Sub {
    read: BufReader<UnixStream>,
}

impl Sub {
    fn new(feed: &Arc<Feed>) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let closer = Closer {
            stream: a.try_clone().unwrap(),
            web: None,
        };
        feed.subscribe(Arc::new(Mutex::new(a)), closer).unwrap();
        Self {
            read: BufReader::new(b),
        }
    }

    /// The next line, which must be the hub's `kind` line for `id` in
    /// `project` (`docs/invocation.md`, "What the hub speaks"): every
    /// archived session is told to every feed subscriber. No code path
    /// enqueues one before the fix, so the read times out there.
    fn forgot(&mut self, kind: &str, id: &str, project: &str) {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {kind} for {id}"));
        let line: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(line["kind"], kind, "{line}");
        assert_eq!(line["payload"]["session_id"], id, "{line}");
        assert_eq!(line["payload"]["project"], project, "{line}");
    }

    /// The next line, which must be `session_left` for `id` with `how`.
    fn left(&mut self, id: &str, how: &str) {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received session_left for {id}"));
        let line: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(line["kind"], "session_left", "{line}");
        assert_eq!(line["payload"]["session_id"], id, "{line}");
        assert_eq!(line["payload"]["how"], how, "{line}");
    }
}

/// Receives one note before the deadline, naming the wait on expiry.
#[track_caller]
fn recv<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(rx)
        .unwrap_or_else(|_| panic!("never received {what}"))
}

/// Advances the clock to the feed scanner's next scheduled scan,
/// waiting until the scanner parks there first: the late session's
/// follow can only arrive after a scan observes its socket.
fn advance_to_scan(temp: &Temp) {
    let feed = temp.feed.as_ref().expect("the feed is started");
    let scanner = feed.scanner_thread().expect("the feed is started");
    temp.clock.advance_when_parked(
        Some(temp.clock.now() + feed::RUN_SCAN),
        Some(scanner),
        feed::RUN_SCAN,
        &Deadline::after(DEADLINE),
    );
}

/// Joins a worker before the subsystem bound, returning its answer.
#[track_caller]
fn join<T: Send + 'static>(handle: thread::JoinHandle<T>, what: &str) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(handle.join()).unwrap_or(());
    });
    Deadline::after(fakes::WHOLE_SUBSYSTEM_WITHIN) // ruling: #4311
        .recv(&rx)
        .unwrap_or_else(|_| panic!("{what} never finished"))
        .unwrap_or_else(|_| panic!("{what} panicked"))
}

#[test]
fn two_overlapping_archives_converge_to_one_marker_and_row() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(9, &["fiber_exited"]);
    temp.exited_row(9);
    // Both archives run at once on owned workers: the gate serializes
    // them, and the second converges without a new event or row.
    let first = {
        let hub = Arc::clone(&hub);
        thread::spawn(move || call(&hub, serde_json::json!({"session": testkit::id(9)})))
    };
    let second = {
        let hub = Arc::clone(&hub);
        thread::spawn(move || call(&hub, serde_json::json!({"session": testkit::id(9)})))
    };
    assert!(join(first, "the first archive").unwrap().is_none());
    assert!(join(second, "the second archive").unwrap().is_none());
    // Convergence: one marker, and the row appended once.
    assert_eq!(archived_lines(&temp.log_text(9)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{}", temp.recent_text());
    assert_eq!(rows[1]["archived"], true);
}

#[test]
fn a_late_on_left_neither_appends_nor_reinserts_after_archive() {
    let mut temp = Temp::new();
    let hub = temp.hub();
    // Crashed: the log's last line is not an exit, and the feed saw
    // its socket close once, so a left entry and a crashed row exist.
    temp.logged(10, &["turn_completed"]);
    temp.running(10);
    temp.start_feed(&hub);
    temp.await_running(&hub, 10);
    let mut sub = Sub::new(&hub.feed);
    // The snapshot is a set: the replayed status plus the always-on
    // line (`docs/events.md`, "Hub feed", "keepers"), empty here.
    // Place is not promised and the close's line comes after the set, so
    // both snapshot lines are read before either one is asserted.
    let mut snapshot: Vec<Value> = Vec::new();
    for _ in 0..2 {
        let mut text = String::new();
        sub.read
            .read_line(&mut text)
            .expect("the snapshot lines arrive");
        snapshot.push(serde_json::from_str(&text).unwrap());
    }
    let mut kinds: Vec<&str> = snapshot
        .iter()
        .map(|line| line["kind"].as_str().unwrap_or("?"))
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["keepers", "session_status"]);
    let keepers = snapshot
        .iter()
        .find(|line| line["kind"] == "keepers")
        .expect("the snapshot holds a keepers line");
    assert_eq!(keepers["payload"], serde_json::json!({"keepers": []}));
    temp.sessions.pop().unwrap().close();
    sub.left(&testkit::id(10), "crashed");
    let id = testkit::id(10);
    // The archive pauses after the live check and before the exclusive
    // open, holding the recent mutex with no marker committed. Armed on
    // the worker thread: the seam is thread-local.
    let (paused_tx, paused_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let archiving = {
        let hub = Arc::clone(&hub);
        let worker_id = id.clone();
        thread::spawn(move || {
            pause_before_commit(move || {
                paused_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            call(&hub, serde_json::json!({"session": worker_id}))
        })
    };
    recv(&paused_rx, "the archive's pause");
    // A late close for the same session reaches the recent mutex only
    // once the scanner follows its new socket.
    let (arrived_tx, arrived_rx) = mpsc::channel::<()>();
    pause_before_recent_mutex(&temp.dir, move || {
        arrived_tx.send(()).unwrap();
    });
    let late = FakeSession::bind(&temp.dir, &id);
    testkit::await_scanner(&temp.clock);
    advance_to_scan(&temp);
    assert!(
        late.await_subscribed(1, DEADLINE),
        "the follow connects before the close"
    );
    late.close();
    recv(&arrived_rx, "the late close at the mutex");
    // The close is blocked on the mutex: no stale row is appended while
    // the archive holds it.
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{}", temp.recent_text());
    release_tx.send(()).unwrap();
    assert!(join(archiving, "the archive").unwrap().is_none());
    // The late close completes: settling the feed proves its transition
    // ran, since its `Settle` guard drops only at the end of `on_left`.
    // It appends no row and reinserts no entry: the default answer lists
    // nothing, archived or otherwise.
    let listed = join(
        thread::spawn({
            let hub = Arc::clone(&hub);
            move || {
                crate::sessions::answer(
                    &hub,
                    &testkit::args(serde_json::json!({})),
                    &*hub.clock,
                    None,
                )
            }
        }),
        "the sessions answer",
    )
    .unwrap()
    .unwrap();
    assert_eq!(listed, serde_json::json!({"live": [], "exited": []}));
    assert_eq!(hub.feed.status_of(&id), None);
    assert_eq!(archived_lines(&temp.log_text(10)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{}", temp.recent_text());
    assert_eq!(rows[1]["archived"], true);
    assert_eq!(rows[1]["how"], "crashed");
}

#[test]
fn recent_listing_repairs_a_marker_without_an_archived_row() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(22, &["fiber_exited", "archived"]);
    temp.exited_row(22);

    // `docs/invocation.md`, "Archiving": a retry repairs the derived row
    // without appending another archive event.
    let answer = feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(answer, serde_json::json!({"sessions": [], "total": 0}));
    assert_eq!(archived_lines(&temp.log_text(22)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["archived"], true);
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn recent_listing_archives_missing_workspaces_and_archived_mode_counts_it() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(12, &["fiber_exited"]);
    temp.exited_row(12);
    let mut sub = Sub::new(&hub.feed);
    let mut initial = String::new();
    sub.read.read_line(&mut initial).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&initial).unwrap()["kind"],
        "keepers"
    );

    let unarchived = feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(unarchived, serde_json::json!({"sessions": [], "total": 0}));
    sub.forgot("session_archived", &testkit::id(12), "-w");
    assert_eq!(archived_lines(&temp.log_text(12)).len(), 1);

    let archived = feed::recent_for_hub(&hub, &args(serde_json::json!({"archived": true})))
        .unwrap()
        .unwrap();
    assert_eq!(archived["total"], 1);
    assert_eq!(archived["sessions"][0]["session_id"], testkit::id(12));
    assert_eq!(archived["sessions"][0]["archived"], true);
    assert_eq!(archived_lines(&temp.log_text(12)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["archived"], true);
}

#[test]
fn archived_cursor_uses_the_post_archive_position_in_the_same_answer() {
    let temp = Temp::new();
    let hub = temp.hub();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    temp.logged(1, &["fiber_exited"]);
    temp.logged(2, &["fiber_exited", "archived"]);
    let rows = [
        crate::recent::RecentRow {
            session_id: SessionId(testkit::id(1)),
            ts: 1,
            project: "-w".to_owned(),
            workspace: temp
                .dir
                .join("missing-workspace")
                .to_string_lossy()
                .into_owned(),
            name: "newly archived".to_owned(),
            how: crate::recent::Left::Exited,
            status: None,
            archived: false,
        },
        crate::recent::RecentRow {
            session_id: SessionId(testkit::id(2)),
            ts: 2,
            project: "-w".to_owned(),
            workspace: workspace.to_string_lossy().into_owned(),
            name: "already archived".to_owned(),
            how: crate::recent::Left::Exited,
            status: None,
            archived: true,
        },
    ];
    for row in rows {
        crate::recent::append(&temp.dir, &row).unwrap();
    }

    // `docs/invocation.md`, "Archiving": an archived query answers after
    // automatic archive; its cursor therefore uses the appended-row order
    // (`RULING-4598-3`).
    let page = feed::recent_for_hub(
        &hub,
        &args(serde_json::json!({"archived": true, "before": testkit::id(1)})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["sessions"][0]["session_id"], testkit::id(2));
}

#[test]
fn an_explicit_archive_cascades_to_exited_delegate_descendants() {
    let temp = Temp::new();
    let hub = temp.hub();
    for n in 30..=32 {
        temp.logged(n, &["fiber_exited"]);
        let parent = match n {
            30 => None,
            31 => Some(testkit::id(30)),
            _ => Some(testkit::id(31)),
        };
        temp.indexed(n, "/w", parent.as_deref());
    }
    temp.logged(33, &["fiber_exited"]);
    temp.indexed(33, "/w", None);

    // `docs/invocation.md`, "Archiving": explicit parent archiving includes
    // every exited delegate descendant.
    assert!(
        call(&hub, serde_json::json!({"session": testkit::id(30)}))
            .unwrap()
            .is_none()
    );
    for n in 30..=32 {
        assert_eq!(archived_lines(&temp.log_text(n)).len(), 1, "session {n}");
    }
    assert!(archived_lines(&temp.log_text(33)).is_empty());
    assert_eq!(crate::recent::read_all(&temp.dir).len(), 7);
}

#[test]
fn a_cascaded_missing_delegate_is_not_appended_twice_in_one_listing() {
    let temp = Temp::new();
    let hub = temp.hub();
    let missing = temp
        .dir
        .join("missing-workspace")
        .to_string_lossy()
        .into_owned();
    temp.logged(20, &["fiber_exited"]);
    temp.logged(21, &["fiber_exited"]);
    temp.indexed(21, &missing, Some(&testkit::id(20)));
    temp.indexed(20, &missing, None);

    // `docs/invocation.md`, "Archiving": the cascade archives the exited
    // delegate with its parent and must not append that delegate twice.
    let answer = feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(answer, serde_json::json!({"sessions": [], "total": 0}));
    assert_eq!(archived_lines(&temp.log_text(20)).len(), 1);
    assert_eq!(archived_lines(&temp.log_text(21)).len(), 1);
    let rows: Vec<Value> = temp
        .recent_text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 4, "{}", temp.recent_text());
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn recent_cursor_uses_pre_archive_position_in_the_same_answer() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(1, &["fiber_exited"]);
    temp.logged(2, &["fiber_exited"]);
    let workspace = temp.dir.join("workspace-one");
    fs::create_dir_all(&workspace).unwrap();
    for (n, path) in [
        (1, workspace.to_string_lossy().into_owned()),
        (
            2,
            temp.dir
                .join("removed-workspace")
                .to_string_lossy()
                .into_owned(),
        ),
    ] {
        crate::recent::append(
            &temp.dir,
            &crate::recent::RecentRow {
                session_id: SessionId(testkit::id(n)),
                ts: n,
                project: "-w".to_owned(),
                workspace: path,
                name: format!("session {n}"),
                how: crate::recent::Left::Exited,
                status: None,
                archived: false,
            },
        )
        .unwrap();
    }
    let page = feed::recent_for_hub(&hub, &args(serde_json::json!({"before": testkit::id(2)})))
        .unwrap()
        .unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["sessions"][0]["session_id"], testkit::id(1), "{page}");
    assert_eq!(archived_lines(&temp.log_text(2)).len(), 1);
    assert!(archived_lines(&temp.log_text(1)).is_empty());
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn keep_two_archives_the_three_oldest_top_level_sessions() {
    let temp = Temp::new();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let hub = temp.hub_with_keep(2);
    for n in 1..=5 {
        temp.logged(n, &["fiber_exited"]);
        temp.indexed(n, &workspace, None);
    }

    let answer = feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(answer["total"], 2);
    assert_eq!(
        answer["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["session_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [testkit::id(5), testkit::id(4)]
    );
    for n in 1..=3 {
        assert_eq!(archived_lines(&temp.log_text(n)).len(), 1, "session {n}");
    }
    for n in 4..=5 {
        assert!(archived_lines(&temp.log_text(n)).is_empty(), "session {n}");
    }
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn keep_two_archives_oldest_parent_and_its_exited_delegates_only() {
    let temp = Temp::new();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let hub = temp.hub_with_keep(2);
    for n in 1..=5 {
        temp.logged(n, &["fiber_exited"]);
        let parent = (n == 4 || n == 5).then(|| testkit::id(1));
        temp.indexed(n, &workspace, parent.as_deref());
    }
    let mut sub = Sub::new(&hub.feed);
    let mut initial = String::new();
    sub.read.read_line(&mut initial).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&initial).unwrap()["kind"],
        "keepers"
    );

    let answer = feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(answer["total"], 2);
    for n in [1, 4, 5] {
        assert_eq!(archived_lines(&temp.log_text(n)).len(), 1, "session {n}");
    }
    for n in [2, 3] {
        assert!(archived_lines(&temp.log_text(n)).is_empty(), "session {n}");
    }
    let mut archived_ids = Vec::new();
    for _ in 0..3 {
        let mut text = String::new();
        sub.read.read_line(&mut text).unwrap();
        let line: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(line["kind"], "session_archived");
        assert_eq!(line["payload"]["project"], "-w");
        archived_ids.push(line["payload"]["session_id"].as_str().unwrap().to_owned());
    }
    archived_ids.sort();
    let mut expected_ids = [testkit::id(1), testkit::id(4), testkit::id(5)];
    expected_ids.sort();
    assert_eq!(archived_ids, expected_ids);
    let rows = crate::recent::read_all(&temp.dir);
    for n in [1, 4, 5] {
        assert!(
            rows.iter()
                .rev()
                .find(|row| row.session_id.0 == testkit::id(n))
                .unwrap()
                .archived,
            "session {n} has an archived recent row"
        );
    }
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn a_running_delegate_does_not_block_archiving_its_parent() {
    let mut temp = Temp::new();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let hub = temp.hub();
    temp.logged(1, &["fiber_exited"]);
    temp.logged(2, &["fiber_exited"]);
    temp.indexed(1, &workspace, None);
    temp.indexed(2, &workspace, Some(&testkit::id(1)));
    temp.running(2);
    temp.start_feed(&hub);
    temp.await_running(&hub, 2);

    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(1)})),
        Ok(None)
    );
    assert_eq!(archived_lines(&temp.log_text(1)).len(), 1);
    assert!(archived_lines(&temp.log_text(2)).is_empty());
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn a_parent_archive_does_not_reappend_an_already_archived_delegate() {
    let temp = Temp::new();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let hub = temp.hub();
    temp.logged(1, &["fiber_exited"]);
    temp.logged(2, &["fiber_exited"]);
    temp.indexed(1, &workspace, None);
    temp.indexed(2, &workspace, Some(&testkit::id(1)));
    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(2)})),
        Ok(None)
    );
    let delegate_log = temp.log_text(2);
    let delegate_row_count = crate::recent::read_all(&temp.dir)
        .iter()
        .filter(|row| row.session_id.0 == testkit::id(2))
        .count();

    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(1)})),
        Ok(None)
    );

    assert_eq!(temp.log_text(2), delegate_log);
    assert_eq!(
        crate::recent::read_all(&temp.dir)
            .iter()
            .filter(|row| row.session_id.0 == testkit::id(2))
            .count(),
        delegate_row_count
    );
    assert_eq!(archived_lines(&temp.log_text(2)).len(), 1);
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn a_missing_workspace_delegate_is_archived_independently_of_retention() {
    let temp = Temp::new();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let hub = temp.hub_with_keep(0);
    temp.logged(1, &["fiber_exited"]);
    temp.logged(2, &["fiber_exited"]);
    temp.indexed(1, &workspace, None);
    temp.indexed(2, "/missing-delegate-workspace", Some(&testkit::id(1)));

    feed::recent_for_hub(&hub, &args(serde_json::json!({})))
        .unwrap()
        .unwrap();
    assert!(archived_lines(&temp.log_text(1)).is_empty());
    assert_eq!(archived_lines(&temp.log_text(2)).len(), 1);
}

// `docs/invocation.md`, "Archiving" promises these results.
#[test]
fn cursor_after_multiple_pre_archive_rows_keeps_its_exact_page_position() {
    let temp = Temp::new();
    let hub = temp.hub();
    let workspace = temp.dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    for n in 1..=4 {
        temp.logged(n, &["fiber_exited"]);
        let path = if n == 3 {
            temp.dir.join("removed-cursor-workspace")
        } else {
            PathBuf::from(&workspace)
        };
        temp.indexed(n, &path.to_string_lossy(), None);
    }

    let page = feed::recent_for_hub(&hub, &args(serde_json::json!({"before": testkit::id(2)})))
        .unwrap()
        .unwrap();
    assert_eq!(page["total"], 3);
    assert_eq!(page["sessions"][0]["session_id"], testkit::id(1));
}

#[test]
fn two_clients_see_fresh_and_retry_archive_lines() {
    let temp = Temp::new();
    let hub = temp.hub();
    temp.logged(9, &["fiber_exited"]);
    temp.exited_row(9);
    let mut first = Sub::new(&hub.feed);
    let mut second = Sub::new(&hub.feed);
    // The subscribe-time snapshot is the always-on keepers line,
    // empty here (`docs/events.md`, "Hub feed", "keepers"): read it
    // before the broadcast lines it precedes.
    for sub in [&mut first, &mut second] {
        let mut text = String::new();
        sub.read
            .read_line(&mut text)
            .expect("the snapshot keepers line arrives");
        let snapshotted: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(snapshotted["kind"], "keepers");
        assert_eq!(snapshotted["payload"], serde_json::json!({"keepers": []}));
    }
    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(9)})),
        Ok(None)
    );
    for sub in [&mut first, &mut second] {
        sub.forgot("session_archived", &testkit::id(9), "-w");
    }
    // (`docs/invocation.md`, "What the hub speaks"): a retry that writes
    // nothing still tells every feed subscriber, so the second call site
    // sends its own line.
    assert_eq!(
        call(&hub, serde_json::json!({"session": testkit::id(9)})),
        Ok(None)
    );
    for sub in [&mut first, &mut second] {
        sub.forgot("session_archived", &testkit::id(9), "-w");
    }
}
