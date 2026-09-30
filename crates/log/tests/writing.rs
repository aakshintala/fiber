//! Writing a session: the directory, `seq`, fsyncs and the torn tail
//! (`docs/events.md`, "Writing" and "The session directory"), asserted on the
//! files left behind.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::*;
use contract::events::Event;
use contract::{ActionId, ErrorCode, TurnId};
use log::{Error, Log};
use serde_json::json;

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[test]
fn a_new_session_directory_holds_the_log_the_lock_and_artifacts_and_nothing_else() {
    let tmp = TestDir::new("layout");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    let mut entries: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    assert_eq!(entries, ["artifacts", "events.jsonl", "session.lock"]);
    assert!(dir.join("artifacts").is_dir());
}

#[test]
fn a_session_directory_is_created_once() {
    let tmp = TestDir::new("twice");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    drop(log);
    assert!(Log::create(tmp.path(), id("s_1")).is_err());
}

#[test]
fn sessions_live_under_the_project_key_in_fiber_home() {
    let dir = log::sessions_dir(Path::new("/h"), Path::new("/Users/alice/work/fiber/.git"));
    assert_eq!(
        dir,
        Path::new("/h/projects/-Users-alice-work-fiber-.git/sessions")
    );
}

#[test]
fn durable_lines_carry_contiguous_seq_and_ephemeral_lines_reach_no_file() {
    let tmp = TestDir::new("seq");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let first = log.append(&session_started(), None, None).unwrap();
    let delta = log.append(&delta("Hel"), None, None).unwrap();
    log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(first.seq.map(|s| s.0), Some(0));
    assert_eq!(delta.seq, None);
    assert_eq!(delta.kind, "assistant_message_delta");
    assert_eq!(
        kinds_and_seqs(&tmp.session(&id("s_1"))),
        [
            ("session_started".to_owned(), Some(0)),
            ("step_started".to_owned(), Some(1)),
        ]
    );
}

#[test]
fn a_line_carries_the_envelope_the_log_fills_in() {
    let tmp = TestDir::new("envelope");
    let log = Log::create(tmp.path(), id("s_9")).unwrap();
    let before = now_ms();
    let returned = log
        .append(
            &empty("assistant_message_started"),
            Some(TurnId("t_1".into())),
            Some(ActionId("a_1".into())),
        )
        .unwrap();
    let after = now_ms();
    let lines = lines(&tmp.session(&id("s_9")));
    let line = &lines[0];
    assert_eq!(line["session_id"], "s_9");
    assert_eq!(line["schema_version"], 1);
    assert_eq!(line["turn_id"], "t_1");
    assert_eq!(line["action_id"], "a_1");
    assert_eq!(line["payload"], serde_json::json!({}));
    let ts = line["ts"].as_u64().unwrap();
    assert!(before <= ts && ts <= after, "{before} <= {ts} <= {after}");
    // What append returns is the line, byte for byte.
    let file = fs::read_to_string(tmp.session(&id("s_9")).join("events.jsonl")).unwrap();
    assert_eq!(file, serde_json::to_string(&returned).unwrap() + "\n");
}

/// The fsyncs a log has made once each event is appended, counted as
/// `docs/performance.md`, "Measuring", counts them: at the call site.
fn fsyncs_after_each(log: &Log, events: &[Event]) -> Vec<(String, u64)> {
    let base = log.fsyncs();
    events
        .iter()
        .map(|e| {
            log.append(e, None, None).unwrap();
            (e.kind().to_owned(), log.fsyncs() - base)
        })
        .collect()
}

fn owned(rows: &[(&str, u64)]) -> Vec<(String, u64)> {
    rows.iter().map(|(k, n)| ((*k).to_owned(), *n)).collect()
}

#[test]
fn a_quiet_text_turn_costs_two_fsyncs_bracketing_the_model_request() {
    let tmp = TestDir::new("fsync-text");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let turn = [
        event("turn_started", json!({"input": []})),
        empty("step_started"),
        empty("assistant_message_started"),
        delta("hi"),
        message_completed(),
        event("turn_completed", json!({"outcome": "completed"})),
    ];
    // The count has risen when `assistant_message_started` returns, so the
    // line is on disk before the request is sent; `step_started` rides on it.
    assert_eq!(
        fsyncs_after_each(&log, &turn),
        owned(&[
            ("turn_started", 0),
            ("step_started", 0),
            ("assistant_message_started", 1),
            ("assistant_message_delta", 1),
            ("assistant_message_completed", 2),
            ("turn_completed", 2),
        ])
    );
}

#[test]
fn a_tool_call_costs_two_fsyncs_the_first_before_it_runs() {
    let tmp = TestDir::new("fsync-tool");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let call = [
        event(
            "tool_call_requested",
            json!({"name": "shell", "arguments": {}}),
        ),
        tool_call_started(),
        event("tool_call_delta", json!({"text": "out"})),
        tool_call_completed(),
    ];
    assert_eq!(
        fsyncs_after_each(&log, &call),
        owned(&[
            ("tool_call_requested", 0),
            ("tool_call_started", 1),
            ("tool_call_delta", 1),
            ("tool_call_completed", 2),
        ])
    );
}

#[test]
fn creating_a_session_fsyncs_its_directory_entries() {
    let tmp = TestDir::new("fsync-create");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    // The session directory's entry in `sessions/`, and the log's, lock's
    // and `artifacts/`'s entries in the session directory.
    assert_eq!(log.fsyncs(), 2);
}

#[test]
fn a_second_writer_refuses_and_names_the_holder() {
    let tmp = TestDir::new("lock");
    let _first = Log::create(tmp.path(), id("s_1")).unwrap();
    let Err(err) = Log::open(tmp.path(), id("s_1")) else {
        panic!("a second writer opened the session");
    };
    let holder = format!("process {}", std::process::id());
    assert!(
        matches!(&err, Error::Held { holder: h, .. } if *h == holder),
        "{err:?}"
    );
    assert_eq!(err.code(), Some(ErrorCode::SessionHeld));
    assert_eq!(
        err.to_string(),
        format!("session s_1 is held by {holder}; only one Fiber process may write a session")
    );
    let lock = fs::read_to_string(tmp.session(&id("s_1")).join("session.lock")).unwrap();
    assert_eq!(lock.trim(), std::process::id().to_string());
}

#[test]
fn the_lock_is_released_when_its_writer_is_dropped() {
    let tmp = TestDir::new("unlock");
    let first = Log::create(tmp.path(), id("s_1")).unwrap();
    first.append(&session_started(), None, None).unwrap();
    drop(first);
    let second = Log::open(tmp.path(), id("s_1")).unwrap();
    let line = second.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(line.seq.map(|s| s.0), Some(1));
}

#[test]
fn opening_a_missing_session_names_it() {
    let tmp = TestDir::new("missing");
    let Err(err) = Log::open(tmp.path(), id("s_none")) else {
        panic!("opened a session that does not exist");
    };
    assert_eq!(err.code(), Some(ErrorCode::SessionNotFound));
    assert!(err.to_string().contains("s_none"), "{err}");
    assert!(!tmp.session(&id("s_none")).exists());
}

#[test]
fn a_reopened_session_continues_seq_where_it_left_off() {
    let tmp = TestDir::new("reopen");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    for _ in 0..3 {
        log.append(&empty("step_started"), None, None).unwrap();
    }
    drop(log);
    let log = Log::open(tmp.path(), id("s_1")).unwrap();
    log.append(&empty("step_started"), None, None).unwrap();
    let seqs: Vec<_> = kinds_and_seqs(&tmp.session(&id("s_1")))
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert_eq!(seqs, [Some(0), Some(1), Some(2), Some(3)]);
}

#[test]
fn a_writer_truncates_a_torn_tail_before_appending() {
    let tmp = TestDir::new("torn-write");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    drop(log);
    let path = tmp.session(&id("s_1")).join("events.jsonl");
    let whole = fs::read(&path).unwrap();
    let mut torn = whole.clone();
    torn.extend_from_slice(br#"{"kind":"step_started","session_id":"s_1","ts":1,"sch"#);
    fs::write(&path, &torn).unwrap();

    let log = Log::open(tmp.path(), id("s_1")).unwrap();
    assert_eq!(fs::read(&path).unwrap(), whole, "the partial line is gone");
    log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(
        kinds_and_seqs(&tmp.session(&id("s_1"))),
        [
            ("session_started".to_owned(), Some(0)),
            ("step_started".to_owned(), Some(1)),
        ]
    );
}

#[test]
fn a_log_that_is_all_torn_tail_starts_again_at_seq_zero() {
    let tmp = TestDir::new("torn-all");
    drop(Log::create(tmp.path(), id("s_1")).unwrap());
    let path = tmp.session(&id("s_1")).join("events.jsonl");
    fs::write(&path, b"{\"kind\":\"sess").unwrap();
    let log = Log::open(tmp.path(), id("s_1")).unwrap();
    let line = log.append(&session_started(), None, None).unwrap();
    assert_eq!(line.seq.map(|s| s.0), Some(0));
    assert_eq!(lines(&tmp.session(&id("s_1"))).len(), 1);
}
