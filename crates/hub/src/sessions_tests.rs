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
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::fake::{FakeSession, status, status_line};
use crate::recent::{Left, RecentRow, session_dir};

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
        let held = fakes::TempDir::new("hl");
        let dir = held.path().join("h");
        fs::create_dir_all(dir.join("run")).unwrap();
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
            fs::create_dir_all(session_dir(&self.dir, project, &id(n))).unwrap();
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

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

/// An idle `session_status` payload in `project`.
fn payload(project: &str, parent: Option<&str>) -> Value {
    let mut payload = status("n", "/w", "idle", parent);
    payload["project"] = json!(project);
    payload
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// `sessions` answered on a thread under [`DEADLINE`], then the feed
/// stopped.
#[track_caller]
fn answered(feed: &Arc<Feed>, home: &std::path::Path, args: Value) -> Value {
    let (tx, rx) = mpsc::channel();
    let (answering, home, args) = (Arc::clone(feed), home.to_path_buf(), self::args(args));
    thread::spawn(move || {
        tx.send(answer(&answering, &home, &args)).unwrap_or(());
    });
    let got = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("sessions answers");
    feed_stop(feed);
    got.unwrap().unwrap()
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
        thread::spawn(move || {
            tx.send(answer(&answering, &home, &sent)).unwrap_or(());
        });
        let got = Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("refused without a wait");
        let (code, _) = got.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad}");
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
