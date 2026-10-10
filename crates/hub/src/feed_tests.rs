//! Tests for the feed: finding sessions in `run/` under the fake clock,
//! relaying their status, deciding how each left, seeding from
//! `recent.jsonl`, fan-out past a dead or slow client, `attention` for a
//! waiting session and a finished turn, `dismiss`, `recent` and stop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::Deadline;
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

/// Stops `feed` under [`DEADLINE`]: a stop that hangs fails the test.
#[track_caller]
fn stop_within(feed: &Arc<Feed>) {
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(feed);
    thread::spawn(move || {
        stopping.stop();
        tx.send(()).unwrap_or(());
    });
    assert!(Deadline::after(DEADLINE).recv(&rx).is_ok(), "stop returns");
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
#[track_caller]
fn await_true(what: &str, done: impl Fn() -> bool + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while !done() {
            thread::yield_now();
        }
        tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&rx).is_ok(),
        "waited for {what}"
    );
}

/// Drops attention listener `id` on a thread and receives its return
/// under [`DEADLINE`]: joining its writer blocks.
#[track_caller]
fn unlisten_within(feed: &Arc<Feed>, id: u64) {
    let (done_tx, done_rx) = mpsc::channel();
    let ending = Arc::clone(feed);
    thread::spawn(move || {
        ending.attention.unlisten(id);
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "unlisten returns"
    );
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
    use std::os::unix::net::UnixListener;

    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    let socket = temp.dir.join("run").join(id(1));
    let listener = UnixListener::bind(&socket).unwrap();
    // The receive runs on a thread: a follow that never connects or
    // sends leaves the bounded wait below to fail, not a blocked test.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut texts = Vec::new();
        for _ in 0..2 {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut read = BufReader::new(stream);
            let mut text = String::new();
            if read.read_line(&mut text).is_err() {
                return;
            }
            texts.push(text);
        }
        tx.send(texts).unwrap_or(());
    });
    feed.follow(id(1));
    feed.follow(id(1));
    let texts = Deadline::after(Duration::from_secs(5))
        .recv(&rx)
        .expect("the hub to send two subscribes");
    assert_eq!(texts.len(), 2);
    let mut got = Vec::new();
    for text in &texts {
        let line: Value = serde_json::from_str(text).unwrap();
        assert_eq!(line["command"], "subscribe");
        assert_eq!(line["args"], json!({"level": "summary"}));
        let sub = line["id"].as_str().unwrap().to_owned();
        assert!(sub.starts_with("c_hub_feed_"), "{line}");
        got.push(sub);
    }
    assert_ne!(got[0], got[1], "two follows use different command ids");
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
}

#[test]
fn a_log_ending_in_rewound_is_exited() {
    let temp = Temp::new();
    let (feed, _, _sub, how) = leave(&temp, "rewound", "idle", false);
    assert_eq!(how, "exited");
    assert_eq!(entry_of(&feed, &id(1)), None);
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    // Under a deadline: ending the live subscriber must never wait on the
    // slow one's blocked writer.
    let (done_tx, done_rx) = mpsc::channel();
    let ending = Arc::clone(&feed);
    thread::spawn(move || {
        ending.unsubscribe(live.id);
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "unsubscribe returns"
    );
    assert_eq!(lock(&feed.state).subscribers.len(), before - 1);
    drop(slow_far);
    stop_within(&feed);
}

#[test]
fn each_subscriber_gets_its_own_id_and_unsubscribe_ends_only_that_one() {
    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    let first = Sub::new(&feed);
    let second = Sub::new(&feed);
    assert_eq!((first.id, second.id), (1, 2));
    let (done_tx, done_rx) = mpsc::channel();
    let ending = Arc::clone(&feed);
    let id = second.id;
    thread::spawn(move || {
        ending.unsubscribe(id);
        done_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "unsubscribe returns"
    );
    let left: Vec<u64> = lock(&feed.state)
        .subscribers
        .iter()
        .map(|sub| sub.id)
        .collect();
    assert_eq!(left, [first.id]);
    stop_within(&feed);
}

#[test]
fn a_scan_skips_followed_sessions_and_delegates() {
    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    let names = BTreeSet::from([id(1), id(2), id(3)]);
    let mut state = lock(&feed.state);
    let (followed, _far) = UnixStream::pair().unwrap();
    let (_reader, stop) = support::stoppable::reader(followed.try_clone().unwrap()).unwrap();
    state.tracked.insert(
        id(1),
        Tracked {
            stream: followed,
            stop,
        },
    );
    state.delegates.insert(id(2));
    assert_eq!(state.fresh(&names), [id(3)]);
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
    stop_within(&feed);
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
    stop_within(&feed);
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
    assert_eq!(feed.dismiss(&args(json!({ "session": crashed }))), Ok(None));
    assert_eq!(entry_of(&feed, &crashed), None);
    stale(&feed, &crashed);
    stop_within(&seeded);
    stop_within(&feed);
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
    assert_eq!(feed.recent(&Map::new()), Ok(Some(json!({"sessions": []}))));
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
    let ids = |page: Option<Value>| -> Vec<String> {
        page.unwrap()["sessions"]
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
    stop_within(&feed);
}

#[test]
fn stop_waits_for_a_writer_still_draining_its_backlog() {
    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    let (writer, mut far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let kept = Arc::new(Mutex::new(writer));
    feed.subscribe(Arc::clone(&kept)).unwrap();
    // Megabytes queued, far more than the socket buffer holds: the writer
    // thread cannot end until the far end has read nearly all of it.
    let line: Line = Arc::from(vec![b'x'; 2_000]);
    let count = 4_000;
    {
        let mut state = lock(&feed.state);
        for _ in 0..count {
            broadcast(&mut state, &line);
        }
    }
    let total = line.len() * count;
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let drained = thread::spawn(move || {
        go_rx.recv().unwrap();
        let mut buf = vec![0; 64 * 1024];
        let mut read = 0;
        while read < total {
            read += far.read(&mut buf).unwrap();
        }
        read
    });
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(&feed);
    let watched = Arc::clone(&kept);
    thread::spawn(move || {
        // The far end starts reading only as stop is called, so the
        // writer is still busy unless stop waits for it.
        go_tx.send(()).unwrap();
        stopping.stop();
        tx.send(Arc::strong_count(&watched)).unwrap_or(());
    });
    let left = Deadline::after(DEADLINE).recv(&rx).expect("stop returns");
    // The test's handle, `watched`, and none from the writer thread.
    assert_eq!(left, 2, "stop returned before its writer thread ended");
    assert_eq!(drained.join().unwrap(), total);
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
    // A writer the test keeps a handle on: its thread drops the other.
    let (kept, _kept_far) = UnixStream::pair().unwrap();
    let kept = Arc::new(Mutex::new(kept));
    feed.subscribe(Arc::clone(&kept)).unwrap();
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(&feed);
    thread::spawn(move || {
        stopping.stop();
        drop(stopping);
        tx.send(()).unwrap_or(());
    });
    assert!(Deadline::after(DEADLINE).recv(&rx).is_ok(), "stop returns");
    // Joined, not just told to end: the scanner and the summary thread
    // each held the feed, and the writer thread held `kept`.
    assert_eq!(Arc::strong_count(&feed), 1);
    assert_eq!(Arc::strong_count(&kept), 1);
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
fn stop_returns_when_a_silent_session_stays_open() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "turn_completed");
    let (_session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    // The status arrived, so the summary reader is tracked and reading.
    assert_eq!(sub.raw("the status"), line);
    feed.skip_shutdown.store(true, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(&feed);
    thread::spawn(move || {
        stopping.stop();
        drop(stopping);
        tx.send(()).unwrap_or(());
    });
    assert!(Deadline::after(DEADLINE).recv(&rx).is_ok(), "stop returns");
    // Joined, not just told to end: the summary thread held the feed.
    assert_eq!(Arc::strong_count(&feed), 1);
    assert!(lock(&feed.state).tracked.is_empty());
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
    // Even when the tail of that line happens to parse on its own.
    let inner = |pad: usize| {
        format!(
            "{{\"kind\":\"fiber_exited\",\"p\":\"{}\"}}",
            "y".repeat(pad)
        )
    };
    let pad = usize::try_from(TAIL).unwrap() - 1 - inner(0).len();
    fs::write(&log, format!("not json {}\n", inner(pad))).unwrap();
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

#[test]
fn a_session_resumed_before_its_end_is_read_is_not_crashed() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "fiber_started");
    let (session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the status"), line);
    // The followed run exits, and the resumed run has written past its
    // `fiber_exited` already.
    let log = recent::session_dir(&temp.dir, "p", &id).join("events.jsonl");
    let mut file = fs::OpenOptions::new().append(true).open(log).unwrap();
    file.write_all(b"{\"kind\":\"fiber_exited\"}\n{\"kind\":\"fiber_started\"}\n")
        .unwrap();
    let back = session.resumed(&temp.dir, &id);
    let again = status_line(&id, &status("again", "/w", "idle", None));
    back.say(&again);
    sub.left(&id, "exited");
    assert!(temp.rows().is_empty());
    clock.advance(RUN_SCAN);
    assert_eq!(sub.raw("the resumed status"), again);
    stop_within(&feed);
}

#[test]
fn an_exit_line_from_before_the_follow_is_not_the_followed_runs() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let id = temp.session(1, "p", "fiber_started");
    // The earlier run's exit line is already in the log when the hub
    // follows: only an exit line written after that point ends this run.
    let log = recent::session_dir(&temp.dir, "p", &id).join("events.jsonl");
    fs::write(
        &log,
        "{\"kind\":\"session_started\"}\n{\"kind\":\"fiber_exited\"}\n{\"kind\":\"fiber_started\"}\n",
    )
    .unwrap();
    let (session, line) = running(&temp, &id, "idle");
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert_eq!(sub.raw("the status"), line);
    // The followed run writes no exit line.
    session.close();
    sub.left(&id, "crashed");
    assert_eq!(entry_of(&feed, &id), Some("crashed"));
    let rows = temp.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["how"], "crashed");
    stop_within(&feed);
}

/// An attention listener's far end: lines read under [`DEADLINE`].
struct Heard {
    id: u64,
    read: BufReader<UnixStream>,
}

impl Heard {
    fn new(feed: &Feed) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let id = feed.attention.listen(Arc::new(Mutex::new(a))).unwrap();
        Self {
            id,
            read: BufReader::new(b),
        }
    }

    fn next(&mut self, what: &str) -> Value {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        assert!(!text.is_empty(), "attention closed before {what}");
        serde_json::from_str(&text).unwrap()
    }
}

/// A `session_status` payload with `state`: `since` is explicit because
/// `fake::status` has `since: 1`, before the hub's start, so an `idle`
/// built from it would never be fresh. `"tool"` runs `"shell"`;
/// `"waiting"` carries `request_id` and `summary "run <request>"`.
fn state_of(state: &str, request: &str, since: u64) -> Value {
    let mut payload = status("n", "/w", "idle", None);
    let map = payload.as_object_mut().unwrap();
    map.insert("state".to_owned(), Value::String(state.to_owned()));
    match state {
        "tool" => {
            map.insert("tool".to_owned(), Value::String("shell".to_owned()));
        }
        "waiting" => {
            map.insert(
                "waiting".to_owned(),
                json!({
                    "request_id": request,
                    "kind": "approval",
                    "summary": format!("run {request}"),
                }),
            );
        }
        _ => {}
    }
    map.insert("since".to_owned(), Value::Number(since.into()));
    payload
}

/// A `turn_completed` log line with `ts`.
fn turned(ts: u64) -> String {
    format!("{{\"kind\":\"turn_completed\",\"ts\":{ts}}}\n")
}

impl Temp {
    /// Appends one line to session `n`'s `events.jsonl` in `project`.
    fn log(&self, n: u64, project: &str, line: &str) {
        let log = recent::session_dir(&self.dir, project, &id(n)).join("events.jsonl");
        fs::OpenOptions::new()
            .append(true)
            .open(log)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
}

/// Binds session 1's socket: the feed follows it once started.

#[test]
fn waiting_then_a_finished_turn_each_send_one_attention() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "turn_completed");
    let heard = Heard::new(&feed);
    let mut heard = heard;
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    let say = |state: &str, request: &str, since: u64| {
        session.say(&status_line(&id(1), &state_of(state, request, since)));
    };
    say("streaming", "r1", WALL + 1);
    say("waiting", "r1", WALL + 1);
    say("waiting", "r1", WALL + 2);
    say("tool", "r1", WALL + 3);
    say("streaming", "r1", WALL + 4);
    say("idle", "r1", WALL + 5);
    say("idle", "r1", WALL + 5);
    say("waiting", "r2", WALL + 6);
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["kind"], "attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    assert_eq!(waiting["payload"]["summary"], "run r1");
    assert_eq!(waiting["payload"]["session_id"], id(1).as_str());
    let finished = heard.next("the finished attention");
    assert_eq!(finished["payload"]["reason"], "finished");
    assert!(finished["payload"].get("summary").is_none());
    // The waiting r2 arrives third, so no duplicate finished came first.
    let again = heard.next("the second waiting attention");
    assert_eq!(again["payload"]["reason"], "waiting");
    assert_eq!(again["payload"]["summary"], "run r2");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn idle_without_a_turn_sends_nothing() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "turn_completed");
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL)));
    let mut renamed = state_of("idle", "r1", WALL);
    renamed["name"] = json!("n2");
    session.say(&status_line(&id(1), &renamed));
    session.say(&status_line(&id(1), &state_of("jobs", "r1", WALL)));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL)));
    session.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    // The waiting r1 arrives first, so no idle or jobs line sent one.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_late_reader_still_sends_one_finished_per_turn() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "turn_completed");
    let session = FakeSession::bind(&temp.dir, &id(1));
    let say = |state: &str, request: &str, since: u64| {
        session.say(&status_line(&id(1), &state_of(state, request, since)));
    };
    say("streaming", "r1", WALL);
    say("idle", "r1", WALL + 1);
    say("streaming", "r1", WALL + 1);
    say("idle", "r1", WALL + 2);
    say("waiting", "r1", WALL + 2);
    let mut heard = Heard::new(&feed);
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    assert_eq!(
        heard.next("the first finished")["payload"]["reason"],
        "finished"
    );
    assert_eq!(
        heard.next("the second finished")["payload"]["reason"],
        "finished"
    );
    // The waiting r1 arrives third, so each turn sent exactly one.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_turn_that_ended_before_the_hub_followed_is_announced() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "session_started");
    temp.log(1, "p", &turned(WALL + 5));
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 5)));
    let finished = heard.next("the finished attention");
    assert_eq!(finished["payload"]["reason"], "finished");
    assert_eq!(finished["payload"]["session_id"], id(1).as_str());
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_turn_that_ended_before_the_hub_started_is_not() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "session_started");
    temp.log(1, "p", &turned(WALL - 1));
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL - 1)));
    session.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    // The waiting r1 arrives first, so the stale idle sent nothing.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_delegate_never_sends_attention() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(9, "p", "session_started");
    temp.log(9, "p", &turned(WALL + 5));
    temp.session(1, "p", "turn_completed");
    let mut heard = Heard::new(&feed);
    let delegate = FakeSession::bind(&temp.dir, &id(9));
    let mut idle = state_of("idle", "r1", WALL + 5);
    idle["parent"] = json!(id(1));
    delegate.say(&status_line(&id(9), &idle));
    let top = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(top.await_subscribed(1, DEADLINE));
    let marked = Arc::clone(&feed);
    await_true("the delegate to be marked", move || {
        lock(&marked.state).delegates.contains(&id(9))
    });
    top.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    // The top-level waiting arrives first, so the delegate sent nothing.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["session_id"], id(1).as_str());
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_resumed_session_raising_its_request_again_is_not_announced_but_its_turn_end_is() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "fiber_exited");
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    assert_eq!(
        heard.next("the waiting attention")["payload"]["reason"],
        "waiting"
    );
    session.close();
    let back = session.resumed(&temp.dir, &id(1));
    back.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    back.say(&status_line(&id(1), &state_of("streaming", "r1", WALL + 8)));
    back.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 9)));
    let dropped = Arc::clone(&feed);
    await_true("the hub to drop the old run", move || {
        entry_of(&dropped, &id(1)) != Some("running")
    });
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    // The finished arrives next, so the repeated waiting r1 sent nothing.
    let finished = heard.next("the finished attention");
    assert_eq!(finished["payload"]["reason"], "finished");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_resumed_run_that_starts_idle_is_not_announced() {
    let temp = Temp::new();
    temp.session(1, "p", "turn_completed");
    temp.append(&row(1, "p", "crashed", Some("streaming")));
    let (feed, clock) = new_feed(&temp);
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 7)));
    session.say(&status_line(&id(1), &state_of("waiting", "r1", WALL + 7)));
    start(&feed, &clock);
    clock.advance(RUN_SCAN);
    assert!(session.await_subscribed(1, DEADLINE));
    // The waiting r1 arrives first, so the idle without a turn sent nothing.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_listener_added_later_gets_no_replay() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "turn_completed");
    let mut first = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("waiting", "r1", WALL)));
    assert_eq!(
        first.next("the waiting attention")["payload"]["reason"],
        "waiting"
    );
    let mut second = Heard::new(&feed);
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 1)));
    // Each listener's next line is the finished: nothing was replayed.
    assert_eq!(
        second.next("the finished attention")["payload"]["reason"],
        "finished"
    );
    assert_eq!(
        first.next("the finished attention")["payload"]["reason"],
        "finished"
    );
    unlisten_within(&feed, first.id);
    unlisten_within(&feed, second.id);
    stop_within(&feed);
}

#[test]
fn older_lines_after_a_snapshot_are_not_announced_twice() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "session_started");
    temp.log(1, "p", &turned(WALL + 5));
    let session = FakeSession::bind(&temp.dir, &id(1));
    let say = |state: &str, request: &str, since: u64| {
        session.say(&status_line(&id(1), &state_of(state, request, since)));
    };
    say("idle", "r1", WALL + 5);
    say("streaming", "r1", WALL + 5);
    say("idle", "r1", WALL + 5);
    say("waiting", "r1", WALL + 5);
    say("streaming", "r1", WALL + 5);
    say("waiting", "r1", WALL + 5);
    say("waiting", "r2", WALL + 5);
    let mut heard = Heard::new(&feed);
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    assert_eq!(
        heard.next("the finished attention")["payload"]["reason"],
        "finished"
    );
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["summary"], "run r1");
    // The waiting r2 arrives third, so neither line was announced twice.
    let again = heard.next("the second waiting attention");
    assert_eq!(again["payload"]["summary"], "run r2");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_resume_that_starts_idle_does_not_announce_a_turn_again() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "session_started");
    temp.log(1, "p", &turned(WALL + 5));
    let mut heard = Heard::new(&feed);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    session.say(&status_line(&id(1), &state_of("streaming", "r1", WALL)));
    session.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 5)));
    assert_eq!(
        heard.next("the finished attention")["payload"]["reason"],
        "finished"
    );
    temp.log(1, "p", "{\"kind\":\"fiber_exited\"}\n");
    session.close();
    let back = session.resumed(&temp.dir, &id(1));
    back.say(&status_line(&id(1), &state_of("idle", "r1", WALL + 5)));
    back.say(&status_line(&id(1), &state_of("waiting", "r1", WALL + 5)));
    let dropped = Arc::clone(&feed);
    await_true("the hub to drop the old run", move || {
        entry_of(&dropped, &id(1)) != Some("running")
    });
    clock.advance(RUN_SCAN);
    await_scanner(&clock);
    // The waiting r1 arrives next, so the repeated idle sent nothing.
    let waiting = heard.next("the waiting attention");
    assert_eq!(waiting["payload"]["reason"], "waiting");
    unlisten_within(&feed, heard.id);
    stop_within(&feed);
}

#[test]
fn a_feed_subscriber_alone_gets_no_attention() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    temp.session(1, "p", "turn_completed");
    let session = FakeSession::bind(&temp.dir, &id(1));
    let mut sub = Sub::new(&feed);
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    let streaming = status_line(&id(1), &state_of("streaming", "r1", WALL));
    let waiting = status_line(&id(1), &state_of("waiting", "r1", WALL));
    session.say(&streaming);
    session.say(&waiting);
    assert_eq!(sub.raw("the streaming status"), streaming);
    assert_eq!(sub.raw("the waiting status"), waiting);
    stop_within(&feed);
}

/// A session directory holding a two-line log: `session_started`, then
/// `last` as the final line.
fn logged(temp: &Temp, n: u64, last: &Value) -> (String, PathBuf) {
    let session = id(n);
    let dir = recent::session_dir(&temp.dir, "-w", &session);
    fs::create_dir_all(&dir).unwrap();
    let first = json!({
        "kind": "session_started", "session_id": session, "ts": 1, "schema_version": 1,
        "payload": {"workspace": "/w"},
    });
    fs::write(
        dir.join("events.jsonl"),
        format!(
            "{first}\n{last}\n",
            first = serde_json::to_string(&first).unwrap(),
            last = serde_json::to_string(last).unwrap(),
        ),
    )
    .unwrap();
    (session, dir)
}

fn rewound_last(old: &str, next: &str) -> Value {
    json!({
        "kind": "rewound", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
        "payload": {"new_session_id": next, "seq": 3, "jobs": []},
    })
}

#[test]
fn on_rewound_fires_once_for_a_rewound_last_line() {
    let temp = Temp::new();
    let (feed, _clock) = new_feed(&temp);
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    assert!(
        feed.on_rewound
            .set(Box::new({
                let seen = Arc::clone(&seen);
                move |from: SessionId, next: SessionId| {
                    seen.lock().unwrap().push((from.0, next.0));
                }
            }))
            .is_ok(),
        "the callback sets"
    );
    let next = id(9);
    let (old, dir) = logged(&temp, 1, &rewound_last(&id(1), &next));
    feed.on_left(&old, Some(("-w".to_owned(), dir, 0)));
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(old, next)],
        "the feed starts the named session once"
    );
}

#[test]
fn on_rewound_ignores_anything_but_a_rewound_last_line() {
    let temp = Temp::new();
    let (feed, _clock) = new_feed(&temp);
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    assert!(
        feed.on_rewound
            .set(Box::new({
                let seen = Arc::clone(&seen);
                move |from: SessionId, next: SessionId| {
                    seen.lock().unwrap().push((from.0, next.0));
                }
            }))
            .is_ok(),
        "the callback sets"
    );
    // An exit starts nothing.
    let (exited, exited_dir) = logged(
        &temp,
        1,
        &json!({
            "kind": "fiber_exited", "session_id": id(1),
            "ts": 2, "schema_version": 1, "seq": 5, "payload": {},
        }),
    );
    feed.on_left(&exited, Some(("-w".to_owned(), exited_dir, 0)));
    // A crash starts nothing.
    let (crashed, crashed_dir) = logged(
        &temp,
        2,
        &json!({
            "kind": "turn_completed", "session_id": id(2),
            "ts": 2, "schema_version": 1, "seq": 5, "payload": {},
        }),
    );
    feed.on_left(&crashed, Some(("-w".to_owned(), crashed_dir, 0)));
    // A session with no directory starts nothing.
    feed.on_left(&id(3), None);
    assert!(
        seen.lock().unwrap().is_empty(),
        "only a rewound last line starts a session"
    );
}

#[test]
fn a_second_hub_lists_a_session_the_first_hub_already_followed() {
    use std::collections::HashSet;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;
    use std::sync::Condvar;

    /// A session socket that answers `subscribe` like `Gate`: the first
    /// command id is accepted, a repeated id is rejected
    /// `duplicate_command`, and only accepted connections get status.
    struct Shared {
        ids: HashSet<String>,
        received: Vec<String>,
        writers: Vec<UnixStream>,
        subscribed: usize,
    }

    const WAIT: Duration = Duration::from_secs(5);

    let temp = Temp::new();
    let id = temp.session(1, "p", "turn_completed");
    let socket = temp.dir.join("run").join(&id);
    let shared: Arc<(Mutex<Shared>, Condvar)> = Arc::new((
        Mutex::new(Shared {
            ids: HashSet::new(),
            received: Vec::new(),
            writers: Vec::new(),
            subscribed: 0,
        }),
        Condvar::new(),
    ));
    let listener = UnixListener::bind(&socket).unwrap();
    let serving = Arc::clone(&shared);
    // Exactly the two follows below connect: each blocking receive runs
    // on a thread, and the test awaits their results with a deadline
    // (`await_subscribed`, `await_received`), so a missing follow fails
    // there instead of blocking the test.
    thread::spawn(move || {
        for _ in 0..2 {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let serving = Arc::clone(&serving);
            thread::spawn(move || {
                let Ok(writer) = stream.try_clone() else {
                    return;
                };
                let mut read = BufReader::new(stream);
                let mut buf = Vec::new();
                // A probe that sends nothing closes; only a subscriber is held.
                match read.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let Some(got) = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|line| line.get("id").cloned())
                    .and_then(|got| got.as_str().map(str::to_owned))
                else {
                    return;
                };
                let mut writer = writer;
                let duplicate = {
                    let (state, grew) = &*serving;
                    let mut state = lock(state);
                    state.received.push(got.clone());
                    let duplicate = !state.ids.insert(got.clone());
                    let ack = if duplicate {
                        json!({
                            "kind": "command_rejected", "ts": 1, "schema_version": 1,
                            "payload": {
                                "code": "duplicate_command",
                                "command_id": got,
                                "message": "A command with this id was already accepted.",
                            },
                        })
                    } else {
                        state.writers.push(writer.try_clone().unwrap());
                        state.subscribed += 1;
                        json!({
                            "kind": "command_accepted", "ts": 1, "schema_version": 1,
                            "payload": { "command_id": got },
                        })
                    };
                    let mut bytes = serde_json::to_vec(&ack).unwrap();
                    bytes.push(b'\n');
                    writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .unwrap_or(());
                    grew.notify_all();
                    duplicate
                };
                let _ = duplicate;
                // Held open, as a live session holds its summary connection:
                // the feed sees no close either way.
                loop {
                    buf.clear();
                    match read.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                }
            });
        }
    });
    let say = |line: &str| {
        let (state, _) = &*shared;
        let state = lock(state);
        for conn in &state.writers {
            let mut conn = conn;
            conn.write_all(line.as_bytes()).unwrap_or(());
        }
    };
    let await_subscribed = |count: usize| {
        let (state, grew) = &*shared;
        let (state, _) = grew
            .wait_timeout_while(lock(state), WAIT, |state| state.subscribed < count)
            .unwrap();
        assert!(
            state.subscribed >= count,
            "the session accepted {count} subscribes"
        );
    };
    let await_received = |count: usize| {
        let (state, grew) = &*shared;
        let (state, _) = grew
            .wait_timeout_while(lock(state), WAIT, |state| state.received.len() < count)
            .unwrap();
        assert!(
            state.received.len() >= count,
            "the session received {count} subscribes"
        );
    };

    let (first, first_clock) = new_feed(&temp);
    let mut first_sub = Sub::new(&first);
    start(&first, &first_clock);
    await_subscribed(1);
    let line = status_line(&id, &status("n", "/w", "idle", None));
    say(&line);
    assert_eq!(first_sub.raw("the session's status"), line);
    assert_eq!(entry_of(&first, &id), Some("running"));

    let (second, second_clock) = new_feed(&temp);
    let mut second_sub = Sub::new(&second);
    start(&second, &second_clock);
    await_received(2);
    let ids = lock(&shared.0).received.clone();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "two follows use different command ids");
    let again = status_line(&id, &status("n2", "/w", "idle", None));
    say(&again);
    {
        let watched = Arc::clone(&second);
        let named = id.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while entry_of(&watched, &named) != Some("running") {
                thread::yield_now();
            }
            tx.send(()).unwrap_or(());
        });
        assert!(
            Deadline::after(WAIT).recv(&rx).is_ok(),
            "the second hub lists the session"
        );
    }
    assert_eq!(
        second_sub.raw("the session's status on the second hub"),
        again
    );
    stop_within(&second);
    stop_within(&first);
}
