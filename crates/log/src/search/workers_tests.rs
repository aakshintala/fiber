//! Tests for the worker threads: how many run, that every worker count gives
//! the one-thread answer, that they run at once, cancellation, the identity
//! cache, a thread that cannot start, and the merge.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::HashSet;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, ThreadId};
use std::time::Duration;

use contract::clock::Wake;
use contract::session_search::{Found, Label};
use contract::tool::Cancel;
use fakes::CancelToken;
use fakes::within;
use proptest::prelude::*;

use super::super::Collect;
use super::super::tests::{After, Home, hit, query};
use super::{count, run, spawn};
use crate::fixtures::{append_raw as raw, line as fixture_line};

/// `available_parallelism` reporting `n` threads.
fn threads(n: usize) -> io::Result<NonZeroUsize> {
    Ok(NonZeroUsize::new(n).unwrap())
}

/// `available_parallelism` failing.
fn unknown() -> io::Result<NonZeroUsize> {
    Err(io::Error::other("unknown"))
}

/// One raw log line of session `id`.
fn line(kind: &str, id: &str, ts: u64, seq: u64, payload: &serde_json::Value) -> Vec<u8> {
    fixture_line(kind, id, ts, seq, payload).into_bytes()
}

fn text(id: &str, ts: u64, seq: u64, text: &str) -> Vec<u8> {
    line(
        "text_completed",
        id,
        ts,
        seq,
        &serde_json::json!({"text": text}),
    )
}

fn named(id: &str, seq: u64, name: &str) -> Vec<u8> {
    line(
        "session_named",
        id,
        1,
        seq,
        &serde_json::json!({"name": name, "by": "person"}),
    )
}

/// A line that holds the query and is not JSON.
const BAD: &[u8] = b"needle not json\n";

/// `n` [`BAD`] lines appended to the session in `dir`.
fn bad(dir: &Path, n: usize) {
    for _ in 0..n {
        raw(dir, BAD);
    }
}

#[test]
fn the_worker_count_is_the_parallelism_capped_by_the_sessions() {
    for (parallelism, sessions, want) in [
        (Some(4), 10, 4),
        (Some(4), 2, 2),
        (Some(1), 10, 1),
        (None, 10, 1),
        (Some(4), 0, 0),
        (Some(4), 4, 4),
    ] {
        let reported = parallelism.map_or_else(unknown, threads);
        assert_eq!(
            count(reported, sessions),
            want,
            "{parallelism:?} threads, {sessions} sessions"
        );
    }
}

/// A store with several projects, a linked project, a linked session and a
/// linked log, problems past the cut in both scopes, hits tied on `ts`
/// across sessions, names, a matched artifact, and another project's
/// session under the own key.
fn rich(home: &Home) {
    let projects = home.path().join("projects");
    let one = home.session("-a", "s_a1", "/a/one", &["needle 1", "needle 2"]);
    raw(&one, &named("s_a1", 10, "alpha"));
    raw(&one, &text("s_a1", 5, 11, "needle tie"));
    let two = home.session("-a", "s_a2", "/a/two", &["needle 3"]);
    bad(&two, 9);
    raw(&two, &text("s_a2", 5, 11, "needle tie"));
    home.session("-a", "s_a3", "/c/x", &["needle other"]);
    let linked = home.session("-a", "s_a4", "/a/one", &[]);
    fs::remove_file(linked.join("events.jsonl")).unwrap();
    symlink(one.join("events.jsonl"), linked.join("events.jsonl")).unwrap();
    let five = home.session("-a", "s_a5", "/a/one", &["needle 5"]);
    bad(&five, 9);
    let six = home.session("-a", "s_a6", "/a/two", &[]);
    bad(&six, 9);
    raw(&six, &text("s_a6", 5, 11, "needle tie"));
    home.session("-b", "s_b1", "/b/one", &["needle b"]);
    symlink(projects.join("-b"), projects.join("-l")).unwrap();
    let c1 = home.session("-c", "s_c1", "/c/one", &[]);
    fs::write(c1.join("artifacts").join("out.txt"), "needle in artifact\n").unwrap();
    raw(
        &c1,
        &line(
            "tool_call_completed",
            "s_c1",
            5,
            1,
            &serde_json::json!({"status": "completed", "content": [{"type": "text", "text": "cut"}], "artifact": "artifacts/out.txt"}),
        ),
    );
    bad(&c1, 4);
    symlink(&one, home.sessions("-c").join("s_c2")).unwrap();
    let c3 = home.session("-c", "s_c3", "/c/two", &["needle c3"]);
    raw(&c3, &named("s_c3", 5, "gamma"));
}

#[test]
fn every_worker_count_gives_the_one_thread_answer() {
    let home = Home::new();
    rich(&home);
    let scan = home.scanner("/a/main");
    for all_projects in [false, true] {
        for limit in [0, 1, 3, 20, 1000] {
            let query = query("needle", all_projects, limit);
            let one = scan.search(&query, &CancelToken::new(), threads(1));
            assert_eq!(one.problems.len(), 20, "{all_projects} {limit}");
            assert!(one.more_problems > 0 && one.total > 5, "{one:?}");
            for parallelism in [unknown(), threads(2), threads(3), threads(8)] {
                let what = format!("all {all_projects}, limit {limit}, {parallelism:?}");
                let found = scan.search(&query, &CancelToken::new(), parallelism);
                assert_eq!(found, one, "{what}");
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// One thread and four give the same answer over any small store.
    #[test]
    fn one_thread_and_four_give_the_same_answer(
        sessions in prop::collection::vec(
            (0..3_usize, any::<bool>(), prop::collection::vec((0..4_usize, 0..3_u64), 0..6)),
            0..10,
        ),
        limit in 0..8_usize,
    ) {
        let home = Home::new();
        for (index, (key, own, lines)) in sessions.iter().enumerate() {
            let id = format!("s_{index}");
            let workspace = if *own { "/a/x" } else { "/c/x" };
            let dir = home.session(["-a", "-b", "-c"][*key], &id, workspace, &[]);
            for (seq, (kind, ts)) in (1..).zip(lines) {
                let bytes = match kind {
                    0 => text(&id, *ts, seq, "needle"),
                    1 => BAD.to_vec(),
                    2 => named(&id, seq, &format!("name {seq}")),
                    _ => line(
                        "tool_call_completed",
                        &id,
                        *ts,
                        seq,
                        &serde_json::json!({"status": "completed", "content": [{"type": "text", "text": "needle out"}]}),
                    ),
                };
                raw(&dir, &bytes);
            }
        }
        let scan = home.scanner("/a/main");
        for all_projects in [false, true] {
            let query = query("needle", all_projects, limit);
            let one = scan.search(&query, &CancelToken::new(), threads(1));
            let four = scan.search(&query, &CancelToken::new(), threads(4));
            prop_assert_eq!(four, one);
        }
    }
}

#[test]
fn hits_tied_on_rank_give_one_answer_and_keep_their_own_names() {
    let home = Home::new();
    // One session directory under two keys, named differently.
    for (key, name) in [("-a", "first"), ("-b", "second")] {
        let dir = home.session(key, "s_dup", "/a/one", &[]);
        raw(&dir, &text("s_dup", 5, 1, "needle dup"));
        raw(&dir, &named("s_dup", 2, name));
    }
    // A log that repeats a `seq` with other text.
    let repeat = home.session("-c", "s_rep", "/a/one", &[]);
    raw(&repeat, &text("s_rep", 5, 1, "needle x"));
    raw(&repeat, &text("s_rep", 5, 1, "needle y"));
    let scan = home.scanner("/a/main");
    for limit in [1, 3, 10] {
        let query = query("needle", true, limit);
        let one = scan.search(&query, &CancelToken::new(), threads(1));
        for parallelism in [threads(2), threads(8)] {
            let found = scan.search(&query, &CancelToken::new(), parallelism);
            assert_eq!(found, one, "limit {limit}");
        }
    }
    let found = scan.search(&query("needle", true, 10), &CancelToken::new(), threads(8));
    assert_eq!(found.total, 4);
    let names: Vec<(String, &str)> = found
        .hits
        .iter()
        .map(|hit| (hit.log.display().to_string(), hit.name.as_str()))
        .collect();
    for (log, name) in &names {
        let want = if log.contains("/-a/") {
            "first"
        } else if log.contains("/-b/") {
            "second"
        } else {
            ""
        };
        assert_eq!(*name, want, "{names:?}");
    }
}

/// How long the threads of [`Rendezvous`] wait for each other.
const MEET: Duration = Duration::from_secs(5);

/// A [`Cancel`] that never cancels. It lets its first check through, then
/// holds every later one until `threads` distinct threads have each made
/// one, or until [`MEET`] passes, which it records.
struct Rendezvous {
    threads: usize,
    state: Mutex<Met>,
    arrived: Condvar,
}

#[derive(Default)]
struct Met {
    checks: usize,
    seen: HashSet<ThreadId>,
    timed_out: bool,
}

impl Cancel for Rendezvous {
    fn is_cancelled(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.checks += 1;
        if state.checks == 1 || state.timed_out {
            return false;
        }
        state.seen.insert(thread::current().id());
        self.arrived.notify_all();
        let (mut state, wait) = self
            .arrived
            .wait_timeout_while(state, MEET, |met| met.seen.len() < self.threads)
            .unwrap();
        if wait.timed_out() {
            state.timed_out = true;
        }
        false
    }

    fn subscribe(&self, _: Weak<dyn Wake>) {}
}

#[test]
fn three_workers_read_three_sessions_at_once() {
    let home = Home::new();
    for id in ["s_1", "s_2", "s_3"] {
        home.session("-a", id, "/a/one", &["needle"]);
    }
    let scan = Arc::new(home.scanner("/a/main"));
    let cancel = Arc::new(Rendezvous {
        threads: 3,
        state: Mutex::new(Met::default()),
        arrived: Condvar::new(),
    });
    let (scanned, held) = (Arc::clone(&scan), Arc::clone(&cancel));
    let found = within("three workers to meet", MEET * 3, move || {
        scanned.search(&query("needle", false, 10), &*held, threads(3))
    });
    let state = cancel.state.lock().unwrap();
    assert!(!state.timed_out, "the workers never met");
    assert_eq!(state.seen.len(), 3);
    let one = scan.search(&query("needle", false, 10), &CancelToken::new(), threads(1));
    assert_eq!(found, one);
    assert_eq!(found.total, 3);
}

#[test]
fn a_cancel_while_finding_sessions_reads_none() {
    let home = Home::new();
    // `-a`'s session log is a link: reading it would list it.
    let first = home.session("-a", "s_1", "/a/one", &[]);
    fs::remove_file(first.join("events.jsonl")).unwrap();
    symlink(home.path(), first.join("events.jsonl")).unwrap();
    home.session("-b", "s_2", "/b/one", &["needle"]);
    let scan = home.scanner("/a/one");
    // The scan's check, the one before `-a`, then the one before `-b`.
    let found = scan.search(&query("needle", true, 10), &After::new(2), threads(4));
    assert_eq!(found, Found::default());
}

/// Whether `part` is `whole` with some items left out, in order.
fn subsequence(part: &[String], whole: &[String]) -> bool {
    let mut whole = whole.iter();
    part.iter().all(|item| whole.any(|other| other == item))
}

#[test]
fn a_cancel_at_any_point_returns_part_of_the_answer() {
    let home = Home::new();
    for n in 0..8 {
        home.session(
            "-a",
            &format!("s_{n}"),
            "/a/one",
            &["needle", "needle again"],
        );
    }
    for n in 8..10 {
        let dir = home.session("-a", &format!("s_{n}"), "/a/one", &[]);
        bad(&dir, 3);
    }
    let scan = Arc::new(home.scanner("/a/one"));
    let counted = After::new(usize::MAX);
    let whole = scan.search(&query("needle", true, 1000), &counted, threads(3));
    assert_eq!((whole.total, whole.problems.len()), (16, 6));
    let checks = counted.checks.load(Ordering::SeqCst);
    for after in 0..checks + 2 {
        let scanned = Arc::clone(&scan);
        let found = within("a cancelled scan", Duration::from_secs(10), move || {
            scanned.search(&query("needle", true, 1000), &After::new(after), threads(3))
        });
        assert!(found.total <= whole.total, "cancel after {after}");
        assert!(
            found.hits.iter().all(|hit| whole.hits.contains(hit)),
            "cancel after {after}: {:?}",
            found.hits
        );
        assert!(
            subsequence(&found.problems, &whole.problems),
            "cancel after {after}: {:?}",
            found.problems
        );
    }
}

#[test]
fn identity_runs_once_per_workspace_whatever_the_workers() {
    let home = Home::new();
    for n in 0..8 {
        let workspace = if n % 2 == 0 { "/a/one" } else { "/a/two" };
        home.session("-a", &format!("s_{n}"), workspace, &["needle"]);
    }
    let scan = home.scanner("/a/main");
    let found = scan.search(&query("needle", false, 10), &CancelToken::new(), threads(4));
    assert_eq!(found.total, 8);
    // The own workspace, then /a/one and /a/two.
    assert_eq!(home.calls(), 3);
    scan.search(&query("needle", false, 10), &CancelToken::new(), threads(4));
    assert_eq!(home.calls(), 5);
}

#[test]
fn a_thread_that_cannot_start_loses_no_session() {
    let home = Home::new();
    for n in 0..6 {
        home.session("-a", &format!("s_{n}"), "/a/one", &["needle"]);
    }
    let scan = home.scanner("/a/one");
    let query = query("needle", false, 1000);
    let one = scan.search(&query, &CancelToken::new(), threads(1));
    assert_eq!(one.total, 6);
    for fails in [1, 2] {
        let calls = AtomicUsize::new(0);
        let found =
            scan.search_spawning(&query, &CancelToken::new(), threads(4), &|scope, body| {
                if calls.fetch_add(1, Ordering::SeqCst) + 1 == fails {
                    Err(io::Error::other("no thread"))
                } else {
                    spawn(scope, body)
                }
            });
        assert_eq!(found, one, "spawn {fails} fails");
        assert_eq!(calls.load(Ordering::SeqCst), fails);
    }
}

#[test]
fn no_sessions_start_no_thread() {
    let calls = AtomicUsize::new(0);
    let searched = run(
        &[],
        4,
        10,
        &CancelToken::new(),
        &|_: &Path, _: &mut Collect| panic!("no session to read"),
        &|scope, body| {
            calls.fetch_add(1, Ordering::SeqCst);
            spawn(scope, body)
        },
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(searched.hits.is_empty() && searched.problems.is_empty());
}

#[test]
fn merged_problems_keep_their_order_under_the_cut() {
    let mut out = Collect::new(10);
    for n in 0..15 {
        out.problem(format!("first {n}"));
    }
    out.problem("discovery".to_owned());
    let mut session = Collect::new(10);
    session.problems = (0..10).map(|n| format!("session {n}")).collect();
    session.more_problems = 3;
    out.merge(session);
    let want: Vec<String> = (0..15)
        .map(|n| format!("first {n}"))
        .chain(["discovery".to_owned()])
        .chain((0..4).map(|n| format!("session {n}")))
        .collect();
    assert_eq!(out.problems, want);
    assert_eq!(out.more_problems, 9);
}

#[test]
fn hits_merged_from_two_collects_equal_one_fed_every_hit() {
    let all: Vec<_> = (0..12_u64)
        .map(|n| {
            let label = [Label::Message, Label::ToolInput, Label::ToolOutput][(n % 3) as usize];
            hit(label, n % 4, ["s_a", "s_b"][(n % 2) as usize], n / 3)
        })
        .collect();
    for limit in [0, 1, 3, 12] {
        let mut whole = Collect::new(limit);
        let (mut left, mut right) = (Collect::new(limit), Collect::new(limit));
        for (n, hit) in all.iter().enumerate() {
            whole.hit(hit.clone());
            if n % 2 == 0 {
                left.hit(hit.clone());
            } else {
                right.hit(hit.clone());
            }
        }
        let mut merged = Collect::new(limit);
        merged.merge(left);
        merged.merge(right);
        assert_eq!(merged.found(), whole.found(), "limit {limit}");
    }
}

#[test]
fn taking_problems_leaves_the_hits() {
    let mut out = Collect::new(10);
    out.hit(hit(Label::Message, 1, "s_a", 1));
    out.problem("one".to_owned());
    let taken = out.take_problems();
    assert_eq!((taken.problems.len(), taken.total), (1, 0));
    let found = out.found();
    assert_eq!((found.hits.len(), found.total), (1, 1));
    assert!(found.problems.is_empty());
}
