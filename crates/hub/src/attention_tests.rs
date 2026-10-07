//! Tests for `attention`: the transition table, the log tail read, the
//! line shapes, listener isolation and what the hub remembers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::SessionStatus;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::fake;

const DEADLINE: Duration = Duration::from_secs(10);

/// `wall()` on a fake clock nobody advanced, in milliseconds.
const WALL: u64 = 1_700_000_000_000;

/// A `since` after the hub's start.
const S: u64 = WALL + 5;

/// Builds a status with `state`, named `n` in `/w`. A `"waiting"` state
/// carries `request_id` and `summary "run <request_id>"`; a `"tool"` state
/// runs `"shell"`.
fn status_of(
    state: &str,
    request: Option<&str>,
    since: u64,
    parent: Option<&str>,
) -> SessionStatus {
    let mut payload = fake::status("n", "/w", "idle", parent);
    let map = payload.as_object_mut().unwrap();
    map.insert("state".to_owned(), Value::String(state.to_owned()));
    map.remove("tool");
    map.remove("waiting");
    match state {
        "tool" => {
            map.insert("tool".to_owned(), Value::String("shell".to_owned()));
        }
        "waiting" => {
            let request = request.unwrap_or("r1");
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
    serde_json::from_value(payload).unwrap()
}

fn announced(request: Option<&str>, since: Option<u64>) -> Announced {
    Announced {
        request: request.map(|request| RequestId(request.to_owned())),
        since,
    }
}

struct Case {
    before: Option<(bool, SessionStatus)>,
    now: SessionStatus,
    unseen: bool,
    fresh: bool,
    announced: Announced,
    expected: Option<Reason>,
}

#[test]
fn reason_follows_the_documented_transitions() {
    let idle = || status_of("idle", None, S, None);
    let cases: Vec<(&str, Case)> = vec![
        (
            "none idle sends nothing",
            Case {
                before: None,
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "none idle unseen sends finished",
            Case {
                before: None,
                now: idle(),
                unseen: true,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "none waiting sends waiting",
            Case {
                before: None,
                now: status_of("waiting", Some("r1"), S, None),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Waiting),
            },
        ),
        (
            "live streaming to idle sends finished",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "live tool to idle sends finished",
            Case {
                before: Some((true, status_of("tool", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "live retrying to idle sends finished",
            Case {
                before: Some((true, status_of("retrying", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "live waiting to idle sends finished",
            Case {
                before: Some((true, status_of("waiting", Some("r1"), S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "live jobs to idle sends nothing",
            Case {
                before: Some((true, status_of("jobs", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "live idle to idle sends nothing",
            Case {
                before: Some((true, status_of("idle", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "a live entry ignores unseen",
            Case {
                before: Some((true, status_of("idle", None, S, None))),
                now: idle(),
                unseen: true,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "left streaming to idle sends nothing",
            Case {
                before: Some((false, status_of("streaming", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "left streaming to idle unseen sends finished",
            Case {
                before: Some((false, status_of("streaming", None, S, None))),
                now: idle(),
                unseen: true,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "an announced since is not announced again",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, Some(S)),
                expected: None,
            },
        ),
        (
            "live idle to idle with another count, since announced, sends nothing",
            Case {
                before: Some((true, status_of("idle", None, S, None))),
                now: {
                    let mut now = status_of("idle", None, S, None);
                    now.clients = 1;
                    now
                },
                unseen: false,
                fresh: true,
                announced: announced(None, Some(S)),
                expected: None,
            },
        ),
        (
            "none idle unseen announced sends nothing",
            Case {
                before: None,
                now: idle(),
                unseen: true,
                fresh: true,
                announced: announced(None, Some(S)),
                expected: None,
            },
        ),
        (
            "another idle period sends finished",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: idle(),
                unseen: false,
                fresh: true,
                announced: announced(None, Some(S - 1)),
                expected: Some(Reason::Finished),
            },
        ),
        (
            "a stale idle sends nothing",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: status_of("idle", None, S, None),
                unseen: false,
                fresh: false,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "none idle unseen stale sends nothing",
            Case {
                before: None,
                now: idle(),
                unseen: true,
                fresh: false,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "the same waiting request is not announced again",
            Case {
                before: Some((true, status_of("waiting", Some("r1"), S, None))),
                now: status_of("waiting", Some("r1"), S + 1, None),
                unseen: false,
                fresh: true,
                announced: announced(Some("r1"), None),
                expected: None,
            },
        ),
        (
            "live waiting r1 to waiting r1 with another count, r1 announced, sends nothing",
            Case {
                before: Some((true, status_of("waiting", Some("r1"), S, None))),
                now: {
                    let mut now = status_of("waiting", Some("r1"), S, None);
                    now.clients = 1;
                    now
                },
                unseen: false,
                fresh: true,
                announced: announced(Some("r1"), None),
                expected: None,
            },
        ),
        (
            "a left waiting request announced sends nothing",
            Case {
                before: Some((false, status_of("waiting", Some("r1"), S, None))),
                now: status_of("waiting", Some("r1"), S, None),
                unseen: false,
                fresh: true,
                announced: announced(Some("r1"), None),
                expected: None,
            },
        ),
        (
            "a left waiting request unannounced sends waiting",
            Case {
                before: Some((false, status_of("waiting", Some("r1"), S, None))),
                now: status_of("waiting", Some("r1"), S, None),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Waiting),
            },
        ),
        (
            "a new waiting request sends waiting",
            Case {
                before: Some((true, status_of("waiting", Some("r1"), S, None))),
                now: status_of("waiting", Some("r2"), S, None),
                unseen: false,
                fresh: true,
                announced: announced(Some("r1"), None),
                expected: Some(Reason::Waiting),
            },
        ),
        (
            "streaming to waiting sends waiting",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: status_of("waiting", Some("r1"), S, None),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: Some(Reason::Waiting),
            },
        ),
        (
            "a turn starting sends nothing",
            Case {
                before: Some((true, status_of("idle", None, S, None))),
                now: status_of("streaming", None, S, None),
                unseen: true,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "streaming to jobs sends nothing",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: status_of("jobs", None, S, None),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "waiting to tool sends nothing",
            Case {
                before: Some((true, status_of("waiting", Some("r1"), S, None))),
                now: status_of("tool", None, S, None),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "a delegate turn end sends nothing",
            Case {
                before: Some((true, status_of("streaming", None, S, None))),
                now: status_of("idle", None, S, Some("s_0000000000000001")),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "a delegate unseen idle sends nothing",
            Case {
                before: None,
                now: status_of("idle", None, S, Some("s_0000000000000001")),
                unseen: true,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
        (
            "a delegate waiting sends nothing",
            Case {
                before: None,
                now: status_of("waiting", Some("r1"), S, Some("s_0000000000000001")),
                unseen: false,
                fresh: true,
                announced: announced(None, None),
                expected: None,
            },
        ),
    ];
    for (name, case) in &cases {
        let before;
        let seen = match &case.before {
            Some((live, status)) => {
                before = *live;
                Some(Seen {
                    status,
                    live: before,
                })
            }
            None => None,
        };
        let facts = Facts {
            before: seen,
            unseen: case.unseen,
            fresh: case.fresh,
            announced: &case.announced,
        };
        assert_eq!(reason(&case.now, &facts), case.expected, "{name}");
    }
}

#[test]
fn turn_ended_at_matches_only_a_complete_turn_completed_with_that_ts() {
    let held = fakes::TempDir::new("ha");
    let log = held.path().join("events.jsonl");
    let turn = |ts: &str| format!("{{\"kind\":\"turn_completed\",\"ts\":{ts}}}\n");
    let other = |kind: &str, ts: &str| format!("{{\"kind\":\"{kind}\",\"ts\":{ts}}}\n");
    // Past `TAIL`: one line is 21 bytes, so 6,000 are about 126 KiB.
    let filler = other("x", "1").repeat(6_000);
    let cases: Vec<(&str, Option<String>, bool)> = vec![
        ("missing", None, false),
        ("empty", Some(String::new()), false),
        (
            "a turn_completed with ts S",
            Some(turn(&S.to_string())),
            true,
        ),
        (
            "a turn_completed with ts S then two other lines",
            Some(format!(
                "{}{}{}",
                turn(&S.to_string()),
                other("x", "1"),
                other("y", "2")
            )),
            true,
        ),
        (
            "a turn_completed with another ts",
            Some(turn(&(S + 1).to_string())),
            false,
        ),
        (
            "another kind with ts S",
            Some(other("x", &S.to_string())),
            false,
        ),
        (
            "a turn_completed with a string ts",
            Some(turn(&format!("\"{S}\""))),
            false,
        ),
        (
            "a turn_completed with no ts",
            Some("{\"kind\":\"turn_completed\"}\n".to_owned()),
            false,
        ),
        (
            "a bad line then a turn_completed with ts S",
            Some(format!("not json\n{}", turn(&S.to_string()))),
            true,
        ),
        (
            "a torn last line",
            Some(turn(&S.to_string()).trim_end().to_owned()),
            false,
        ),
        (
            "a turn_completed after 100 KiB of other lines",
            Some(format!("{filler}{}", turn(&S.to_string()))),
            true,
        ),
        (
            "a turn_completed before 100 KiB of other lines",
            Some(format!("{}{filler}", turn(&S.to_string()))),
            false,
        ),
        (
            "descending ts around a turn_completed with ts S",
            Some(format!(
                "{}{}{}",
                other("x", "9"),
                turn(&S.to_string()),
                other("y", "3")
            )),
            true,
        ),
    ];
    for (name, body, expected) in &cases {
        match body {
            None => {
                std::fs::remove_file(&log).unwrap_or(());
            }
            Some(body) => {
                std::fs::write(&log, body).unwrap();
            }
        }
        assert_eq!(turn_ended_at(&log, S), *expected, "{name}");
    }
}

#[test]
fn turn_ended_at_at_a_window_edge_counts_a_whole_line_but_not_a_partial_one() {
    let held = fakes::TempDir::new("hc");
    let log = held.path().join("events.jsonl");
    let target = format!("{{\"kind\":\"turn_completed\",\"ts\":{S}}}\n");
    let filler = "{\"kind\":\"x\",\"ts\":1}\n";
    let tail = usize::try_from(crate::feed::TAIL).unwrap();
    // Whole lines plus empty lines to an exact byte length.
    let pad = |n: usize| {
        format!(
            "{}{}",
            filler.repeat(n / filler.len()),
            "\n".repeat(n % filler.len())
        )
    };
    // The window holds exactly the target and what follows it, so it
    // starts at the target's first byte, right after a newline.
    let suffix = pad(tail - target.len());
    assert_eq!(target.len() + suffix.len(), tail);
    std::fs::write(&log, format!("{filler}{target}{suffix}")).unwrap();
    assert!(
        turn_ended_at(&log, S),
        "a complete line at the window boundary counts"
    );
    // The window starts inside a trash line at the target's brace: the
    // remainder parses, but it is partial, so it must not count.
    std::fs::write(&log, format!("{filler}TRASH{target}{suffix}")).unwrap();
    assert!(
        !turn_ended_at(&log, S),
        "a partial first line never counts, even when it parses"
    );
}

#[test]
fn ended_unseen_needs_a_since_at_or_after_the_hub_start() {
    let held = fakes::TempDir::new("hb");
    let log = held.path().join("events.jsonl");
    let line = |ts: u64| format!("{{\"kind\":\"turn_completed\",\"ts\":{ts}}}\n");
    std::fs::write(&log, format!("{}{}", line(WALL - 1), line(WALL))).unwrap();
    let clock = FakeClock::new();
    let attention = Attention::new(Arc::clone(&clock) as Arc<dyn Clock>);
    assert!(attention.ended_unseen(&log, &status_of("idle", None, WALL, None)));
    assert!(!attention.ended_unseen(&log, &status_of("idle", None, WALL - 1, None)));
    assert!(!attention.ended_unseen(&log, &status_of("idle", None, WALL + 1, None)));
}

fn listened(attention: &Attention) -> BufReader<UnixStream> {
    let (a, b) = UnixStream::pair().unwrap();
    b.set_read_timeout(Some(DEADLINE)).unwrap();
    attention.listen(Arc::new(Mutex::new(a))).unwrap();
    BufReader::new(b)
}

fn read_line(read: &mut BufReader<UnixStream>, what: &str) -> Value {
    let mut text = String::new();
    read.read_line(&mut text)
        .unwrap_or_else(|_| panic!("never received {what}"));
    assert!(!text.is_empty(), "attention closed before {what}");
    serde_json::from_str(&text).unwrap()
}

/// Waits under [`DEADLINE`] until `done` holds, naming `what` on expiry.
fn await_true(what: &str, done: impl Fn() -> bool + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    thread::spawn(move || {
        while !done() {
            if cancel_rx.try_recv().is_ok() {
                return;
            }
            thread::yield_now();
        }
        tx.send(()).unwrap_or(());
    });
    if rx.recv_timeout(DEADLINE).is_err() {
        cancel_tx.send(()).unwrap_or(());
        panic!("waited for {what}");
    }
}

/// Drops listener `id` on a thread and receives its return under
/// [`DEADLINE`]: joining the writer blocks.
fn unlisten_within(attention: &Arc<Attention>, id: u64, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    let ending = Arc::clone(attention);
    thread::spawn(move || {
        ending.unlisten(id);
        done_tx.send(()).unwrap_or(());
    });
    assert!(done_rx.recv_timeout(DEADLINE).is_ok(), "{what}");
}

#[test]
fn a_waiting_line_carries_the_summary_and_a_finished_line_does_not() {
    let clock = FakeClock::new();
    let attention = Attention::new(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut read = listened(&attention);
    let waiting: SessionStatus =
        serde_json::from_value(fake::status("n", "/w", "waiting", None)).unwrap();
    attention.notify("s_0000000000000001", None, &waiting, false);
    assert_eq!(
        read_line(&mut read, "the waiting attention"),
        json!({
            "kind": "attention", "ts": WALL, "schema_version": 1,
            "payload": {
                "name": "n", "reason": "waiting",
                "session_id": "s_0000000000000001",
                "summary": "run ls", "workspace": "/w",
            },
        })
    );
    let before = Seen {
        status: &waiting,
        live: true,
    };
    attention.notify(
        "s_0000000000000001",
        Some(before),
        &status_of("idle", None, WALL, None),
        false,
    );
    assert_eq!(
        read_line(&mut read, "the finished attention"),
        json!({
            "kind": "attention", "ts": WALL, "schema_version": 1,
            "payload": {
                "name": "n", "reason": "finished",
                "session_id": "s_0000000000000001", "workspace": "/w",
            },
        })
    );
}

#[test]
fn a_dead_or_slow_listener_does_not_stop_a_live_one() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (slow, slow_far) = UnixStream::pair().unwrap();
    let slow = attention.listen(Arc::new(Mutex::new(slow))).unwrap();
    let (dead, dead_far) = UnixStream::pair().unwrap();
    attention.listen(Arc::new(Mutex::new(dead))).unwrap();
    drop(dead_far);
    let (live, live_far) = UnixStream::pair().unwrap();
    let live = attention.listen(Arc::new(Mutex::new(live))).unwrap();
    let mut read = BufReader::new(live_far);
    read.get_mut().set_read_timeout(Some(DEADLINE)).unwrap();
    let idle = status_of("idle", None, S, None);
    let streaming = status_of("streaming", None, S, None);
    let heard = Seen {
        status: &streaming,
        live: true,
    };
    attention.notify("a", Some(heard), &idle, false);
    assert_eq!(
        read_line(&mut read, "the first attention")["payload"]["reason"],
        "finished"
    );
    let ended = Arc::clone(&attention);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while ended.ended() != 1 {
            thread::yield_now();
        }
        tx.send(()).unwrap_or(());
    });
    assert!(rx.recv_timeout(DEADLINE).is_ok(), "the dead writer ends");
    assert_eq!(
        attention.listeners(),
        3,
        "the dead sender stays until the next send"
    );
    attention.notify(
        "a",
        Some(heard),
        &status_of("idle", None, S + 1, None),
        false,
    );
    assert_eq!(
        attention.listeners(),
        2,
        "the failed send drops the dead listener"
    );
    assert_eq!(
        read_line(&mut read, "the second attention")["payload"]["reason"],
        "finished"
    );
    let count = 1_000;
    for n in 0..count {
        let mut waiting = status_of("waiting", Some(&format!("r{n}")), S, None);
        waiting.name = format!("{n}:{}", "x".repeat(2_000));
        attention.notify("a", None, &waiting, false);
    }
    for n in 0..count {
        let line = read_line(&mut read, "every attention");
        assert!(
            line["payload"]["name"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{n}:")),
            "line {n} in order"
        );
    }
    unlisten_within(&attention, live, "the live unlisten returns");
    drop(slow_far);
    unlisten_within(&attention, slow, "the slow unlisten returns");
}

#[test]
fn unlisten_ends_only_that_listener() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (first, first_far) = UnixStream::pair().unwrap();
    first_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let held = Arc::new(Mutex::new(first));
    let first = attention.listen(Arc::clone(&held)).unwrap();
    let mut second = listened(&attention);
    unlisten_within(&attention, first, "unlisten returns");
    drop(held);
    attention.notify("a", None, &status_of("waiting", Some("r1"), S, None), false);
    assert_eq!(
        read_line(&mut second, "the attention")["payload"]["reason"],
        "waiting"
    );
    let mut first = BufReader::new(first_far);
    let mut buf = [0; 1];
    use std::io::Read;
    assert_eq!(first.read(&mut buf).unwrap(), 0, "the first listener ends");
}

#[test]
fn notify_remembers_what_it_announced_per_session_until_forgotten() {
    let clock = FakeClock::new();
    let attention = Attention::new(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut read = listened(&attention);
    let streaming = status_of("streaming", None, S, None);
    let heard = Seen {
        status: &streaming,
        live: true,
    };
    let idle = status_of("idle", None, S, None);
    let waiting = status_of("waiting", Some("r1"), S, None);
    let reasons = |line: Value| line["payload"]["reason"].clone();
    attention.notify("a", Some(heard), &idle, false);
    assert_eq!(reasons(read_line(&mut read, "finished for a")), "finished");
    // The same idle period sends nothing: the next line is a's waiting.
    attention.notify("a", Some(heard), &idle, false);
    attention.notify("a", None, &waiting, false);
    assert_eq!(reasons(read_line(&mut read, "waiting for a")), "waiting");
    // The same request sends nothing: the next lines are b's pair.
    attention.notify("a", None, &waiting, false);
    attention.notify("b", Some(heard), &idle, false);
    assert_eq!(reasons(read_line(&mut read, "finished for b")), "finished");
    attention.notify("b", None, &waiting, false);
    assert_eq!(reasons(read_line(&mut read, "waiting for b")), "waiting");
    attention.forget("a");
    attention.notify("a", Some(heard), &idle, false);
    assert_eq!(
        reasons(read_line(&mut read, "finished for a again")),
        "finished"
    );
    attention.notify("a", Some(heard), &idle, false);
    attention.notify("a", None, &waiting, false);
    assert_eq!(
        reasons(read_line(&mut read, "waiting for a again")),
        "waiting"
    );
}

#[test]
fn unlisten_waits_for_its_writer_still_draining_its_backlog() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let kept = Arc::new(Mutex::new(writer));
    let id = attention.listen(Arc::clone(&kept)).unwrap();
    // Megabytes queued, far more than the socket buffer holds: the writer
    // thread cannot end until the far end has read nearly all of it.
    let count = 4_000;
    for n in 0..count {
        let request = format!("r{n}");
        let mut waiting = status_of("waiting", Some(&request), S, None);
        waiting.name = format!("{n}:{}", "x".repeat(2_000));
        attention.notify("s_0000000000000001", None, &waiting, false);
    }
    let (tx, rx) = mpsc::channel();
    let ending = Arc::clone(&attention);
    let watched = Arc::clone(&kept);
    thread::spawn(move || {
        ending.unlisten(id);
        tx.send(Arc::strong_count(&watched)).unwrap_or(());
    });
    // The far end holds its reader until the test-only join point confirms
    // unlisten reached the writer join. Under the no-op-`join` mutant the
    // counter never advances, so this wait fails on every run.
    let joining = Arc::clone(&attention);
    await_true("unlisten to reach its writer join", move || {
        joining.joining() == 1
    });
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "unlisten returned while its writer still held a full backlog"
    );
    let drained = thread::spawn(move || {
        let mut read = BufReader::new(far);
        let mut text = String::new();
        for _ in 0..count {
            text.clear();
            read.read_line(&mut text).unwrap();
            assert!(!text.is_empty());
        }
    });
    let left = rx.recv_timeout(DEADLINE).expect("unlisten returns");
    // The test's handle, `watched`, and none from the writer thread.
    assert_eq!(left, 2, "unlisten returned before its writer ended");
    let (joined_tx, joined_rx) = mpsc::channel();
    thread::spawn(move || {
        joined_tx.send(drained.join()).unwrap_or(());
    });
    let joined = joined_rx
        .recv_timeout(DEADLINE)
        .expect("the far end drains");
    joined.unwrap();
}
