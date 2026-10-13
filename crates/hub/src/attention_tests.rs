//! Tests for `attention`: the transition table, the log tail read, the
//! line shapes, listener isolation and what the hub remembers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::SessionStatus;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::*;
use crate::fake;
use crate::testkit::await_true;

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

#[test]
fn announce_reaches_every_listener() {
    let clock = FakeClock::new();
    let attention = Attention::new(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut first = listened(&attention);
    let mut second = listened(&attention);
    let line = b"{\"kind\":\"device_changed\",\"payload\":{\"device\":\"phone\",\"change\":\"paired\",\"by\":\"local\"}}\n";
    drop(attention.announce(Arc::from(line.as_slice())));
    let expected: Value = serde_json::from_slice(line).unwrap();
    assert_eq!(
        read_line(&mut first, "the first listener's device change"),
        expected
    );
    assert_eq!(
        read_line(&mut second, "the second listener's device change"),
        expected
    );
}

/// A shutdown handle for a listener on `writer`: its own clone, so
/// cleanup never clones at the moment it runs.
fn closer_for(writer: &UnixStream) -> Closer {
    Closer {
        stream: writer.try_clone().unwrap(),
        web: None,
    }
}

fn listened(attention: &Attention) -> BufReader<UnixStream> {
    let (a, b) = UnixStream::pair().unwrap();
    b.set_read_timeout(Some(DEADLINE)).unwrap();
    let (closer, retired) = (closer_for(&a), closer_for(&a));
    attention
        .listen(Arc::new(Mutex::new(a)), closer, retired)
        .unwrap();
    BufReader::new(b)
}

fn read_line(read: &mut BufReader<UnixStream>, what: &str) -> Value {
    let mut text = String::new();
    read.read_line(&mut text)
        .unwrap_or_else(|_| panic!("never received {what}"));
    assert!(!text.is_empty(), "attention closed before {what}");
    serde_json::from_str(&text).unwrap()
}

/// Drops listener `id` on a thread and receives its return under
/// [`DEADLINE`]: joining the writer blocks.
#[track_caller]
fn unlisten_within(attention: &Arc<Attention>, id: u64, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    let ending = Arc::clone(attention);
    thread::spawn(move || {
        ending.unlisten(id);
        done_tx.send(()).unwrap_or(());
    });
    assert!(Deadline::after(DEADLINE).recv(&done_rx).is_ok(), "{what}");
}

#[test]
fn a_waiting_line_carries_the_summary_and_a_finished_line_does_not() {
    let clock = FakeClock::new();
    let attention = Attention::new(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut read = listened(&attention);
    let waiting: SessionStatus =
        serde_json::from_value(fake::status("n", "/w", "waiting", None)).unwrap();
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
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
    drop(attention.notify(
        "s_0000000000000001",
        Some(before),
        &status_of("idle", None, WALL, None),
        false,
    ));
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
    let (slow_closer, slow_retired) = (closer_for(&slow), closer_for(&slow));
    let slow = attention
        .listen(Arc::new(Mutex::new(slow)), slow_closer, slow_retired)
        .unwrap();
    let (dead, dead_far) = UnixStream::pair().unwrap();
    let (dead_closer, dead_retired) = (closer_for(&dead), closer_for(&dead));
    attention
        .listen(Arc::new(Mutex::new(dead)), dead_closer, dead_retired)
        .unwrap();
    drop(dead_far);
    let (live, live_far) = UnixStream::pair().unwrap();
    let (live_closer, live_retired) = (closer_for(&live), closer_for(&live));
    let live = attention
        .listen(Arc::new(Mutex::new(live)), live_closer, live_retired)
        .unwrap();
    let mut read = BufReader::new(live_far);
    read.get_mut().set_read_timeout(Some(DEADLINE)).unwrap();
    let idle = status_of("idle", None, S, None);
    let streaming = status_of("streaming", None, S, None);
    let heard = Seen {
        status: &streaming,
        live: true,
    };
    drop(attention.notify("a", Some(heard), &idle, false));
    assert_eq!(
        read_line(&mut read, "the first attention")["payload"]["reason"],
        "finished"
    );
    assert_eq!(
        attention.listeners(),
        3,
        "the dead sender stays until the next send"
    );
    // The dead writer ends on its first failed write; further sends retire
    // it. Each send carries a distinct idle `since`, so none is
    // deduplicated, and every send reaches the live listener.
    let polls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&polls);
    let retiring = Arc::clone(&attention);
    await_true("the dead listener to drop", move || {
        if retiring.listeners() == 2 {
            return true;
        }
        let n = polls.fetch_add(1, Ordering::SeqCst);
        let streaming = status_of("streaming", None, S, None);
        let idle = status_of("idle", None, S + 2 + n, None);
        let heard = Seen {
            status: &streaming,
            live: true,
        };
        drop(retiring.notify("a", Some(heard), &idle, false));
        false
    });
    let extra = counted.load(Ordering::SeqCst);
    assert_eq!(
        read_line(&mut read, "the second attention")["payload"]["reason"],
        "finished"
    );
    for _ in 1..extra {
        read_line(&mut read, "a further attention");
    }
    let count = 1_000;
    for n in 0..count {
        let mut waiting = status_of("waiting", Some(&format!("r{n}")), S, None);
        waiting.name = format!("{n}:{}", "x".repeat(2_000));
        drop(attention.notify("a", None, &waiting, false));
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
fn a_stalled_listener_is_dropped_at_the_bound_and_a_live_one_keeps_every_line() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    // Stalled: the test holds the writer mutex before listening, so the
    // writer thread can never dequeue.
    let (stalled, stalled_far) = UnixStream::pair().unwrap();
    stalled_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (stalled_closer, stalled_retired) = (closer_for(&stalled), closer_for(&stalled));
    let held = Arc::new(Mutex::new(stalled));
    let guard = held.lock().unwrap();
    let stalled_id = attention
        .listen(Arc::clone(&held), stalled_closer, stalled_retired)
        .unwrap();
    // Live: drains each line before the next notify, in order.
    let (live, live_far) = UnixStream::pair().unwrap();
    live_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (live_closer, live_retired) = (closer_for(&live), closer_for(&live));
    attention
        .listen(Arc::new(Mutex::new(live)), live_closer, live_retired)
        .unwrap();
    let mut read = BufReader::new(live_far);
    // QUEUE+2 distinct waiting requests: dedup cannot hide a send, and
    // one dequeued in-flight item still leaves the bound firing.
    let count = crate::feed::QUEUE + 2;
    for n in 0..count {
        let request = format!("r{n}");
        let waiting = status_of("waiting", Some(&request), S, None);
        let closers = attention.notify("s_0000000000000001", None, &waiting, false);
        // Only the stalled connection can ever report: shut it outside
        // every lock, as production callers do.
        if !closers.is_empty() {
            crate::feed::close_connections(closers, UnixStream::shutdown);
        }
        let line = read_line(&mut read, "every attention");
        assert_eq!(line["payload"]["reason"], "waiting");
        assert_eq!(line["payload"]["summary"], format!("run r{n}"));
    }
    // The bound fired while the mutex was still held: the stalled
    // registration is gone and the live one is retained.
    assert_eq!(attention.listeners(), 1, "the stalled listener is gone");
    drop(guard);
    // The overflow shut the stalled connection both ways: EOF, not a line.
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(stalled_far).read(&mut buf).unwrap(),
        0,
        "the stalled side sees EOF"
    );
    // Unlistening the removed id is a no-op, and stop joins the retired
    // writer: no Arc survives it.
    attention.unlisten(stalled_id);
    let stopping = Arc::clone(&attention);
    let (stopped_tx, stopped_rx) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&stopped_rx).is_ok(),
        "stop returns"
    );
    assert_eq!(Arc::strong_count(&held), 1, "the writer ended and joined");
}

#[test]
fn announce_reports_a_stalled_listener_without_silently_dropping_it() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (stalled, stalled_far) = UnixStream::pair().unwrap();
    stalled_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (stalled_closer, stalled_retired) = (closer_for(&stalled), closer_for(&stalled));
    let held = Arc::new(Mutex::new(stalled));
    let guard = held.lock().unwrap();
    attention
        .listen(Arc::clone(&held), stalled_closer, stalled_retired)
        .unwrap();
    let line = b"{\"kind\":\"device_changed\",\"payload\":{}}\n";
    let mut reported = 0;
    for _ in 0..crate::feed::QUEUE + 2 {
        let closers = attention.announce(Arc::from(&line[..]));
        if !closers.is_empty() {
            reported += closers.len();
            crate::feed::close_connections(closers, UnixStream::shutdown);
        }
    }
    assert_eq!(reported, 1, "the overflow hands back exactly one shutdown");
    assert_eq!(attention.listeners(), 0, "the stalled listener is gone");
    drop(guard);
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(stalled_far).read(&mut buf).unwrap(),
        0,
        "the stalled side sees EOF"
    );
    let stopping = Arc::clone(&attention);
    let (stopped_tx, stopped_rx) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&stopped_rx).is_ok(),
        "stop returns"
    );
    assert_eq!(Arc::strong_count(&held), 1, "the writer ended and joined");
}

#[test]
fn stop_shuts_active_but_leaves_pending_receiving() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    // Active: listened, then one waiting line starts its writer.
    let (second, second_far) = UnixStream::pair().unwrap();
    second_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (second_closer, second_retired) = (closer_for(&second), closer_for(&second));
    let second_writer = Arc::new(Mutex::new(second));
    attention
        .listen(Arc::clone(&second_writer), second_closer, second_retired)
        .unwrap();
    let waiting = status_of("waiting", Some("r1"), S, None);
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
    assert_eq!(
        attention.writers_started(),
        1,
        "the notified listener started"
    );
    assert_eq!(
        read_line(
            &mut BufReader::new(second_far.try_clone().unwrap()),
            "the attention"
        )["payload"]["reason"],
        "waiting"
    );
    // Pending: listened after the only line, so no writer ever starts.
    let (first, first_far) = UnixStream::pair().unwrap();
    first_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (first_closer, first_retired) = (closer_for(&first), closer_for(&first));
    let first_writer = Arc::new(Mutex::new(first));
    attention
        .listen(Arc::clone(&first_writer), first_closer, first_retired)
        .unwrap();
    assert_eq!(attention.writers_started(), 1, "listening starts nothing");
    // The registry shuts: no later listen registers and no later line is
    // queued or reported.
    let stopping = Arc::clone(&attention);
    let (stopped_tx, stopped_rx) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&stopped_rx).is_ok(),
        "stop returns"
    );
    let (later, _) = UnixStream::pair().unwrap();
    let (later_closer, later_retired) = (closer_for(&later), closer_for(&later));
    assert!(
        attention
            .listen(Arc::new(Mutex::new(later)), later_closer, later_retired)
            .is_none(),
        "no listen past a stop"
    );
    let waiting = status_of("waiting", Some("r2"), S, None);
    assert!(
        attention
            .notify("s_0000000000000001", None, &waiting, false)
            .is_empty()
    );
    let line = b"{\"kind\":\"device_changed\",\"payload\":{}}";
    assert!(attention.announce(Arc::from(&line[..])).is_empty());
    assert_eq!(attention.listeners(), 0);
    // The pending socket was never shut: a server-side write still reaches
    // its far end, so a relay on the same connection keeps receiving past
    // the feed's stop.
    first_writer.lock().unwrap().write_all(b"x").unwrap();
    let mut buf = [0; 1];
    let mut first_far = first_far;
    first_far.read_exact(&mut buf).unwrap();
    assert_eq!(buf, [b'x'], "the pending socket was never shut");
    // The active socket was shut: its far end sees EOF.
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(second_far).read(&mut buf).unwrap(),
        0,
        "the active side sees EOF"
    );
    assert_eq!(Arc::strong_count(&first_writer), 1, "no pending writer");
    assert_eq!(
        Arc::strong_count(&second_writer),
        1,
        "the active writer joined"
    );
}

#[test]
fn the_first_attention_starts_one_writer_and_later_lines_reuse_it() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (closer, retired) = (closer_for(&writer), closer_for(&writer));
    let kept = Arc::new(Mutex::new(writer));
    attention
        .listen(Arc::clone(&kept), closer, retired)
        .unwrap();
    assert_eq!(attention.writers_started(), 0, "listening starts nothing");
    let mut read = BufReader::new(far);
    // The first waiting line starts exactly one writer and delivers its
    // documented payload.
    let waiting = status_of("waiting", Some("r1"), S, None);
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
    assert_eq!(attention.writers_started(), 1, "the first line starts one");
    let line = read_line(&mut read, "the waiting attention");
    assert_eq!(line["payload"]["reason"], "waiting");
    assert_eq!(line["payload"]["session_id"], "s_0000000000000001");
    assert_eq!(line["payload"]["summary"], "run r1");
    // The next waiting and finished lines reuse that same writer, in order.
    let streaming = status_of("streaming", None, S, None);
    let heard = Seen {
        status: &streaming,
        live: true,
    };
    let idle = status_of("idle", None, S, None);
    drop(attention.notify("s_0000000000000001", Some(heard), &idle, false));
    let waiting = status_of("waiting", Some("r2"), S, None);
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
    assert_eq!(attention.writers_started(), 1, "still one writer");
    assert_eq!(
        read_line(&mut read, "the finished attention")["payload"]["reason"],
        "finished"
    );
    assert_eq!(
        read_line(&mut read, "the second waiting attention")["payload"]["reason"],
        "waiting"
    );
    drop(kept);
}

#[test]
fn a_pending_unlisten_starts_no_writer() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, _far) = UnixStream::pair().unwrap();
    let (closer, retired) = (closer_for(&writer), closer_for(&writer));
    let id = attention
        .listen(Arc::new(Mutex::new(writer)), closer, retired)
        .unwrap();
    attention.unlisten(id);
    assert_eq!(attention.listeners(), 0);
    let waiting = status_of("waiting", Some("r1"), S, None);
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
    assert_eq!(attention.writers_started(), 0, "no writer ever started");
    assert_eq!(attention.listeners(), 0);
}

#[test]
fn concurrent_first_notifies_start_one_writer() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (closer, retired) = (closer_for(&writer), closer_for(&writer));
    attention
        .listen(Arc::new(Mutex::new(writer)), closer, retired)
        .unwrap();
    // Two threads race their first lines through one barrier: whichever
    // activates, the other only enqueues.
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Arc::new(Mutex::new(go_rx));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut handles = Vec::new();
    for session in ["s_00000000000000c1", "s_00000000000000c2"] {
        let attention = Arc::clone(&attention);
        let go_rx = Arc::clone(&go_rx);
        let done_tx = done_tx.clone();
        handles.push(
            thread::Builder::new()
                .name("attention-test-first".to_owned())
                .spawn(move || {
                    go_rx.lock().unwrap().recv().unwrap_or(());
                    let waiting = status_of("waiting", Some("r1"), S, None);
                    drop(attention.notify(session, None, &waiting, false));
                    done_tx.send(()).unwrap_or(());
                })
                .unwrap(),
        );
    }
    drop(done_tx);
    go_tx.send(()).unwrap_or(());
    go_tx.send(()).unwrap_or(());
    for _ in 0..2 {
        Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("both first lines to land");
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(attention.writers_started(), 1, "one spawn for both firsts");
    // Both lines arrived, one per session.
    let mut read = BufReader::new(far);
    let mut sessions = Vec::new();
    for _ in 0..2 {
        sessions
            .push(read_line(&mut read, "each first attention")["payload"]["session_id"].clone());
    }
    sessions.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(
        sessions,
        [
            Value::String("s_00000000000000c1".to_owned()),
            Value::String("s_00000000000000c2".to_owned())
        ]
    );
}

#[test]
fn first_announce_starts_the_writer_and_later_attention_reuses_it() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (closer, retired) = (closer_for(&writer), closer_for(&writer));
    let kept = Arc::new(Mutex::new(writer));
    attention
        .listen(Arc::clone(&kept), closer, retired)
        .unwrap();
    // A device notice needs no attention first: it starts the writer and
    // delivers.
    let line = b"{\"kind\":\"device_changed\",\"payload\":{\"device\":\"phone\"}}\n";
    drop(attention.announce(Arc::from(&line[..])));
    assert_eq!(attention.writers_started(), 1, "the notice starts one");
    let mut read = BufReader::new(far);
    assert_eq!(
        read_line(&mut read, "the device change")["kind"],
        "device_changed"
    );
    // Later attention reuses that writer, and retiring it keeps the count.
    let waiting = status_of("waiting", Some("r1"), S, None);
    drop(attention.notify("s_0000000000000001", None, &waiting, false));
    assert_eq!(attention.writers_started(), 1, "still one writer");
    assert_eq!(
        read_line(&mut read, "the waiting attention")["payload"]["reason"],
        "waiting"
    );
    drop(kept);
}

#[test]
fn unlisten_ends_only_that_listener() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (first, first_far) = UnixStream::pair().unwrap();
    first_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (first_closer, first_retired) = (closer_for(&first), closer_for(&first));
    let held = Arc::new(Mutex::new(first));
    let first = attention
        .listen(Arc::clone(&held), first_closer, first_retired)
        .unwrap();
    let mut second = listened(&attention);
    unlisten_within(&attention, first, "unlisten returns");
    drop(held);
    drop(attention.notify("a", None, &status_of("waiting", Some("r1"), S, None), false));
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
    drop(attention.notify("a", Some(heard), &idle, false));
    assert_eq!(reasons(read_line(&mut read, "finished for a")), "finished");
    // The same idle period sends nothing: the next line is a's waiting.
    drop(attention.notify("a", Some(heard), &idle, false));
    drop(attention.notify("a", None, &waiting, false));
    assert_eq!(reasons(read_line(&mut read, "waiting for a")), "waiting");
    // The same request sends nothing: the next lines are b's pair.
    drop(attention.notify("a", None, &waiting, false));
    drop(attention.notify("b", Some(heard), &idle, false));
    assert_eq!(reasons(read_line(&mut read, "finished for b")), "finished");
    drop(attention.notify("b", None, &waiting, false));
    assert_eq!(reasons(read_line(&mut read, "waiting for b")), "waiting");
    attention.forget("a");
    drop(attention.notify("a", Some(heard), &idle, false));
    assert_eq!(
        reasons(read_line(&mut read, "finished for a again")),
        "finished"
    );
    drop(attention.notify("a", Some(heard), &idle, false));
    drop(attention.notify("a", None, &waiting, false));
    assert_eq!(
        reasons(read_line(&mut read, "waiting for a again")),
        "waiting"
    );
}

#[test]
fn unlisten_after_overflow_joins_without_draining() {
    let clock = FakeClock::new();
    let attention = Arc::new(Attention::new(Arc::clone(&clock) as Arc<dyn Clock>));
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (writer_closer, writer_retired) = (closer_for(&writer), closer_for(&writer));
    let kept = Arc::new(Mutex::new(writer));
    // Held by the test: the writer cannot dequeue, so the bound fires
    // instead of megabytes queueing behind it.
    let guard = kept.lock().unwrap();
    let id = attention
        .listen(Arc::clone(&kept), writer_closer, writer_retired)
        .unwrap();
    let mut reported = 0;
    for n in 0..crate::feed::QUEUE + 2 {
        let request = format!("r{n}");
        let waiting = status_of("waiting", Some(&request), S, None);
        let closers = attention.notify("s_0000000000000001", None, &waiting, false);
        if !closers.is_empty() {
            reported += closers.len();
            crate::feed::close_connections(closers, UnixStream::shutdown);
        }
    }
    assert_eq!(reported, 1, "the overflow hands back exactly one shutdown");
    assert_eq!(attention.listeners(), 0, "the bound fired while stalled");
    drop(guard);
    // The far end sees EOF, not the queued megabytes: the overflow shut
    // the connection instead of draining it.
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(far).read(&mut buf).unwrap(),
        0,
        "the stalled side sees EOF"
    );
    // Unlistening the removed id is a no-op, and stop joins the retired
    // writer: no Arc survives it.
    attention.unlisten(id);
    let stopping = Arc::clone(&attention);
    let (stopped_tx, stopped_rx) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).unwrap_or(());
    });
    assert!(
        Deadline::after(DEADLINE).recv(&stopped_rx).is_ok(),
        "stop returns"
    );
    assert_eq!(Arc::strong_count(&kept), 1, "the writer ended and joined");
}
