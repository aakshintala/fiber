//! Tests for `sessions`: live statuses then exited rows, each session once,
//! scoped by `project`, and the arguments it refuses.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::ErrorCode;
use contract::clock::{Clock, Wake};
use contract::diag::Timing;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::fake::{FakeSession, status, status_line};
use crate::recent::{Left, RecentRow};
use crate::testkit::{args, id};
use log::session_dir;

/// One named deadline per wait: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    sessions: Vec<FakeSession>,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::with_run("hl");
        Self {
            dir,
            held,
            sessions: Vec::new(),
        }
    }

    /// Session `n` running in `project`, idle.
    fn running(&mut self, n: u64, project: &str) {
        let session = FakeSession::bind(&self.dir, &id(n));
        session.say(&status_line(&id(n), &payload(project, None)));
        self.sessions.push(session);
    }

    /// Appends an exited row for session `n` named `name`, its directory
    /// made when `dir`.
    fn exited(&self, n: u64, project: &str, name: &str, parent: Option<&str>, dir: bool) {
        if dir {
            fs::create_dir_all(session_dir(&self.dir, project, &contract::SessionId(id(n))))
                .unwrap();
        }
        let row = RecentRow {
            session_id: contract::SessionId(id(n)),
            ts: n,
            project: project.to_owned(),
            workspace: "/w".to_owned(),
            name: name.to_owned(),
            how: Left::Exited,
            status: Some(serde_json::from_value(payload(project, parent)).unwrap()),
        };
        recent::append(&self.dir, &row).unwrap();
    }

    /// A started feed over this home, its first scan settled.
    fn feed(&self) -> Arc<Feed> {
        let clock = FakeClock::new();
        let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
        let feed = Arc::new(Feed::new(&self.dir, timed));
        feed.start();
        feed
    }
}

/// An idle `session_status` payload in `project`.
fn payload(project: &str, parent: Option<&str>) -> Value {
    let mut payload = status("n", "/w", "idle", parent);
    payload["project"] = json!(project);
    payload
}

/// `sessions` answered on a thread under [`DEADLINE`], then the feed
/// stopped: untimed, as an info-level dispatch answers it.
#[track_caller]
fn answered(feed: &Arc<Feed>, home: &std::path::Path, args: Value) -> Value {
    let clock: Arc<dyn Clock> = FakeClock::new();
    answered_timed(feed, home, args, clock, None)
}

/// `sessions` answered on a thread under [`DEADLINE`] with an explicit
/// clock and timing pair, then the feed stopped.
#[track_caller]
fn answered_timed(
    feed: &Arc<Feed>,
    home: &std::path::Path,
    args: Value,
    clock: Arc<dyn Clock>,
    timing: Option<Timing>,
) -> Value {
    let got = answer_once(feed, home, args, clock, timing);
    feed_stop(feed);
    got.unwrap().unwrap()
}

/// `sessions` answered once on a thread under [`DEADLINE`], the feed
/// left running: the caller stops it after any mid-flight check.
#[track_caller]
fn answer_once(
    feed: &Arc<Feed>,
    home: &std::path::Path,
    args: Value,
    clock: Arc<dyn Clock>,
    timing: Option<Timing>,
) -> Result<Option<Value>, Refused> {
    let (tx, rx) = mpsc::channel();
    let (answering, home, args) = (Arc::clone(feed), home.to_path_buf(), self::args(args));
    thread::spawn(move || {
        tx.send(answer(
            &answering,
            &home,
            &args,
            clock.as_ref(),
            timing.as_ref(),
        ))
        .unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("sessions answers")
}

/// The live and exited ids `sessions` answers.
#[track_caller]
fn listed(feed: &Arc<Feed>, home: &std::path::Path, args: Value) -> (Vec<String>, Vec<String>) {
    ids_of(&answered(feed, home, args))
}

#[track_caller]
fn feed_stop(feed: &Arc<Feed>) {
    let (tx, rx) = mpsc::channel();
    let feed = Arc::clone(feed);
    thread::spawn(move || {
        feed.stop();
        tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&rx).is_ok(),
        "the feed stops"
    );
}

fn ids_of(answer: &Value) -> (Vec<String>, Vec<String>) {
    let ids = |key: &str| -> Vec<String> {
        answer[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["session_id"].as_str().unwrap().to_owned())
            .collect()
    };
    (ids("live"), ids("exited"))
}

#[test]
fn no_project_answers_every_live_status_then_every_exited_row_newest_first() {
    let mut temp = Temp::new();
    temp.running(7, "p");
    temp.running(3, "q");
    temp.exited(1, "p", "one", None, true);
    temp.exited(2, "q", "two", None, true);
    let feed = temp.feed();
    let got = answered(&feed, &temp.dir, json!({}));
    assert_eq!(ids_of(&got), (vec![id(3), id(7)], vec![id(2), id(1)]));
    let live = got["live"].as_array().unwrap();
    assert_eq!(live[0]["status"], payload("q", None));
    assert_eq!(live[1]["status"], payload("p", None));
    assert_eq!(got["exited"][1]["name"], "one");
}

#[test]
fn project_scopes_live_and_exited_alike() {
    let mut temp = Temp::new();
    temp.running(7, "p");
    temp.running(3, "q");
    temp.exited(1, "p", "one", None, true);
    temp.exited(2, "q", "two", None, true);
    let feed = temp.feed();
    assert_eq!(
        listed(&feed, &temp.dir, json!({"project": "p"})),
        (vec![id(7)], vec![id(1)])
    );
}

#[test]
fn a_running_sessions_row_is_listed_live_only() {
    let mut temp = Temp::new();
    temp.exited(7, "p", "earlier run", None, true);
    temp.running(7, "p");
    let feed = temp.feed();
    assert_eq!(listed(&feed, &temp.dir, json!({})), (vec![id(7)], vec![]));
}

#[test]
fn delegates_and_gone_directories_are_skipped_and_the_newest_row_wins() {
    let temp = Temp::new();
    temp.exited(1, "p", "delegate", Some(&id(9)), true);
    temp.exited(2, "p", "gone", None, false);
    temp.exited(3, "p", "older", None, true);
    temp.exited(3, "p", "newer", None, true);
    let feed = temp.feed();
    let got = answered(&feed, &temp.dir, json!({}));
    assert_eq!(ids_of(&got), (vec![], vec![id(3)]));
    assert_eq!(got["exited"][0]["name"], "newer");
}

#[test]
fn arguments_that_do_not_fit_are_invalid_arguments() {
    let temp = Temp::new();
    // Never started: a refusal is answered before any wait.
    let clock: Arc<dyn Clock> = FakeClock::new();
    let feed = Arc::new(Feed::new(&temp.dir, clock));
    for bad in [
        json!({"x": "p"}),
        json!({"project": 1}),
        json!({"project": null}),
        json!({"project": "p", "x": "q"}),
    ] {
        let (tx, rx) = mpsc::channel();
        let (answering, home, sent) = (Arc::clone(&feed), temp.dir.clone(), args(bad.clone()));
        let clock = FakeClock::new();
        thread::spawn(move || {
            tx.send(answer(&answering, &home, &sent, clock.as_ref(), None))
                .unwrap_or(());
        });
        let got = Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("refused without a wait");
        let refused = got.unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidArguments, "{bad}");
    }
    feed_stop(&feed);
}

#[test]
fn a_session_in_the_snapshot_whose_row_lands_before_the_read_is_listed_once_as_live() {
    let temp = Temp::new();
    let snapshot = vec![(id(4), serde_json::from_value(payload("p", None)).unwrap())];
    // The session exits between the snapshot and the read of
    // `recent.jsonl`: its row is there when the list reads it.
    temp.exited(4, "p", "n", None, true);
    temp.exited(5, "p", "n", None, true);
    let got = list(snapshot, &temp.dir, None);
    assert_eq!(ids_of(&got), (vec![id(4)], vec![id(5)]));
}

#[test]
fn project_leaves_out_a_live_session_of_another_project() {
    let temp = Temp::new();
    let snapshot = vec![(id(4), serde_json::from_value(payload("q", None)).unwrap())];
    temp.exited(5, "p", "n", None, true);
    let got = list(snapshot, &temp.dir, Some("p"));
    assert_eq!(ids_of(&got), (vec![], vec![id(5)]));
}

/// A debug writer and its callable over `home` on `clock`: the pair
/// connection dispatch lends the listing.
fn debug_timing(home: &std::path::Path, clock: &Arc<FakeClock>) -> Timing {
    let timed: Arc<dyn Clock> = Arc::clone(clock) as Arc<dyn Clock>;
    let diag = log::diag::Diag::new(
        home,
        log::diag::Process::Hub,
        log::diag::Level::Debug,
        timed,
    );
    diag.timings()
}

/// Every `op_timing` line in the home's hub log, in file order: none
/// when no timer completed yet.
fn op_timings(home: &std::path::Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(home.join("logs").join("hub.log")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &Value| line["code"] == "op_timing")
        .collect()
}

/// The wire promise on a `sessions.list` line: exactly `op` and `ms`
/// with the whole wait in `ms`, and none of the listing's projects or
/// statuses anywhere in the line.
#[track_caller]
fn assert_list_line(lines: &[Value], ms: u64) {
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
    assert_eq!(data["op"], json!("sessions.list"), "{data}");
    assert_eq!(data["ms"], Value::from(ms), "{data}");
    let line = serde_json::to_string(&lines[0]).unwrap();
    assert!(!line.contains("sentinel"), "{line}");
}

#[test]
fn debug_times_a_listing_with_only_op_and_ms() {
    let mut temp = Temp::new();
    temp.running(7, "sentinel-proj");
    temp.exited(2, "sentinel-proj", "two", None, true);
    let feed = temp.feed();
    let clock = FakeClock::new();
    let timing = debug_timing(&temp.dir, &clock);
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let got = answer_once(&feed, &temp.dir, json!({}), timed, Some(timing))
        .unwrap()
        .unwrap();
    assert_eq!(ids_of(&got), (vec![id(7)], vec![id(2)]));
    // No clock move: the whole wait reads zero.
    assert_list_line(&op_timings(&temp.dir), 0);
    // The timer changes no answer: untimed, the same listing answers
    // the same rows.
    let plain = answer_once(&feed, &temp.dir, json!({}), FakeClock::new(), None)
        .unwrap()
        .unwrap();
    assert_eq!(got, plain);
    feed_stop(&feed);
    // Still exactly the one timed line: the untimed answer wrote none.
    assert_list_line(&op_timings(&temp.dir), 0);
}

#[test]
fn invalid_arguments_are_an_entered_attempt_with_one_line() {
    let temp = Temp::new();
    // Never started: a refusal is answered before any wait.
    let clock: Arc<dyn Clock> = FakeClock::new();
    let feed = Arc::new(Feed::new(&temp.dir, clock));
    let clock = FakeClock::new();
    let timing = debug_timing(&temp.dir, &clock);
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let (tx, rx) = mpsc::channel();
    let (answering, home, sent) = (
        Arc::clone(&feed),
        temp.dir.clone(),
        args(json!({"project": "sentinel-proj", "x": 1})),
    );
    thread::spawn(move || {
        tx.send(answer(
            &answering,
            &home,
            &sent,
            timed.as_ref(),
            Some(&timing),
        ))
        .unwrap_or(());
    });
    let refused = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("refused without a wait")
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArguments);
    assert_list_line(&op_timings(&temp.dir), 0);
    feed_stop(&feed);
}

/// A clock that explodes on any monotonic read: the untimed listing
/// takes none.
struct NoReadClock;

impl Clock for NoReadClock {
    fn now(&self) -> Instant {
        panic!("the untimed listing reads no clock");
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _for: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, _wait: &mut dyn FnMut(Option<Duration>)) {}

    fn subscribe(&self, _wake: Weak<dyn Wake>) {}
}

#[test]
fn untimed_answers_read_no_clock_and_write_no_line() {
    let mut temp = Temp::new();
    temp.running(7, "sentinel-proj");
    let feed = temp.feed();
    let clock: Arc<dyn Clock> = Arc::new(NoReadClock);
    let got = answer_once(&feed, &temp.dir, json!({}), Arc::clone(&clock), None)
        .unwrap()
        .unwrap();
    assert_eq!(ids_of(&got), (vec![id(7)], vec![]));
    feed_stop(&feed);
    assert!(
        !temp.dir.join("logs").exists(),
        "no timer means no diagnostic file"
    );
}

#[test]
fn the_timer_covers_settlement_and_reports_the_whole_wait() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let feed = Arc::new(Feed::new(&temp.dir, Arc::clone(&timed)));
    // A session the first scan follows that never reports: the listing
    // parks on the clock beside the scanner.
    let session = FakeSession::bind(&temp.dir, &id(1));
    crate::testkit::start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    let until = clock.now() + crate::feed::RUN_SCAN;
    let timing = debug_timing(&temp.dir, &clock);
    let (tx, rx) = mpsc::channel();
    let (answering, home) = (Arc::clone(&feed), temp.dir.clone());
    let sent = args(json!({}));
    let responder = Arc::clone(&timed);
    thread::spawn(move || {
        tx.send(answer(
            &answering,
            &home,
            &sent,
            responder.as_ref(),
            Some(&timing),
        ))
        .unwrap_or(());
    });
    assert!(
        clock.await_parked_count(until, 2, fakes::Deadline::after(DEADLINE)),
        "the listing parks beside the scanner"
    );
    // The guard is alive but uncompleted: settlement holds the answer,
    // so no line is written until it finishes.
    assert!(
        op_timings(&temp.dir).is_empty(),
        "no completed timer while settlement waits"
    );
    clock.advance_when_parked_threads(
        Some(until),
        2,
        crate::feed::RUN_SCAN,
        &Deadline::after(DEADLINE),
    );
    let got = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("sessions answers")
        .unwrap()
        .unwrap();
    assert_eq!(ids_of(&got), (vec![], vec![]));
    feed_stop(&feed);
    // The stamp was taken at entry, before the wait: the whole
    // settlement wait is in the one line.
    let want_ms =
        u64::try_from(crate::feed::RUN_SCAN.as_millis()).expect("the scan bound fits in ms");
    assert_list_line(&op_timings(&temp.dir), want_ms);
}
