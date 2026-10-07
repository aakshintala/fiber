//! Tests for the session list's data: folding statuses, `session_left`,
//! `recent` pages, keys, glyphs and lines.

use super::{
    Left, Level, Row, Sessions, State, Subs, delete_line, dependents, from_status, line,
    opening, quit_line, recent_rows,
};
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

const PROJECT: &str = "-Users-a-work-fiber-.git";
const OTHER: &str = "-Users-a-work-lens-.git";

/// A `session_status` payload with `state` beside the base keys.
fn payload(state: Value) -> Value {
    let mut payload = json!({
        "name": "fix the parser",
        "workspace": "/Users/a/work/fiber",
        "project": PROJECT,
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
            "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
        "delegates": 0,
        "jobs": 0,
        "model": "test/model", "clients": 0,
    });
    let into = payload.as_object_mut().unwrap_or_else(|| panic!("object"));
    for (key, value) in state.as_object().unwrap_or_else(|| panic!("state object")) {
        into.insert(key.clone(), value.clone());
    }
    payload
}

/// A `session_status` envelope for `session` in `state`.
fn envelope(session: &str, state: Value) -> Envelope {
    Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload(state).as_object().cloned().unwrap_or_default(),
    }
}

/// The rows shown for `project`, unscoped.
fn shown(sessions: &Sessions) -> Vec<String> {
    sessions
        .shown(PROJECT, false)
        .iter()
        .map(|row| line(row, PROJECT))
        .collect()
}

/// A `recent` answer's `result` from `rows`.
fn result(rows: Vec<Value>) -> Value {
    json!({"sessions": rows})
}

/// One `recent` row: its session, name, how it ended, and its last status.
fn recent_row(session: &str, name: &str, how: &str, status: Option<Value>) -> Value {
    let mut row = json!({
        "session_id": session,
        "ts": 0,
        "project": PROJECT,
        "workspace": "/Users/a/work/fiber",
        "name": name,
        "how": how,
    });
    if let (Some(into), Some(status)) = (row.as_object_mut(), status) {
        into.insert("status".to_owned(), status);
    }
    row
}

#[test]
fn a_status_adds_a_row_and_a_second_updates_it_in_place() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
    )));
    assert_eq!(shown(&sessions), ["●  fix the parser"]);
    let key = sessions.shown(PROJECT, false)[0].key;
    let mut renamed = payload(json!({"state": "streaming"}));
    renamed["name"] = json!("tidy docs");
    sessions.status(from_status(&Envelope {
        payload: renamed.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}))
    }));
    assert_eq!(shown(&sessions), ["●  tidy docs"]);
    assert_eq!(sessions.shown(PROJECT, false)[0].key, key);
}

#[test]
fn a_new_status_goes_after_the_rows_already_there() {
    let mut sessions = Sessions::default();
    for session in ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"] {
        sessions.status(from_status(&envelope(session, json!({"state": "idle"}))));
    }
    assert_eq!(shown(&sessions), ["✓  fix the parser", "✓  fix the parser"]);
    let ids: Vec<String> = sessions
        .shown(PROJECT, false)
        .iter()
        .map(|row| row.id.0.clone())
        .collect();
    assert_eq!(ids, ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"]);
}

#[test]
fn a_status_for_a_recent_row_moves_it_and_keeps_its_key() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ])),
        true,
    );
    let key = sessions.shown(PROJECT, false)[1].key;
    sessions.status(from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "streaming"}),
    )));
    // The resumed session moves into the feed rows, live again.
    assert_eq!(shown(&sessions), ["●  fix the parser", "○  first"]);
    let rows = sessions.shown(PROJECT, false);
    assert_eq!(rows[0].id.0, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(rows[0].key, key);
    assert_eq!(rows[0].left, None);
}

#[test]
fn a_later_status_with_a_new_name_renames_the_row() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
    )));
    let mut renamed = payload(json!({"state": "idle"}));
    renamed["name"] = json!("tidy docs");
    sessions.status(from_status(&Envelope {
        payload: renamed.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    }));
    assert_eq!(shown(&sessions), ["✓  tidy docs"]);
}

#[test]
fn a_crashed_left_marks_the_row_in_place() {
    let mut sessions = Sessions::default();
    for session in ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"] {
        sessions.status(from_status(&envelope(session, json!({"state": "idle"}))));
    }
    sessions.left(&SessionId("s_aaaaaaaaaaaaaaaa".to_owned()), Left::Crashed);
    assert_eq!(shown(&sessions), ["✗  fix the parser", "✓  fix the parser"]);
}

#[test]
fn an_exited_left_marks_the_row_in_place() {
    let mut sessions = Sessions::default();
    for session in ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"] {
        sessions.status(from_status(&envelope(session, json!({"state": "idle"}))));
    }
    sessions.left(&SessionId("s_bbbbbbbbbbbbbbbb".to_owned()), Left::Exited);
    assert_eq!(shown(&sessions), ["✓  fix the parser", "○  fix the parser"]);
}

#[test]
fn a_left_for_an_unknown_session_changes_nothing() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
    )));
    sessions.left(&SessionId("s_bbbbbbbbbbbbbbbb".to_owned()), Left::Exited);
    assert_eq!(shown(&sessions), ["✓  fix the parser"]);
}

#[test]
fn the_first_recent_page_replaces_skips_feed_rows_and_keeps_keys() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_ffffffffffffffff",
        json!({"state": "streaming"}),
    )));
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ])),
        true,
    );
    let keys: Vec<u64> = sessions
        .shown(PROJECT, false)
        .iter()
        .map(|row| row.key)
        .collect();
    // A first page listing the same ids again keeps their keys, drops the
    // missing row, and skips the id already in the feed.
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "crashed", None),
            recent_row("s_ffffffffffffffff", "live", "exited", None),
        ])),
        true,
    );
    assert_eq!(
        shown(&sessions),
        ["●  fix the parser", "○  second", "✗  third"]
    );
    assert_eq!(
        sessions.shown(PROJECT, false)[1].key,
        keys[2],
        "the kept row keeps its key",
    );
}

#[test]
fn an_older_page_skips_ids_already_in_the_feed() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
    )));
    // The older page names the live id again beside a new one: only
    // the new one appends.
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "live", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ])),
        false,
    );
    assert_eq!(shown(&sessions), ["●  fix the parser", "○  second"]);
    assert_eq!(sessions.shown(PROJECT, false).len(), 2);
}

#[test]
fn remove_drops_only_the_named_row() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ])),
        true,
    );
    sessions.remove(&SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert_eq!(shown(&sessions), ["○  second"]);
}

#[test]
fn removing_a_feed_row_keeps_the_others_in_order() {
    let mut sessions = Sessions::default();
    for session in [
        "s_aaaaaaaaaaaaaaaa",
        "s_bbbbbbbbbbbbbbbb",
        "s_cccccccccccccccc",
    ] {
        sessions.status(from_status(&envelope(session, json!({"state": "idle"}))));
    }
    sessions.remove(&SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    let ids: Vec<String> = sessions
        .shown(PROJECT, false)
        .iter()
        .map(|row| row.id.0.clone())
        .collect();
    assert_eq!(ids, ["s_aaaaaaaaaaaaaaaa", "s_cccccccccccccccc"]);
}

#[test]
fn spend_adds_cost_and_subscription_cost() {
    let mut spending = payload(json!({"state": "streaming"}));
    spending["spend"] = json!({"tokens": {"input": 1, "cache_read": 0,
        "cache_write": {}, "output": 2}, "cost": 0.41, "subscription_cost": 0.19});
    let row = from_status(&Envelope {
        payload: spending.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}))
    });
    assert!((row.spend - 0.60).abs() < 1e-9);
    assert_eq!(line(&row, PROJECT), "●  fix the parser  $0.60");
}

#[test]
fn a_recent_row_adds_cost_and_subscription_cost() {
    let mut status = payload(json!({"state": "idle"}));
    status["spend"] = json!({"tokens": {"input": 1, "cache_read": 0,
        "cache_write": {}, "output": 2}, "cost": 0.41, "subscription_cost": 0.19});
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![recent_row(
            "s_aaaaaaaaaaaaaaaa",
            "old work",
            "exited",
            Some(status),
        )])),
        true,
    );
    let rows = sessions.shown(PROJECT, false);
    assert!((rows[0].spend - 0.60).abs() < 1e-9);
    assert_eq!(shown(&sessions), ["○  old work  $0.60"]);
}

#[test]
fn an_older_recent_page_appends_and_skips_rows_already_listed() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![recent_row(
            "s_aaaaaaaaaaaaaaaa",
            "first",
            "exited",
            None,
        )])),
        true,
    );
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "crashed", None),
        ])),
        false,
    );
    assert_eq!(shown(&sessions), ["○  first", "✗  second"]);
}

#[test]
fn a_recent_row_keeps_its_last_status_waiting_text_and_spend() {
    // A left row whose last status was waiting keeps its waiting text;
    // its glyph is ○.
    let mut waiting = payload(json!({"state": "waiting", "waiting": {
        "request_id": "r_1", "kind": "approval", "summary": "shell"}}));
    waiting["spend"] = json!({"tokens": {"input": 1, "cache_read": 0,
        "cache_write": {}, "output": 2}, "cost": 0.41, "subscription_cost": 0.0});
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![recent_row(
            "s_aaaaaaaaaaaaaaaa",
            "old work",
            "exited",
            Some(waiting),
        )])),
        true,
    );
    assert_eq!(shown(&sessions), ["○  old work  approval: shell · $0.41"]);
}

#[test]
fn a_recent_row_that_does_not_parse_is_skipped() {
    let rows = recent_rows(&result(vec![
        json!({"ts": 0, "name": "no session"}),
        recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
    ]));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.0, "s_aaaaaaaaaaaaaaaa");
    assert!(recent_rows(&json!({})).is_empty());
    assert!(recent_rows(&json!({"sessions": {}})).is_empty());
}

#[test]
fn keys_are_never_reused_after_a_row_drops() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "exited", None),
        ])),
        true,
    );
    let keys: Vec<u64> = sessions
        .shown(PROJECT, false)
        .iter()
        .map(|row| row.key)
        .collect();
    assert_eq!(keys.len(), 3);
    assert!(keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2]);
    // A first page that drops a row, then a status for that id: a new key,
    // larger than every earlier one.
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "exited", None),
        ])),
        true,
    );
    sessions.status(from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "idle"}),
    )));
    let rows = sessions.shown(PROJECT, false);
    let revived = rows
        .iter()
        .find(|row| row.id.0 == "s_bbbbbbbbbbbbbbbb")
        .unwrap_or_else(|| panic!("the revived row"));
    assert!(keys.iter().all(|key| *key != revived.key));
    assert!(keys.iter().all(|key| revived.key > *key));
}

#[test]
fn an_unreadable_status_is_a_cannot_attach_row() {
    let mut newer = envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}));
    newer.schema_version = contract::SCHEMA_VERSION + 1;
    assert_eq!(from_status(&newer).state, State::Unreadable);
    let broken = Envelope {
        payload: [("state".to_owned(), json!("streaming"))]
            .into_iter()
            .collect(),
        ..envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "streaming"}))
    };
    assert_eq!(from_status(&broken).state, State::Unreadable);
    let mut sessions = Sessions::default();
    sessions.status(from_status(&newer));
    sessions.status(from_status(&broken));
    assert_eq!(
        shown(&sessions),
        [
            "?  s_aaaaaaaaaaaaaaaa  cannot attach",
            "?  s_bbbbbbbbbbbbbbbb  cannot attach",
        ]
    );
}

#[test]
fn each_state_has_its_glyph() {
    let states = [
        (json!({"state": "streaming"}), "●"),
        (json!({"state": "tool", "tool": "shell"}), "●"),
        (json!({"state": "retrying"}), "●"),
        (
            json!({"state": "waiting", "waiting": {"request_id": "r_1",
                "kind": "approval", "summary": "shell"}}),
            "!",
        ),
        (json!({"state": "jobs"}), "●"),
        (json!({"state": "idle"}), "✓"),
    ];
    for (state, glyph) in states {
        let row = from_status(&envelope("s_aaaaaaaaaaaaaaaa", state));
        assert!(line(&row, PROJECT).starts_with(glyph), "{row:?}");
    }
    let mut live = from_status(&envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"})));
    live.left = Some(Left::Exited);
    assert!(line(&live, PROJECT).starts_with("○"));
    live.left = Some(Left::Crashed);
    assert!(line(&live, PROJECT).starts_with("✗"));
    let unreadable = from_status(&Envelope {
        payload: [("state".to_owned(), json!("idle"))].into_iter().collect(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    });
    assert!(line(&unreadable, PROJECT).starts_with("?"));
}

#[test]
fn the_line_shows_waiting_text_for_an_approval_and_a_question() {
    let mut spending = payload(json!({"state": "waiting", "waiting": {
        "request_id": "r_1", "kind": "approval",
        "summary": "shell cargo publish --dry-run"}}));
    spending["spend"] = json!({"tokens": {"input": 1, "cache_read": 0,
        "cache_write": {}, "output": 2}, "cost": 0.41, "subscription_cost": 0.0});
    let approval = from_status(&Envelope {
        payload: spending.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "waiting"}))
    });
    assert!(approval.waiting.is_some());
    assert_eq!(
        line(&approval, PROJECT),
        "!  fix the parser  approval: shell cargo publish --dry-run · $0.41"
    );
    let question = from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "waiting", "waiting": {"request_id": "q_1",
            "kind": "question", "summary": "2 of 3 answered"}}),
    ));
    assert_eq!(
        line(&question, PROJECT),
        "!  fix the parser  question: 2 of 3 answered"
    );
}

#[test]
fn spend_is_left_out_at_zero_and_shown_below_a_cent() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
    )));
    let mut drip = payload(json!({"state": "idle"}));
    drip["name"] = json!("tidy docs");
    drip["spend"] = json!({"tokens": {"input": 1, "cache_read": 0,
        "cache_write": {}, "output": 2}, "cost": 0.001, "subscription_cost": 0.0});
    sessions.status(from_status(&Envelope {
        payload: drip.as_object().cloned().unwrap_or_default(),
        ..envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "idle"}))
    }));
    assert_eq!(
        shown(&sessions),
        ["✓  fix the parser", "✓  tidy docs  <$0.01"]
    );
}

#[test]
fn the_segment_shows_only_outside_the_launch_project() {
    let mut here = payload(json!({"state": "idle"}));
    here["workspace"] = json!("/Users/a/work/fiber");
    let mut away = payload(json!({"state": "idle"}));
    away["name"] = json!("tidy docs");
    away["workspace"] = json!("/Users/a/work/lens");
    away["project"] = json!(OTHER);
    let mut sessions = Sessions::default();
    sessions.status(from_status(&Envelope {
        payload: here.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    }));
    sessions.status(from_status(&Envelope {
        payload: away.as_object().cloned().unwrap_or_default(),
        ..envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "idle"}))
    }));
    assert_eq!(
        shown(&sessions),
        ["✓  fix the parser", "✓  tidy docs  lens"]
    );
}

#[test]
fn an_empty_name_shows_the_id() {
    let mut payload = payload(json!({"state": "idle"}));
    payload["name"] = json!("");
    let row = from_status(&Envelope {
        payload: payload.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    });
    assert_eq!(line(&row, PROJECT), "✓  s_aaaaaaaaaaaaaaaa");
}

#[test]
fn a_note_replaces_the_detail() {
    let row = Row {
        key: 0,
        id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        name: "fix the parser".to_owned(),
        workspace: "/Users/a/work/fiber".to_owned(),
        project: PROJECT.to_owned(),
        state: State::Waiting,
        left: None,
        waiting: Some("approval: shell".to_owned()),
        spend: 0.41,
        jobs: 0,
        delegates: 0,
        clients: 0,
        note: Some("held by another process".to_owned()),
    };
    assert_eq!(
        line(&row, PROJECT),
        "!  fix the parser  held by another process"
    );
}

/// A row named `name`, live and idle with no detail.
fn named(name: &str) -> Row {
    Row {
        key: 0,
        id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        name: name.to_owned(),
        workspace: "/Users/a/work/fiber".to_owned(),
        project: PROJECT.to_owned(),
        state: State::Idle,
        left: None,
        waiting: None,
        spend: 0.0,
        jobs: 0,
        delegates: 0,
        clients: 0,
        note: None,
    }
}

#[test]
fn control_characters_in_a_name_draw_as_spaces_in_the_row_line() {
    // Every control character draws as a space: the printable chars
    // around them stay, so flipping the `is_control` predicate fails.
    let row = named("a\nb\tc\u{1b}d\u{07}e");
    assert_eq!(line(&row, PROJECT), "✓  a b c d e");
    assert_eq!(
        row.name, "a\nb\tc\u{1b}d\u{07}e",
        "the stored name keeps its characters",
    );
}

#[test]
fn control_characters_in_a_name_draw_as_spaces_in_the_delete_question() {
    let row = named("a\nb\tc\u{1b}d\u{07}e");
    assert_eq!(
        delete_line(&row),
        "Delete a b c d e (s_aaaaaaaaaaaaaaaa)? \
         It cannot be undone · enter delete · esc keep"
    );
    assert_eq!(
        row.name, "a\nb\tc\u{1b}d\u{07}e",
        "the stored name keeps its characters",
    );
}

#[test]
fn a_name_without_control_characters_draws_unchanged() {
    let row = named("fix the parser");
    assert_eq!(line(&row, PROJECT), "✓  fix the parser");
    assert_eq!(
        delete_line(&row),
        "Delete fix the parser (s_aaaaaaaaaaaaaaaa)? \
         It cannot be undone · enter delete · esc keep"
    );
}

/// A session id for the subscription tests.
fn session() -> SessionId {
    SessionId("s_aaaaaaaaaaaaaaaa".to_owned())
}

#[test]
fn expected_is_the_last_in_flight_level_then_the_accepted_one() {
    let mut subs = Subs::default();
    assert_eq!(subs.expected(&session()), None);
    assert!(!subs.pending(&session()));
    subs.sent("c_1".to_owned(), session(), Level::Full);
    assert_eq!(subs.expected(&session()), Some(Level::Full));
    assert!(subs.pending(&session()));
    subs.sent("c_2".to_owned(), session(), Level::Summary);
    assert_eq!(subs.expected(&session()), Some(Level::Summary));
    subs.answered("c_2", true);
    assert_eq!(subs.expected(&session()), Some(Level::Full));
    assert!(!subs.full(&session()));
    subs.answered("c_1", true);
    assert_eq!(subs.expected(&session()), Some(Level::Full));
    assert!(subs.full(&session()));
    assert!(!subs.pending(&session()));
}

#[test]
fn a_rejection_leaves_the_accepted_level() {
    let mut subs = Subs::default();
    subs.sent("c_1".to_owned(), session(), Level::Full);
    subs.answered("c_1", true);
    subs.sent("c_2".to_owned(), session(), Level::Summary);
    subs.answered("c_2", false);
    assert_eq!(subs.expected(&session()), Some(Level::Full));
    assert!(subs.full(&session()));
    assert!(!subs.pending(&session()));
}

#[test]
fn an_id_not_in_flight_is_not_ours() {
    let mut subs = Subs::default();
    assert_eq!(subs.answered("c_deadbeefdeadbeef", true), None);
    assert_eq!(subs.answered("c_deadbeefdeadbeef", false), None);
    subs.sent("c_1".to_owned(), session(), Level::Full);
    assert_eq!(
        subs.answered("c_1", true),
        Some(session()),
        "an answered id names its session",
    );
    assert_eq!(subs.answered("c_1", true), None);
}

#[test]
fn opening_from_each_level() {
    assert_eq!(opening(None).to_vec(), vec![Level::Full]);
    assert_eq!(opening(Some(Level::Summary)).to_vec(), vec![Level::Full]);
    assert_eq!(
        opening(Some(Level::Full)).to_vec(),
        vec![Level::Summary, Level::Full]
    );
}

#[test]
fn hidden_counts_waiting_rows_only() {
    let mut sessions = Sessions::default();
    let mut waiting = payload(json!({"state": "waiting", "waiting": {
        "request_id": "r_1", "kind": "approval", "summary": "shell"}}));
    waiting["project"] = json!(OTHER);
    sessions.status(from_status(&Envelope {
        payload: waiting.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "waiting"}))
    }));
    let mut idle = payload(json!({"state": "idle"}));
    idle["project"] = json!(OTHER);
    sessions.status(from_status(&Envelope {
        payload: idle.as_object().cloned().unwrap_or_default(),
        ..envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "idle"}))
    }));
    sessions.status(from_status(&envelope(
        "s_cccccccccccccccc",
        json!({"state": "idle"}),
    )));
    assert_eq!(sessions.hidden(PROJECT), (2, 1));
    assert_eq!(sessions.hidden(OTHER), (1, 0));
}

#[test]
fn toggle_line_names_waiting_or_scoping_back() {
    assert_eq!(
        super::toggle_line(2, false),
        "2 waiting in other projects · show all"
    );
    assert_eq!(
        super::toggle_line(0, false),
        "0 waiting in other projects · show all"
    );
    assert_eq!(super::toggle_line(0, true), "show this project only");
}

#[test]
fn dependents_reads_each_backticked_id_once_in_order() {
    use contract::SessionId;
    let id = |session: &str| SessionId(session.to_owned());
    assert!(dependents("no sessions here").is_empty());
    assert_eq!(
        dependents("Session `s_0123456789abcdef` is held."),
        vec![id("s_0123456789abcdef")]
    );
    assert_eq!(
        dependents(
            "Session `s_0123456789abcdef` has sessions that continue it: \
             `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too."
        ),
        vec![
            id("s_0123456789abcdef"),
            id("s_1111111111111111"),
            id("s_2222222222222222"),
        ]
    );
    // A repeat names its session once.
    assert_eq!(
        dependents(
            "Session `s_0123456789abcdef` has sessions that continue it: \
             `s_0123456789abcdef`, `s_1111111111111111`."
        ),
        vec![id("s_0123456789abcdef"), id("s_1111111111111111")]
    );
    // A malformed `s_` token, and one outside backticks, read nothing.
    assert!(dependents("Session `s_short` has sessions.").is_empty());
    assert!(dependents("Session `s_0123456789ABCDEF` has sessions.").is_empty());
    assert!(dependents("Session s_0123456789abcdef has sessions.").is_empty());
    // An empty pair, and a trailing id without its closing backtick,
    // name nothing.
    assert!(dependents("Session `` has sessions.").is_empty());
    assert!(dependents("Session `s_0123456789abcdef has sessions.").is_empty());
}

#[test]
fn quit_line_names_working_and_elsewhere() {
    assert_eq!(
        quit_line(2, 0),
        "2 sessions working · enter leave them running · c close all · esc stay"
    );
    assert_eq!(
        quit_line(1, 0),
        "1 session working · enter leave them running · c close all · esc stay"
    );
    assert_eq!(
        quit_line(1, 1),
        "1 session working, 1 also open elsewhere · enter leave them running · c close all · esc stay"
    );
    assert_eq!(
        quit_line(3, 2),
        "3 sessions working, 2 also open elsewhere · enter leave them running · c close all · esc stay"
    );
}
