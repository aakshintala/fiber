//! Tests for `/clear`'s start and close lifecycle.

use crate::app::commands::commands_testkit::{
    SESSION, attached, connected, enter, hub_line, now, sent, session_line, turn_starts, type_text,
};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use serde_json::{Value, json};
use std::path::PathBuf;

/// A waiting `session_status` for `session`, held on `kind` with request
/// `r_1`.
fn waiting_status(session: &str, kind: &str) -> Line {
    old_line(
        session,
        "session_status",
        json!({
            "name": "fix the parser", "workspace": "/w", "project": "-w",
            "state": "waiting",
            "waiting": {"request_id": "r_1", "kind": kind, "summary": "Run it"},
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
                "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "fake/m", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `permission_requested` approval for `session`.
fn approval(session: &str) -> Line {
    let mut line = old_line(
        session,
        "permission_requested",
        json!({
            "request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"},
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.action_id = Some(contract::ActionId("a_1".to_owned()));
    }
    line
}

/// A `session_status` for `session` with `workspace`, `model` and
/// optional `thinking`.
fn clear_status(session: &str, workspace: &str, model: &str, thinking: Option<&str>) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": workspace, "project": "-w",
        "state": "idle", "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {},
            "output": 2}, "cost": 0.0, "subscription_cost": 0.0},
        "model": model, "delegates": 0, "jobs": 0, "clients": 0,
    });
    if let Some(thinking) = thinking {
        payload["thinking"] = json!(thinking);
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `preamble_built` for `session` naming `credential` or its absence.
fn clear_preamble(session: &str, model: &str, credential: Option<&str>) -> Line {
    let mut line = session_line(
        "preamble_built",
        json!({
            "reason": "start", "model": model, "context_window": 100,
            "thinking": "high", "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [], "credential": credential,
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(session.to_owned());
    }
    line
}

/// A credential-changing `model_changed` for `session`.
fn clear_model_changed(session: &str, credential: Option<&str>) -> Line {
    old_line(
        session,
        "model_changed",
        json!({
            "before": {"model": "fake/mod.el", "cache_lifetime": "5m", "credential": "L1"},
            "after": {"model": "fake/mod.el", "cache_lifetime": "5m", "credential": credential},
            "source": "driver",
        }),
    )
}

/// A `kind` line from `session`, parsed from the hub's side.
fn old_line(session: &str, kind: &str, payload: Value) -> Line {
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

/// An app with home attached to [`SESSION`], so rows fold.
fn homed() -> App {
    let mut app = attached();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app
}

/// Accepts hub command `id` with a new `session_id`, returning the lines
/// the acceptance sends, parsed.
fn accept_start(app: &mut App, id: &Value, session: &str) -> Vec<Value> {
    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": id, "result": {"session_id": session}}),
    ))
    .iter()
    .map(|line| serde_json::from_str(line).unwrap_or_default())
    .collect()
}

/// Folds one assistant reply of `text` into the attached session, so
/// the screen holds a conversation.
fn reply(app: &mut App, text: &str) {
    for (kind, payload) in [
        ("assistant_message_started", json!({})),
        ("text_completed", json!({"text": text})),
        (
            "assistant_message_completed",
            json!({"outcome": "completed"}),
        ),
    ] {
        let mut line = session_line(kind, payload);
        if let Line::Session(envelope) = &mut line {
            envelope.action_id = Some(contract::ActionId("a_1".to_owned()));
        }
        app.on_line(line);
    }
}

const NEW: &str = "s_nnnnnnnnnnnnnnnn";

/// The `/clear` start carries the old session's wire state
/// (`docs/tui.md:1932–1939`, "Clearing").
#[test]
fn clear_sends_a_start_with_the_old_session_s_wire() {
    let mut app = attached();
    app.on_line(clear_status(
        SESSION,
        "/w/work space",
        "fake/mod.el",
        Some("high"),
    ));
    app.on_line(clear_preamble(SESSION, "fake/mod.el", Some("L1")));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(lines[0].get("session_id").is_none());
    assert_eq!(lines[0]["args"]["workspace"], "/w/work space");
    assert_eq!(lines[0]["args"]["model"], "fake/mod.el");
    assert_eq!(lines[0]["args"]["worktree"], false);
    assert!(
        lines[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"models."fake/mod.el".thinking=high"#))
                    && overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    assert!(lines[0]["args"].get("continues").is_none());
    assert_eq!(app.draft(), "");
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

/// A build or switch that names no credential overrides an earlier start
/// label (`docs/tui.md:1941–1945`, "Clearing").
#[test]
fn clear_does_not_restore_a_start_credential_after_explicit_absence() {
    for event in [
        clear_preamble(SESSION, "fake/mod.el", None),
        clear_model_changed(SESSION, None),
    ] {
        let mut app = attached();
        app.on_line(clear_status(SESSION, "/w", "fake/mod.el", None));
        app.panel_state
            .note_start(&json!({"overrides": [r#"providers."fake".credential=L1"#]}));
        assert_eq!(app.panel_state.clear_credential(), Some(Some("L1")));
        app.on_line(event);
        assert_eq!(app.panel_state.clear_credential(), Some(None));

        let lines = sent(enter(&mut app, "/clear"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["command"], "start");
        assert!(lines[0]["args"].get("overrides").is_none());
    }
}

/// One clear start omits unset values as documented
/// (`docs/tui.md:1932–1939`, `docs/events.md:453–454`).
#[test]
fn clear_omits_thinking_credential_and_continues_when_unset() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"]["workspace"], "/w");
    assert_eq!(lines[0]["args"]["model"], "fake/m");
    assert_eq!(lines[0]["args"]["worktree"], false);
    assert!(lines[0]["args"].get("overrides").is_none());
    assert!(lines[0]["args"].get("continues").is_none());
}

/// A clear of a worktree session continues it (`docs/tui.md:1932–1939`).
#[test]
fn clear_of_a_worktree_session_continues_it() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w/tree", "fake/m", None));
    let mut line = session_line(
        "session_started",
        json!({
            "workspace": "/w/tree",
            "variables": {"path": "", "names": [], "source": "inherited"},
            "worktree": {"path": "/w/tree", "branch": "b"},
        }),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(SESSION.to_owned());
    }
    app.on_line(line);
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert_eq!(lines[0]["args"]["continues"], SESSION);
    assert_eq!(lines[0]["args"]["worktree"], false);

    accept_start(&mut app, &lines[0]["id"], NEW);
    app.on_line(clear_status(NEW, "/w/tree", "fake/m", None));
    let mut continued = session_line(
        "session_started",
        json!({
            "workspace": "/w/tree",
            "variables": {"path": "", "names": [], "source": "inherited"},
            "worktree": {"path": "/w/tree", "branch": "b"},
        }),
    );
    if let Line::Session(envelope) = &mut continued {
        envelope.session_id = contract::SessionId(NEW.to_owned());
    }
    app.on_line(continued);
    let next = sent(enter(&mut app, "/clear"));
    assert_eq!(next[0]["args"]["continues"], NEW);
}

/// Clearing waits for the `start` answer before moving the screen and
/// closing the old session (`docs/tui.md:1934–1938, 1947–1952`).
#[test]
fn clear_switches_only_on_the_start_answer_then_closes_old() {
    let mut app = attached();
    turn_starts(&mut app);
    reply(&mut app, "hello");
    assert!(!app.lines().is_empty());
    app.on_line(clear_status(SESSION, "/w", "fake/m", Some("high")));
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"].clone();
    // The start goes out alone: no close before the answer, and the
    // screen stays on the old session.
    assert_eq!(lines.len(), 1);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    let after: Vec<Value> = accept_start(&mut app, &id, NEW);
    assert!(after.iter().any(|line| {
        line["command"] == "subscribe"
            && line["session_id"] == NEW
            && line["args"] == json!({"level": "full"})
    }));
    assert!(
        after
            .iter()
            .any(|line| { line["command"] == "commands" && line["session_id"] == NEW })
    );
    assert!(after.iter().any(|line| {
        line["command"] == "close"
            && line["session_id"] == SESSION
            && line["args"] == json!({"now": false})
    }));
    assert!(!after.iter().any(|line| line["command"] == "prompt"));
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(app.draft(), "");
    assert!(app.lines().is_empty());
}

/// `/clear` keeps the names the terminal learned: the new session's
/// card labels the same sender by its name, not its id.
#[test]
fn clear_keeps_the_learned_sender_names() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    app.on_line(crate::app::testkit::sender_status("worker", true));
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"].clone();
    accept_start(&mut app, &id, NEW);
    assert!(app.lines().is_empty());
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(NEW.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [
            {"type": "message", "source": "driver",
             "content": [{"type": "text", "text": "go"}]},
            {"type": "message", "source": "session",
             "from_session_id": crate::app::testkit::SENDER,
             "content": [{"type": "text", "text": "please check"}]},
        ]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    let shown: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        shown.iter().any(|line| line.contains("worker · ")),
        "the cleared conversation keeps the name: {shown:?}"
    );
}

const NEW2: &str = "s_mmmmmmmmmmmmmmmm";

/// A second `/clear` cannot replace the still-pending `start`
/// (`docs/tui.md:1937–1938`).
#[test]
fn clear_waiting_for_start_answer_does_not_send_a_second_start() {
    let mut app = attached();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let first = sent(enter(&mut app, "/clear"));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["command"], "start");

    assert!(matches!(enter(&mut app, "/clear"), Effect::None));
    assert_eq!(app.draft(), "");
    let answered = accept_start(&mut app, &first[0]["id"], NEW);
    assert!(
        answered
            .iter()
            .any(|line| { line["command"] == "close" && line["session_id"] == SESSION })
    );
    assert!(!answered.iter().any(|line| line["command"] == "prompt"));
}

/// Two `/clear`s before any prompt: each `start` uses its session's
/// `session_status` model, thinking only when present, and keeps the
/// credential the first `start` named (`docs/tui.md:1932–1943`).
#[test]
fn clear_twice_before_any_prompt_keeps_the_start_credential() {
    let mut app = connected();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        model: Some("test/model".to_owned()),
        ..Default::default()
    });
    app.set_launch_overrides(vec![r#"providers."fake".credential=L1"#.to_owned()]);
    let first = sent(enter(&mut app, "hi"));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["command"], "start");
    assert!(
        first[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    let started: Vec<Value> = accept_start(&mut app, &first[0]["id"], NEW);
    // The home `start` answers with its first prompt, as today, beside
    // the feed and recent home refills.
    assert!(started.iter().any(|line| line["command"] == "prompt"));
    assert!(started.iter().any(|line| line["command"] == "subscribe"));
    // The new session's own status names another model with no
    // thinking level.
    app.on_line(clear_status(NEW, "/w", "fake/m2", None));
    let second = sent(enter(&mut app, "/clear"));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["command"], "start");
    assert_eq!(second[0]["args"]["model"], "fake/m2");
    assert!(
        second[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
    let answered: Vec<Value> = accept_start(&mut app, &second[0]["id"], NEW2);
    assert_eq!(
        answered
            .iter()
            .filter(|line| line["command"] == "prompt")
            .count(),
        0
    );
    // The third `start` uses the newest status again, thinking present.
    app.on_line(clear_status(NEW2, "/w", "fake/m3", Some("low")));
    let third = sent(enter(&mut app, "/clear"));
    assert_eq!(third.len(), 1);
    assert_eq!(third[0]["command"], "start");
    assert_eq!(third[0]["args"]["model"], "fake/m3");
    assert!(
        third[0]["args"]["overrides"]
            .as_array()
            .is_some_and(|overrides| {
                overrides.contains(&json!(r#"models.fake/m3.thinking=low"#))
                    && overrides.contains(&json!(r#"providers."fake".credential=L1"#))
            })
    );
}

#[test]
fn clear_refusal_only_settles_the_matching_start() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("start id"));

    assert!(!app.clear_rejected("c_unrelated", "busy", "Unrelated."));
    assert!(app.clear_pending());
    assert!(app.clear_rejected(id, "busy", "Busy."));
    assert!(!app.clear_pending());
    assert_eq!(app.notice(), Some("Busy."));
}

/// A refused clear start leaves the old session in place and sends no
/// close (`docs/tui.md:1943–1945`).
#[test]
fn clear_start_refused_keeps_the_old_session_with_one_notice() {
    let mut app = homed();
    // The introducing status queues its project's scoped ask, drained
    // on the same outgoing pass as the fresh feed and recent.
    let queued: Vec<Value> = app
        .on_line(clear_status(SESSION, "/w", "fake/m", Some("high")))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The introducing status queues its project's scoped ask (`docs/tui.md:268`):
    // assert only its presence, not the outgoing order or count.
    assert!(
        queued
            .iter()
            .any(|line| line["command"] == "recent" && line["args"]["project"] == "-w")
    );
    let lines = sent(enter(&mut app, "/clear"));
    let id = lines[0]["id"].clone();
    // Typing after the dispatch stays: the refusal takes nothing.
    type_text(&mut app, "more");
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": id, "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert_eq!(app.draft(), "more");
    assert_eq!(app.notice(), Some("Busy."));
    // The intent is gone: clearing again dispatches a fresh `start`.
    app.draft.clear();
    let again = sent(enter(&mut app, "/clear"));
    assert_eq!(again.len(), 1);
    assert_eq!(again[0]["command"], "start");
    assert_ne!(again[0]["id"], lines[0]["id"]);
}

#[test]
fn clear_start_lost_before_write_keeps_the_old_session() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let Effect::Send(out) = enter(&mut app, "/clear") else {
        panic!("the start goes out");
    };
    app.write_failed(&out);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(app.notice().is_some());
}

#[test]
fn clear_close_refused_stays_on_the_new_session_with_one_notice() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", Some("high")));
    let lines = sent(enter(&mut app, "/clear"));
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    let rejected = old_line(
        SESSION,
        "command_rejected",
        json!({"command_id": close["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    // The screen stays on the new session; the old one stays a live
    // row, never marked closing.
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(app.notice(), Some("Busy."));
    let home = app.home.as_ref().unwrap_or_else(|| panic!("home"));
    let old = home
        .sessions
        .row(&contract::SessionId(SESSION.to_owned()))
        .unwrap_or_else(|| panic!("old row"));
    assert!(old.left.is_none());
    assert_eq!(
        old.status.as_ref().map(|status| status.closing),
        Some(false)
    );
}

/// Only the matching accepted `close` answer settles the clear's close
/// intent (`docs/tui.md`, "Clearing").
#[test]
fn clear_close_answer_settles_only_its_matching_command_id() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let start = sent(enter(&mut app, "/clear"));
    let after = accept_start(&mut app, &start[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    let close_id = close["id"].as_str().unwrap_or_else(|| panic!("close id"));

    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": "c_unrelated", "result": {}}),
    ));
    assert_eq!(app.clear.close.as_deref(), Some(close_id));
    app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": close_id, "result": {}}),
    ));
    assert_eq!(app.clear.close, None);
}

#[test]
fn clear_close_lost_before_write_names_the_lost_link() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    let after: Vec<String> = app.on_line(hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": NEW}}),
    ));
    let close = after
        .iter()
        .find(|line| line.contains("\"close\""))
        .unwrap_or_else(|| panic!("the close goes out"))
        .clone();
    app.write_failed(&[close]);
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert_eq!(
        app.notice(),
        Some("The link to the old session was lost before its `close` went out.")
    );
    let home = app.home.as_ref().unwrap_or_else(|| panic!("home"));
    let old = home
        .sessions
        .row(&contract::SessionId(SESSION.to_owned()))
        .unwrap_or_else(|| panic!("old row"));
    assert!(old.left.is_none());
}

#[test]
fn clear_with_an_approval_wait_opens_the_overlay_first() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert!(app.clear_open());
    assert_eq!(
        app.clear_question(),
        Some(("fix the parser".to_owned(), "approval".to_owned(), false))
    );
}

#[test]
fn clear_with_a_question_wait_names_the_question() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "question"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert!(app.clear_open());
    assert_eq!(
        app.clear_question(),
        Some(("fix the parser".to_owned(), "question".to_owned(), false))
    );
}

/// Enter clears a wait only after the clear start answers
/// (`docs/tui.md:1954–1960`, "Clearing").
#[test]
fn clear_overlay_enter_sends_the_start_then_closes_with_now() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(!app.clear_open());
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    assert_eq!(close["args"], json!({"now": true}));
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
}

#[test]
fn clear_overlay_esc_stays() {
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    // The consumed command left the draft with the overlay, so typing
    // `/clear` again dispatches fresh.
    assert_eq!(app.draft(), "");
}

/// A refused overlay start sends no close (`docs/tui.md:1943–1945`).
#[test]
fn clear_overlay_refused_start_closes_with_one_notice() {
    let mut app = homed();
    // The introducing status queues its project's scoped ask, drained
    // on the same outgoing pass as the fresh feed and recent.
    let queued: Vec<Value> = app
        .on_line(waiting_status(SESSION, "approval"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The introducing status queues its project's scoped ask (`docs/tui.md:268`):
    // assert only its presence, not the outgoing order or count.
    assert!(
        queued
            .iter()
            .any(|line| line["command"] == "recent" && line["args"]["project"] == "-w")
    );
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    let rejected = hub_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "busy", "message": "Busy."}),
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert_eq!(app.notice(), Some("Busy."));
}

/// The clear choice sends a start and stay sends nothing
/// (`docs/tui.md:1954–1959`, "Clearing").
#[test]
fn clear_overlay_clicks_do_what_their_keys_do() {
    use crate::home::{ClearChoice, Spot};
    let mut app = homed();
    app.on_line(waiting_status(SESSION, "approval"));
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    let lines = sent(app.home_click(Spot::Clear(ClearChoice::Clear)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    assert!(!app.clear_open());
    // Staying sends nothing and keeps the old session.
    assert_eq!(enter(&mut app, "/clear"), Effect::None);
    assert_eq!(app.home_click(Spot::Clear(ClearChoice::Stay)), Effect::None);
    assert!(!app.clear_open());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

/// A wait arriving after dispatch keeps the close polite
/// (`docs/tui.md:1959–1960`).
#[test]
fn clear_late_approval_keeps_the_close_polite() {
    let mut app = homed();
    app.on_line(clear_status(SESSION, "/w", "fake/m", None));
    let lines = sent(enter(&mut app, "/clear"));
    assert_eq!(lines.len(), 1);
    // The approval arrives after the dispatch, before its answer.
    app.on_line(approval(SESSION));
    let after = accept_start(&mut app, &lines[0]["id"], NEW);
    let close = after
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the close goes out"));
    assert_eq!(close["args"], json!({"now": false}));
    // The old request waits in the shared queue, shown and answerable.
    assert!(app.queue.open());
}
