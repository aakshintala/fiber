//! Tests for the first scan settling and the live snapshot: a listing waits
//! for the sessions the first scan followed, for at most one rescan, and
//! never for a session found later.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::super::{Entry, Feed, RUN_SCAN, lock};
use crate::fake::{FakeSession, status, status_line};
use crate::recent::{self, Left, RecentRow};

/// One named deadline per wait: the feed answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hs");
        let dir = held.path().join("h");
        fs::create_dir_all(dir.join("run")).unwrap();
        Self { dir, held }
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

fn new_feed(temp: &Temp) -> (Arc<Feed>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    (Arc::new(Feed::new(&temp.dir, timed)), clock)
}

/// The instant the scanner, and a listing started now, park until.
fn next_scan(clock: &FakeClock) -> Instant {
    clock.now() + RUN_SCAN
}

/// Starts `feed` and waits until its first scan is done and it parks.
fn start(feed: &Arc<Feed>, clock: &FakeClock) {
    feed.start();
    assert!(
        clock.await_parked(next_scan(clock), DEADLINE),
        "the scanner parks"
    );
}

/// Runs [`Feed::settled`] on a thread; the receiver gets the live ids
/// it then reads.
fn listing(feed: &Arc<Feed>) -> mpsc::Receiver<Vec<String>> {
    let (tx, rx) = mpsc::channel();
    let feed = Arc::clone(feed);
    thread::spawn(move || {
        feed.settled();
        let ids = feed.live().into_iter().map(|(id, _)| id).collect();
        tx.send(ids).unwrap_or(());
    });
    rx
}

/// Waits until the listing thread parks beside the scanner.
fn await_listing_parked(clock: &FakeClock, until: Instant) {
    assert!(
        clock.await_parked_count(until, 2, DEADLINE),
        "the listing parks beside the scanner"
    );
}

fn stop_within(feed: &Arc<Feed>) {
    let (tx, rx) = mpsc::channel();
    let stopping = Arc::clone(feed);
    thread::spawn(move || {
        stopping.stop();
        tx.send(()).unwrap_or(());
    });
    assert!(rx.recv_timeout(DEADLINE).is_ok(), "stop returns");
}

fn pause_settle_wait(feed: &Feed) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *lock(&feed.settle_pause) = Some(super::super::SettlePause {
        arrived: arrived_tx,
        release: release_rx,
    });
    (arrived_rx, release_tx)
}

fn await_pause(arrived: &mpsc::Receiver<()>) {
    assert!(
        arrived.recv_timeout(DEADLINE).is_ok(),
        "the listing reaches its wait"
    );
}

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

fn say_idle(session: &FakeSession, id: &str) {
    session.say(&status_line(id, &status("n", "/w", "idle", None)));
}

/// A silent session the first scan followed, and a listing parked on it.
fn parked_on_silent(temp: &Temp) -> (Arc<Feed>, Arc<FakeClock>, FakeSession, Instant) {
    let (feed, clock) = new_feed(temp);
    let session = FakeSession::bind(&temp.dir, &id(1));
    start(&feed, &clock);
    assert!(session.await_subscribed(1, DEADLINE));
    let until = next_scan(&clock);
    (feed, clock, session, until)
}

#[test]
fn a_listing_waits_for_the_status_of_each_session_the_first_scan_followed() {
    let temp = Temp::new();
    let (feed, clock, session, until) = parked_on_silent(&temp);
    let rx = listing(&feed);
    await_listing_parked(&clock, until);
    assert!(rx.try_recv().is_err(), "parked, so not answered");
    say_idle(&session, &id(1));
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), [id(1)]);
    stop_within(&feed);
}

#[test]
fn a_session_that_ends_before_its_status_settles_the_listing() {
    let temp = Temp::new();
    let (feed, clock, session, until) = parked_on_silent(&temp);
    let rx = listing(&feed);
    await_listing_parked(&clock, until);
    session.close();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn a_silent_session_holds_the_listing_for_one_rescan_at_most() {
    let temp = Temp::new();
    let (feed, clock, _session, until) = parked_on_silent(&temp);
    let rx = listing(&feed);
    await_listing_parked(&clock, until);
    clock.advance(RUN_SCAN.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_count(until, 2, DEADLINE),
        "both still parked before the bound"
    );
    assert!(rx.try_recv().is_err(), "not answered before the bound");
    clock.advance(Duration::from_millis(1));
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn stopping_the_feed_releases_a_listing_before_any_scan() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let rx = listing(&feed);
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the listing waits for the first scan"
    );
    assert!(rx.try_recv().is_err(), "no scan yet, so not answered");
    stop_within(&feed);
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
}

#[test]
fn a_stop_between_the_first_scan_check_and_wait_releases_a_listing() {
    let temp = Temp::new();
    let (feed, _) = new_feed(&temp);
    let (arrived, release) = pause_settle_wait(&feed);
    let rx = listing(&feed);
    await_pause(&arrived);
    let (done_tx, done_rx) = mpsc::channel();
    let stopping = Arc::clone(&feed);
    thread::spawn(move || {
        stopping.stop();
        done_tx.send(()).unwrap_or(());
    });
    let flag = Arc::clone(&feed);
    await_true("stop marks the feed", move || lock(&flag.state).stopped);
    release.send(()).unwrap();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    assert!(done_rx.recv_timeout(DEADLINE).is_ok(), "stop returns");
}

#[test]
fn a_scan_finish_between_the_first_scan_check_and_wait_releases_a_listing() {
    let temp = Temp::new();
    let (feed, _clock) = new_feed(&temp);
    let (arrived, release) = pause_settle_wait(&feed);
    let rx = listing(&feed);
    await_pause(&arrived);
    feed.start();
    let flag = Arc::clone(&feed);
    await_true("the scan finishes", move || lock(&flag.state).scanned);
    release.send(()).unwrap();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn a_status_between_the_post_scan_check_and_wait_releases_a_listing() {
    let temp = Temp::new();
    let (feed, _clock, _session, _until) = parked_on_silent(&temp);
    let (arrived, release) = pause_settle_wait(&feed);
    let rx = listing(&feed);
    await_pause(&arrived);
    let line = status_line(&id(1), &status("n", "/w", "idle", None));
    let feeding = Arc::clone(&feed);
    thread::spawn(move || {
        assert!(feeding.on_line(&id(1), line.as_bytes(), None));
    });
    let flag = Arc::clone(&feed);
    await_true("the status lands", move || {
        matches!(
            lock(&flag.state).entries.get(&id(1)),
            Some(Entry::Running(_))
        )
    });
    release.send(()).unwrap();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), [id(1)]);
    stop_within(&feed);
}

#[test]
fn a_listing_made_before_the_first_scan_is_answered_by_it() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let rx = listing(&feed);
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the listing waits for the first scan"
    );
    assert!(rx.try_recv().is_err(), "no scan yet, so not answered");
    feed.start();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn a_deadline_passing_before_the_first_scan_does_not_release_the_listing() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    let rx = listing(&feed);
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the listing waits for the first scan"
    );
    let mark = clock.advance_marked(RUN_SCAN + Duration::from_millis(1));
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "still waiting for the first scan past the bound"
    );
    assert!(rx.try_recv().is_err(), "a deadline is not a scan");
    feed.start();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn a_session_found_after_the_first_scan_is_not_awaited() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    start(&feed, &clock);
    let session = FakeSession::bind(&temp.dir, &id(1));
    clock.advance(RUN_SCAN);
    assert!(session.await_subscribed(1, DEADLINE));
    assert!(clock.await_parked(next_scan(&clock), DEADLINE));
    let rx = listing(&feed);
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap(), Vec::<String>::new());
    stop_within(&feed);
}

#[test]
fn live_is_every_running_session_in_id_order_and_no_left_one() {
    let temp = Temp::new();
    let (feed, clock) = new_feed(&temp);
    // A crashed session seeded from `recent.jsonl`: in the feed, not live.
    let crashed = id(5);
    fs::create_dir_all(recent::session_dir(&temp.dir, "p", &crashed)).unwrap();
    let row = RecentRow {
        session_id: contract::SessionId(crashed.clone()),
        ts: 1,
        project: "p".to_owned(),
        workspace: "/w".to_owned(),
        name: "n".to_owned(),
        how: Left::Crashed,
        status: Some(serde_json::from_value(status("n", "/w", "idle", None)).unwrap()),
    };
    recent::append(&temp.dir, &row).unwrap();
    let second = FakeSession::bind(&temp.dir, &id(9));
    say_idle(&second, &id(9));
    let first = FakeSession::bind(&temp.dir, &id(2));
    say_idle(&first, &id(2));
    start(&feed, &clock);
    assert!(listing(&feed).recv_timeout(DEADLINE).is_ok());
    assert!(matches!(
        lock(&feed.state).entries.get(&crashed),
        Some(Entry::Left(_, Left::Crashed))
    ));
    let live: Vec<String> = feed.live().into_iter().map(|(id, _)| id).collect();
    assert_eq!(live, [id(2), id(9)]);
    stop_within(&feed);
}
