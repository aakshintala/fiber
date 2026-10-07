//! Tests for the feed: finding sessions in `run/` under the fake clock,
//! relaying their status, deciding how each left, seeding from
//! `recent.jsonl`, fan-out past a dead or slow client, `dismiss`,
//! `recent` and stop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::fake::{FakeSession, status, status_line};

/// One named deadline per wait: the feed answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

/// `wall()` on a fake clock nobody advanced, in milliseconds.
const WALL: u64 = 1_700_000_000_000;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hf");
        let dir = held.path().join("h");
        fs::create_dir_all(dir.join("run")).unwrap();
        Self { dir, held }
    }

    /// Session `n`'s directory in `project`, its log ending in `last`.
    fn session(&self, n: u64, project: &str, last: &str) -> String {
        let id = id(n);
        let dir = recent::session_dir(&self.dir, project, &id);
        fs::create_dir_all(&dir).unwrap();
        let log = format!("{{\"kind\":\"session_started\"}}\n{{\"kind\":\"{last}\"}}\n");
        fs::write(dir.join("events.jsonl"), log).unwrap();
        id
    }

    fn rows(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.join("recent.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn append(&self, row: &Value) {
        let row: RecentRow = serde_json::from_value(row.clone()).unwrap();
        recent::append(&self.dir, &row).unwrap();
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

/// A row for session `n` as JSON.
fn row(n: u64, project: &str, how: &str, state: Option<&str>) -> Value {
    let mut row = json!({
        "session_id": id(n), "ts": n, "project": project, "workspace": "/w",
        "name": "n", "how": how,
    });
    if let Some(state) = state {
        row["status"] = status("n", "/w", state, None);
    }
    row
}

/// A feed over `temp` on a fake clock, not yet started.
fn new_feed(temp: &Temp) -> (Arc<Feed>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    (Arc::new(Feed::new(&temp.dir, timed)), clock)
}

/// Waits until the scanner parks for the scan after now.
fn await_scanner(clock: &FakeClock) {
    assert!(
        clock.await_parked(clock.now() + RUN_SCAN, DEADLINE),
        "the scanner parks until the next scan"
    );
}

/// Starts `feed`, and waits until its scanner parks for the next scan.
fn start(feed: &Arc<Feed>, clock: &FakeClock) {
    feed.start();
    await_scanner(clock);
}

/// A feed subscriber's far end: lines read under [`DEADLINE`].
struct Sub {
    id: u64,
    read: BufReader<UnixStream>,
}

impl Sub {
    fn new(feed: &Feed) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let id = feed.subscribe(Arc::new(Mutex::new(a))).unwrap();
        Self {
            id,
            read: BufReader::new(b),
        }
    }

    fn raw(&mut self, what: &str) -> String {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "the feed closed before {what}");
        text
    }

    fn next(&mut self, what: &str) -> Value {
        serde_json::from_str(&self.raw(what)).unwrap()
    }

    /// The next line, which must be `session_left` for `id` with `how`.
    fn left(&mut self, id: &str, how: &str) {
        let line = self.next("session_left");
        assert_eq!(line["kind"], "session_left", "{line}");
        assert_eq!(line["payload"], json!({"session_id": id, "how": how}));
        assert!(line.get("session_id").is_none());
        assert_eq!(line["schema_version"], 1);
        assert_eq!(line["ts"], WALL);
    }
}

/// Waits under [`DEADLINE`] until `done` holds, naming `what` on expiry.
fn await_true(what: &str, done: impl Fn() -> bool + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while !done() {
            thread::yield_now();
        }
        tx.send(()).unwrap_or(());
    });
    assert!(rx.recv_timeout(DEADLINE).is_ok(), "waited for {what}");
}

fn entry_of(feed: &Feed, id: &str) -> Option<&'static str> {
    lock(&feed.state).entries.get(id).map(|entry| match entry {
        Entry::Running(_) => "running",
        Entry::Left(_, Left::Exited) => "exited",
        Entry::Left(_, Left::Crashed) => "crashed",
    })
}

/// A running session `id` saying `state`; its status line.
fn running(temp: &Temp, id: &str, state: &str) -> (FakeSession, String) {
    let session = FakeSession::bind(&temp.dir, id);
    let line = status_line(id, &status("n", "/w", state, None));
    session.say(&line);
    (session, line)
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn the_summary_subscribe_is_the_documented_line() {
    assert_eq!(
        serde_json::from_slice::<Value>(SUBSCRIBE).unwrap(),
        json!({"id": "c_hub_feed", "command": "subscribe", "args": {"level": "summary"}})
    );
}

#[test]
fn a_running_session_is_found_at_start_and_its_status_relayed_byte_for_byte() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "turn_completed");
    let (session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    assert_eq!(sub.raw("the session's status"), line);
    assert_eq!(entry_of(&feed, &id), Some("running"));
    feed.stop();
}

#[test]
fn a_socket_that_appears_after_start_is_found_after_one_scan() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    start(&feed, &clock);
    let id = temp.session(1, "p", "turn_completed");
    let (session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    // Exactly one period: the scan is due at, not after, it.
    clock.advance(RUN_SCAN);
    assert!(session.await_subscribed(1, DEADLINE));
    assert_eq!(sub.raw("the late session's status"), line);
    feed.stop();
}

#[test]
fn a_name_that_is_not_a_session_id_is_never_connected() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let hub = FakeSession::bind(&temp.dir, "hub");
    let id = temp.session(1, "p", "turn_completed");
    let (session, _) = running(&temp, &id, "idle");
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    // The scan that found the session is complete: `hub` was skipped.
    assert!(!lock(&feed.state).tracked.contains_key("hub"));
    assert!(!hub.await_subscribed(1, Duration::ZERO));
    feed.stop();
}

#[test]
fn a_delegate_never_reaches_a_subscriber_and_is_not_connected_again() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let delegate = id(9);
    let child = FakeSession::bind(&temp.dir, &delegate);
    child.say(&status_line(
        &delegate,
        &status("d", "/w", "idle", Some(&id(1))),
    ));
    let top = temp.session(1, "p", "turn_completed");
    let (session, first) = running(&temp, &top, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    let marked = Arc::clone(&feed);
    let named = delegate.clone();
    await_true("the delegate to be marked", move || {
        lock(&marked.state).delegates.contains(&named)
    });
    assert!(!lock(&feed.state).tracked.contains_key(&delegate));
    let second = status_line(&top, &status("n2", "/w", "idle", None));
    session.say(&second);
    assert_eq!(sub.raw("the top-level status"), first);
    assert_eq!(sub.raw("the top-level change"), second);
    // A later scan leaves the delegate alone.
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    assert!(!child.await_subscribed(2, Duration::ZERO));
    assert_eq!(entry_of(&feed, &delegate), None);
    feed.stop();
}

/// Runs session 1 to its socket's close, its log ending in `last` and its
/// status in `state`; returns the `how` of its `session_left`.
fn leave(
    temp: &Temp,
    last: &str,
    state: &str,
    killed: bool,
) -> (Arc<Feed>, Arc<FakeClock>, Sub, String) {
    let (feed, clock) = new_feed(temp);
    let id = temp.session(1, "p", last);
    let (session, line) = running(temp, &id, state);
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the status"), line);
    if killed {
        session.kill();
    } else {
        session.close();
    }
    let left = sub.next("session_left");
    assert_eq!(left["kind"], "session_left");
    assert_eq!(left["payload"]["session_id"], id.as_str());
    let how = left["payload"]["how"].as_str().unwrap().to_owned();
    (feed, clock, sub, how)
}

#[test]
fn a_log_ending_in_fiber_exited_is_exited_and_the_session_leaves_the_feed() {
    let temp = Temp::new();
    let (feed, _, _sub, how) = leave(&temp, "fiber_exited", "idle", false);
    assert_eq!(how, "exited");
    assert_eq!(entry_of(&feed, &id(1)), None);
    assert!(temp.rows().is_empty(), "the session appends its own row");
    feed.stop();
}

#[test]
fn a_log_ending_in_rewound_is_exited() {
    let temp = Temp::new();
    let (feed, _, _sub, how) = leave(&temp, "rewound", "idle", false);
    assert_eq!(how, "exited");
    assert_eq!(entry_of(&feed, &id(1)), None);
    feed.stop();
}

#[test]
fn any_other_last_line_is_crashed_with_a_row_and_the_session_stays() {
    let temp = Temp::new();
    let (feed, _, _sub, how) = leave(&temp, "turn_completed", "idle", true);
    assert_eq!(how, "crashed");
    assert_eq!(entry_of(&feed, &id(1)), Some("crashed"));
    let rows = temp.rows();
    assert_eq!(rows.len(), 1);
    let row = rows.first().unwrap();
    assert_eq!(row["session_id"], id(1).as_str());
    assert_eq!(row["how"], "crashed");
    assert_eq!(row["project"], "p");
    assert_eq!(row["workspace"], "/w");
    assert_eq!(row["name"], "n");
    assert_eq!(row["ts"], WALL);
    assert_eq!(row["status"]["state"], "idle");
    // A fresh subscriber is sent the crashed session first.
    let mut fresh = Sub::new(&feed);
    assert_eq!(fresh.next("the crashed status")["kind"], "session_status");
    fresh.left(&id(1), "crashed");
    feed.stop();
}

#[test]
fn a_session_that_exits_waiting_stays_in_the_feed() {
    let temp = Temp::new();
    let (feed, _, _sub, how) = leave(&temp, "fiber_exited", "waiting", false);
    assert_eq!(how, "exited");
    assert_eq!(entry_of(&feed, &id(1)), Some("exited"));
    assert!(temp.rows().is_empty());
    let mut fresh = Sub::new(&feed);
    let line = fresh.next("the waiting status");
    assert_eq!(line["payload"]["state"], "waiting");
    fresh.left(&id(1), "exited");
    feed.stop();
}

#[test]
fn a_session_whose_directory_is_gone_exited_and_leaves_nothing() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = id(1);
    let (session, line) = running(&temp, &id, "waiting");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the status"), line);
    session.kill();
    sub.left(&id, "exited");
    assert_eq!(entry_of(&feed, &id), None);
    assert!(temp.rows().is_empty());
    feed.stop();
}

#[test]
fn a_session_that_never_sent_a_status_gets_no_session_left() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let quiet = temp.session(1, "p", "fiber_exited");
    let silent = FakeSession::bind(&temp.dir, &quiet);
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert!(silent.await_subscribed(1, DEADLINE));
    silent.close();
    let watched = Arc::clone(&feed);
    let named = quiet.clone();
    await_true("the quiet session to be dropped", move || {
        !lock(&watched.state).tracked.contains_key(&named)
    });
    let other = temp.session(2, "p", "turn_completed");
    let (_session, line) = running(&temp, &other, "idle");
    clock.advance(RUN_SCAN);
    assert_eq!(sub.raw("the next session's status"), line);
    feed.stop();
}

#[test]
fn a_crashed_session_that_comes_back_is_running_again() {
    let temp = Temp::new();
    let (feed, clock, mut sub, how) = leave(&temp, "turn_completed", "idle", true);
    assert_eq!(how, "crashed");
    let id = id(1);
    fs::remove_file(temp.dir.join("run").join(&id)).unwrap();
    let back = FakeSession::bind(&temp.dir, &id);
    let line = status_line(&id, &status("again", "/w", "idle", None));
    back.say(&line);
    clock.advance(RUN_SCAN);
    assert!(back.await_subscribed(1, DEADLINE));
    assert_eq!(sub.raw("the resumed status"), line);
    assert_eq!(entry_of(&feed, &id), Some("running"));
    let (code, _) = feed.dismiss(&args(json!({"session": id}))).unwrap_err();
    assert_eq!(code, ErrorCode::StaleRequest);
    feed.stop();
}

#[test]
fn a_dead_or_slow_subscriber_does_not_stop_a_live_one() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "turn_completed");
    let session = FakeSession::bind(&temp.dir, &id);
    // Never read: its socket buffer fills and its writer blocks.
    let (slow, slow_far) = UnixStream::pair().unwrap();
    feed.subscribe(Arc::new(Mutex::new(slow))).unwrap();
    // Gone: its writes fail.
    let (dead, dead_far) = UnixStream::pair().unwrap();
    drop(dead_far);
    feed.subscribe(Arc::new(Mutex::new(dead))).unwrap();
    let mut live = Sub::new(&feed);
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    let name = "x".repeat(2_000);
    let count = 1_000;
    for n in 0..count {
        session.say(&status_line(
            &id,
            &status(&format!("{n}:{name}"), "/w", "idle", None),
        ));
    }
    for n in 0..count {
        let line = live.next("every status");
        assert!(
            line["payload"]["name"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{n}:")),
            "line {n} in order"
        );
    }
    let before = lock(&feed.state).subscribers.len();
    feed.unsubscribe(live.id);
    assert_eq!(lock(&feed.state).subscribers.len(), before - 1);
    drop(slow_far);
    feed.stop();
}

#[test]
fn a_snapshot_sends_left_sessions_before_running_ones() {
    let temp = Temp::new();
    // The running session sorts first, so order is not the map's.
    let live = temp.session(1, "p", "turn_completed");
    let crashed = temp.session(2, "p", "turn_completed");
    temp.append(&row(2, "p", "crashed", Some("idle")));
    let (feed, clock) = new_feed(&temp);
    let (_session, line) = running(&temp, &live, "idle");
    start(&feed, &clock);
    let watched = Arc::clone(&feed);
    let named = live.clone();
    await_true("the live session to be running", move || {
        entry_of(&watched, &named) == Some("running")
    });
    let mut sub = Sub::new(&feed);
    let first = sub.next("the crashed status");
    assert_eq!(first["kind"], "session_status");
    assert_eq!(first["session_id"], crashed.as_str());
    assert_eq!(first["ts"], 2);
    sub.left(&crashed, "crashed");
    assert_eq!(sub.raw("the running status"), line);
    feed.stop();
}

#[test]
fn start_seeds_crashed_and_waiting_sessions_but_not_running_or_gone_ones() {
    let temp = Temp::new();
    temp.session(1, "p", "turn_completed");
    temp.append(&row(1, "p", "crashed", Some("idle")));
    temp.session(2, "p", "fiber_exited");
    temp.append(&row(2, "p", "exited", Some("waiting")));
    temp.append(&row(3, "p", "crashed", Some("idle")));
    temp.session(4, "p", "turn_completed");
    temp.append(&row(4, "p", "crashed", Some("idle")));
    temp.session(5, "p", "fiber_exited");
    temp.append(&row(5, "p", "exited", Some("idle")));
    temp.session(6, "p", "turn_completed");
    temp.append(&row(6, "p", "crashed", None));
    let (feed, clock) = new_feed(&temp);
    // Running: its socket accepts, so it is not seeded.
    let alive = FakeSession::bind(&temp.dir, &id(4));
    feed.start();
    assert_eq!(entry_of(&feed, &id(1)), Some("crashed"));
    assert_eq!(entry_of(&feed, &id(2)), Some("exited"));
    assert_eq!(entry_of(&feed, &id(3)), None);
    assert_eq!(entry_of(&feed, &id(5)), None);
    // With no status there is nothing to send.
    assert_eq!(entry_of(&feed, &id(6)), None);
    await_scanner(&clock);
    // The running one sent no status, so it is in no entry at all.
    assert!(alive.await_subscribed(1, DEADLINE));
    assert_eq!(entry_of(&feed, &id(4)), None);
    let mut sub = Sub::new(&feed);
    assert_eq!(sub.next("the first seed")["session_id"], id(1).as_str());
    sub.left(&id(1), "crashed");
    assert_eq!(sub.next("the second seed")["payload"]["state"], "waiting");
    sub.left(&id(2), "exited");
    feed.stop();
}

#[test]
fn dismiss_drops_only_a_crashed_session() {
    let temp = Temp::new();
    let (feed, _, _sub, _) = leave(&temp, "turn_completed", "idle", true);
    let crashed = id(1);
    let waiting = temp.session(2, "p", "fiber_exited");
    temp.append(&row(2, "p", "exited", Some("waiting")));
    let (seeded, _) = new_feed(&temp);
    seeded.start();
    assert_eq!(entry_of(&seeded, &waiting), Some("exited"));
    let stale = |feed: &Feed, session: &str| {
        let (code, _) = feed
            .dismiss(&args(json!({ "session": session })))
            .unwrap_err();
        assert_eq!(code, ErrorCode::StaleRequest, "{session}");
    };
    stale(&seeded, &waiting);
    stale(&feed, &id(7));
    assert_eq!(
        feed.dismiss(&args(json!({ "session": crashed }))),
        Ok(json!({}))
    );
    assert_eq!(entry_of(&feed, &crashed), None);
    stale(&feed, &crashed);
    seeded.stop();
    feed.stop();
}

#[test]
fn dismiss_and_recent_refuse_arguments_that_do_not_fit() {
    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    for bad in [
        json!({}),
        json!({"session": 1}),
        json!({"session": "s", "extra": "x"}),
    ] {
        let (code, _) = feed.dismiss(&args(bad.clone())).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad}");
    }
    for bad in [
        json!({"before": 1}),
        json!({"project": true}),
        json!({"limit": "x"}),
        json!({"before": id(5)}),
    ] {
        let (code, _) = feed.recent(&args(bad.clone())).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad}");
    }
    assert_eq!(feed.recent(&Map::new()), Ok(json!({"sessions": []})));
}

#[test]
fn recent_answers_a_page_without_running_sessions() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    for n in 1..=3 {
        let project = if n == 2 { "q" } else { "p" };
        temp.session(n, project, "fiber_exited");
        temp.append(&row(n, project, "exited", None));
    }
    // Session 3 runs again.
    let (_session, line) = running(&temp, &id(3), "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the running status"), line);
    let ids = |page: Value| -> Vec<String> {
        page["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["session_id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(feed.recent(&Map::new()).unwrap()), [id(2), id(1)]);
    assert_eq!(
        ids(feed.recent(&args(json!({"project": "p"}))).unwrap()),
        [id(1)]
    );
    assert_eq!(
        ids(feed.recent(&args(json!({"before": id(2)}))).unwrap()),
        [id(1)]
    );
    feed.stop();
}

#[test]
fn stop_ends_every_thread_and_records_no_crash() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "turn_completed");
    let (_session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the status"), line);
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(&feed);
    thread::spawn(move || {
        stopping.stop();
        tx.send(()).unwrap_or(());
    });
    assert!(rx.recv_timeout(DEADLINE).is_ok(), "stop returns");
    assert!(temp.rows().is_empty());
    assert_eq!(entry_of(&feed, &id), Some("running"));
    assert!(lock(&feed.state).tracked.is_empty());
    assert!(lock(&feed.scanner).is_none());
    let mut rest = String::new();
    assert_eq!(sub.read.read_line(&mut rest).unwrap(), 0, "{rest}");
    let (other, _far) = UnixStream::pair().unwrap();
    assert!(feed.subscribe(Arc::new(Mutex::new(other))).is_none());
}

#[test]
fn the_last_kind_reads_only_a_whole_last_line() {
    let temp = Temp::new();
    let log = temp.dir.join("events.jsonl");
    assert_eq!(last_kind(&log), None);
    fs::write(&log, "").unwrap();
    assert_eq!(last_kind(&log), None);
    fs::write(&log, "{\"kind\":\"a\"}\n{\"kind\":\"fiber_exited\"}\n").unwrap();
    assert_eq!(last_kind(&log).as_deref(), Some("fiber_exited"));
    fs::write(&log, "{\"kind\":\"fiber_exited\"}").unwrap();
    assert_eq!(last_kind(&log).as_deref(), Some("fiber_exited"));
    // A torn tail does not parse.
    fs::write(&log, "{\"kind\":\"fiber_exited\"}\n{\"kind\":\"tu").unwrap();
    assert_eq!(last_kind(&log), None);
    // A long log: only its tail is read.
    let filler = format!("{{\"kind\":\"x\",\"p\":\"{}\"}}\n", "y".repeat(100_000));
    fs::write(&log, format!("{filler}{{\"kind\":\"rewound\"}}\n")).unwrap();
    assert_eq!(last_kind(&log).as_deref(), Some("rewound"));
    // A last line longer than the tail is neither exit line.
    fs::write(&log, format!("{{\"kind\":\"fiber_exited\"}}\n{filler}")).unwrap();
    assert_eq!(last_kind(&log), None);
    // A last line well inside the tail reads whole.
    let long = format!("{{\"kind\":\"z\",\"p\":\"{}\"}}\n", "y".repeat(2_000));
    fs::write(&log, format!("{filler}{long}")).unwrap();
    assert_eq!(last_kind(&log).as_deref(), Some("z"));
}

#[test]
fn how_left_names_both_exit_lines() {
    let temp = Temp::new();
    for (last, how) in [
        ("fiber_exited", Left::Exited),
        ("rewound", Left::Exited),
        ("turn_completed", Left::Crashed),
    ] {
        let dir = temp.dir.join(last);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            format!("{{\"kind\":\"{last}\"}}\n"),
        )
        .unwrap();
        assert_eq!(how_left(&dir), how, "{last}");
    }
    assert_eq!(how_left(&temp.dir.join("none")), Left::Crashed);
}
