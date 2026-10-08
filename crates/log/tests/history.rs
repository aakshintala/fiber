//! The history chain over logs written by `Log`: segments root first, the
//! fold point, and the last line.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use common::*;
use contract::Seq;
use contract::events::Event;
use serde_json::json;

fn turn_started() -> Event {
    event("turn_started", json!({"input": []}))
}

fn turn_completed() -> Event {
    event("turn_completed", json!({"outcome": "completed"}))
}

fn forked_started(from: &str, at: u64) -> Event {
    event(
        "session_started",
        json!({"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            "forked_from": {"session_id": from, "seq": at}}),
    )
}

#[test]
fn two_logs_written_by_log_chain_root_first() {
    let tmp = TestDir::new("history-chain");
    let clock = fakes::clock::FakeClock::new();
    let root = log::Log::create(tmp.path(), id("s_aaaaaaaaaaaaaaaa"), clock.clone()).unwrap();
    root.append(&session_started(), None, None).unwrap();
    root.append(&turn_started(), None, None).unwrap();
    root.append(&turn_completed(), None, None).unwrap();
    drop(root);
    let child = log::Log::create(tmp.path(), id("s_bbbbbbbbbbbbbbbb"), clock).unwrap();
    child
        .append(&forked_started("s_aaaaaaaaaaaaaaaa", 1), None, None)
        .unwrap();
    child.append(&turn_started(), None, None).unwrap();
    drop(child);

    let segments = log::history(&tmp.session(&id("s_bbbbbbbbbbbbbbbb"))).unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].session_id, id("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(segments[0].to, Some(Seq(1)));
    assert_eq!(segments[1].session_id, id("s_bbbbbbbbbbbbbbbb"));
    assert_eq!(segments[1].to, None);
    let root_kinds: Vec<String> = segments[0]
        .lines(0)
        .unwrap()
        .map(|line| line.unwrap().kind)
        .collect();
    assert_eq!(
        root_kinds,
        ["session_started", "turn_started", "turn_completed"]
            .into_iter()
            .take(2)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    );
    let folded = log::history_to(&tmp.session(&id("s_bbbbbbbbbbbbbbbb")), Seq(2)).unwrap();
    assert_eq!(folded[1].to, Some(Seq(2)));
}

#[test]
fn the_last_line_is_the_logs_last_complete_line() {
    let tmp = TestDir::new("history-last");
    let clock = fakes::clock::FakeClock::new();
    let log = log::Log::create(tmp.path(), id("s_aaaaaaaaaaaaaaaa"), clock).unwrap();
    log.append(&session_started(), None, None).unwrap();
    log.append(&turn_started(), None, None).unwrap();
    drop(log);
    let found = log::last_line(&tmp.session(&id("s_aaaaaaaaaaaaaaaa"))).unwrap();
    assert_eq!(found.kind, "turn_started");
    assert_eq!(found.seq.unwrap(), Seq(1));
}
