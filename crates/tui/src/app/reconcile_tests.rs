//! Tests for the reconnect reconcile (`docs/tui.md`, "A dropped connection"):
//! after a reconnect the terminal sends `sessions` once and drops every live
//! row its answer omits, except one with a feed line after `sessions`.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Link};
use crate::home::Launch;
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        version: "0.0.1".to_owned(),
        logo_glyph: "⌇".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A hub answer of `kind` with `payload`.
fn hub(kind: &str, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A line from `session` of `kind` with `payload`.
fn from(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for `session`, named `name`.
fn live(session: &str, name: &str) -> Line {
    from(
        session,
        "session_status",
        json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// Parses command lines.
fn parsed(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Each line's `command`.
fn names(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["command"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Writes `lines` as the loop does once each write succeeds.
fn write(app: &mut App, lines: Vec<String>) -> Vec<String> {
    for line in &lines {
        app.wrote(line);
    }
    lines
}

/// Folds `line` and writes what it sends.
fn fold(app: &mut App, line: Line) -> Vec<String> {
    let lines = app.on_line(line);
    write(app, lines)
}

/// The first `hub_hello`: home's feed and first `recent` page, written.
fn linked(app: &mut App) -> Vec<Value> {
    let lines = parsed(&fold(app, hello()));
    assert_eq!(names(&lines), ["feed", "recent"]);
    lines
}

/// The connection drops and a new one says `hub_hello`: what goes out,
/// written.
fn reconnect(app: &mut App) -> Vec<String> {
    app.disconnected();
    app.next_retry();
    fold(app, hello())
}

/// The `sessions` id in `lines`, when exactly one is out.
fn sessions_id(lines: &[String]) -> String {
    let values = parsed(lines);
    let mut ids: Vec<String> = values
        .iter()
        .filter(|line| line["command"] == "sessions")
        .map(|line| line["id"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(ids.len(), 1, "one sessions: {values:?}");
    ids.pop().unwrap_or_default()
}

/// A `sessions` answer for `id` listing `running`.
fn sessions_answer(id: &str, running: &[&str]) -> Line {
    let live: Vec<Value> = running
        .iter()
        .map(|session| json!({"session_id": session}))
        .collect();
    hub(
        "command_accepted",
        json!({"command_id": id, "result": {"live": live, "exited": []}}),
    )
}

/// The rows home draws.
fn rows(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(_, line, _)| line).collect())
        .unwrap_or_default()
}

#[test]
fn a_live_row_missing_from_sessions_is_dropped() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    let out = reconnect(&mut app);
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    fold(&mut app, sessions_answer(&sessions_id(&out), &[]));
    assert!(rows(&app).is_empty());
}

#[test]
fn a_row_with_a_feed_line_after_sessions_is_kept() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    // A feed line after `sessions` was sent: the hub missed it, the feed
    // did not.
    fold(&mut app, live(S_B, "tidy docs"));
    fold(&mut app, sessions_answer(&sessions_id(&out), &[]));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
}

#[test]
fn a_row_listed_by_sessions_is_kept() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    fold(&mut app, sessions_answer(&sessions_id(&out), &[S_B]));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
}

#[test]
fn a_status_before_sessions_does_not_protect() {
    let mut app = home();
    linked(&mut app);
    // The only status arrived before `sessions` was sent, so nothing after
    // protects the row.
    fold(&mut app, live(S_B, "tidy docs"));
    fold(&mut app, live(S_A, "other work"));
    let out = reconnect(&mut app);
    fold(&mut app, sessions_answer(&sessions_id(&out), &[S_A]));
    assert_eq!(rows(&app), ["✓  other work"]);
}

#[test]
fn sessions_goes_out_once_per_reconnect_after_feed_and_not_first() {
    let mut app = home();
    // Not on the first connection.
    let first = linked(&mut app);
    assert_eq!(names(&first), ["feed", "recent"]);
    // Once per reconnect, after feed and recent.
    let one = parsed(&reconnect(&mut app));
    assert_eq!(names(&one), ["feed", "recent", "sessions"]);
    let two = parsed(&reconnect(&mut app));
    assert_eq!(names(&two), ["feed", "recent", "sessions"]);
    assert_ne!(two[2]["id"], one[2]["id"]);
    // No frame after sends another.
    let after = parsed(&fold(&mut app, live(S_B, "tidy docs")));
    assert!(!names(&after).contains(&"sessions".to_owned()));
}

#[test]
fn a_stale_sessions_answer_is_ignored() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let first = reconnect(&mut app);
    let stale = sessions_id(&first);
    let second = reconnect(&mut app);
    let current = sessions_id(&second);
    assert_ne!(stale, current);
    // Before a later drop, the old answer changes nothing.
    fold(&mut app, sessions_answer(&stale, &[]));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    // The current answer still drops.
    fold(&mut app, sessions_answer(&current, &[]));
    assert!(rows(&app).is_empty());
    // Answered twice, the same id changes nothing more.
    fold(&mut app, sessions_answer(&current, &[S_B]));
    assert!(rows(&app).is_empty());
}

#[test]
fn a_rejected_sessions_keeps_the_rows() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    let id = sessions_id(&out);
    fold(
        &mut app,
        hub(
            "command_rejected",
            json!({"command_id": id, "code": "internal", "message": "nope"}),
        ),
    );
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn a_rejected_sessions_ends_the_wait() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    let id = sessions_id(&out);
    fold(
        &mut app,
        hub(
            "command_rejected",
            json!({"command_id": id, "code": "internal", "message": "nope"}),
        ),
    );
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    // The wait is off, so a later answer with the same id deletes nothing.
    fold(&mut app, sessions_answer(&id, &[]));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
}

#[test]
fn a_rejection_with_another_id_leaves_the_wait() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    let id = sessions_id(&out);
    fold(
        &mut app,
        hub(
            "command_rejected",
            json!({"command_id": "c_other", "code": "internal", "message": "nope"}),
        ),
    );
    // The wait is still on, so the answer with its id drops the row.
    fold(&mut app, sessions_answer(&id, &[]));
    assert!(rows(&app).is_empty());
}

#[test]
fn a_sessions_answer_without_live_drops_everything() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    fold(
        &mut app,
        hub(
            "command_accepted",
            json!({"command_id": sessions_id(&out), "result": {}}),
        ),
    );
    assert!(rows(&app).is_empty());
}

#[test]
fn exited_rows_survive_sessions() {
    let mut app = home();
    let first = linked(&mut app);
    fold(
        &mut app,
        hub(
            "command_accepted",
            json!({"command_id": first[1]["id"], "result": {"sessions": [{
                "session_id": S_B, "ts": 0, "project": "-w", "workspace": "/w",
                "name": "old work", "how": "exited",
            }]}}),
        ),
    );
    assert_eq!(rows(&app), ["○  old work"]);
    assert_eq!(app.link, Link::Up);
    let out = reconnect(&mut app);
    fold(&mut app, sessions_answer(&sessions_id(&out), &[]));
    assert_eq!(rows(&app), ["○  old work"]);
}

#[test]
fn a_sessions_answer_after_a_drop_before_the_next_hello_drops_nothing() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let out = reconnect(&mut app);
    let old = sessions_id(&out);
    // A later disconnect before the answer: the wait is gone.
    app.disconnected();
    app.next_retry();
    fold(&mut app, sessions_answer(&old, &[]));
    assert_eq!(rows(&app), ["✓  tidy docs"]);
    // The next connection reconciles afresh.
    let again = fold(&mut app, hello());
    fold(&mut app, sessions_answer(&sessions_id(&again), &[]));
    assert!(rows(&app).is_empty());
}
