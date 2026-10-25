//! Tests for the session list's data: folding statuses, `session_left`,
//! `recent` pages, keys, glyphs and lines.

use super::{
    HomeScreen, Left, Level, ListLine, Row, Sessions, State, Subs, delete_line, dependents,
    from_status, glyph, line, line_with, opening, quit_count, recent_rows,
};
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

/// Session rows in the form test assertions read them.
#[cfg(test)]
pub(crate) fn home_test_rows(screen: &HomeScreen) -> Vec<(u64, String)> {
    screen
        .list
        .iter()
        .filter_map(|line| match line {
            ListLine::Live {
                key,
                glyph,
                name,
                wait,
                ..
            } => {
                let line = wait.as_ref().filter(|wait| !wait.is_empty()).map_or_else(
                    || format!("{glyph}  {name}"),
                    |wait| format!("{glyph}  {name}  {wait}"),
                );
                Some((*key, line))
            }
            ListLine::Exited {
                key, glyph, name, ..
            } => Some((*key, format!("{glyph}  {name}"))),
            ListLine::Head { .. } | ListLine::ExitedHead { .. } | ListLine::More { .. } => None,
        })
        .collect()
}

const PROJECT: &str = "-Users-a-work-fiber-.git";
const OTHER: &str = "-Users-a-work-lens-.git";

/// A `session_status` payload with `state` beside the base keys.
fn payload(state: Value) -> Value {
    let mut payload = json!({
        "name": "fix the parser",
        "workspace": "/Users/a/work/fiber",
        "project": PROJECT,
        "started": 0, "calls": 0, "since": 0,
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
        .all()
        .iter()
        .map(|row| line(row, Some(PROJECT)))
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

fn section_row(id: &str, project: &str, started: u64) -> Row {
    from_status(&envelope(
        id,
        json!({"state": "idle", "project": project,
        "workspace": format!("/work/{project}"), "started": started}),
    ))
}

#[test]
fn sections_put_the_launch_project_first_then_first_start_order() {
    let mut sessions = Sessions::default();
    for (id, project) in [("s_a", "other"), ("s_b", PROJECT), ("s_c", "third")] {
        sessions.status(section_row(id, project, 0));
    }
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(
        sections.iter().map(|s| s.project).collect::<Vec<_>>(),
        [PROJECT, "other", "third"]
    );
    assert_eq!(sections[1].name, "other");
    assert_eq!(sections[1].start, Some(sections[1].live[0].key));
    assert_eq!(
        sessions
            .sections(Some("absent"))
            .iter()
            .map(|s| s.project)
            .collect::<Vec<_>>(),
        ["other", PROJECT, "third"]
    );
    assert_eq!(sessions.all().len(), 3);
}

#[test]
fn an_exited_only_project_sorts_after_live_projects_by_earliest_started() {
    let mut sessions = Sessions::default();
    for (id, project, started) in [
        ("s_a", "late", 3),
        ("s_b", "early", 1),
        ("s_c", "live", 9),
        ("s_d", "tie", 3),
    ] {
        sessions.status(section_row(id, project, started));
    }
    for id in ["s_a", "s_b", "s_d"] {
        sessions.left(&SessionId(id.into()), Left::Exited);
    }
    let mut missing = section_row("s_e", "missing", 0);
    missing.status = None;
    sessions.status(missing);
    sessions.left(&SessionId("s_e".into()), Left::Exited);
    assert_eq!(
        sessions
            .sections(Some("absent"))
            .iter()
            .map(|s| s.project)
            .collect::<Vec<_>>(),
        ["live", "early", "late", "tie", "missing"]
    );
    assert_eq!(sessions.sections(Some("late"))[0].project, "late");
}

#[test]
fn a_crashed_feed_row_is_live_in_counts_and_spend() {
    let mut sessions = Sessions::default();
    for (id, spend) in [
        ("s_a", 1.35),
        ("s_b", 1.10),
        ("s_c", 0.05),
        ("s_d", 0.42),
        ("s_e", 1.0),
    ] {
        let mut row = section_row(id, PROJECT, 0);
        row.spend = spend;
        sessions.status(row);
    }
    sessions.left(&SessionId("s_c".into()), Left::Crashed);
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(sections[0].live.len(), 5);
    assert_eq!(
        crate::format::money(sections[0].live.iter().map(|r| r.spend).sum()),
        "$3.92"
    );
    assert!(sections[0].exited.is_empty());
}

#[test]
fn a_feed_row_that_left_exited_joins_the_exited_rows_newest_first() {
    let mut sessions = Sessions::default();
    for id in ["s_a", "s_b", "s_c"] {
        sessions.status(section_row(id, PROJECT, 0));
    }
    sessions.left(&SessionId("s_a".into()), Left::Exited);
    sessions.left(&SessionId("s_c".into()), Left::Exited);
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_d", "recent", "exited", None)])).0,
        true,
    );
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(
        sections[0]
            .live
            .iter()
            .map(|r| r.id.0.as_str())
            .collect::<Vec<_>>(),
        ["s_b"]
    );
    assert_eq!(
        sections[0]
            .exited
            .iter()
            .map(|r| r.id.0.as_str())
            .collect::<Vec<_>>(),
        ["s_c", "s_a", "s_d"]
    );
}

#[test]
fn exited_rows_cap_at_three_and_count_the_rest() {
    for count in [0, 1, 2, 3, 4] {
        let mut sessions = Sessions::default();
        sessions.status(section_row("s_live", PROJECT, 0));
        sessions.recent(
            recent_rows(&result(
                (0..count)
                    .map(|i| recent_row(&format!("s_{i}"), "recent", "exited", None))
                    .collect(),
            ))
            .0,
            true,
        );
        let sections = sessions.sections(Some(PROJECT));
        assert_eq!(sections[0].shown_exited, count.min(3));
        assert_eq!(sections[0].hidden(), count.saturating_sub(3));
    }
}

#[test]
fn expanding_a_project_shows_every_loaded_exited_row_and_only_that_project() {
    let mut sessions = Sessions::default();
    for project in [PROJECT, OTHER] {
        for i in 0..4 {
            let id = format!("s_{project}_{i}");
            sessions.status(section_row(&id, project, 0));
            sessions.left(&SessionId(id), Left::Exited);
        }
    }
    sessions.expand(PROJECT);
    sessions.expand(PROJECT);
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(sections[0].shown_exited, 4);
    assert_eq!(sections[0].hidden(), 0);
    assert_eq!(sections[1].shown_exited, 3);
    assert_eq!(sections[1].hidden(), 1);
    sessions.recent(Vec::new(), true);
    assert_eq!(sessions.sections(Some(PROJECT))[0].shown_exited, 4);
}

#[test]
fn an_unreadable_row_stays_in_its_stored_project() {
    let mut sessions = Sessions::default();
    let mut row = section_row("s_a", "stored", 0);
    row.state = State::Unreadable;
    row.status = None;
    row.workspace.clear();
    sessions.status(row);
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(sections[0].project, "stored");
    assert_eq!(sections[0].name, "stored");
    assert_eq!(sections[0].start, None);
}

#[test]
fn a_recent_row_keeps_its_last_status() {
    let rows = recent_rows(&result(vec![
        recent_row(
            "s_a",
            "present",
            "exited",
            Some(payload(json!({"state": "idle", "turns": 5, "since": 123}))),
        ),
        recent_row("s_b", "absent", "exited", None),
    ]))
    .0;
    let status = rows[0].status.as_ref().unwrap();
    assert_eq!(status.turns, Some(5));
    assert_eq!(status.since, 123);
    assert_eq!(rows[1].status, None);
}

#[test]
fn a_status_adds_a_row_and_a_second_updates_it_in_place() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
    )));
    assert_eq!(shown(&sessions), ["●  fix the parser"]);
    let key = sessions.all()[0].key;
    let mut renamed = payload(json!({"state": "streaming"}));
    renamed["name"] = json!("tidy docs");
    sessions.status(from_status(&Envelope {
        payload: renamed.as_object().cloned().unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}))
    }));
    assert_eq!(shown(&sessions), ["●  tidy docs"]);
    assert_eq!(sessions.all()[0].key, key);
}

#[test]
fn a_status_that_began_before_the_held_one_changes_nothing() {
    let mut sessions = Sessions::default();
    let id = "s_aaaaaaaaaaaaaaaa";
    let state_at =
        |state: &str, since: u64| json!({"state": state, "started": 0, "calls": 0, "since": since});
    sessions.status(from_status(&envelope(id, state_at("idle", 9))));
    // Just below the held `since`: a late copy, ignored.
    sessions.status(from_status(&envelope(id, state_at("streaming", 8))));
    assert_eq!(shown(&sessions), ["✓  fix the parser"]);
    // At the held `since`: the same status again, folded.
    sessions.status(from_status(&envelope(id, state_at("streaming", 9))));
    assert_eq!(shown(&sessions), ["●  fix the parser"]);
    // Above it: newer, folded.
    sessions.status(from_status(&envelope(id, state_at("idle", 10))));
    assert_eq!(shown(&sessions), ["✓  fix the parser"]);
}

#[test]
fn a_status_in_the_same_millisecond_loses_to_a_held_waiting_one() {
    let mut sessions = Sessions::default();
    let id = "s_aaaaaaaaaaaaaaaa";
    let waiting = json!({"state": "waiting", "started": 0, "calls": 0, "since": 9,
        "waiting": {"request_id": "r_1", "kind": "approval", "summary": "x"}});
    let tool = json!({"state": "tool", "started": 0, "calls": 0, "since": 9, "tool": "bash"});
    sessions.status(from_status(&envelope(id, tool.clone())));
    // Waiting replaces a tool that began in the same millisecond.
    sessions.status(from_status(&envelope(id, waiting.clone())));
    assert_eq!(shown(&sessions), ["!  fix the parser  approval: x"]);
    // The late copy of the tool does not replace the waiting.
    sessions.status(from_status(&envelope(id, tool)));
    assert_eq!(shown(&sessions), ["!  fix the parser  approval: x"]);
    // Another waiting status in that millisecond folds.
    sessions.status(from_status(&envelope(id, waiting)));
    assert_eq!(shown(&sessions), ["!  fix the parser  approval: x"]);
}

#[test]
fn a_new_status_goes_after_the_rows_already_there() {
    let mut sessions = Sessions::default();
    for session in ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"] {
        sessions.status(from_status(&envelope(session, json!({"state": "idle"}))));
    }
    assert_eq!(shown(&sessions), ["✓  fix the parser", "✓  fix the parser"]);
    let ids: Vec<String> = sessions.all().iter().map(|row| row.id.0.clone()).collect();
    assert_eq!(ids, ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"]);
}

#[test]
fn a_status_for_a_recent_row_moves_it_and_keeps_its_key() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ]))
        .0,
        true,
    );
    let key = sessions.all()[1].key;
    sessions.status(from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "streaming"}),
    )));
    // The resumed session moves into the feed rows, live again.
    assert_eq!(shown(&sessions), ["●  fix the parser", "○  first"]);
    let rows = sessions.all();
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
        ]))
        .0,
        true,
    );
    let keys: Vec<u64> = sessions.all().iter().map(|row| row.key).collect();
    // A first page listing the same ids again keeps their keys, drops the
    // missing row, and skips the id already in the feed.
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "crashed", None),
            recent_row("s_ffffffffffffffff", "live", "exited", None),
        ]))
        .0,
        true,
    );
    assert_eq!(
        shown(&sessions),
        ["●  fix the parser", "○  second", "✗  third"]
    );
    assert_eq!(sessions.all()[1].key, keys[2], "the kept row keeps its key",);
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
        ]))
        .0,
        false,
    );
    assert_eq!(shown(&sessions), ["●  fix the parser", "○  second"]);
    assert_eq!(sessions.all().len(), 2);
}

#[test]
fn remove_drops_only_the_named_row() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
        ]))
        .0,
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
    let ids: Vec<String> = sessions.all().iter().map(|row| row.id.0.clone()).collect();
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
    assert_eq!(line(&row, Some(PROJECT)), "●  fix the parser  $0.60");
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
        )]))
        .0,
        true,
    );
    let rows = sessions.all();
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
        )]))
        .0,
        true,
    );
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "crashed", None),
        ]))
        .0,
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
        )]))
        .0,
        true,
    );
    assert_eq!(shown(&sessions), ["○  old work  approval: shell · $0.41"]);
}

#[test]
fn a_recent_row_that_does_not_parse_is_skipped() {
    let rows = recent_rows(&result(vec![
        json!({"ts": 0, "name": "no session"}),
        recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
    ]))
    .0;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.0, "s_aaaaaaaaaaaaaaaa");
    assert!(recent_rows(&json!({})).0.is_empty());
    assert!(recent_rows(&json!({"sessions": {}})).0.is_empty());
}

#[test]
fn keys_are_never_reused_after_a_row_drops() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_bbbbbbbbbbbbbbbb", "second", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "exited", None),
        ]))
        .0,
        true,
    );
    let keys: Vec<u64> = sessions.all().iter().map(|row| row.key).collect();
    assert_eq!(keys.len(), 3);
    assert!(keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2]);
    // A first page that drops a row, then a status for that id: a new key,
    // larger than every earlier one.
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_aaaaaaaaaaaaaaaa", "first", "exited", None),
            recent_row("s_cccccccccccccccc", "third", "exited", None),
        ]))
        .0,
        true,
    );
    sessions.status(from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "idle"}),
    )));
    let rows = sessions.all();
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
        assert!(line(&row, Some(PROJECT)).starts_with(glyph), "{row:?}");
    }
    let mut live = from_status(&envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"})));
    live.left = Some(Left::Exited);
    assert!(line(&live, Some(PROJECT)).starts_with("○"));
    live.left = Some(Left::Crashed);
    assert!(line(&live, Some(PROJECT)).starts_with("✗"));
    let unreadable = from_status(&Envelope {
        payload: [("state".to_owned(), json!("idle"))].into_iter().collect(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    });
    assert!(line(&unreadable, Some(PROJECT)).starts_with("?"));
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
        line(&approval, Some(PROJECT)),
        "!  fix the parser  approval: shell cargo publish --dry-run · $0.41"
    );
    let question = from_status(&envelope(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "waiting", "waiting": {"request_id": "q_1",
            "kind": "question", "summary": "2 of 3 answered"}}),
    ));
    assert_eq!(
        line(&question, Some(PROJECT)),
        "!  fix the parser  question: 2 of 3 answered"
    );
    let offer = from_status(&envelope(
        "s_cccccccccccccccc",
        json!({"state": "waiting", "waiting": {"request_id": "o_1",
            "kind": "offer", "summary": "switch to plan mode"}}),
    ));
    assert_eq!(
        line(&offer, Some(PROJECT)),
        "!  fix the parser  offer: switch to plan mode"
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
    assert_eq!(line(&row, Some(PROJECT)), "✓  s_aaaaaaaaaaaaaaaa");
}

#[test]
fn a_note_replaces_the_detail() {
    let row = Row {
        key: 0,
        id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        name: "fix the parser".to_owned(),
        workspace: "/Users/a/work/fiber".to_owned(),
        project: PROJECT.to_owned(),
        git: false,
        state: State::Waiting,
        left: None,
        waiting: Some("approval: shell".to_owned()),
        spend: 0.41,
        jobs: 0,
        delegates: 0,
        clients: 0,
        note: Some("held by another process".to_owned()),
        status: None,
    };
    assert_eq!(
        line(&row, Some(PROJECT)),
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
        git: false,
        state: State::Idle,
        left: None,
        waiting: None,
        spend: 0.0,
        jobs: 0,
        delegates: 0,
        clients: 0,
        note: None,
        status: None,
    }
}

#[test]
fn control_characters_in_a_name_draw_as_spaces_in_the_row_line() {
    // Every control character draws as a space: the printable chars
    // around them stay, so flipping the `is_control` predicate fails.
    let row = named("a\nb\tc\u{1b}d\u{07}e");
    assert_eq!(line(&row, Some(PROJECT)), "✓  a b c d e");
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
         It cannot be undone · Enter delete · Esc keep"
    );
    assert_eq!(
        row.name, "a\nb\tc\u{1b}d\u{07}e",
        "the stored name keeps its characters",
    );
}

#[test]
fn a_name_without_control_characters_draws_unchanged() {
    let row = named("fix the parser");
    assert_eq!(line(&row, Some(PROJECT)), "✓  fix the parser");
    assert_eq!(
        delete_line(&row),
        "Delete fix the parser (s_aaaaaaaaaaaaaaaa)? \
         It cannot be undone · Enter delete · Esc keep"
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
    assert_eq!(opening(None).to_vec(), vec![Level::Summary]);
    assert_eq!(opening(Some(Level::Summary)).to_vec(), vec![]);
    assert_eq!(opening(Some(Level::Full)).to_vec(), vec![Level::Summary]);
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
fn quit_count_names_working_and_elsewhere() {
    assert_eq!(quit_count(2, 0), "2 sessions working");
    assert_eq!(quit_count(1, 0), "1 session working");
    assert_eq!(quit_count(1, 1), "1 session working, 1 also open elsewhere");
    assert_eq!(
        quit_count(3, 2),
        "3 sessions working, 2 also open elsewhere"
    );
}

/// A `session_status` payload with `state` in git on `main`.
fn git_payload(state: Value) -> Value {
    let mut payload = payload(state);
    payload["git"] = json!({"branch": "main"});
    payload
}

#[test]
fn a_feed_row_is_in_git_when_its_status_carries_git() {
    // Both sides of the `is_some`: a status with git reads true, one
    // without reads false.
    let row = from_status(&Envelope {
        payload: git_payload(json!({"state": "idle"}))
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "idle"}))
    });
    assert!(row.git);
    let plain = from_status(&envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "idle"})));
    assert!(!plain.git);
}

#[test]
fn a_recent_row_is_in_git_when_its_last_status_is() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row(
                "s_aaaaaaaaaaaaaaaa",
                "git work",
                "exited",
                Some(git_payload(json!({"state": "idle"}))),
            ),
            recent_row("s_bbbbbbbbbbbbbbbb", "plain work", "exited", None),
        ]))
        .0,
        true,
    );
    let rows = sessions.all();
    assert!(rows[0].git);
    assert!(!rows[1].git);
}

#[test]
fn an_unreadable_row_is_never_in_git() {
    let mut newer = envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}));
    newer.schema_version = contract::SCHEMA_VERSION + 1;
    assert!(!from_status(&newer).git);
}

#[test]
fn in_git_names_a_workspace_with_a_git_row() {
    let mut sessions = Sessions::default();
    sessions.status(from_status(&Envelope {
        payload: git_payload(json!({"state": "streaming"}))
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ..envelope("s_aaaaaaaaaaaaaaaa", json!({"state": "streaming"}))
    }));
    let mut away = payload(json!({"state": "idle"}));
    away["workspace"] = json!("/Users/a/work/lens");
    sessions.status(from_status(&Envelope {
        payload: away.as_object().cloned().unwrap_or_default(),
        ..envelope("s_bbbbbbbbbbbbbbbb", json!({"state": "idle"}))
    }));
    // The git row's workspace reads true; a workspace whose row has no
    // git, and one with no row at all, read false.
    assert!(sessions.in_git("/Users/a/work/fiber"));
    assert!(!sessions.in_git("/Users/a/work/lens"));
    assert!(!sessions.in_git("/Users/a/work/nowhere"));
}

#[test]
fn line_with_draws_the_glyph_it_is_given() {
    let row = from_status(&envelope(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
    ));
    assert!(
        line_with(&row, Some(PROJECT), "⠋").starts_with("⠋"),
        "{row:?}"
    );
    assert!(
        line_with(&row, Some(PROJECT), "●").starts_with("●"),
        "{row:?}"
    );
}

#[test]
fn line_is_line_with_the_still_glyph() {
    for state in [
        json!({"state": "streaming"}),
        json!({"state": "retrying"}),
        json!({"state": "jobs"}),
        json!({"state": "idle"}),
    ] {
        let row = from_status(&envelope("s_aaaaaaaaaaaaaaaa", state));
        assert_eq!(
            line(&row, Some(PROJECT)),
            line_with(&row, Some(PROJECT), glyph(&row))
        );
    }
}

/// One exited `recent` row for `session` in `project`.
fn project_row(session: &str, project: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": project,
        "workspace": format!("/work/{project}"),
        "name": session,
        "how": "exited",
    })
}

/// The ids of every loaded row, feed first.
fn ids(sessions: &Sessions) -> Vec<&str> {
    sessions.all().iter().map(|row| row.id.0.as_str()).collect()
}

#[test]
fn recent_rows_returns_the_answer_total_when_it_is_an_integer() {
    let (rows, total) = recent_rows(&json!({"sessions": [], "total": 7}));
    assert!(rows.is_empty());
    assert_eq!(total, Some(7));
}

#[test]
fn recent_rows_leaves_the_total_empty_without_an_integer() {
    for result in [
        json!({"sessions": []}),
        json!({"sessions": [], "total": "7"}),
        json!({"sessions": [], "total": 7.5}),
        json!({"sessions": [], "total": -1}),
        json!({"sessions": [], "total": true}),
        json!({"sessions": [], "total": json!({"n": 7})}),
        json!({"sessions": [], "total": null}),
    ] {
        assert_eq!(recent_rows(&result).1, None, "{result}");
    }
    // Sessions may be missing while the total is present: the rows are
    // empty and the total still parses.
    assert_eq!(recent_rows(&json!({"total": 7})), (Vec::new(), Some(7)));
}

#[test]
fn a_scoped_page_appends_without_touching_other_projects_rows() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_u", "unscoped", "exited", None)])).0,
        true,
    );
    let (rows, total) = recent_rows(&json!({"sessions": [project_row("s_p", OTHER)], "total": 4}));
    assert_eq!(total, Some(4));
    sessions.scoped(OTHER, rows);
    sessions.note_total(OTHER, total.unwrap_or(0));
    assert_eq!(ids(&sessions), ["s_u", "s_p"]);
    assert_eq!(sessions.total_for(OTHER), Some(4));
    assert_eq!(sessions.total_for(PROJECT), None);
    let sections = sessions.sections(Some(PROJECT));
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].total, None);
    assert_eq!(sections[1].total, Some(4));
}

#[test]
fn a_scoped_page_skips_feed_and_earlier_scoped_duplicates() {
    let mut sessions = Sessions::default();
    sessions.status(section_row("s_feed", PROJECT, 0));
    let (first, _) = recent_rows(&json!({"sessions": [project_row("s_feed", OTHER)]}));
    sessions.scoped(OTHER, first);
    assert_eq!(ids(&sessions), ["s_feed"]);
    let (page, _) = recent_rows(&json!({
        "sessions": [project_row("s_feed", OTHER), project_row("s_new", OTHER)],
    }));
    sessions.scoped(OTHER, page);
    assert_eq!(ids(&sessions), ["s_feed", "s_new"]);
    let (again, _) = recent_rows(&json!({"sessions": [project_row("s_new", OTHER)]}));
    sessions.scoped(OTHER, again);
    assert_eq!(ids(&sessions), ["s_feed", "s_new"]);
}

#[test]
fn out_of_order_scoped_pages_keep_both_projects_rows() {
    let mut sessions = Sessions::default();
    let (other_first, _) = recent_rows(&json!({"sessions": [project_row("s_o1", OTHER)]}));
    sessions.scoped(OTHER, other_first);
    let (own, _) = recent_rows(&json!({"sessions": [project_row("s_p1", PROJECT)]}));
    sessions.scoped(PROJECT, own);
    sessions.note_total(PROJECT, 2);
    sessions.note_total(OTHER, 3);
    let (other_second, _) = recent_rows(&json!({
        "sessions": [project_row("s_o1", OTHER), project_row("s_o2", OTHER)],
    }));
    sessions.scoped(OTHER, other_second);
    assert_eq!(ids(&sessions), ["s_o1", "s_p1", "s_o2"]);
    assert_eq!(sessions.total_for(PROJECT), Some(2));
    assert_eq!(sessions.total_for(OTHER), Some(3));
}

#[test]
fn scoped_folds_leave_the_unscoped_cursor_and_paging_alone() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![
            recent_row("s_a", "a", "exited", None),
            recent_row("s_b", "b", "exited", None),
        ]))
        .0,
        true,
    );
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_c", "c", "exited", None)])).0,
        false,
    );
    assert!(sessions.more());
    assert_eq!(
        sessions.last_recent().map(|row| row.id.0.as_str()),
        Some("s_c")
    );
    let (scoped, _) = recent_rows(&json!({"sessions": [project_row("s_p", OTHER)]}));
    sessions.scoped(OTHER, scoped);
    sessions.note_total(OTHER, 9);
    assert!(sessions.more());
    assert_eq!(
        sessions.last_recent().map(|row| row.id.0.as_str()),
        Some("s_c")
    );
    assert_eq!(sessions.total_for(OTHER), Some(9));
    sessions.recent(Vec::new(), false);
    assert!(!sessions.more());
    assert_eq!(
        sessions.last_recent().map(|row| row.id.0.as_str()),
        Some("s_c")
    );
}

#[test]
fn the_unscoped_cursor_follows_unscoped_pages_only() {
    let mut sessions = Sessions::default();
    assert_eq!(sessions.last_recent(), None);
    sessions.recent(Vec::new(), true);
    assert_eq!(sessions.last_recent(), None);
    assert!(!sessions.more());
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_a", "a", "exited", None)])).0,
        true,
    );
    assert_eq!(
        sessions.last_recent().map(|row| row.id.0.as_str()),
        Some("s_a")
    );
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_b", "b", "exited", None)])).0,
        false,
    );
    assert_eq!(
        sessions.last_recent().map(|row| row.id.0.as_str()),
        Some("s_b")
    );
}

#[test]
fn totals_drop_together_while_rows_and_paging_stay() {
    let mut sessions = Sessions::default();
    sessions.recent(
        recent_rows(&result(vec![recent_row("s_a", "a", "exited", None)])).0,
        true,
    );
    sessions.note_total(PROJECT, 5);
    sessions.note_total(OTHER, 2);
    sessions.mark_dirty(PROJECT);
    sessions.drop_all_totals();
    assert_eq!(sessions.total_for(PROJECT), None);
    assert_eq!(sessions.total_for(OTHER), None);
    assert!(!sessions.is_dirty(PROJECT));
    assert_eq!(ids(&sessions), ["s_a"]);
    assert!(sessions.more());
}

#[test]
fn dirty_marks_clear_one_project_at_a_time() {
    let mut sessions = Sessions::default();
    assert!(!sessions.is_dirty(PROJECT));
    sessions.mark_dirty(PROJECT);
    assert!(sessions.is_dirty(PROJECT));
    assert!(!sessions.is_dirty(OTHER));
    sessions.mark_dirty(PROJECT);
    assert!(sessions.is_dirty(PROJECT));
    sessions.clear_dirty(OTHER);
    assert!(sessions.is_dirty(PROJECT));
    sessions.clear_dirty(PROJECT);
    assert!(!sessions.is_dirty(PROJECT));
}

#[test]
fn status_left_and_remove_leave_every_stored_total_untouched() {
    let mut sessions = Sessions::default();
    sessions.status(section_row("s_a", PROJECT, 0));
    sessions.note_total(PROJECT, 5);
    sessions.status(section_row("s_b", OTHER, 1));
    sessions.note_total(OTHER, 3);
    sessions.status(section_row("s_a", PROJECT, 2));
    sessions.left(&SessionId("s_a".into()), Left::Exited);
    sessions.left(&SessionId("s_unknown".into()), Left::Crashed);
    sessions.remove(&SessionId("s_b".into()));
    sessions.remove(&SessionId("s_unknown".into()));
    assert_eq!(sessions.total_for(PROJECT), Some(5));
    assert_eq!(sessions.total_for(OTHER), Some(3));
    // A rail dismiss drops the card through the same `remove`, with the
    // total held for the next scoped answer.
    sessions.remove(&SessionId("s_a".into()));
    assert_eq!(sessions.total_for(PROJECT), Some(5));
}
