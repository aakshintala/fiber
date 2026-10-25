//! Quota queries are attachment-local and driven by input, never a timer.
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::app::items::testkit::{SESSION, commands, home, live, session_line};
use crate::app::{App, Link};
use crate::link::Line;

fn attached() -> App {
    let mut app = home();
    app.on_line(crate::app::items::testkit::hello());
    app.outbox.link = Link::Up;
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

fn queries(lines: Vec<String>) -> Vec<Value> {
    commands(lines)
        .into_iter()
        .filter(|v| v["command"] == "quota")
        .collect()
}

fn first(app: &mut App) -> String {
    let rows = queries(app.on_line(session_line("clients", json!({"clients": []}))));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], SESSION);
    rows[0]["id"].as_str().unwrap().to_owned()
}

fn answer(app: &mut App, id: &str) {
    assert!(
        queries(app.on_line(session_line(
            "command_accepted",
            json!({
                "command_id": id, "result": {"quota": [], "notice_at": 80.0}
            })
        )))
        .is_empty()
    );
}

#[test]
fn attach_and_focus_send_once_and_never_enter_pending() {
    let mut app = attached();
    let id = first(&mut app);
    assert!(!app.outbox.pending.contains_key(&id));
    assert!(app.quota_keypress().is_none());
    assert!(app.quota_focus().is_none());
    answer(&mut app, &id);
    assert!(app.quota_focus().is_some());
    assert!(app.quota_focus().is_none());
}

#[test]
fn keypress_uses_the_answer_age_at_the_five_minute_boundary() {
    for (millis, sends) in [(299_999, false), (300_000, true), (300_001, true)] {
        let mut app = attached();
        let clock = FakeClock::new();
        let now = clock.now();
        app.set_now(now, 0);
        let id = first(&mut app);
        answer(&mut app, &id);
        app.set_now(now + Duration::from_millis(millis), millis);
        assert_eq!(app.quota_keypress().is_some(), sends, "{millis}");
    }
}

#[test]
fn the_matching_answer_stores_rows_and_threshold_and_reattach_clears_them() {
    let mut app = attached();
    let id = first(&mut app);
    let quota = json!([{"provider": "codex", "credential": "work", "credit": {"remaining": 4.2}}]);
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id": id,
        "result": {"quota": quota, "notice_at": 70.0}}),
    ));
    assert_eq!(
        serde_json::to_value(&app.panel_state.quota.rows).unwrap(),
        quota
    );
    assert_eq!(app.panel_state.quota.notice_at, 70.0);
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(app.panel_state.quota.rows.is_empty());
}

#[test]
fn focus_sends_even_with_a_fresh_answer() {
    let mut app = attached();
    let clock = FakeClock::new();
    app.set_now(clock.now(), 0);
    let id = first(&mut app);
    answer(&mut app, &id);
    assert!(app.quota_keypress().is_none());
    assert!(app.quota_focus().is_some());
}

#[test]
fn an_answer_without_a_clock_never_makes_keypress_due() {
    let mut app = attached();
    let id = first(&mut app);
    answer(&mut app, &id);
    app.set_now(FakeClock::new().now() + Duration::from_secs(600), 600_000);
    assert!(app.quota_keypress().is_none());
    assert!(app.quota_focus().is_some());
}

#[test]
fn only_the_matching_answer_on_this_session_clears_the_flight() {
    let mut app = attached();
    let id = first(&mut app);
    answer(&mut app, "c_other");
    assert!(app.quota_focus().is_none());
    let mut other = session_line(
        "command_accepted",
        json!({
            "command_id": id, "result": {"quota": [], "notice_at": 80.0}
        }),
    );
    if let Line::Session(ref mut envelope) = other {
        envelope.session_id = contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    }
    app.on_line(other);
    assert!(app.quota_focus().is_none());
    answer(&mut app, &id);
    assert!(app.quota_focus().is_some());
}

#[test]
fn an_unrelated_rejection_does_not_refuse_the_quota_query() {
    let mut app = attached();
    let id = first(&mut app);
    app.on_line(session_line(
        "command_rejected",
        json!({"command_id": "c_other", "code": "unknown_command", "message": "other"}),
    ));
    assert!(app.quota_focus().is_none());
    answer(&mut app, &id);
    assert!(app.quota_focus().is_some());
}

#[test]
fn a_turn_end_shares_the_branch_edge_and_is_not_held_during_a_flight() {
    let mut app = attached();
    let lines = commands(app.on_line(live(SESSION, json!({"state": "idle"}))));
    // The introducing status queues its project's scoped ask beside the
    // turn's own lines.
    assert_eq!(
        lines
            .iter()
            .map(|v| v["command"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["shell", "quota", "recent"]
    );
    assert_eq!(lines[2]["args"]["project"], "-w");
    let branch = lines[0]["id"].as_str().unwrap();
    let quota = lines[1]["id"].as_str().unwrap();
    app.on_line(session_line("command_accepted", json!({"command_id": branch,
        "result": {"output": "main\n", "process": {"exit_code": 0, "signal": null, "timed_out": false}}})));
    answer(&mut app, quota);
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    let ended = commands(app.on_line(live(SESSION, json!({"state": "idle"}))));
    assert_eq!(
        ended
            .iter()
            .map(|v| v["command"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["shell", "quota"]
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    assert!(queries(app.on_line(live(SESSION, json!({"state": "idle"})))).is_empty());
    answer(&mut app, ended[1]["id"].as_str().unwrap());
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
}

#[test]
fn matching_rejections_mark_query_refused_until_reattach() {
    for hub in [false, true] {
        let mut app = attached();
        let id = first(&mut app);
        let payload =
            json!({"command_id": id, "code": "unknown_command", "message": "old session"});
        let line = if hub {
            Line::Hub(contract::HubLine {
                kind: "command_rejected".to_owned(),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                payload: payload.as_object().unwrap().clone(),
            })
        } else {
            session_line("command_rejected", payload)
        };
        assert!(queries(app.on_line(line)).is_empty());
        assert!(matches!(app.panel_state.quota.ask, super::Ask::Refused));
        assert!(app.quota_focus().is_none());
        app.on_line(live(SESSION, json!({"state": "streaming"})));
        assert!(queries(app.on_line(live(SESSION, json!({"state": "idle"})))).is_empty());
        app.attach(contract::SessionId(SESSION.to_owned()));
        first(&mut app);
    }
}

#[test]
fn a_left_row_sends_nothing_and_reattach_discards_a_lost_answer() {
    let mut app = home();
    crate::app::items::testkit::opened(&mut app);
    let super::Ask::Asked(old) = &app.panel_state.quota.ask else {
        panic!("attach must send a quota query");
    };
    let old = old.0.clone();
    answer(&mut app, &old);
    let payload = json!({"session_id": SESSION, "how": "exited"});
    app.on_line(Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().unwrap().clone(),
    }));
    assert!(app.quota_focus().is_none());
    app.on_line(live(SESSION, json!({"state": "idle"})));
    let lost = app.quota_focus().unwrap();
    app.attach(contract::SessionId(SESSION.to_owned()));
    let new = first(&mut app);
    assert_ne!(serde_json::from_str::<Value>(&lost).unwrap()["id"], new);
    answer(&mut app, &old);
    assert!(app.quota_focus().is_none());
    answer(&mut app, &new);
    assert!(app.quota_focus().is_some());
}

#[test]
fn no_queries_without_a_card_session_or_live_link() {
    let mut app = home();
    app.outbox.link = Link::Up;
    assert!(app.quota_focus().is_none());
    let mut app = attached();
    app.home.as_mut().unwrap().launch.panel_cards = vec!["session".to_owned()];
    assert!(queries(app.on_line(session_line("clients", json!({"clients": []})))).is_empty());
    assert!(app.quota_focus().is_none());
    let mut app = attached();
    app.outbox.link = Link::Down;
    assert!(app.quota_focus().is_none());
    app.outbox.link = Link::Up;
    first(&mut app);
}

// debt: two `sent` copies, move this helper to `app::items::testkit` when that path
// is in the writing fence.
/// The command lines an effect sends, parsed.
fn sent(effect: crate::app::Effect) -> Vec<Value> {
    match effect {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. }
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_) => Vec::new(),
    }
}

#[test]
fn opening_the_view_sends_fetch_true_and_shares_the_flight() {
    let mut app = attached();
    let lines = sent(app.quota_view());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "quota");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"fetch": true}));
    assert!(
        !app.outbox
            .pending
            .contains_key(lines[0]["id"].as_str().unwrap())
    );
    assert!(app.session_view_open());
    assert!(app.panel_state.quota.fetching());
    // A second opening shares the in-flight fetch: no new line.
    assert!(sent(app.quota_view()).is_empty());
    assert!(app.panel_state.quota.fetching());
}

#[test]
fn opening_the_view_replaces_a_panel_query_in_flight_with_a_fetch() {
    let mut app = attached();
    let panel = first(&mut app);
    assert!(app.panel_state.quota.fetching());
    // The panel's fetch-less query is still outstanding: opening the view
    // replaces it with a fetch instead of showing `fetching` with none.
    let lines = sent(app.quota_view());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "quota");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"fetch": true}));
    assert_ne!(lines[0]["id"], panel);
    assert!(app.session_view_open());
    // The abandoned panel answer folds nothing: the flight stays up for
    // the fetch's answer.
    answer(&mut app, &panel);
    assert!(app.panel_state.quota.fetching());
    // A second opening shares the in-flight fetch: no new line.
    assert!(sent(app.quota_view()).is_empty());
}

#[test]
fn opening_the_view_without_a_live_link_sends_nothing() {
    let mut app = attached();
    app.outbox.link = Link::Down;
    assert!(sent(app.quota_view()).is_empty());
    assert!(app.session_view_open());
    assert!(!app.panel_state.quota.fetching());
}

#[test]
fn opening_the_view_retries_after_a_refusal() {
    let mut app = attached();
    let lines = sent(app.quota_view());
    let id = lines[0]["id"].as_str().unwrap().to_owned();
    assert!(
        queries(app.on_line(session_line(
            "command_rejected",
            json!({"command_id": id, "code": "denied", "message": "No."})
        )))
        .is_empty()
    );
    assert!(!app.panel_state.quota.fetching());
    let retry = sent(app.quota_view());
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0]["command"], "quota");
    assert_eq!(retry[0]["args"], json!({"fetch": true}));
    assert_ne!(retry[0]["id"], lines[0]["id"]);
}

#[test]
fn a_rejected_query_leaves_the_cached_rows_drawn() {
    let mut app = attached();
    let panel = first(&mut app);
    let quota = json!([{"provider": "codex", "credential": "work", "credit": {"remaining": 4.2}}]);
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id": panel, "result": {"quota": quota, "notice_at": 70.0}}),
    ));
    // A rejected panel query keeps the last answer's rows drawn.
    let line = app.quota_focus().unwrap();
    let id = serde_json::from_str::<Value>(&line).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        queries(app.on_line(session_line(
            "command_rejected",
            json!({"command_id": id, "code": "denied", "message": "No."})
        )))
        .is_empty()
    );
    assert_eq!(
        serde_json::to_value(&app.panel_state.quota.rows).unwrap(),
        quota
    );
    // A rejected view fetch keeps them too.
    let lines = sent(app.quota_view());
    let id = lines[0]["id"].as_str().unwrap().to_owned();
    assert!(
        queries(app.on_line(session_line(
            "command_rejected",
            json!({"command_id": id, "code": "denied", "message": "No."})
        )))
        .is_empty()
    );
    assert_eq!(
        serde_json::to_value(&app.panel_state.quota.rows).unwrap(),
        quota
    );
}

#[test]
fn a_foreign_quota_answer_updates_rows_without_clearing_the_flight() {
    let mut app = attached();
    let id = first(&mut app);
    // Another surface's quota answer folds into the card, while the
    // panel's own flight stays up for its own answer.
    let quota = json!([{"provider": "codex", "credential": "work", "credit": {"remaining": 4.2}}]);
    assert!(
        queries(app.on_line(session_line(
            "command_accepted",
            json!({"command_id": "c_other", "result": {"quota": quota, "notice_at": 70.0}})
        )))
        .is_empty()
    );
    assert_eq!(
        serde_json::to_value(&app.panel_state.quota.rows).unwrap(),
        quota
    );
    assert_eq!(app.panel_state.quota.notice_at, 70.0);
    assert!(app.panel_state.quota.fetching());
    assert!(app.quota_focus().is_none());
    answer(&mut app, &id);
    assert!(app.quota_focus().is_some());
}
