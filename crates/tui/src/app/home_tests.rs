//! Tests for home's state on the app: when home draws, and what `start`
//! names.

use super::{App, Effect};
use crate::app::testkit::{SENDER, hello, home_with, sender_status};
use crate::home::{Launch, ListLine, Spot};
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::local_time::{new_york, turn_started_at};
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

use crate::Configure;
use crate::configure_fake::Fake;

/// The launch home opens: local /w with the model chip set.
fn launch() -> Launch {
    Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    }
}

/// A `permission_requested` review, which opens the approval panel.
fn review() -> Line {
    Line::Session(contract::Envelope {
        kind: "permission_requested".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn remote_home_ignores_the_launch_project_in_sections_and_row_text() {
    let mut app = home_with(80, 24, launch());
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        ..Default::default()
    });
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "first",
        "/a",
        "other",
        json!({"state":"idle"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "second",
        "/b",
        "sentinel-project",
        json!({"state":"idle"}),
    ));
    let screen = app.home_screen().unwrap();
    let heads: Vec<_> = screen
        .list
        .iter()
        .filter_map(|line| {
            if let crate::home::ListLine::Head { name, .. } = line {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(heads, ["a", "b"]);
    let row = app.home.as_ref().unwrap().sessions.all()[1].key;
    let text = app.home_text(Spot::Entry(row)).unwrap();
    assert!(text.ends_with("  b"), "{text}");
    assert!(!text.contains("sentinel-project"));
}

#[test]
fn remote_project_of_uses_newest_status_then_first_row_on_ties() {
    for (first_since, second_since, expected) in [(1, 2, "q"), (2, 1, "p"), (2, 2, "p")] {
        let mut app = home_with(80, 24, launch());
        for (id, project, since) in [
            ("s_aaaaaaaaaaaaaaaa", "p", first_since),
            ("s_bbbbbbbbbbbbbbbb", "q", second_since),
        ] {
            let mut row = live(id, project, "/remote", project, json!({"state":"idle"}));
            if let Line::Session(envelope) = &mut row {
                envelope.payload.insert("since".to_owned(), json!(since));
            }
            app.on_line(row);
        }
        let sessions = &app.home.as_ref().unwrap().sessions;
        assert_eq!(sessions.project_of("/remote"), Some(expected));
        assert_eq!(sessions.project_of("/elsewhere"), None);
    }
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions":[exited_in("s_aaaaaaaaaaaaaaaa", "old", "/remote", "p")]}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "new",
        "/remote",
        "q",
        json!({"state":"idle"}),
    ));
    assert_eq!(
        app.home.as_ref().unwrap().sessions.project_of("/remote"),
        Some("q")
    );
    for absent_first in [false, true] {
        let mut sessions = crate::home::Sessions::default();
        let make = |id: &str, project: &str| {
            let Line::Session(envelope) =
                live(id, project, "/remote", project, json!({"state":"idle"}))
            else {
                unreachable!()
            };
            crate::home::from_status(&envelope)
        };
        let mut absent = make("s_aaaaaaaaaaaaaaaa", "absent");
        absent.status = None;
        let present = make("s_bbbbbbbbbbbbbbbb", "present");
        if absent_first {
            sessions.status(absent);
            sessions.status(present);
        } else {
            sessions.status(present);
            sessions.status(absent);
        }
        assert_eq!(sessions.project_of("/remote"), Some("present"));
    }
}

#[test]
fn an_app_without_home_draws_the_conversation_screen() {
    let app = App::new(PathBuf::from("/w"));
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn an_attached_app_draws_the_conversation_screen() {
    let mut app = home_with(80, 24, launch());
    assert!(app.on_home());
    assert!(app.home_screen().is_some());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn the_zone_survives_a_session_switch() {
    let mut app = home_with(80, 24, launch());
    app.set_zone(new_york());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.go_home();
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    // 2026-10-08T14:15Z, 10:15 in New York.
    app.on_line(turn_started_at("s_bbbbbbbbbbbbbbbb", "go", 1791468900000));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(
        texts,
        vec![
            "".to_owned(),
            "▄▄▄▄▄".to_owned(),
            " go ▐".to_owned(),
            "▀▀▀▀▀".to_owned(),
            "you 10:15 ".to_owned(),
            "".to_owned()
        ]
    );
}

#[test]
fn a_named_recent_row_seeds_the_sender_label_and_a_statusless_row_keeps_it() {
    let mut app = home_with(80, 24, launch());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let lines: Vec<Value> = commands(app.on_line(hello()));
    let recent = lines
        .iter()
        .find(|line| line["command"] == "recent")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("recent id: {lines:?}"))
        .to_owned();
    // The unscoped `recent` answer's row carries the sender's last
    // status, which named it: an exited sender still labels by its
    // actual name (`docs/tui.md`, "Turns").
    let named_row = json!({
        "session_id": "s_bbbbbbbbbbbbbbbb", "ts": 0, "project": "other",
        "workspace": "/o", "name": "worker", "how": "exited",
        "status": {
            "name": "worker", "named": true, "workspace": "/o", "project": "other",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        },
    });
    let out = app.on_line(accepted(&recent, json!({"sessions": [named_row]})));
    let scoped: Vec<Value> = commands(out)
        .into_iter()
        .filter(|line| line["command"] == "recent")
        .collect();
    // A project that first appears earns one scoped `recent` ask
    // (`docs/tui.md`, "Home", "The session list").
    assert_eq!(scoped.len(), 1, "the new project earns one scoped ask");
    app.on_line(turn_with_sender_message("s_aaaaaaaaaaaaaaaa"));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        texts.iter().any(|line| line.contains("worker · ")),
        "the seeded name labels the message: {texts:?}"
    );
    // The scoped answer's row carries no status: the learned name stays.
    let id = scoped[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("scoped id: {scoped:?}"))
        .to_owned();
    let bare_row = json!({
        "session_id": "s_bbbbbbbbbbbbbbbb", "ts": 0, "project": "other",
        "workspace": "/o", "name": "worker", "how": "exited",
    });
    app.on_line(accepted(&id, json!({"sessions": [bare_row], "total": 1})));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        texts.iter().any(|line| line.contains("worker · ")),
        "the learned name survives a statusless row: {texts:?}"
    );
}

#[test]
fn a_feed_status_names_the_sender_only_when_it_is_named() {
    let mut app = home_with(80, 24, launch());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.on_line(hello());
    app.on_line(turn_with_sender_message("s_aaaaaaaaaaaaaaaa"));
    let texts =
        |app: &App| -> Vec<String> { app.lines().iter().map(ToString::to_string).collect() };
    // Home lists the sender by its first prompt, which never labels it.
    app.on_line(sender_status("first prompt row", false));
    let shown = texts(&app);
    assert!(
        shown
            .iter()
            .any(|line| line.contains(&format!("{SENDER} · "))),
        "an unnamed status labels by id: {shown:?}"
    );
    assert!(
        !shown.iter().any(|line| line.contains("first prompt row")),
        "{shown:?}"
    );
    app.on_line(sender_status("worker", true));
    let shown = texts(&app);
    assert!(
        shown.iter().any(|line| line.contains("worker · ")),
        "a named status labels by name: {shown:?}"
    );
    // The latest status wins: one that is no longer named drops it.
    app.on_line(sender_status("first prompt row", false));
    let shown = texts(&app);
    assert!(
        shown
            .iter()
            .any(|line| line.contains(&format!("{SENDER} · "))),
        "the id labels again: {shown:?}"
    );
}

#[test]
fn an_unnamed_home_row_with_a_display_name_still_labels_by_sender_id() {
    let mut app = home_with(80, 24, launch());
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let lines: Vec<Value> = commands(app.on_line(hello()));
    let recent = lines
        .iter()
        .find(|line| line["command"] == "recent")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("recent id: {lines:?}"))
        .to_owned();
    // The row's status carries a display name but does not name the
    // sender: only an actual name labels it, so the message falls back
    // to the sender id (`docs/tui.md`, "Turns").
    let unnamed_row = json!({
        "session_id": "s_bbbbbbbbbbbbbbbb", "ts": 0, "project": "other",
        "workspace": "/o", "name": "worker", "how": "exited",
        "status": {
            "name": "first prompt row", "named": false, "workspace": "/o", "project": "other",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        },
    });
    let out = app.on_line(accepted(&recent, json!({"sessions": [unnamed_row]})));
    let scoped: Vec<Value> = commands(out)
        .into_iter()
        .filter(|line| line["command"] == "recent")
        .collect();
    // A project that first appears earns one scoped `recent` ask
    // (`docs/tui.md`, "Home", "The session list").
    assert_eq!(scoped.len(), 1, "the new project earns one scoped ask");
    app.on_line(turn_with_sender_message("s_aaaaaaaaaaaaaaaa"));
    let texts: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        texts
            .iter()
            .any(|line| line.contains(&format!("{SENDER} · "))),
        "the sender id labels the message: {texts:?}"
    );
    assert!(
        !texts.iter().any(|line| line.contains("first prompt row")),
        "an unnamed status names nothing: {texts:?}"
    );
}

/// A `turn_started` in `session` carrying the sender's message beside
/// the driver's.
fn turn_with_sender_message(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"input": [
            {"type": "message", "source": "driver", "content": [{"type": "text", "text": "go"}]},
            {"type": "message", "source": "session", "from_session_id": "s_bbbbbbbbbbbbbbbb",
             "content": [{"type": "text", "text": "please check"}]},
        ]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn the_key_map_draws_over_home() {
    let mut app = home_with(80, 24, launch());
    app.open_keymap();
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

#[test]
fn an_approval_panel_draws_over_home() {
    let mut app = home_with(80, 24, launch());
    app.on_line(review());
    assert!(app.panel().is_some());
    assert!(!app.on_home());
    assert!(app.home_screen().is_none());
}

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: serde_json::Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String("invalid_arguments".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(message.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A `session_status` for `session` whose hub accepted `close`: the row
/// stays live until its turn finishes.
fn closing(session: &str) -> Line {
    let mut line = status(session);
    let Line::Session(envelope) = &mut line else {
        panic!("session line");
    };
    envelope
        .payload
        .insert("closing".to_owned(), serde_json::Value::Bool(true));
    line
}

/// A `turn_started` for `session` with one message.
fn turn_started(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"input": [{"type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// A `turn_started` for `session`, answering the `commands` it asks for
/// with no rows: a turn start may change the runnable set, so the
/// terminal asks again at each one.
fn turn_started_answered(app: &mut App, session: &str) {
    let asked = commands(app.on_line(turn_started(session)));
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["command"], "commands");
    let id = asked[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("commands id"));
    let answer = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "result": {"commands": []}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(answer).is_empty());
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<serde_json::Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The rows home draws, as lines.
fn rows(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(_, line)| line)
                .collect()
        })
        .unwrap_or_default()
}

/// Every listed row's line, drawn or not: what the feed and `recent` folded.
fn listed(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.sessions
                .all()
                .iter()
                .map(|row| crate::home::line(row, home.launch_project()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn home_screen_header_counts_follow_the_state_order() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live("s_wait", "waiting", "/w", "-w", json!({"state":"waiting", "waiting":{"request_id":"r_1", "kind":"approval", "summary":"approve"}})));
    app.on_line(live(
        "s_work",
        "working",
        "/other",
        "-other",
        json!({"state":"streaming"}),
    ));
    app.on_line(live(
        "s_retry",
        "retrying",
        "/w",
        "-w",
        json!({"state":"retrying"}),
    ));
    app.on_line(live(
        "s_crash",
        "crashed",
        "/w",
        "-w",
        json!({"state":"idle"}),
    ));
    app.on_line(left("s_crash", "crashed"));
    app.on_line(live(
        "s_ready",
        "ready",
        "/w",
        "-w",
        json!({"state":"idle"}),
    ));
    let screen = app.home_screen().unwrap();
    assert_eq!(
        screen
            .header
            .counts
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        [
            "1 waiting on you",
            "1 working",
            "1 retrying",
            "1 crashed",
            "1 ready"
        ]
    );
    assert_eq!(screen.header.spend.as_deref(), Some("$0.00 live"));
}

#[test]
fn home_screen_header_says_no_sessions_yet_without_live_rows() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_old", "exited")]);
    let screen = app.home_screen().unwrap();
    assert!(screen.header.counts.is_empty());
    assert_eq!(screen.header.spend, None);
    assert!(!screen.empty);
}

#[test]
fn section_header_counts_live_spend_and_waiting() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live("s_wait", "waiting", "/w", "-w", json!({"state":"waiting", "waiting":{"request_id":"r_1", "kind":"approval", "summary":"approve"}, "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.25,"subscription_cost":0.0}})));
    app.on_line(live("s_other", "other", "/w", "-w", json!({"state":"streaming", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":0.10,"subscription_cost":0.0}})));
    let screen = app.home_screen().unwrap();
    let crate::home::ListLine::Head {
        live,
        spend,
        waiting,
        ..
    } = &screen.list[0]
    else {
        panic!("project head")
    };
    assert_eq!((*live, spend.as_str(), *waiting), (2, "$1.35", 1));
}

#[test]
fn the_header_spend_is_the_sum_of_the_live_rows() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live("s_a", "a", "/w", "-w", json!({"state":"streaming", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.25,"subscription_cost":0.0}})));
    app.on_line(live("s_b", "b", "/other", "-other", json!({"state":"idle", "spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":2.10,"subscription_cost":0.0}})));
    assert_eq!(
        app.home_screen().unwrap().header.spend.as_deref(),
        Some("$3.35 live")
    );
}

#[test]
fn hello_sends_feed_then_recent_once() {
    let mut app = home_with(80, 24, launch());
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "feed");
    assert_eq!(lines[1]["command"], "recent");
    assert!(lines[1].get("args").is_none());
    assert_ne!(lines[0]["id"], lines[1]["id"]);
}

#[test]
fn no_feed_before_hello_and_none_on_a_refused_schema() {
    let mut app = home_with(80, 24, launch());
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    let mut newer = hello();
    if let Line::Hub(hello) = &mut newer {
        hello.schema_version = contract::SCHEMA_VERSION + 1;
    }
    assert!(app.on_line(newer).is_empty());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
}

#[test]
fn a_status_after_hello_queues_one_scoped_ask() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let lines = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(lines.len(), 1);
    // `docs/invocation.md:1102` (recent command).
    assert_eq!(lines[0]["command"], "recent");
    // `docs/tui.md:268` (scoped ask carries the project's key).
    assert_eq!(lines[0]["args"]["project"], "-w");
    assert!(lines[0]["args"].get("before").is_none());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
}

#[test]
fn a_recent_answer_for_an_older_id_changes_nothing() {
    let mut app = home_with(80, 24, launch());
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // An answer for an id that is not the latest `recent`: nothing folds.
    assert!(
        app.on_line(accepted("c_deadbeefdeadbeef", json!({"sessions": []})))
            .is_empty()
    );
    assert!(rows(&app).is_empty());
    app.on_line(accepted(
        recent,
        json!({"sessions": [{
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "ts": 0,
            "project": "-w",
            "workspace": "/w",
            "name": "old work",
            "how": "exited",
        }]}),
    ));
    assert_eq!(rows(&app), ["○  old work"]);
}

#[test]
fn a_rejected_feed_is_one_notice() {
    let mut app = home_with(80, 24, launch());
    let lines = commands(app.on_line(hello()));
    let feed = lines[0]["id"].as_str().unwrap_or_else(|| panic!("feed id"));
    assert!(app.on_line(refused(feed, "no feed")).is_empty());
    assert_eq!(app.notice(), Some("no feed"));
    assert!(rows(&app).is_empty());
}

#[test]
fn session_status_for_the_attached_session_still_reaches_the_conversation() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start = commands(lines);
    let id = start[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("start id"));
    app.on_line(accepted(id, json!({"session_id": "s_aaaaaaaaaaaaaaaa"})));
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    // The status folds into the rows, and passes through to the
    // conversation below, queuing its project's scoped ask beside it.
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(queued.len(), 1);
    // `docs/tui.md:268` (scoped ask carries the project's key).
    assert_eq!(queued[0]["args"]["project"], "-w");
    assert_eq!(listed(&app), ["✓  fix the parser"]);
    turn_started_answered(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn an_attention_line_changes_nothing() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let attention = Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": "s_aaaaaaaaaaaaaaaa"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(attention).is_empty());
    assert!(rows(&app).is_empty());
    assert_eq!(app.notice(), None);
}

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

#[test]
fn answers_and_a_left_fill_and_mark_the_list() {
    let mut app = home_with(80, 24, launch());
    let lines = commands(app.on_line(hello()));
    let feed = lines[0]["id"].as_str().unwrap_or_else(|| panic!("feed id"));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // The feed's own answer carries no rows.
    assert!(app.on_line(accepted(feed, json!({}))).is_empty());
    assert!(rows(&app).is_empty());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(rows(&app), ["✓  fix the parser"]);
    // `session_left` marks the row in place; one for an unknown session
    // changes nothing.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    assert!(
        app.on_line(left("s_bbbbbbbbbbbbbbbb", "crashed"))
            .is_empty()
    );
    assert_eq!(rows(&app), ["○  fix the parser"]);
    // The recent page fills the exited rows after the feed rows.
    app.on_line(accepted(
        recent,
        json!({"sessions": [{
            "session_id": "s_bbbbbbbbbbbbbbbb",
            "ts": 0,
            "project": "-w",
            "workspace": "/w",
            "name": "old work",
            "how": "crashed",
        }]}),
    ));
    assert_eq!(rows(&app), ["○  fix the parser", "○  old work"]);
}

#[test]
fn a_left_with_an_unknown_how_changes_nothing() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    let left = Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa", "how": "vanished"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    assert!(app.on_line(left).is_empty());
    assert_eq!(rows(&app), ["✓  fix the parser"]);
}

/// A live row whose latest status accepted `close` stays on home as a
/// CLOSING row with no ⌥N, while other live rows keep their start order
/// and numbers (`docs/tui.md:242–253, 1936–1937`).
#[test]
fn a_closing_row_shows_closing_with_no_number() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    app.on_line(closing("s_bbbbbbbbbbbbbbbb"));
    let mut live = app
        .home_screen()
        .unwrap_or_else(|| panic!("home screen"))
        .list
        .into_iter()
        .filter_map(|line| match line {
            ListLine::Live {
                word,
                tone,
                number,
                id,
                ..
            } => Some((id, word, tone, number)),
            ListLine::Head { .. }
            | ListLine::ExitedHead { .. }
            | ListLine::Exited { .. }
            | ListLine::More { .. } => None,
        });
    assert_eq!(
        live.next(),
        Some((
            "s_aaaaaaaaaaaaaaaa".to_owned(),
            "READY",
            crate::theme::Role::Muted,
            Some(1)
        )),
        "the live row keeps its number"
    );
    assert_eq!(
        live.next(),
        Some((
            "s_bbbbbbbbbbbbbbbb".to_owned(),
            "CLOSING",
            crate::theme::Role::Muted,
            None
        )),
        "the closing row reads CLOSING with no number"
    );
    assert!(live.next().is_none());
}

#[test]
fn start_names_the_launch_workspace() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/w");
    assert!(line["args"].get("content").is_none());
    // The placeholder goes with the first prompt.
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}

/// The keys of the rows home draws, in order.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
        .unwrap_or_default()
}

/// The drawn `More` row's key for `project`: the first hidden loaded
/// row when one hides, else the key the total path assigned.
fn more_key(app: &App, project: &str) -> Option<u64> {
    let home = app.home.as_ref()?;
    app.home_screen()?
        .list
        .into_iter()
        .find_map(|line| match line {
            ListLine::More { key, .. } => {
                (home.sessions.by_key(key)?.project == project).then_some(key)
            }
            ListLine::Head { .. }
            | ListLine::Live { .. }
            | ListLine::ExitedHead { .. }
            | ListLine::Exited { .. } => None,
        })
}

/// Opens the row with `key`, with the parsed lines going out.
fn open(app: &mut App, key: u64) -> Vec<Value> {
    match app.home_click(Spot::Entry(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("opening sends"),
    }
}

/// A live `session_status` for `session`, named `name`, in `workspace` of
/// `project`, in `state`.
fn live(session: &str, name: &str, workspace: &str, project: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": workspace,
        "project": project,
        "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
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

/// A session `command_accepted` for `id` from `session`.
fn session_accepted(session: &str, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), Value::String(id.to_owned()))]
            .into_iter()
            .collect(),
    })
}

/// A session `command_rejected` for `id` from `session`, with `code` and
/// `message`.
fn session_refused(session: &str, id: &str, code: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id`, with `code` and `message`.
fn hub_refused(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// One exited `recent` row for `session`.
fn exited(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "exited",
    })
}

/// One exited recent row in `workspace` and `project`.
fn exited_in(session: &str, name: &str, workspace: &str, project: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": project,
        "workspace": workspace,
        "name": name,
        "how": "exited",
    })
}

/// Answers `recent` with exited rows for `sessions`: the scoped fan-out
/// lines, if any.
fn answer_recent(app: &mut App, recent: &str, sessions: &[(&str, &str)]) -> Vec<String> {
    let result = json!({"sessions": sessions
        .iter()
        .map(|(session, name)| exited(session, name))
        .collect::<Vec<_>>()});
    app.on_line(accepted(recent, result))
}

/// Links the app: the feed and recent ids, both waiting for their
/// answers. An open picker's listing goes out beside them.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = commands(app.on_line(hello()))
        .into_iter()
        .filter(|line| line["command"] != "directories")
        .collect();
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// Types `text` without pressing Enter.
fn type_text(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
}

/// Types `text` and presses Enter.
fn enter_text(app: &mut App, text: &str) -> Effect {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Enter, now)
}

/// Opens the first row home draws, with the parsed lines going out.
fn open_first(app: &mut App) -> Vec<Value> {
    let key = keys(app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    open(app, key)
}

/// Accepts the opening summary and checks the commands line it releases.
fn accept_summary(app: &mut App, session: &str, ack: &str) {
    let sent = app.on_line(session_accepted(session, ack));
    assert_eq!(sent.len(), 1);
    let commands: Value = serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], session);
}

/// Starts a session from the draft and accepts the start for `session`:
/// the app attached, its `full` subscribe in flight.
fn start_session(app: &mut App, session: &str) -> Vec<Value> {
    let Effect::Send(lines) = enter_text(app, "hi") else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    let out: Vec<Value> = app
        .on_line(accepted(
            start["id"].as_str().unwrap_or_else(|| panic!("start id")),
            json!({"session_id": session}),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{err}")))
        .collect();
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["command"], "subscribe");
    assert_eq!(out[2]["command"], "prompt");
    out
}

#[test]
fn enter_on_a_focused_live_row_subscribes_summary_asks_commands_and_attaches() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // Focus the second row directly: keyboard traversal now ends at the
    // chip row, while this test checks Enter on a focused live row.
    let row = keys(&app)[1];
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Entry(row)));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter opens the row");
    };
    let lines: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_bbbbbbbbbbbbbbbb");
    assert_eq!(lines[0]["args"]["level"], "summary");
    let ack = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    accept_summary(&mut app, "s_bbbbbbbbbbbbbbbb", ack);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_bbbbbbbbbbbbbbbb")
    );
    assert!(!app.on_home());
}

#[test]
fn opening_an_exited_row_sends_the_same_and_the_hub_resumes() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"]["level"], "summary");
    let ack = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn a_started_session_is_recorded_full_and_leaving_lowers_it() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("/home leaves");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"]["level"], "summary");
    assert!(app.on_home());
}

#[test]
fn opening_a_summary_session_sends_no_subscribe_then_raises() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    let started = start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    let full = started[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", full))
            .is_empty()
    );
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let Effect::Send(lowering) = enter_text(&mut app, "/home") else {
        panic!("home lowers");
    };
    let lowering: Value = serde_json::from_str(&lowering[0]).unwrap_or_else(|err| panic!("{err}"));
    let ack = lowering["id"]
        .as_str()
        .unwrap_or_else(|| panic!("lowering id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    // The connection holds accepted `summary`, so no subscribe goes out; the
    // `full` raise follows the first frame instead.
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "commands");
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(raise["args"]["level"], "full");
    let ack = raise["id"].as_str().unwrap_or_else(|| panic!("raise id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn opening_a_full_session_sends_summary_then_full() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first[0]["args"]["level"], "summary");
    // Home during the open, before any answer: the row is not live, so no
    // lowering goes out.
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    // The earlier summary is still in flight: commands waits for its
    // acceptance, and a refusal must send no commands.
    let lines = open_first(&mut app);
    assert!(lines.is_empty());
    assert!(app.take_raise().is_none());
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    let sent = app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack));
    assert_eq!(sent.len(), 1);
    let commands: Value = serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], "s_aaaaaaaaaaaaaaaa");
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["args"]["level"], "full");
    assert_ne!(first[0]["id"], raise["id"]);
}

#[test]
fn a_screen_coordinate_click_on_a_home_row_opens_it() {
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::{Pointer, TargetId};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let mut app = home_with(80, 24, launch());
    app.set_size(160, 48);
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = keys(&app)[0];
    // A focused ✕ normalizes to its row for drawing (`docs/tui.md`,
    // "The session list"); clicking the drawn row opens that session.
    app.focus = Some(TargetId::Home(Spot::Stop(key)));
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let row = targets
        .iter()
        .find(|target| target.id == TargetId::Home(Spot::Entry(key)))
        .copied()
        .unwrap_or_else(|| panic!("the home row draws a click target"));
    assert_eq!(
        buf[(row.rect.x, row.rect.y)].bg,
        crate::theme::Role::Selection.color(),
        "Spot::Stop focuses its list row"
    );

    let col = row.rect.x.saturating_add(1);
    let row_y = row.rect.y;
    assert_eq!(
        crate::mouse::hit(&targets, col, row_y),
        Some(TargetId::Home(Spot::Entry(key)))
    );
    let now = fakes::clock::FakeClock::new().now();
    let mut pointer = Pointer::default();
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col,
        row: row_y,
    };
    assert_eq!(
        app.on_mouse(&press, &targets, &mut pointer, true, now),
        Effect::None
    );
    let effect = app.on_mouse(
        &Mouse {
            kind: MouseKind::Release,
            ..press
        },
        &targets,
        &mut pointer,
        true,
        now,
    );
    let Effect::Send(lines) = effect else {
        panic!("clicking the home row opens its session");
    };
    let subscribe: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
}

#[test]
fn a_click_on_a_row_opens_it() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = keys(&app)[0];
    match app.on_click(crate::mouse::TargetId::Home(Spot::Entry(key))) {
        Effect::Send(lines) => {
            assert_eq!(lines.len(), 1, "commands waits for subscribe acceptance");
            let subscribe: Value =
                serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(subscribe["command"], "subscribe");
            let ack = subscribe["id"]
                .as_str()
                .unwrap_or_else(|| panic!("subscribe id"));
            let sent = app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack));
            assert_eq!(sent.len(), 1);
            let commands: Value =
                serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(commands["command"], "commands");
            assert_eq!(commands["session_id"], "s_aaaaaaaaaaaaaaaa");
        }
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("a click opens the row"),
    }
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn a_click_on_a_key_with_no_row_does_nothing() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Entry(9999))),
        Effect::None
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn opening_requires_home() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let key = keys(&app).into_iter().next().unwrap_or(9999);
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.home_screen().is_none());
}

#[test]
fn opening_needs_the_link_up() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn opening_while_a_start_waits_does_nothing() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Enter, now);
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn an_unreadable_row_does_not_open_and_says_so() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    let mut newer = live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    );
    if let Line::Session(envelope) = &mut newer {
        envelope.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(newer);
    let key = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(app.home_click(Spot::Entry(key)), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(
        app.notice(),
        Some("Cannot attach: its status did not parse.")
    );
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_held"
            && text == "Cannot attach: its status did not parse."
    )));
}

#[test]
fn lines_before_the_acknowledgement_fold_the_row_and_skip_the_conversation() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    assert!(
        app.on_line(live(
            "s_aaaaaaaaaaaaaaaa",
            "tidy docs",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_empty()
    );
    // The session is not attached yet, so its turn start sends nothing.
    assert!(app.on_line(turn_started("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.on_line(review()).is_empty());
    assert_eq!(listed(&app), ["●  tidy docs"]);
    assert!(app.lines().is_empty());
    assert!(app.badge().is_none());
    assert!(app.panel().is_none());
}

#[test]
fn the_acknowledgement_and_lines_after_it_reach_the_conversation() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let summary = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", summary);
    // The staged open raises `full` after the first frame; only the
    // raise's acknowledgement ends the gate.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["args"]["level"], "full");
    let ack = raise["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack))
            .is_empty()
    );
    turn_started_answered(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
}

#[test]
fn an_approval_before_the_acknowledgement_is_not_queued() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    assert!(app.on_line(review()).is_empty());
    assert!(app.badge().is_none());
    assert!(app.panel().is_none());
}

#[test]
fn a_refused_open_goes_home_with_a_note_and_a_notice() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(hub_refused(ack, "session_held", "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(listed(&app), ["●  fix the parser  held by another process"]);
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_session_rejection_of_the_full_subscribe_does_the_same() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert_eq!(listed(&app), ["●  fix the parser  held by another process"]);
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_refused_open_after_going_home_keeps_the_draft() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    // Leaving mid-open lowers nothing: only `summary` is in flight.
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
    type_text(&mut app, "keep me");
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert_eq!(app.input().expand(), "keep me");
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_refusal_of_the_pending_subscribe_fails_the_reopen() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let first = open_first(&mut app);
    let stale = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"))
        .to_owned();
    // Leaving mid-open lowers nothing: only `summary` is in flight.
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
    // The reopen waits on the existing subscribe; its refusal sends no commands.
    assert!(open_first(&mut app).is_empty());
    assert!(app.take_raise().is_none());
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &stale,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert!(app.on_home());
    assert!(app.session().is_none());
    assert_eq!(app.notice(), Some("held by another process"));
    assert!(app.take_raise().is_none());
}

#[test]
fn a_refused_first_stage_leaves_the_level_and_the_next_open_sends_summary_again() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    let second = open_first(&mut app);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["args"]["level"], "summary");
    let ack = second[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
}

#[test]
fn a_rejected_pending_summary_fails_the_reopen() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let summary = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"))
        .to_owned();
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    // The reopen waits on the existing subscribe; its refusal sends no commands.
    assert!(open_first(&mut app).is_empty());
    assert!(app.take_raise().is_none());
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &summary,
            "invalid_arguments",
            "already at summary"
        ))
        .is_empty()
    );
    assert!(app.on_home());
    assert!(app.session().is_none());
    assert_eq!(app.notice(), Some("already at summary"));
    assert!(app.take_raise().is_none());
}

#[test]
fn a_refused_two_step_open_fails_the_open() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let lines = open_first(&mut app);
    assert!(lines.is_empty());
    assert!(app.take_raise().is_none());
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
    // The raise is the open's subscribe in flight: refusing it fails
    // the open.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["args"]["level"], "full");
    let ack = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &ack,
            "invalid_arguments",
            "already at full"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("already at full"));
}

#[test]
fn a_refused_one_step_open_fails_without_a_retry() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first.len(), 1);
    let first_stage = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first stage id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &first_stage,
            "invalid_arguments",
            "already at full"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(listed(&app), ["○  old work  already at full"]);
    assert_eq!(app.notice(), Some("already at full"));
}

#[test]
fn after_a_refused_open_the_next_open_sends_summary_raises_and_attaches() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    let first_stage = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first stage id"))
        .to_owned();
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            &first_stage,
            "invalid_arguments",
            "the log cannot be read"
        ))
        .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    let second = open_first(&mut app);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["command"], "subscribe");
    assert_eq!(second[0]["args"]["level"], "summary");

    let again = second[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("again id"))
        .to_owned();
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", &again);
    // The raise ends the gate; the replay then folds.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["args"]["level"], "full");
    let raised = raise["id"].as_str().unwrap_or_else(|| panic!("raised id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", raised))
            .is_empty()
    );
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    turn_started_answered(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(
        app.lines()
            .iter()
            .any(|line| line.to_string().contains("hi"))
    );
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // Held at full, leaving lowers it.
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("/home lowers");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["args"]["level"], "summary");
}

#[test]
fn leaving_a_session_that_left_sends_no_subscribe() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
}

#[test]
fn leaving_with_the_link_down_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.disconnected();
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    assert!(app.on_home());
}

#[test]
fn slash_close_sends_close_now_and_no_subscribe() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let Effect::Send(lines) = enter_text(&mut app, "/close") else {
        panic!("/close sends");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "close");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"], json!({"now": true}));
    assert!(app.on_home());
}

#[test]
fn home_without_home_state_sends_nothing_on_leave() {
    let mut app = App::new(PathBuf::from("/w"));
    assert_eq!(app.leave(), Effect::None);
}

#[test]
fn home_during_an_open_of_a_resuming_session_sends_no_lowering() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(first[0]["args"]["level"], "summary");
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let ack = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", &ack);
    // Nothing is held at `full`, so the live status lowers nothing.
    let out = app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    assert!(out.is_empty());
    assert!(app.session().is_none());
}

#[test]
fn an_accepted_full_for_the_attached_session_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
}

#[test]
fn no_summary_while_a_subscribe_is_in_flight() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let Effect::Send(lowered) = enter_text(&mut app, "/home") else {
        panic!("/home lowers");
    };
    assert_eq!(lowered.len(), 1);
    assert!(
        app.on_line(live(
            "s_aaaaaaaaaaaaaaaa",
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_empty()
    );
}

#[test]
fn no_summary_for_a_left_row() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &[("s_aaaaaaaaaaaaaaaa", "old work")]);
    let first = open_first(&mut app);
    assert_eq!(enter_text(&mut app, "/home"), Effect::None);
    let ack = first[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
    assert!(app.session().is_none());
}

#[test]
fn the_file_listing_follows_an_opened_row_outside_the_launch_workspace() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "lens work",
        "/other",
        "-other",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    assert_eq!(app.workspace(), Some(PathBuf::from("/other")));
    let ack = lines[0]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    accept_summary(&mut app, "s_aaaaaaaaaaaaaaaa", ack);
    assert_eq!(app.workspace(), Some(PathBuf::from("/other")));
}

#[test]
fn without_home_the_workspace_is_the_launch_directory() {
    let app = App::new(PathBuf::from("/w"));
    assert_eq!(app.workspace(), Some(PathBuf::from("/w")));
}

/// Renders home at 80x24 and takes its targets, so keys move among drawn
/// stops.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

/// Two live rows, the first streaming and the second idle.
fn two_rows(app: &mut App) {
    linked(app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
}

#[test]
fn home_arrows_focus_the_list_and_entry_bar() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn up_on_a_home_with_nothing_open_focuses_the_last_row_and_esc_drops_it() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.top_surface(), crate::app::Surface::Draft);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn typing_into_the_empty_draft_inserts_text_with_rows_present() {
    // Printable keys reach the draft while the box has focus: only ↓
    // enters the list, and j and k move only while it has focus.
    for key in ['j', 'k', 'c', 'x'] {
        let mut app = home_with(80, 24, launch());
        two_rows(&mut app);
        drawn(&mut app);
        let now = fakes::clock::FakeClock::new().now();
        assert_eq!(app.on_key(Key::Char(key), now), Effect::None);
        assert_eq!(app.focused(), None, "{key} keeps the box focused");
        assert_eq!(app.input().expand(), key.to_string());
    }
}

#[test]
fn down_with_a_draft_moves_in_the_draft() {
    // A one-line draft keeps its text while ↓ focuses the chip row.
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    type_text(&mut app, "x");
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.input().expand(), "x");
    // A two-line draft's ↓ moves the cursor first.
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    type_text(&mut app, "a");
    assert_eq!(app.on_edit(crate::keys::Edit::ShiftEnter), Effect::None);
    type_text(&mut app, "b");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "a\nb");
}

#[test]
fn down_with_the_search_panel_open_moves_its_selection() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let _ = app.on_key(Key::CtrlR, now);
    assert!(app.completions().is_some());
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.completions().is_some());
}

#[test]
fn down_with_no_rows_does_nothing() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // ↓ focuses the chip row, and a second ↓ changes nothing.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
}

#[test]
fn down_on_the_conversation_screen_is_unchanged() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn up_and_j_walk_the_row_stops() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    // Every click target is a focus stop: the row's ✕ comes next.
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Stop(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn j_does_not_move_from_a_row_before_the_last_drawn_stop() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let rows = [
        exited_in("s_aaaaaaaaaaaaaaaa", "alpha", "/work/alpha", "alpha"),
        exited_in("s_bbbbbbbbbbbbbbbb", "beta", "/work/beta", "beta"),
        exited_in("s_cccccccccccccccc", "gamma", "/work/gamma", "gamma"),
    ];
    // The unscoped answer fans out one scoped ask per project.
    let fanout = commands(app.on_line(accepted(&recent, json!({"sessions": rows}))));
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(fanout.len(), 3);
    let (_, targets) = drawn_frame(&app);
    let entries: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    // `docs/tui.md:254-255` (up to three exited rows draw).
    assert_eq!(entries.len(), 3, "all three exited rows draw");
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[2])))
    );
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[1])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Start(entries[2])))
    );
}

#[test]
fn j_from_the_last_list_row_clears_focus_without_skipping() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    for (session, name, workspace, project) in [
        ("s_aaaaaaaaaaaaaaaa", "alpha", "/work/alpha", "alpha"),
        ("s_bbbbbbbbbbbbbbbb", "beta", "/work/beta", "beta"),
        ("s_cccccccccccccccc", "gamma", "/work/gamma", "gamma"),
    ] {
        app.on_line(live(
            session,
            name,
            workspace,
            project,
            json!({"state": "idle"}),
        ));
        app.on_line(left(session, "crashed"));
    }
    let (_, targets) = drawn_frame(&app);
    let mut entries: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    entries.dedup();
    assert_eq!(entries.len(), 3, "all three crashed rows draw");
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(entries[2])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn up_focuses_the_last_loaded_row_and_scrolls() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    let all = keys(&app);
    assert_eq!(all.len(), 15);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(all[14])))
    );
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let drawn: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert!(drawn.contains(&all[14]));
    assert!(!drawn.contains(&all[0]));
}

#[test]
fn down_on_the_last_row_of_the_list_focuses_the_entry_bar() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
}

#[test]
fn slash_resume_from_a_session_focuses_the_first_exited_row_after_the_home_frame() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    // The conversation frame first, as the loop draws it.
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let exited = *keys(&app)
        .get(1)
        .expect("the exited row follows the live row");
    let row = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.by_key(exited))
        .unwrap_or_else(|| panic!("the exited row is present"));
    assert_eq!(row.left, Some(crate::home::Left::Exited));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(exited)))
    );
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter opens the focused row");
    };
    let lines: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_bbbbbbbbbbbbbbbb");
}

#[test]
fn slash_resume_with_no_rows_leaves_the_box_focused() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    drawn(&mut app);
    assert_eq!(enter_text(&mut app, "/resume"), Effect::None);
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn y_on_a_focused_row_copies_its_line() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("✓  fix the parser".to_owned())
    );
}

/// The chips home draws, as text left to right.
fn chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|chip| chip.text).collect())
        .unwrap_or_default()
}

/// Clicks the workspace chip: the picker opens browsing the chip's
/// workspace, asking for its one listing. Answers it with no
/// subdirectories and moves down onto the first recent.
fn open_on_recents(app: &mut App) {
    let Effect::Send(lines) = app.home_click(Spot::Workspace) else {
        panic!("the picker asks for its root's listing");
    };
    let asks = commands(lines);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["command"], "directories");
    let id = asks[0]["id"].as_str().unwrap_or_else(|| panic!("id"));
    app.on_line(accepted(id, json!({"directories": [], "path": "/listed"})));
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
}

/// The workspace picker home draws: its list and the selected index.
fn picker(app: &App) -> Option<(Vec<String>, usize)> {
    app.home_screen().and_then(|screen| {
        screen
            .picker
            .map(|picker| (picker.recents, picker.focus.unwrap_or(0)))
    })
}

/// Three live rows: one in the launch directory, two elsewhere, one
/// workspace twice.
fn three_workspaces(app: &mut App) {
    linked(app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "here",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "away",
        "/other",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_cccccccccccccccc",
        "again",
        "/other",
        "-w",
        json!({"state": "idle"}),
    ));
    app.on_line(live(
        "s_dddddddddddddddd",
        "third",
        "/third",
        "-w",
        json!({"state": "idle"}),
    ));
}

#[test]
fn the_chip_defaults_to_the_launch_directory() {
    let app = home_with(80, 24, launch());
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
}

#[test]
fn the_picker_lists_the_launch_directory_then_row_workspaces_once_each() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(
        picker(&app),
        Some((
            vec!["/w".to_owned(), "/other".to_owned(), "/third".to_owned(),],
            0,
        ))
    );
}

#[test]
fn the_picker_holds_at_most_ten() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    for n in 0..12u8 {
        let session = format!("s_{n:016x}");
        let workspace = format!("/w{n}");
        app.on_line(live(
            &session,
            "fix the parser",
            &workspace,
            "-w",
            json!({"state": "idle"}),
        ));
    }
    open_on_recents(&mut app);
    let (list, selected) = picker(&app).unwrap_or_else(|| panic!("the picker"));
    assert_eq!(selected, 0);
    assert_eq!(list.len(), 10);
    assert_eq!(list[0], "/w");
    assert_eq!(list[9], "/w8");
}

#[test]
fn picker_keys_move_choose_and_close() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(1));
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(2));
    // The selection clamps to the list.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(2));
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(1));
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(0));
    // Enter chooses the selected workspace for the next start.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[other]");
    // Esc closes the picker, keeping the workspace as it was.
    open_on_recents(&mut app);
    assert!(picker(&app).is_some());
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[other]");
}

#[test]
fn other_keys_do_nothing_in_the_picker() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert_eq!(app.on_key(Key::PageDown, now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(picker(&app).map(|(_, selected)| selected), Some(0));
    assert_eq!(app.focused(), None);
}

#[test]
fn ctrl_c_passes_through_the_picker() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(app.picker_key(&Key::CtrlC), None);
    assert!(picker(&app).is_some());
}

#[test]
fn clicking_plus_chooses_that_projects_workspace_and_focuses_the_entry_bar() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "other project work",
        "/other",
        "-other",
        json!({"state": "idle", "git": {"branch": "main"}}),
    ));
    let key = app
        .home_screen()
        .and_then(|screen| {
            screen.list.into_iter().find_map(|line| match line {
                crate::home::ListLine::Head {
                    name,
                    start: Some(key),
                    ..
                } if name == "other" => Some(key),
                crate::home::ListLine::Head { .. }
                | crate::home::ListLine::Live { .. }
                | crate::home::ListLine::ExitedHead { .. }
                | crate::home::ListLine::Exited { .. }
                | crate::home::ListLine::More { .. } => None,
            })
        })
        .unwrap_or_else(|| panic!("the other project's plus target"));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == crate::mouse::TargetId::Home(Spot::Start(key)) })
    );
    app.drawn(&targets);
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert!(chips(&app).contains(&"[x] new worktree".to_owned()));
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Worktree));
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Start(key))),
        Effect::None
    );
    assert_eq!(app.focused(), None, "the entry bar has focus");
    assert!(chips(&app).contains(&"[ ] new worktree".to_owned()));

    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start = commands(lines)[0].clone();
    assert_eq!(start["command"], "start");
    assert_eq!(start["args"]["workspace"], "/other");
    assert!(start["args"].get("worktree").is_none());
}

#[test]
fn start_names_the_chosen_workspace() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/other");
}

#[test]
fn a_click_on_a_picker_row_chooses_it() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(2)), Effect::None);
    assert_eq!(picker(&app), None);
    assert_eq!(chips(&app)[0], "[third]");
}

#[test]
fn a_click_past_the_picker_list_does_nothing() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(99)), Effect::None);
    assert!(picker(&app).is_some());
    assert_eq!(chips(&app)[0], "[w]");
}

#[test]
fn the_file_listing_follows_the_picked_workspace() {
    let mut app = home_with(80, 24, launch());
    three_workspaces(&mut app);
    assert_eq!(app.workspace(), Some(PathBuf::from("/w")));
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(app.workspace(), Some(PathBuf::from("/other")));
}

/// An app on home inside git: scoped to the launch project.
fn git_home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: true,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A waiting `session_status` for `session` in `workspace` of `project`.
fn waiting(session: &str, name: &str, workspace: &str, project: &str) -> Line {
    live(
        session,
        name,
        workspace,
        project,
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell"}}),
    )
}

#[test]
fn the_first_recent_page_names_no_project() {
    let mut app = git_home();
    let lines = commands(app.on_line(hello()));
    let id = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert_eq!(lines[1], json!({"id": id, "command": "recent"}));
}

#[test]
fn down_from_the_last_row_asks_the_next_page_before_the_last_loaded_recent_row() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // ↓ on the last row asks the next page at once.
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "recent");
    assert_eq!(line["args"]["before"], "s_bbbbbbbbbbbbbbbb");
    assert!(line["args"].get("project").is_none());
    // Focus stays on the last row's line, never its ✕.
    let row = keys(&app)[1];
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
}

#[test]
fn no_page_is_asked_from_a_feed_row() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // The `recent` page names only the feed row, so nothing lists, but
    // the answer was not empty.
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "fix the parser")]}),
    ));
    assert!(app.home.as_ref().is_some_and(|home| home.sessions.more()));
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
}

#[test]
fn down_from_the_last_row_asks_nothing_after_an_empty_page() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    // An empty older page ends paging.
    assert!(
        app.on_line(accepted(
            page["id"].as_str().unwrap_or_else(|| panic!("page id")),
            json!({"sessions": []})
        ))
        .is_empty()
    );
    assert!(app.home.as_ref().is_some_and(|home| !home.sessions.more()));
    // ↓ returns nothing with focus on the row's line.
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn enter_on_more_expands_in_place_and_focuses_the_first_revealed_row() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
        ],
    );
    let key = more_key(&app, "-w").expect("the fourth row is hidden");
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .any(|target| { target.id == crate::mouse::TargetId::Home(Spot::More(key)) })
    );
    app.drawn(&targets);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(key)))
    );
    let screen = app.home_screen().unwrap();
    assert!(screen.list.iter().any(|line| matches!(
        line,
        crate::home::ListLine::Exited { key: shown, .. } if *shown == key
    )));
    assert!(
        !screen
            .list
            .iter()
            .any(|line| matches!(line, crate::home::ListLine::More { .. }))
    );
}

#[test]
fn a_click_on_more_expands_the_same_project_only() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let rows = [
        ("s_aaaaaaaaaaaaaaaa", "a1", "/w", "-w"),
        ("s_bbbbbbbbbbbbbbbb", "a2", "/w", "-w"),
        ("s_cccccccccccccccc", "a3", "/w", "-w"),
        ("s_dddddddddddddddd", "a4", "/w", "-w"),
        ("s_eeeeeeeeeeeeeeee", "b1", "/other", "-other"),
        ("s_ffffffffffffffff", "b2", "/other", "-other"),
        ("s_1111111111111111", "b3", "/other", "-other"),
        ("s_2222222222222222", "b4", "/other", "-other"),
    ]
    .into_iter()
    .map(|(id, name, workspace, project)| exited_in(id, name, workspace, project))
    .collect::<Vec<_>>();
    // The unscoped answer fans out one scoped ask per project.
    let fanout = commands(app.on_line(accepted(&recent, json!({"sessions": rows}))));
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(fanout.len(), 2);
    let first_hidden = more_key(&app, "-w").expect("first project has more");
    let second_hidden = more_key(&app, "-other").expect("second project has more");
    assert_eq!(app.home_click(Spot::More(first_hidden)), Effect::None);
    let screen = app.home_screen().unwrap();
    let shown = screen
        .list
        .iter()
        .filter_map(|line| match line {
            crate::home::ListLine::Head { .. }
            | crate::home::ListLine::Live { .. }
            | crate::home::ListLine::ExitedHead { .. }
            | crate::home::ListLine::More { .. } => None,
            crate::home::ListLine::Exited { key, .. } => Some(*key),
        })
        .collect::<Vec<_>>();
    assert!(shown.contains(&first_hidden));
    assert!(!shown.contains(&second_hidden));
    assert_eq!(
        screen
            .list
            .iter()
            .filter_map(|line| match line {
                crate::home::ListLine::Head { .. }
                | crate::home::ListLine::Live { .. }
                | crate::home::ListLine::ExitedHead { .. }
                | crate::home::ListLine::Exited { .. } => None,
                crate::home::ListLine::More { key, .. } => Some(*key),
            })
            .collect::<Vec<_>>(),
        [second_hidden]
    );
}

#[test]
fn y_on_more_copies_its_label() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
        ],
    );
    let key = more_key(&app, "-w").expect("the fourth row is hidden");
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("… 1 more".to_owned())
    );
}

#[test]
fn y_on_more_copies_a_positive_count_but_not_zero() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
            ("s_eeeeeeeeeeeeeeee", "fifth"),
        ],
    );
    let key = more_key(&app, "-w").expect("two rows are hidden");
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(
        app.on_key(Key::Char('y'), now),
        Effect::Copy("… 2 more".to_owned())
    );

    app.home.as_mut().unwrap().sessions.expand("-w");
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    assert_eq!(app.on_key(Key::Char('y'), now), Effect::None);
}

#[test]
fn slash_resume_with_live_rows_but_no_exited_rows_leaves_the_box_focused() {
    let mut app = git_home();
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(waiting("s_bbbbbbbbbbbbbbbb", "away", "/lens", "-other"));
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "here",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(app.focused(), None);
}

/// The blocker lines home draws above the box.
fn blockers(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.blockers)
        .unwrap_or_default()
}

/// Sends `text` as a `start` and answers it with `session`.
fn start_rejected(app: &mut App, id: &str, message: &str) {
    assert!(
        app.on_line(hub_refused(id, "start_failed", message))
            .is_empty()
    );
}

#[test]
fn a_rejected_start_sets_the_blocker_lines() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    let message = "No provider key.\nRun fiber login to fix it.";
    start_rejected(&mut app, id, message);
    assert_eq!(
        blockers(&app),
        ["No provider key.", "Run fiber login to fix it."]
    );
    // The refusal stays a notice as today.
    assert_eq!(app.notice(), Some(message));
    // Answering `recent` fills the list around the blockers.
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
    ));
    assert_eq!(
        blockers(&app),
        ["No provider key.", "Run fiber login to fix it."]
    );
    assert_eq!(rows(&app), ["○  old work"]);
}

#[test]
fn a_rejected_prompt_sets_none() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    let Effect::Send(lines) = enter_text(&mut app, "yo") else {
        panic!("Enter sends the prompt");
    };
    let prompt: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("prompt: {err}"));
    let id = prompt["id"].as_str().unwrap_or_else(|| panic!("prompt id"));
    assert!(
        app.on_line(hub_refused(id, "invalid_arguments", "bad prompt"))
            .is_empty()
    );
    assert!(
        app.home
            .as_ref()
            .is_some_and(|home| home.blockers.is_empty())
    );
}

#[test]
fn the_next_start_clears_them() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    start_rejected(
        &mut app,
        start["id"].as_str().unwrap_or_else(|| panic!("start id")),
        "No provider key.",
    );
    assert_eq!(blockers(&app), ["No provider key."]);
    let Effect::Send(_) = enter_text(&mut app, "again") else {
        panic!("Enter tries again");
    };
    assert!(blockers(&app).is_empty());
}

#[test]
fn the_feed_arriving_keeps_them() {
    let mut app = home_with(80, 24, launch());
    let (feed, recent) = linked(&mut app);
    let Effect::Send(lines) = enter_text(&mut app, "hi") else {
        panic!("Enter sends the start");
    };
    let start: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    start_rejected(
        &mut app,
        start["id"].as_str().unwrap_or_else(|| panic!("start id")),
        "No provider key.",
    );
    assert!(app.on_line(accepted(&feed, json!({}))).is_empty());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_bbbbbbbbbbbbbbbb", "old work")]}),
    ));
    assert_eq!(blockers(&app), ["No provider key."]);
}

#[test]
fn x_on_an_unsubscribed_live_row_sends_summary_then_close_now() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let lines = stop(&mut app);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"]["level"], "summary");
    assert_eq!(lines[1]["command"], "close");
    assert_eq!(lines[1]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[1]["args"], json!({"now": true}));
    assert_ne!(lines[0]["id"], lines[1]["id"]);
}

#[test]
fn x_on_a_subscribed_row_sends_close_now_only() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let first = stop(&mut app);
    let summary = first[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", summary))
            .is_empty()
    );
    let lines = stop(&mut app);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "close");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"], json!({"now": true}));
}

#[test]
fn an_accepted_stop_drops_its_kept_line() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let lines = stop(&mut app);
    let close = lines
        .iter()
        .find(|line| line["command"] == "close")
        .unwrap_or_else(|| panic!("the ✕ closes: {lines:?}"));
    let id = close["id"].as_str().unwrap_or_else(|| panic!("an id"));
    // The loop writes the close: a reconnect would send it again.
    app.outbox.wrote(&close.to_string());
    assert!(app.outbox.is_kept(id));
    app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", id));
    assert!(!app.outbox.is_kept(id));
}

#[test]
fn take_ask_for_an_unknown_command_keeps_its_pending_entry() {
    // Every command answer passes `take_ask` on its way through home:
    // only a taken ask settles anything.
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    app.outbox.insert(
        "c_1".to_owned(),
        crate::app::Kind::Start,
        crate::input::Draft::default(),
    );
    assert!(app.take_ask("c_1").is_none());
    assert!(app.outbox.pending.contains_key("c_1"));
}

#[test]
fn the_foot_names_the_key_maps_bound_key() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!("f2"));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F2 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_the_key_map_out_when_unbound() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · Ctrl+C twice to quit"
    );
}

#[test]
fn x_with_the_link_down_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    app.connect_failed("gone".to_owned());
    app.on_line(status("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn a_refused_close_notes_the_row() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let lines = stop(&mut app);
    let close = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"));
    let message = "Session `s_aaaaaaaaaaaaaaaa` is held.";
    assert!(
        app.on_line(session_refused(
            "s_aaaaaaaaaaaaaaaa",
            close,
            "session_held",
            message
        ))
        .is_empty()
    );
    assert_eq!(
        listed(&app),
        [
            format!("●  fix the parser  {message}"),
            "✓  tidy docs".to_owned(),
        ]
    );
    assert_eq!(app.notice(), Some(message));
}

#[test]
fn x_on_an_exited_row_asks_to_delete() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn backspace_on_a_focused_exited_row_asks() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn delete_on_a_focused_crashed_row_asks() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [crashed("s_aaaaaaaaaaaaaaaa", "dead work")]}),
    ));
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert_eq!(
        question(&app),
        "Delete dead work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn backspace_on_a_focused_live_row_does_nothing() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn backspace_with_focus_on_another_stop_is_still_swallowed() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Workspace));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert!(!app.quit_open());
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
    assert!(app.input().expand().is_empty());
}

#[test]
fn enter_sends_delete_without_cascade() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(lines[0]["args"]["session"], "s_aaaaaaaaaaaaaaaa");
    assert!(lines[0]["args"].get("cascade").is_none());
}

#[test]
fn esc_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

/// Draws `app` at 80x24, returning its text and targets.
fn drawn_frame(app: &App) -> (String, Vec<crate::mouse::Target>) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

#[test]
fn clicking_the_drawn_cross_keeps_the_session() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    // The draw holds exactly one ✕ target, ending the title row.
    let (text, targets) = drawn_frame(&app);
    let title = text
        .lines()
        .position(|row| row.contains("Delete session"))
        .unwrap_or_else(|| panic!("a title row"));
    let keep: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
        .collect();
    assert_eq!(keep.len(), 1, "one ✕ target");
    assert_eq!(keep[0].rect.y, u16::try_from(title).unwrap_or(u16::MAX));
    assert_eq!((keep[0].rect.width, keep[0].rect.height), (1, 1));
    let row = text
        .lines()
        .nth(title)
        .unwrap_or_else(|| panic!("a title row"));
    let before: String = row.chars().take(keep[0].rect.x as usize).collect();
    assert!(
        before.contains("Delete session"),
        "the ✕ ends the title: {row}"
    );
    assert_eq!(
        row.chars().nth(keep[0].rect.x as usize),
        Some('✕'),
        "the target is the ✕: {row}"
    );
    // Clicking it closes the question without sending a command.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "the question closes");
    assert_eq!(
        app.home_screen()
            .map(|screen| crate::home::home_test_rows(&screen).len())
            .unwrap_or_default(),
        1,
        "the session row remains"
    );
}

#[test]
fn clicking_the_cross_on_the_cascade_question_keeps_everything() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    let (_, targets) = drawn_frame(&app);
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
            .count(),
        1,
        "one ✕ target"
    );
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "the question closes");
    assert_eq!(
        app.home_screen()
            .map(|screen| crate::home::home_test_rows(&screen).len())
            .unwrap_or_default(),
        1,
        "the session row remains"
    );
}

#[test]
fn a_stale_keep_click_changes_nothing() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    assert!(question(&app).is_empty(), "no question is open");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(question(&app).is_empty(), "no question opens");
}

#[test]
fn a_keep_click_leaves_the_quit_question_open() {
    let mut app = home_with(80, 24, launch());
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "busy work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.quit();
    assert!(app.quit_open(), "the quit question is open");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Keep)),
        Effect::None
    );
    assert!(app.quit_open(), "the quit question stays open");
}

#[test]
fn clicks_on_the_delete_frame_do_nothing() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let asked = question(&app);
    assert!(!asked.is_empty(), "the question is open");
    let (text, targets) = drawn_frame(&app);
    // One slab-wide target over every frame row, under the ✕.
    let slabs: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Question))
        .collect();
    assert_eq!(slabs.len(), 1, "one slab target");
    let slab = slabs[0].rect;
    let rows: Vec<&str> = text.lines().collect();
    let title = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Delete session"))
            .unwrap_or_else(|| panic!("a title row")),
    )
    .unwrap_or(u16::MAX);
    let body = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Delete old work"))
            .unwrap_or_else(|| panic!("a body row")),
    )
    .unwrap_or(u16::MAX);
    let footer = u16::try_from(
        rows.iter()
            .position(|row| row.contains("Enter delete · Esc keep"))
            .unwrap_or_else(|| panic!("a footer row")),
    )
    .unwrap_or(u16::MAX);
    // The title row away from the ✕, both edge rows, a pad row, a
    // body row and the footer row all hit the slab, and clicking
    // opens nothing and replaces no question.
    for (col, row) in [
        (slab.x.saturating_add(2), title),
        (slab.x.saturating_add(5), slab.y),
        (slab.x.saturating_add(5), slab.y.saturating_add(1)),
        (slab.x.saturating_add(5), body),
        (slab.x.saturating_add(5), footer),
        (slab.x.saturating_add(5), slab.bottom().saturating_sub(1)),
    ] {
        assert_eq!(
            crate::mouse::hit(&targets, col, row),
            Some(crate::mouse::TargetId::Home(Spot::Question)),
            "a frame cell at {col},{row} hits the slab"
        );
        assert_eq!(
            app.on_click(crate::mouse::TargetId::Home(Spot::Question)),
            Effect::None
        );
    }
    assert_eq!(question(&app), asked, "the question stays");
    assert!(app.session().is_none(), "no session opens");
}

#[test]
fn hovering_the_delete_slab_tints_nothing() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut plain = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut plain, None);
    let slab = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Question))
        .unwrap_or_else(|| panic!("a slab target"))
        .rect;
    let keep = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Keep))
        .unwrap_or_else(|| panic!("a ✕ target"))
        .rect;
    // A body cell under the pointer draws as without one.
    let mut hovered = ratatui::buffer::Buffer::empty(area);
    crate::view::render(
        &app,
        area,
        &mut hovered,
        Some((slab.x.saturating_add(5), slab.y.saturating_add(4))),
    );
    assert_eq!(hovered, plain, "no lift on the slab");
    // The ✕ still lifts, so the comparison above is sensitive.
    let mut crossed = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut crossed, Some((keep.x, keep.y)));
    assert_ne!(crossed, plain, "the ✕ lifts");
}

#[test]
fn other_keys_do_nothing_in_the_question() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn ctrl_c_passes_through_the_question() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
    assert!(app.hint());
}

#[test]
fn up_and_down_scroll_the_delete_question() {
    let mut app = home_with(80, 24, launch());
    // A small screen: the cascade question wraps past what fits, so the
    // offset scrolls by one row.
    app.set_size(60, 12);
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    // Down and Up scroll the wrapped rows past the screen: Down toward
    // the later rows, holding at the last one, Up back toward the
    // first, holding at the top.
    assert_eq!(question_scroll(&app), 0);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(question_scroll(&app), 1);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(question_scroll(&app), 1);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(question_scroll(&app), 0);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(question_scroll(&app), 0);
    assert!(!question(&app).is_empty(), "the question stays open");
}

#[test]
fn an_accepted_delete_removes_the_row_and_asks_recent_again() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let lines = commands(app.on_line(accepted(&delete, json!({}))));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "recent");
    assert!(lines[0].get("args").is_none());
    assert_eq!(rows(&app), ["○  older work"]);
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn dependents_ask_again_naming_them_and_enter_sends_cascade_with_expect() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too.";
    assert!(
        app.on_line(hub_refused(&delete, "session_has_dependents", message))
            .is_empty()
    );
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 2 sessions that continue it: \
        s_1111111111111111, s_2222222222222222? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "delete");
    assert_eq!(
        lines[0]["args"],
        json!({
            "session": "s_0123456789abcdef",
            "cascade": true,
            "expect": [
                "s_0123456789abcdef",
                "s_1111111111111111",
                "s_2222222222222222",
            ],
        })
    );
}

#[test]
fn a_stale_cascade_asks_again_with_the_new_set() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    // One other session reads singular.
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 1 session that continues it: \
        s_1111111111111111? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let cascade = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("cascade id"))
        .to_owned();
    let message = "The sessions this delete would remove are now `s_0123456789abcdef`, \
        `s_1111111111111111`, `s_3333333333333333`. Nothing was deleted.";
    assert!(
        app.on_line(hub_refused(&cascade, "stale_request", message))
            .is_empty()
    );
    assert_eq!(
        question(&app),
        "Delete fix the parser (s_0123456789abcdef) and 2 sessions that continue it: \
        s_1111111111111111, s_3333333333333333? It cannot be undone · Enter delete all · Esc keep"
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let lines = commands(lines);
    assert_eq!(
        lines[0]["args"]["expect"],
        json!([
            "s_0123456789abcdef",
            "s_1111111111111111",
            "s_3333333333333333",
        ])
    );
}

#[test]
fn a_dependents_refusal_with_no_other_ids_notes_the_row() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_0123456789abcdef`. `--cascade` deletes them too.";
    assert!(
        app.on_line(hub_refused(&delete, "session_has_dependents", message))
            .is_empty()
    );
    assert_eq!(listed(&app), [format!("○  fix the parser  {message}")]);
    assert_eq!(app.notice(), Some(message));
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
    // The notice carries the refusal's own code, not a default.
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_has_dependents" && text == message
    )));
}

#[test]
fn a_refusal_code_is_the_payload_code_or_malformed() {
    // An empty code is no code: the notice then reads `malformed`.
    let payload = |value: Value| value.as_object().cloned().unwrap_or_default();
    assert_eq!(
        super::refusal_code(&payload(json!({"code": "session_held"}))),
        "session_held"
    );
    assert_eq!(
        super::refusal_code(&payload(json!({"code": ""}))),
        "malformed"
    );
    assert_eq!(super::refusal_code(&payload(json!({}))), "malformed");
}

#[test]
fn a_refused_cascade_delete_notes_the_row() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_0123456789abcdef", "fix the parser");
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(
            &delete,
            "session_has_dependents",
            "Session `s_0123456789abcdef` has sessions that continue it: \
            `s_1111111111111111`. `--cascade` deletes them too."
        ))
        .is_empty()
    );
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes all");
    };
    let cascade = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("cascade id"))
        .to_owned();
    let message = "Session `s_0123456789abcdef` is held.";
    assert!(
        app.on_line(hub_refused(&cascade, "session_held", message))
            .is_empty()
    );
    assert_eq!(listed(&app), [format!("○  fix the parser  {message}")]);
    assert_eq!(app.notice(), Some(message));
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn unreadable_rows_have_no_x() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let mut envelope = live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    );
    if let crate::link::Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited("s_cccccccccccccccc", "old work")]}),
    ));
    // A readable live row ends in a ✕; exited and unreadable rows have
    // none.
    let screen = app.home_screen().unwrap_or_else(|| panic!("home screen"));
    let rows = crate::home::home_test_rows(&screen);
    let has_x: Vec<bool> = screen
        .list
        .iter()
        .filter_map(|line| match line {
            crate::home::ListLine::Live { stop, .. } => Some(*stop),
            crate::home::ListLine::Exited { .. } => Some(false),
            crate::home::ListLine::Head { .. }
            | crate::home::ListLine::ExitedHead { .. }
            | crate::home::ListLine::More { .. } => None,
        })
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(has_x[0]);
    assert!(!has_x[1]);
    assert!(!has_x[2]);
}

#[test]
fn x_on_an_unreadable_row_says_so() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    let mut envelope = status("s_aaaaaaaaaaaaaaaa");
    if let crate::link::Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    assert_eq!(click_stop(&mut app), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Cannot attach: its status did not parse.")
    );
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_held"
            && text == "Cannot attach: its status did not parse."
    )));
}

#[test]
fn enter_with_the_link_down_keeps_the_question() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    app.disconnected();
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn an_accepted_close_changes_nothing() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    let lines = stop(&mut app);
    let close = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("close id"));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", close))
            .is_empty()
    );
    assert_eq!(listed(&app), ["●  fix the parser", "✓  tidy docs"]);
    assert_eq!(app.notice(), None);
}

#[test]
fn backspace_with_the_box_focused_edits_the_draft() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    type_text(&mut app, "x");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert!(app.input().expand().is_empty());
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn delete_edit_in_the_question_does_nothing() {
    let mut app = home_with(80, 24, launch());
    exited_row(&mut app, "s_aaaaaaaaaaaaaaaa", "old work");
    click_stop(&mut app);
    assert_eq!(app.on_edit(crate::keys::Edit::Delete), Effect::None);
    assert_eq!(
        question(&app),
        "Delete old work (s_aaaaaaaaaaaaaaaa)? It cannot be undone · Enter delete · Esc keep"
    );
}

#[test]
fn stop_on_a_key_with_no_row_does_nothing() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    assert_eq!(app.home_click(Spot::Stop(999)), Effect::None);
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn y_on_a_focused_x_copies_nothing() {
    let mut app = home_with(80, 24, launch());
    two_rows(&mut app);
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // ↑ focuses the last row, then another ↑ reaches the first; j steps
    // onto its ✕.
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Stop(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('y'), now), Effect::None);
}

#[test]
fn a_crashed_left_marks_the_known_row_crashed() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // A crash marks the known row in place; the other row keeps working.
    assert!(
        app.on_line(left("s_aaaaaaaaaaaaaaaa", "crashed"))
            .is_empty()
    );
    assert_eq!(rows(&app), ["✗  fix the parser", "✓  tidy docs"]);
}

#[test]
fn an_accepted_hub_acknowledgement_ends_the_open() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    let lines = open_first(&mut app);
    let ack = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("ack id"))
        .to_owned();
    // Accepting the open's last subscribe ends the gate: the session
    // stays attached, with no note and no notice.
    assert!(app.on_line(accepted(&ack, json!({}))).is_empty());
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
    assert!(app.notice().is_none());
}

#[test]
fn statuses_gate_only_the_opening_session() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    open_first(&mut app);
    // While the first row opens, its statuses fold into its row and are
    // dropped; another session's statuses still reach the conversation.
    assert!(
        app.home_line(&live(
            "s_aaaaaaaaaaaaaaaa",
            "renamed",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_some()
    );
    assert!(
        app.home_line(&live(
            "s_bbbbbbbbbbbbbbbb",
            "renamed",
            "/w",
            "-w",
            json!({"state": "streaming"}),
        ))
        .is_none()
    );
}

#[test]
fn a_rejected_lower_while_home_sends_no_subscribe() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    // The second row opens at `summary`, then raises to `full`;
    // going home lowers it, with the lowering subscribe in flight.
    let second = keys(&app)[1];
    let lines = open(&mut app, second);
    let first_stage = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first stage id"))
        .to_owned();
    accept_summary(&mut app, "s_bbbbbbbbbbbbbbbb", &first_stage);
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["args"]["level"], "full");
    let raised = raise["id"].as_str().unwrap_or_else(|| panic!("raised id"));
    assert!(
        app.on_line(session_accepted("s_bbbbbbbbbbbbbbbb", raised))
            .is_empty()
    );
    let Effect::Send(lines) = enter_text(&mut app, "/home") else {
        panic!("going home lowers");
    };
    assert_eq!(lines.len(), 1);
    let lower: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(lower["command"], "subscribe");
    assert_eq!(lower["args"]["level"], "summary");
    let lowering = lower["id"]
        .as_str()
        .unwrap_or_else(|| panic!("lowering id"))
        .to_owned();
    // Refusing the lowering while home sends nothing more: the accepted
    // level stays `full` without another subscribe.
    assert!(
        app.on_line(session_refused(
            "s_bbbbbbbbbbbbbbbb",
            &lowering,
            "session_held",
            "held by another process"
        ))
        .is_empty()
    );
}

#[test]
fn home_rows_include_an_unscoped_project_below_the_fold() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    for n in 0..9u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    // The tenth row lives in another project: outside git it still
    // lists, below the fold.
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "away",
        "/lens",
        "-other",
        json!({"state": "idle"}),
    ));
    for n in 9..14u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            json!({"state": "idle"}),
        ));
    }
    let screen = app
        .home_screen()
        .unwrap_or_else(|| panic!("the home screen"));
    assert_eq!(crate::home::home_test_rows(&screen).len(), 15);
    assert!(
        crate::home::home_test_rows(&screen)
            .iter()
            .any(|(_, row)| row.contains("away")),
        "the other project's session remains visible"
    );
}

#[test]
fn an_accepted_delete_closes_its_question() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    app.on_line(accepted(&delete, json!({})));
    assert_eq!(rows(&app), ["○  older work"]);
    // Its answer closes the question it asked about: the prompt clears,
    // so the drawn question goes with it.
    assert!(
        app.home
            .as_ref()
            .is_some_and(|home| home.questions.prompt().is_none())
    );
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.question.is_none())
    );
}

#[test]
fn an_answer_for_another_delete_keeps_the_newer_question() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    // A newer question asks about the second row; the first row's answer
    // leaves it open.
    let second = keys(&app)[1];
    app.home_click(Spot::Stop(second));
    app.on_line(accepted(&delete, json!({})));
    assert_eq!(rows(&app), ["○  older work"]);
    assert!(
        question(&app).contains("s_bbbbbbbbbbbbbbbb"),
        "the newer question stays open"
    );
}

#[test]
fn an_accepted_delete_asks_the_first_page_unscoped() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited("s_bbbbbbbbbbbbbbbb", "older work"),
        ]}),
    ));
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(lines)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let lines = commands(app.on_line(accepted(&delete, json!({}))));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "recent");
    assert!(lines[0].get("args").is_none());
}

#[test]
fn the_next_page_in_git_is_unscoped() {
    let mut app = git_home();
    let (_, recent) = linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "recent");
    assert_eq!(line["args"], json!({"before": "s_bbbbbbbbbbbbbbbb"}));
}

/// A live `session_status` for `session` in `workspace`, in git on
/// `main`.
fn git_live(session: &str, name: &str, workspace: &str) -> Line {
    live(
        session,
        name,
        workspace,
        "-w",
        json!({"state": "idle", "git": {"branch": "main"}}),
    )
}

/// Whether home draws the new worktree switch.
fn shows_switch(app: &App) -> bool {
    chips(app).iter().any(|chip| chip.contains("new worktree"))
}

#[test]
fn the_switch_is_hidden_outside_git() {
    // Even with a git row in the launch workspace: outside git the
    // launch directory decides, and it is not in git.
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "fix the parser", "/w"));
    assert!(!shows_switch(&app));
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
}

#[test]
fn a_chosen_row_workspace_takes_its_git_flag() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "plain work",
        "/plain-ws",
        "-w",
        json!({"state": "idle"}),
    ));
    // The launch directory is in git, so the switch shows, off.
    assert_eq!(
        chips(&app),
        [
            "[w]",
            "[ ] new worktree",
            "[test/model]",
            "[thinking: default]",
            "Enter starts a session",
        ]
    );
    // Turning it on, then choosing the workspace without git: hidden,
    // and the choice turns the switch off.
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[plain-ws]");
    assert!(!shows_switch(&app));
    // Choosing the workspace with git shows the switch again, off: the
    // earlier choice turned it off.
    open_on_recents(&mut app);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn choosing_the_launch_workspace_with_no_rows_keeps_the_switch() {
    // No row names the launch directory, so reading the feed would
    // lose the flag: the choice keeps the launch flag.
    let mut app = git_home();
    linked(&mut app);
    assert!(shows_switch(&app));
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[w]");
    assert!(shows_switch(&app));
}

#[test]
fn a_chosen_workspace_keeps_the_switch_after_its_row_leaves() {
    // The flag is read when the workspace is chosen, so dropping its
    // backing row keeps the switch.
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert!(shows_switch(&app));
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .sessions
        .remove(&contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(rows(&app).is_empty());
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert!(shows_switch(&app));
}

#[test]
fn outside_git_choosing_a_plain_workspace_hides_the_switch() {
    // The launch directory is outside git, and a git row elsewhere
    // must not leak its flag into the chosen workspace.
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "plain work",
        "/plain-ws",
        "-w",
        json!({"state": "idle"}),
    ));
    assert!(!shows_switch(&app));
    open_on_recents(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[0], "[plain-ws]");
    assert!(!shows_switch(&app));
}

#[test]
fn clicking_a_picker_row_resets_the_switch_off() {
    let mut app = git_home();
    linked(&mut app);
    app.on_line(git_live("s_aaaaaaaaaaaaaaaa", "git work", "/git-ws"));
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    open_on_recents(&mut app);
    assert_eq!(app.home_click(Spot::Pick(1)), Effect::None);
    assert_eq!(chips(&app)[0], "[git-ws]");
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn clicking_the_switch_toggles_it() {
    let mut app = git_home();
    assert_eq!(chips(&app)[1], "[ ] new worktree");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Worktree)),
        Effect::None
    );
    assert_eq!(chips(&app)[1], "[x] new worktree");
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Home(Spot::Worktree)),
        Effect::None
    );
    assert_eq!(chips(&app)[1], "[ ] new worktree");
    // The switch copies and opens nothing.
    assert_eq!(app.home_text(Spot::Worktree), None);
}

#[test]
fn enter_on_the_focused_switch_toggles_it() {
    let mut app = git_home();
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::Worktree));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[1], "[x] new worktree");
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(chips(&app)[1], "[ ] new worktree");
}

#[test]
fn start_sends_worktree_only_when_the_switch_is_on_and_shown() {
    // Shown and on: the key goes out with the picked workspace.
    let mut app = git_home();
    app.on_line(hello());
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    assert_eq!(line["command"], "start");
    assert_eq!(line["args"]["workspace"], "/w");
    assert_eq!(line["args"]["worktree"], true);
    // Shown and off: the key is absent.
    let mut app = git_home();
    assert!(app.start_args().get("worktree").is_none());
    // Hidden, even with the switch on: the key is absent.
    let mut app = home_with(80, 24, launch());
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let args = app.start_args();
    assert_eq!(args["workspace"], "/w");
    assert!(args.get("worktree").is_none());
}

#[test]
fn start_carries_the_terminal_layer_in_overrides() {
    // No layer, no key: the shape every existing start keeps.
    let mut app = home_with(80, 24, launch());
    let bare = app.start_args();
    assert_eq!(bare["workspace"], "/w");
    assert!(bare.get("overrides").is_none());
    // The layer rides in order, beside the workspace.
    app.set_launch_overrides(vec![
        "model=acme/m".to_owned(),
        "retry.attempts=2".to_owned(),
    ]);
    let args = app.start_args();
    assert_eq!(args["workspace"], "/w");
    assert_eq!(
        args["overrides"],
        json!(["model=acme/m", "retry.attempts=2"])
    );
    assert!(args.get("worktree").is_none());
    // Beside the worktree switch: both keys ride.
    let mut app = git_home();
    app.launch_overrides = vec!["model=acme/m".to_owned()];
    assert_eq!(app.home_click(Spot::Worktree), Effect::None);
    let args = app.start_args();
    assert_eq!(args["overrides"], json!(["model=acme/m"]));
    assert_eq!(args["worktree"], true);
}

/// Focuses the last list stop from the entry bar.
fn focus_last(app: &mut App) {
    drawn(app);
    let last = keys(app)
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("a row"));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(last)))
    );
}

/// The foot home draws.
fn foot(app: &App) -> String {
    app.home_screen()
        .map(|screen| {
            screen
                .foot
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// The delete question home draws, unwrapped.
fn question(app: &App) -> String {
    app.home_screen()
        .and_then(|screen| screen.question)
        .unwrap_or_default()
}

/// The delete question's scroll offset: Up shows earlier rows, Down
/// later ones.
fn question_scroll(app: &App) -> usize {
    app.home_screen()
        .map(|screen| screen.question_scroll)
        .unwrap_or_default()
}

/// Clicks the first row's ✕, with the parsed lines going out.
fn stop(app: &mut App) -> Vec<Value> {
    let key = keys(app)[0];
    match app.home_click(Spot::Stop(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("the ✕ sends"),
    }
}

/// Clicks the first row's ✕.
fn click_stop(app: &mut App) -> Effect {
    let key = keys(app)[0];
    app.home_click(Spot::Stop(key))
}

/// One crashed `recent` row for `session`.
fn crashed(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "crashed",
    })
}

/// An exited row from `recent`, by `session`.
fn exited_row(app: &mut App, session: &str, name: &str) {
    let (_, recent) = linked(app);
    app.on_line(accepted(
        &recent,
        json!({"sessions": [exited(session, name)]}),
    ));
}

#[test]
fn the_foot_names_the_default_keys() {
    let app = home_with(80, 24, launch());
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_bolds_each_key_and_dims_the_rest() {
    let app = home_with(80, 24, launch());
    let screen = app.home_screen().expect("home draws");
    let bold: Vec<String> = screen
        .foot
        .spans
        .iter()
        .filter(|span| {
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        })
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(bold, ["Enter", "↑↓", "Delete", "F1", "Ctrl+C"]);
    for span in &screen.foot.spans {
        if bold.contains(&span.content.to_string()) {
            assert!(
                !span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::DIM),
                "keys are bold, not dim: {}",
                span.content
            );
        } else {
            assert!(
                span.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::DIM),
                "labels and joiners are dim: {}",
                span.content
            );
            assert!(
                !span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD),
                "only keys are bold: {}",
                span.content
            );
        }
    }
}

#[test]
fn the_foot_uses_rebound_labels_in_their_slots() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!("f2"));
    user.insert("open_focused".to_owned(), json!("ctrl+o"));
    user.insert("toggle_ledgers".to_owned(), json!([]));
    user.insert("delete_session".to_owned(), json!("x"));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Ctrl+O opens the focused session, or starts one from the box · ↑↓ move · x stops a live one, deletes an exited one · F2 the key map · Ctrl+C twice to quit"
    );
    let screen = app.home_screen().expect("home draws");
    let bold: Vec<String> = screen
        .foot
        .spans
        .iter()
        .filter(|span| {
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        })
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(bold, ["Ctrl+O", "↑↓", "x", "F2", "Ctrl+C"]);
}

#[test]
fn the_foot_lists_only_the_first_key_of_a_multi_key_list() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("delete_session".to_owned(), json!(["f7", "f8"]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · F7 stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_open_focused_out_when_unbound() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("open_focused".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_delete_session_out_when_unbound() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("delete_session".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_unbound_pairs_out_without_stray_joiners() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    user.insert("open_focused".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "↑↓ move · Delete stops a live one, deletes an exited one · Ctrl+C twice to quit"
    );
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    user.insert("delete_session".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Ctrl+C twice to quit"
    );
    let mut user = serde_json::Map::new();
    user.insert("open_focused".to_owned(), json!([]));
    user.insert("delete_session".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(
        foot(&app),
        "↑↓ move · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_with_nothing_bound_shows_only_move_and_quit() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    user.insert("open_focused".to_owned(), json!([]));
    user.insert("delete_session".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(foot(&app), "↑↓ move · Ctrl+C twice to quit");
}

#[test]
fn the_foot_shows_the_plain_quit_hint_while_armed() {
    let mut app = home_with(80, 24, launch());
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlC, now);
    let screen = app.home_screen().expect("home draws");
    assert_eq!(screen.foot.spans.len(), 1);
    let span = &screen.foot.spans[0];
    assert_eq!(span.content.as_ref(), crate::app::QUIT_HINT);
    assert!(
        !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert!(
        !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::DIM)
    );
}

/// Attaches `session` with its live row fed, then goes `/home`: the
/// back key is live for it.
fn home_from(app: &mut App, session: &str) {
    start_session(app, session);
    app.on_line(live(
        session,
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    assert!(matches!(enter_text(app, "/home"), Effect::Send(_)));
    assert!(app.on_home());
}

#[test]
fn the_foot_names_the_back_key_after_home_from_a_live_session() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · → or Esc back to the session · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_names_the_rebound_back_key() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("close_or_interrupt".to_owned(), json!("f5"));
    app.set_keys(crate::KeysSetup { user });
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · → or F5 back to the session · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_leaves_the_back_key_out_when_unbound() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("close_or_interrupt".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · → back to the session · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn the_foot_keeps_quit_named_when_the_key_map_is_unbound() {
    let mut app = home_with(80, 24, launch());
    let mut user = serde_json::Map::new();
    user.insert("key_map".to_owned(), json!([]));
    user.insert("close_or_interrupt".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · → back to the session · Delete stops a live one, deletes an exited one · Ctrl+C twice to quit"
    );
}

#[test]
fn the_launch_home_foot_names_no_back_key() {
    let app = home_with(80, 24, launch());
    assert!(!foot(&app).contains("back to the session"));
}

#[test]
fn opening_a_row_then_closing_names_no_back_key() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(foot(&app).contains("→ or Esc back to the session"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "other work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    let key = app
        .home
        .as_ref()
        .and_then(|home| {
            home.sessions
                .row(&contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()))
        })
        .map(|row| row.key)
        .expect("row B");
    open(&mut app, key);
    let Effect::Send(_) = enter_text(&mut app, "/close") else {
        panic!("/close sends");
    };
    assert!(app.on_home());
    assert_eq!(
        foot(&app),
        "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn a_card_switch_then_closing_names_no_back_key() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(foot(&app).contains("→ or Esc back to the session"));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "other work",
        "/w",
        "-w",
        json!({"state": "idle"}),
    ));
    // A card switch: the session on screen left, then the card's
    // session opens as one from home does.
    let _ = app.leave();
    let key = app
        .home
        .as_ref()
        .and_then(|home| {
            home.sessions
                .row(&contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()))
        })
        .map(|row| row.key)
        .expect("row B");
    app.open_entry(key);
    let Effect::Send(_) = enter_text(&mut app, "/close") else {
        panic!("/close sends");
    };
    assert!(app.on_home());
    assert!(!foot(&app).contains("back to the session"));
}

#[test]
fn closing_straight_from_a_session_names_no_back_key() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let Effect::Send(_) = enter_text(&mut app, "/close") else {
        panic!("/close sends");
    };
    assert!(app.on_home());
    assert!(!foot(&app).contains("back to the session"));
}

#[test]
fn home_on_home_keeps_the_back_key() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    let _ = enter_text(&mut app, "/home");
    assert!(app.on_home());
    assert!(foot(&app).contains("→ or Esc back to the session"));
}

#[test]
fn new_on_home_keeps_the_back_key() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    let _ = enter_text(&mut app, "/new");
    assert!(app.on_home());
    assert!(foot(&app).contains("→ or Esc back to the session"));
}

#[test]
fn the_back_hint_bolds_the_key_and_dims_the_rest() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    let screen = app.home_screen().expect("home draws");
    let span = |content: &str| {
        screen
            .foot
            .spans
            .iter()
            .find(|span| span.content.as_ref() == content)
            .unwrap_or_else(|| panic!("{content} draws"))
    };
    for key in ["→", "Esc"] {
        let style = span(key).style;
        assert!(
            style.add_modifier.contains(ratatui::style::Modifier::BOLD),
            "{key} is bold"
        );
        assert!(
            !style.add_modifier.contains(ratatui::style::Modifier::DIM),
            "{key} is not dim"
        );
    }
    for label in [" or ", " back to the session"] {
        let style = span(label).style;
        assert!(
            style.add_modifier.contains(ratatui::style::Modifier::DIM),
            "{label} is dim"
        );
        assert!(
            !style.add_modifier.contains(ratatui::style::Modifier::BOLD),
            "{label} is not bold"
        );
    }
}

/// A steering queue with one row for `session`.
fn steering_queue(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "steering_queue".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"messages": [{
            "content": [{"type": "text", "text": "steer me"}],
            "source": "driver",
            "command_id": "c_1",
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn slash_new_from_a_session_with_a_focused_steering_row_lands_on_the_entry_bar() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    app.on_line(steering_queue("s_aaaaaaaaaaaaaaaa"));
    app.select_steering(0);
    assert_eq!(app.draft(), "steer me");
    // Esc returns to the box, clearing the steering selection, so
    // `/new` can be typed.
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.draft(), "");
    assert!(matches!(enter_text(&mut app, "/new"), Effect::Send(_)));
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert_eq!(app.draft(), "");
}

#[test]
fn a_left_with_no_ask_in_flight_sends_one_scoped_ask() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    // An unknown session names no row: no trigger.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    // A live session's status queues its scoped ask, drained at once
    // (`docs/tui.md:268`: one ask in flight per project).
    let queued = commands(app.on_line(status("s_aaaaaaaaaaaaaaaa")));
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(queued.len(), 1);
    let first = line_id(&queued[0]).to_owned();
    // While in flight, the left marks dirty and sends nothing, keeping
    // the held fallback.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  fix the parser"]);
    // The answer folds the total and sends exactly one follow-up.
    let follow = answer_value(
        &mut app,
        &first,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old")], "total": 1}),
    );
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&follow), ["-w"]);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    let second = line_id(&follow[0]).to_owned();
    assert!(
        app.on_line(accepted(&second, json!({"sessions": [], "total": 1}),))
            .is_empty()
    );
}

#[test]
fn a_reconnect_drops_totals_and_refetches_every_project() {
    let mut app = home_with(80, 24, launch());
    let (feed, recent) = linked(&mut app);
    assert!(app.on_line(accepted(&feed, json!({}))).is_empty());
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/other", "-other"),
        ]}),
    );
    let own_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    assert!(
        app.on_line(accepted(
            &own_id,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")], "total": 7}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(7));
    // The drop and the second hello reset the connection state.
    app.disconnected();
    let lines = commands(app.on_line(hello()));
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| {
            line["command"]
                .as_str()
                .unwrap_or_else(|| panic!("a command: {line}"))
        })
        .collect();
    // The reconnect asks feed, an unscoped recent, scoped refetches, and
    // sessions: assert only presence, not the outgoing order or count, since
    // no `docs/<area>.md` line promises the order.
    assert!(kinds.contains(&"feed"));
    assert!(kinds.contains(&"recent"));
    assert!(kinds.contains(&"sessions"));
    // The scoped refetches cover both projects; request order is open.
    let scoped: Vec<Value> = lines
        .iter()
        .filter(|line| {
            line.get("args")
                .is_some_and(|args| args.get("project").is_some())
        })
        .cloned()
        .collect();
    let mut projects = fanout_projects(&scoped);
    projects.sort_unstable();
    // `docs/tui.md:268` (one ask per project).
    assert_eq!(projects, ["-other", "-w"]);
    // Totals are empty but rows stay for the refetch.
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-other"), None);
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b  other"]);
    // The stale scoped answer matches nothing and folds nothing.
    assert!(
        app.on_line(accepted(
            &own_id,
            json!({"sessions": [exited_in("s_z", "z", "/w", "-w")], "total": 9}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b  other"]);
    // The refetched unscoped page triggers nothing new: both projects
    // already have asks in flight.
    let recent_id = lines
        .iter()
        .find(|line| line["command"] == "recent" && line.get("args").is_none())
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(
            recent_id,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
        ))
        .is_empty()
    );
}

#[test]
fn replayed_status_then_left_only_retriggers_without_counting() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    assert!(
        app.on_line(accepted(
            &first,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")], "total": 1}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // A replayed status for a known section queues nothing and keeps
    // the total.
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // A replayed left retriggers one ask without incrementing.
    let queued = commands(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")));
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&queued), ["-w"]);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // A second replayed left while in flight only marks dirty.
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // Only a fresh scoped answer moves the total, sending the one
    // follow-up the dirty replay earned.
    let second = line_id(&queued[0]).to_owned();
    let follow = answer_value(
        &mut app,
        &second,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")], "total": 2}),
    );
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&follow), ["-w"]);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(2));
    assert!(
        app.on_line(accepted(
            line_id(&follow[0]),
            json!({"sessions": [], "total": 2}),
        ))
        .is_empty()
    );
}

#[test]
fn an_accepted_delete_asks_the_deleted_projects_scoped_page() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited_in("s_bbbbbbbbbbbbbbbb", "other", "/other", "-other"),
        ]}),
    );
    let own_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    let other_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-other")
        .map(line_id)
        .unwrap_or_else(|| panic!("the other project's scoped ask"))
        .to_owned();
    assert!(
        app.on_line(hub_refused(&other_id, "gone", "no recent"))
            .is_empty()
    );
    assert!(
        app.on_line(accepted(&own_id, json!({"sessions": [], "total": 1})))
            .is_empty()
    );
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(sent) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(sent)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    let outgoing = commands(app.on_line(accepted(&delete, json!({}))));
    let refill = outgoing
        .iter()
        .find(|line| line["command"] == "recent" && line.get("args").is_none())
        .unwrap_or_else(|| panic!("the unscoped refill"));
    let refill_id = line_id(refill).to_owned();
    let scoped: Vec<Value> = outgoing
        .iter()
        .filter(|line| {
            line.get("args")
                .is_some_and(|args| args.get("project").is_some())
        })
        .cloned()
        .collect();
    // `docs/tui.md:268` (the delete directly refreshes its project's total).
    assert_eq!(fanout_projects(&scoped), ["-w"]);
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  other  other"]);
    // The refill returns the already-seen project whose scoped ask was
    // refused. It must not retry it without a new trigger.
    assert!(
        answer_value(
            &mut app,
            &refill_id,
            json!({"sessions": [exited_in("s_bbbbbbbbbbbbbbbb", "other", "/other", "-other")]})
        )
        .is_empty()
    );
}

#[test]
fn an_accepted_delete_while_scoped_in_flight_marks_dirty() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited("s_aaaaaaaaaaaaaaaa", "old work"),
            exited_in("s_bbbbbbbbbbbbbbbb", "other", "/other", "-other"),
        ]}),
    );
    let own_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    click_stop(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(sent) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete = commands(sent)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("delete id"))
        .to_owned();
    // The scoped ask is still in flight: the refresh keeps the unscoped
    // refill only, marking the project dirty.
    let lines = commands(app.on_line(accepted(&delete, json!({}))));
    // The refresh keeps an unscoped refill with no scoped ask while one is
    // already in flight.
    assert!(
        lines
            .iter()
            .any(|line| line["command"] == "recent" && line.get("args").is_none()),
        "an unscoped refill went out: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| line
            .get("args")
            .and_then(|args| args.get("project"))
            .is_none()),
        "no scoped ask while one is in flight: {lines:?}"
    );
    // Its answer folds and schedules exactly one fresh scoped ask.
    let follow = answer_value(
        &mut app,
        &own_id,
        json!({"sessions": [exited_in("s_cccccccccccccccc", "new", "/w", "-w")], "total": 1}),
    );
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&follow), ["-w"]);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  new", "○  other  other"]);
}

#[test]
fn a_new_section_status_queues_one_scoped_ask_for_home_outgoing() {
    let mut app = home_with(80, 24, launch());
    app.on_line(hello());
    // `home_line` returns `None`, so the status reaches the session.
    assert_eq!(app.home_line(&status("s_aaaaaaaaaaaaaaaa")), None);
    // `home_outgoing` drains the queued scoped ask.
    let lines = commands(app.home_outgoing());
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(lines.len(), 1);
    // `docs/invocation.md:1102` (recent command).
    assert_eq!(lines[0]["command"], "recent");
    assert_eq!(lines[0]["args"]["project"], "-w");
    assert!(lines[0]["args"].get("before").is_none());
    // No duplicate while in flight: the trigger marks dirty instead.
    assert_eq!(app.home_line(&status("s_aaaaaaaaaaaaaaaa")), None);
    assert!(app.home_outgoing().is_empty());
}

#[test]
fn triggers_send_nothing_while_the_link_is_down() {
    let mut app = home_with(80, 24, launch());
    assert_eq!(app.home_line(&status("s_aaaaaaaaaaaaaaaa")), None);
    assert!(app.home_outgoing().is_empty());
    assert_eq!(
        app.home_line(&left("s_aaaaaaaaaaaaaaaa", "exited")),
        Some(Vec::new())
    );
}

/// The `(section head, exited head, more)` counts in draw order: the
/// section's workspace tail, its exited header count, and its More
/// count, skipping rows.
fn list_shapes(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| {
            screen
                .list
                .into_iter()
                .map(|line| match line {
                    ListLine::Head { name, .. } => format!("section {name}"),
                    ListLine::ExitedHead { count } => format!("head {count}"),
                    ListLine::More { count, .. } => format!("more {count}"),
                    ListLine::Exited { .. } | ListLine::Live { .. } => "row".to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Answers each scoped ask in `lines` with `total` and no rows.
fn answer_scoped_empty(app: &mut App, lines: &[Value], total: usize) {
    for line in lines {
        assert!(
            app.on_line(accepted(
                line_id(line),
                json!({"sessions": [], "total": total})
            ))
            .is_empty()
        );
    }
}

#[test]
fn home_counts_come_from_the_held_total() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/other", "-other"),
        ]}),
    );
    let launch_id = fanout
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    let other_id = fanout
        .iter()
        .find(|line| line["args"]["project"] == "-other")
        .map(line_id)
        .unwrap_or_else(|| panic!("the other project's scoped ask"))
        .to_owned();
    assert!(
        app.on_line(accepted(&launch_id, json!({"sessions": [], "total": 7})))
            .is_empty()
    );
    assert!(
        app.on_line(accepted(&other_id, json!({"sessions": [], "total": 1})))
            .is_empty()
    );
    // `docs/tui.md:233-234` (sections order), `254-255` (exited rows and More), and `268` (counts from scoped totals).
    assert_eq!(
        list_shapes(&app),
        [
            "section w",
            "head 7",
            "row",
            "more 6",
            "section other",
            "head 1",
            "row",
        ]
    );
    // `More` carries its section's project through its key.
    let key = more_key(&app, "-w").expect("a more row");
    assert_eq!(row_key(&app, "s_aaaaaaaaaaaaaaaa"), key);
    // `docs/tui.md:268` (More counts from the scoped total).
    assert_eq!(app.home_text(Spot::More(key)).as_deref(), Some("… 6 more"));
}

#[test]
fn home_draws_no_exited_head_at_total_zero() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_scoped_empty(&mut app, &fanout, 0);
    // `docs/tui.md:254-255` (exited rows and More) and `268` (count from the scoped total).
    assert_eq!(list_shapes(&app), ["section w", "row"]);
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  a"]);
}

#[test]
fn home_draws_no_more_when_the_total_meets_the_loaded_rows() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_scoped_empty(&mut app, &fanout, 2);
    // `docs/tui.md:254-255` (More when rows remain) and `268` (More counts from the scoped total).
    assert_eq!(list_shapes(&app), ["section w", "head 2", "row", "row"]);
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(rows, ["○  a", "○  b"]);
}

#[test]
fn home_more_draws_one_more_at_total_one_above_shown() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_scoped_empty(&mut app, &fanout, 3);
    // `docs/tui.md:254-255` (exited rows and More) and `268` (More counts from the scoped total).
    assert_eq!(
        list_shapes(&app),
        ["section w", "head 3", "row", "row", "more 1"]
    );
    let key = more_key(&app, "-w").expect("a more row");
    // `docs/tui.md:268` (More counts from the scoped total).
    assert_eq!(app.home_text(Spot::More(key)).as_deref(), Some("… 1 more"));
}

#[test]
fn home_draws_no_more_when_the_total_falls_below_loaded() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_scoped_empty(&mut app, &fanout, 1);
    // Below loaded rows the count never goes negative and no `More`
    // draws.
    // `docs/tui.md:254-255` (More when rows remain) and `268` (More counts from the scoped total).
    assert_eq!(list_shapes(&app), ["section w", "head 1", "row", "row"]);
}

#[test]
fn enter_on_more_with_more_held_asks_the_next_scoped_page() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_scoped_empty(&mut app, &fanout, 3);
    let key = more_key(&app, "-w").expect("a more row");
    let Effect::Send(lines) = app.home_click(Spot::More(key)) else {
        panic!("More opens the next scoped page");
    };
    // `docs/tui.md:268` (opening it pages the project's `recent`).
    assert_eq!(lines.len(), 1);
    let page: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
    // The answer appends the next page under the same total.
    assert!(
        app.on_line(accepted(
            page["id"].as_str().unwrap_or_else(|| panic!("page id")),
            json!({"sessions": [exited_in("s_cccccccccccccccc", "c", "/w", "-w")], "total": 3}),
        ))
        .is_empty()
    );
    // `docs/tui.md:254-255` (exited rows and More) and `268` (counts from the scoped total).
    assert_eq!(
        list_shapes(&app),
        ["section w", "head 3", "row", "row", "more 1"]
    );
}

#[test]
fn enter_on_more_at_or_below_the_total_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/w", "-w"),
        ]}),
    );
    answer_scoped_empty(&mut app, &fanout, 2);
    // The second row's key still resolves the project, but the held
    // total says nothing more: no page goes out (`docs/tui.md:268`).
    let key = row_key(&app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(app.home_click(Spot::More(key)), Effect::None);
}

#[test]
fn down_on_the_last_row_pages_the_project_while_it_holds_more() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let fanout = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w")]}),
    );
    answer_scoped_empty(&mut app, &fanout, 3);
    // The `More` row is the last drawn stop: Enter opens the project's
    // next scoped page past its last loaded row.
    let key = more_key(&app, "-w").expect("a more row");
    drawn(&mut app);
    app.focus = Some(crate::mouse::TargetId::Home(Spot::More(key)));
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter on More pages the project");
    };
    // `docs/tui.md:268` (opening it pages the project's `recent`).
    assert_eq!(lines.len(), 1);
    let page: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["project"], "-w");
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
}

/// The row key of `session` on home.
fn row_key(app: &App, session: &str) -> u64 {
    app.home
        .as_ref()
        .and_then(|home| home.sessions.row(&contract::SessionId(session.to_owned())))
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("row {session}"))
}

#[test]
fn slash_home_from_a_session_focuses_the_on_screen_sessions_row() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "first work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    home_from(&mut app, "s_bbbbbbbbbbbbbbbb");
    drawn(&mut app);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row_key(
            &app,
            "s_bbbbbbbbbbbbbbbb"
        ))))
    );
}

#[test]
fn slash_home_focuses_an_exited_row_hidden_behind_more() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    answer_recent(
        &mut app,
        &recent,
        &[
            ("s_aaaaaaaaaaaaaaaa", "newest"),
            ("s_bbbbbbbbbbbbbbbb", "second"),
            ("s_cccccccccccccccc", "third"),
            ("s_dddddddddddddddd", "fourth"),
        ],
    );
    // The fourth row hides behind more; opening it attaches its
    // exited session.
    let key = more_key(&app, "-w").expect("the fourth row is hidden");
    open(&mut app, key);
    let _ = enter_text(&mut app, "/home");
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(key)))
    );
}

#[test]
fn slash_home_with_no_row_for_the_session_leaves_the_box_focused() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let _ = enter_text(&mut app, "/home");
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn slash_home_on_home_leaves_focus_unchanged() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    drawn(&mut app);
    // The entry bar focused, with a live came-from row behind it:
    // `/home` must not refocus that row.
    app.focus = None;
    let _ = enter_text(&mut app, "/home");
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn an_exited_came_from_session_hides_the_back_hint() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert!(foot(&app).contains("→ or Esc back to the session"));
    assert!(app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited")).is_empty());
    assert!(!foot(&app).contains("back to the session"));
}

#[test]
fn slash_new_then_right_returns_to_the_session_left() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(enter_text(&mut app, "/new"), Effect::None);
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    let Effect::Send(_) = app.on_edit(Edit::Right) else {
        panic!("→ goes back");
    };
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
}

#[test]
fn slash_new_then_esc_returns_to_the_session_left() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    home_from(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(enter_text(&mut app, "/new"), Effect::None);
    assert!(app.on_home());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(_) = app.on_key(Key::Esc, now) else {
        panic!("Esc goes back");
    };
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
}

#[test]
fn slash_resume_then_right_returns_to_the_session_left() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    drawn(&mut app);
    let exited = *keys(&app)
        .get(1)
        .expect("the exited row follows the live row");
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(exited)))
    );
    let Effect::Send(_) = app.on_edit(Edit::Right) else {
        panic!("→ goes back");
    };
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
}

#[test]
fn slash_resume_then_esc_returns_to_the_session_left() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "current work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    answer_recent(&mut app, &recent, &[("s_bbbbbbbbbbbbbbbb", "old work")]);
    drawn(&mut app);
    assert!(matches!(enter_text(&mut app, "/resume"), Effect::Send(_)));
    assert!(app.on_home());
    drawn(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(_) = app.on_key(Key::Esc, now) else {
        panic!("Esc goes back");
    };
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some("s_aaaaaaaaaaaaaaaa")
    );
    assert!(!app.on_home());
}

/// An app on home with `seam` answering configuration reads.
fn offered(seam: Arc<Fake>) -> App {
    let mut app = home_with(80, 24, launch());
    app.set_configure(Some(seam as Arc<dyn Configure>));
    app
}

/// The seam's pending answer, as the startup resolve reads it.
fn resolve(app: &mut App, seam: &Fake) {
    let pending = seam.import_pending();
    app.set_import_offer(pending);
}

/// The offer's sent line, parsed: pressing `key` with no session, on
/// a linked app.
fn press(app: &mut App, key: Key) -> Value {
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(key, now) else {
        panic!("the key sends");
    };
    assert_eq!(lines.len(), 1);
    serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"))
}

/// A hub `command_accepted` carrying the offer's import `result`.
fn hub_accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "result": result})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A hub `command_rejected` for the offer's import.
fn hub_rejected(id: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({
            "command_id": id,
            "code": "invalid_arguments",
            "message": "No.",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// The import report the hub answers with: names and reasons only.
fn import_result() -> Value {
    json!({
        "imported": {
            "mcp_servers": ["alpha"],
            "skills": ["lint"],
            "commands": ["plan"],
            "instructions": ["AGENTS.md"],
        },
        "left_behind": [{"item": "auth.json", "reason": "credentials are never imported"}],
    })
}

/// The offer's notices, oldest first, capped like the box shows.
fn offer_notices(app: &App) -> Vec<String> {
    app.notices()
        .iter()
        .map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => format!("{code} {text}"),
            crate::app::NoticeLine::More(n) => format!("+{n} more"),
        })
        .collect()
}

#[test]
fn the_offer_shows_iff_the_seam_says_pending() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let screen = app.home_screen().unwrap();
    assert!(screen.question_import);
    assert!(
        screen
            .question
            .as_deref()
            .unwrap()
            .contains("Bring settings"),
    );
    let mut app = offered(seam);
    resolve(&mut app, &Fake::new(Vec::new()));
    let screen = app.home_screen().unwrap();
    assert!(!screen.question_import);
    assert_eq!(screen.question, None);
}

#[test]
fn the_offer_keys_send_the_named_source_with_no_session() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('1'));
    assert_eq!(line["command"], "import");
    assert_eq!(line["args"], json!({"source": "claude-code"}));
    assert_eq!(line.get("session_id"), None);
    let line = press(&mut app, Key::Char('2'));
    assert_eq!(line["args"], json!({"source": "codex"}));
    assert_eq!(line.get("session_id"), None);
}

#[test]
fn the_offer_press_before_the_link_holds_until_hello() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    // The link is down: the press queues the import instead of
    // dropping it, and the offer stays open for its answer.
    let now = fakes::clock::FakeClock::new().now();
    let Effect::None = app.on_key(Key::Char('1'), now) else {
        panic!("the early press holds");
    };
    assert!(app.home_screen().unwrap().question_import);
    let lines: Vec<Value> = commands(app.on_line(hello()));
    let import = lines
        .iter()
        .find(|line| line["command"] == "import")
        .unwrap_or_else(|| panic!("the held import goes out on hello"));
    assert_eq!(import["args"], json!({"source": "claude-code"}));
    assert_eq!(import.get("session_id"), None);
}

#[test]
fn the_offer_swallows_enter_arrows_and_other_keys() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for key in [
        Key::Enter,
        Key::Up,
        Key::Down,
        Key::Char('3'),
        Key::Char('a'),
    ] {
        assert!(
            matches!(app.on_key(key.clone(), now), Effect::None),
            "{key:?} is swallowed"
        );
    }
    assert!(app.home_screen().unwrap().question_import);
}

#[test]
fn the_offer_sends_nothing_with_the_link_down() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let now = fakes::clock::FakeClock::new().now();
    assert!(matches!(app.on_key(Key::Char('1'), now), Effect::None));
    assert!(app.home_screen().unwrap().question_import);
}

#[test]
fn an_accepted_offer_answer_opens_the_report_and_closes_the_offer() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('2'));
    let id = line["id"].as_str().unwrap().to_owned();
    assert!(app.on_line(hub_accepted(&id, import_result())).is_empty());
    // The offer closes with no notices, and the report names the
    // source the offer sent with both counts.
    let report = app
        .import_report()
        .expect("the offer's accept opens the report");
    assert_eq!(report.title(), "Imported from Codex");
    assert_eq!((report.copied_count(), report.left_count()), (4, 1));
    assert!(app.notices().is_empty());
    assert!(!app.home_screen().unwrap().question_import);
    assert!(!app.home.as_ref().unwrap().import_offer);
    // Esc closes the report and leaves no notices behind.
    let now = fakes::clock::FakeClock::new().now();
    assert!(matches!(app.on_key(Key::Esc, now), Effect::None));
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn a_malformed_offer_answer_notices_and_leaves_the_offer_open() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('1'));
    let id = line["id"].as_str().unwrap().to_owned();
    // The hub answers what the terminal cannot read as a command
    // answer: the refused path with the offer open.
    assert!(
        app.on_line(hub_accepted(&id, json!({"bogus": true})))
            .is_empty()
    );
    assert!(app.import_report().is_none());
    assert_eq!(app.notice(), Some("rejected"));
    assert_eq!(offer_notices(&app), ["malformed rejected"]);
    assert!(app.home_screen().unwrap().question_import);
    // The malformed answer drops the source entry, so a second answer
    // for the id changes nothing.
    assert!(app.on_line(hub_accepted(&id, import_result())).is_empty());
    assert!(app.import_report().is_none());
    assert_eq!(offer_notices(&app), ["malformed rejected"]);
    // The taken ask is gone, so the offer sends again for a retry.
    let retry = press(&mut app, Key::Char('1'));
    assert_ne!(retry["id"], line["id"]);
}

#[test]
fn a_rejected_offer_answer_notices_and_leaves_the_offer_open() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('1'));
    let id = line["id"].as_str().unwrap().to_owned();
    assert!(app.on_line(hub_rejected(&id)).is_empty());
    assert_eq!(app.notice(), Some("No."));
    assert!(app.home_screen().unwrap().question_import);
    // The taken ask is gone, so the offer sends again for a retry.
    let retry = press(&mut app, Key::Char('1'));
    assert_ne!(retry["id"], line["id"]);
}

#[test]
fn an_accepted_offer_answer_with_a_null_result_notices_and_leaves_the_offer_open() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('1'));
    let id = line["id"].as_str().unwrap().to_owned();
    // The hub accepts without the import's report: the refused path
    // with the offer open, as for an unparseable payload.
    assert!(app.on_line(hub_accepted(&id, Value::Null)).is_empty());
    assert!(app.import_report().is_none());
    assert_eq!(app.notice(), Some("rejected"));
    assert_eq!(offer_notices(&app), ["malformed rejected"]);
    assert!(app.home_screen().unwrap().question_import);
}

#[test]
fn esc_dismisses_for_the_run_only() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    let _ = linked(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert!(matches!(app.on_key(Key::Esc, now), Effect::None));
    assert!(!app.home_screen().unwrap().question_import);
    // The next run resolves again: still no receipt means the offer
    // fires again.
    resolve(&mut app, &seam);
    assert!(app.home_screen().unwrap().question_import);
}

#[test]
fn a_receipt_landing_mid_run_neither_retracts_nor_blocks_the_offer() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = true;
    }
    let mut app = offered(Arc::clone(&seam));
    resolve(&mut app, &seam);
    // The receipt lands mid-run: the seam now says no offer, but the
    // shown offer stays, and its answer still folds.
    if let Ok(mut pending) = seam.pending.lock() {
        *pending = false;
    }
    assert!(app.home_screen().unwrap().question_import);
    let _ = linked(&mut app);
    let line = press(&mut app, Key::Char('1'));
    let id = line["id"].as_str().unwrap().to_owned();
    assert!(app.on_line(hub_accepted(&id, import_result())).is_empty());
    // The answer still folds: the offer closes into the report naming
    // the pressed source, with no notices.
    let report = app
        .import_report()
        .expect("the offer's accept opens the report");
    assert_eq!(report.title(), "Imported from Claude Code");
    assert_eq!((report.copied_count(), report.left_count()), (4, 1));
    assert!(app.notices().is_empty());
    assert!(!app.home_screen().unwrap().question_import);
}

#[test]
fn an_accept_arriving_while_its_session_reopens_falls_through_the_gate() {
    let mut app = home_with(80, 24, launch());
    linked(&mut app);
    let started = start_session(&mut app, "s_aaaaaaaaaaaaaaaa");
    let full = started[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &full))
            .is_empty()
    );
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "old work",
        "/w",
        "-w",
        json!({"state": "streaming"}),
    ));
    // The import goes out while attached.
    let Effect::Send(import) = enter_text(&mut app, "/import codex") else {
        panic!("the import sends");
    };
    assert_eq!(import.len(), 1);
    let import: Value = serde_json::from_str(&import[0]).unwrap_or_else(|err| panic!("{err}"));
    let id = import["id"]
        .as_str()
        .unwrap_or_else(|| panic!("import id"))
        .to_owned();
    // Leaving lowers the connection; accepting the lowering, then
    // reopening, leaves the raise in flight, so the session is gated.
    let Effect::Send(lowering) = enter_text(&mut app, "/home") else {
        panic!("leaving lowers");
    };
    assert_eq!(lowering.len(), 1);
    let lowering: Value = serde_json::from_str(&lowering[0]).unwrap_or_else(|err| panic!("{err}"));
    let ack = lowering["id"]
        .as_str()
        .unwrap_or_else(|| panic!("lowering id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &ack))
            .is_empty()
    );
    let lines = open_first(&mut app);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "commands");
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["args"]["level"], "full");
    // The import's accept arrives while gated: it still opens the
    // report, with no notices, closing the pending entry.
    let accepted = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"command_id": id, "result": import_result()})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(accepted).is_empty());
    let report = app
        .import_report()
        .expect("the gated accept opens the report");
    assert_eq!(report.title(), "Imported from Codex");
    assert_eq!((report.copied_count(), report.left_count()), (4, 1));
    assert!(app.notices().is_empty());
    assert!(!app.outbox.pending.contains_key(&id));
}

/// Answers `id` with `result`: the outgoing lines, parsed.
fn answer_value(app: &mut App, id: &str, result: Value) -> Vec<Value> {
    app.on_line(accepted(id, result))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `project` arg of each outgoing `recent` line, in order.
fn fanout_projects(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| {
            // `docs/invocation.md:1102` (each fanout line is a scoped `recent`).
            assert_eq!(line["command"], "recent");
            line["args"]["project"]
                .as_str()
                .unwrap_or_else(|| panic!("a scoped ask: {line}"))
        })
        .collect()
}

/// The outgoing line's id.
fn line_id(line: &Value) -> &str {
    line["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id: {line}"))
}

/// The stored scoped total for `project`.
fn stored(app: &App, project: &str) -> Option<usize> {
    app.home
        .as_ref()
        .and_then(|home| home.sessions.total_for(project))
}

#[test]
fn an_unscoped_answer_fans_out_one_scoped_ask_per_new_project() {
    let mut app = home_with(80, 24, launch());
    let (feed, recent) = linked(&mut app);
    assert!(app.on_line(accepted(&feed, json!({}))).is_empty());
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "other", "/other", "-other"),
        ]}),
    );
    let mut projects = fanout_projects(&lines);
    projects.sort_unstable();
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(projects, ["-other", "-w"]);
    for line in &lines {
        assert!(line["args"].get("before").is_none());
    }
    // `docs/tui.md:268` (one ask per project): the two new projects'
    // asks carry distinct ids; request order is open.
    let mut ids: Vec<&str> = lines.iter().map(line_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    // A stale unscoped id answers nothing more.
    assert!(
        app.on_line(accepted("c_deadbeefdeadbeef", json!({"sessions": []})))
            .is_empty()
    );
}

#[test]
fn an_unscoped_answer_carrying_a_total_stores_nothing() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")], "total": 12}),
    );
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&lines), ["-w"]);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
}

#[test]
fn scoped_answers_key_rows_and_total_by_project() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "other", "/other", "-other"),
        ]}),
    );
    let other_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-other")
        .map(line_id)
        .unwrap_or_else(|| panic!("the other project's scoped ask"))
        .to_owned();
    let own_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    // Out of order: `-other` first.
    assert!(
        app.on_line(accepted(
            &other_id,
            json!({"sessions": [exited_in("s_o2", "o2", "/other", "-other")], "total": 2}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-other"), Some(2));
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // `-w` next, with a duplicate and a new row: pages append.
    assert!(
        app.on_line(accepted(
            &own_id,
            json!({"sessions": [
                exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w"),
                exited_in("s_cccccccccccccccc", "new", "/w", "-w"),
            ], "total": 3}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    // The rows tie on recency, so no `docs/<area>.md` line promises
    // their order: assert the rows as a set.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(
        rows,
        ["○  new", "○  o2  other", "○  old", "○  other  other",]
    );
    // A stale scoped id drops without touching rows or totals.
    assert!(
        app.on_line(accepted(
            &own_id,
            json!({"sessions": [exited_in("s_z", "z", "/w", "-w")], "total": 9}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(3));
    // A stale scoped id leaves the rows as they were: same rows,
    // whatever their order, since ties promise none.
    let mut rows = listed(&app);
    rows.sort_unstable();
    assert_eq!(
        rows,
        ["○  new", "○  o2  other", "○  old", "○  other  other",]
    );
}

#[test]
fn a_dirty_answer_sends_exactly_one_follow_up() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    // A trigger arrived while the ask was in flight.
    app.home.as_mut().unwrap().sessions.mark_dirty("-w");
    let follow = answer_value(
        &mut app,
        &first,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")], "total": 5}),
    );
    // `docs/tui.md:268` (one ask in flight per project).
    assert_eq!(follow.len(), 1);
    // `docs/tui.md:268` (one ask per project; request order is open).
    assert_eq!(fanout_projects(&follow), ["-w"]);
    assert_ne!(line_id(&follow[0]), first);
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(5));
    // The follow-up answers with no further trigger: nothing more.
    let second = line_id(&follow[0]).to_owned();
    assert!(
        app.on_line(accepted(&second, json!({"sessions": [], "total": 5}),))
            .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(5));
}

#[test]
fn a_scoped_refusal_keeps_the_total_notices_once_and_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    app.home.as_mut().unwrap().sessions.mark_dirty("-w");
    assert!(
        app.on_line(hub_refused(&first, "gone", "no recent"))
            .is_empty()
    );
    assert_eq!(app.notice(), Some("no recent"));
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, .. } if code == "gone"
    )));
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  old"]);
    // Pending and dirty cleared: the same id matches nothing, and no
    // follow-up went out.
    assert!(
        app.on_line(accepted(&first, json!({"sessions": []})))
            .is_empty()
    );
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, .. } if code == "gone"
    )));
    assert!(!app.home.as_ref().unwrap().sessions.is_dirty("-w"));
}

#[test]
fn a_scoped_malformed_answer_keeps_the_total_notices_once_and_sends_nothing() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    app.home.as_mut().unwrap().sessions.mark_dirty("-w");
    let follow = answer_value(
        &mut app,
        &first,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")], "total": 5}),
    );
    let second = line_id(&follow[0]).to_owned();
    let before = listed(&app);
    assert!(
        app.on_line(accepted(&second, json!({"sessions": {}})))
            .is_empty()
    );
    assert_eq!(app.notice(), Some("rejected"));
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, .. } if code == "malformed"
    )));
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(5));
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), before);
}

#[test]
fn a_scoped_answer_missing_sessions_or_total_notices_once_and_folds_nothing() {
    // Both `sessions` and `total` are required by the hub contract
    // (`docs/invocation.md`, "The hub" and `docs/events.md`,
    // "`command_accepted`"): a missing `total` or a missing `sessions`
    // takes the `malformed` notice path with no retry. Each case runs in a
    // fresh app so the two `malformed` notices (same text, deduped while
    // stacked per `docs/tui.md`, "Notices") do not collapse into one.
    for (result, label) in [
        (
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
            "missing total",
        ),
        (json!({"total": 5}), "missing sessions"),
    ] {
        let mut app = home_with(80, 24, launch());
        let (_, recent) = linked(&mut app);
        let lines = answer_value(
            &mut app,
            &recent,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
        );
        let first = line_id(&lines[0]).to_owned();
        app.home.as_mut().unwrap().sessions.mark_dirty("-w");
        let follow = answer_value(
            &mut app,
            &first,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")], "total": 5}),
        );
        let second = line_id(&follow[0]).to_owned();
        let before = listed(&app);
        // An incomplete answer would fold rows without a count on the old path.
        assert!(app.on_line(accepted(&second, result)).is_empty(), "{label}");
        assert_eq!(app.notice(), Some("rejected"), "{label}");
        assert!(
            app.notices().iter().any(|line| matches!(
                line,
                crate::app::NoticeLine::Notice { code, .. } if code == "malformed"
            )),
            "{label}"
        );
        // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
        assert_eq!(stored(&app, "-w"), Some(5), "{label}");
        // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
        assert_eq!(listed(&app), before, "{label}");
        assert!(
            !app.home.as_ref().unwrap().sessions.is_dirty("-w"),
            "{label}"
        );
    }
}

#[test]
fn an_unscoped_older_page_after_a_scoped_refusal_sends_nothing() {
    // A scoped refusal keeps no total with no retry: ordinary pagination
    // must not retry the already-seen project without a new trigger
    // (`docs/tui.md:268`, one ask in flight per project).
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    assert!(
        app.on_line(hub_refused(&first, "gone", "no recent"))
            .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
    // The older unscoped page holds the same project: no scoped retry.
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(page) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    let page_id = page["id"].as_str().unwrap_or_else(|| panic!("page id"));
    assert!(page["args"].get("project").is_none());
    let fanout = answer_value(
        &mut app,
        page_id,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    assert!(fanout.is_empty());
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), None);
}

#[test]
fn unknown_before_on_an_unscoped_page_leaves_totals_alone() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")]}),
    );
    let first = line_id(&lines[0]).to_owned();
    assert!(
        app.on_line(accepted(
            &first,
            json!({"sessions": [exited_in("s_aaaaaaaaaaaaaaaa", "old", "/w", "-w")], "total": 1}),
        ))
        .is_empty()
    );
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // The project's loaded rows already match its total, so the next
    // page stays unscoped.
    focus_last(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(page) = app.on_key(Key::Down, now) else {
        panic!("the last row asks the next page");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    let page_id = page["id"].as_str().unwrap_or_else(|| panic!("page id"));
    assert_eq!(page["args"]["before"], "s_aaaaaaaaaaaaaaaa");
    assert!(
        app.on_line(hub_refused(page_id, "unknown_before", "too old"))
            .is_empty()
    );
    assert_eq!(app.notice(), Some("too old"));
    // `docs/invocation.md:1102` (recent total) and `docs/tui.md:268` (counts from total).
    assert_eq!(stored(&app, "-w"), Some(1));
    // `docs/tui.md:254-255` (newest first) and `docs/tui.md:233-234` (sections order).
    assert_eq!(listed(&app), ["○  old"]);
}

#[test]
fn out_of_order_scoped_answers_leave_the_unscoped_cursor() {
    let mut app = home_with(80, 24, launch());
    let (_, recent) = linked(&mut app);
    let lines = answer_value(
        &mut app,
        &recent,
        json!({"sessions": [
            exited_in("s_aaaaaaaaaaaaaaaa", "a", "/w", "-w"),
            exited_in("s_bbbbbbbbbbbbbbbb", "b", "/other", "-other"),
        ]}),
    );
    let own_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-w")
        .map(line_id)
        .unwrap_or_else(|| panic!("the launch project's scoped ask"))
        .to_owned();
    let other_id = lines
        .iter()
        .find(|line| line["args"]["project"] == "-other")
        .map(line_id)
        .unwrap_or_else(|| panic!("the other project's scoped ask"))
        .to_owned();
    assert!(
        app.on_line(accepted(
            &other_id,
            json!({"sessions": [exited_in("s_o2", "o2", "/other", "-other")], "total": 2}),
        ))
        .is_empty()
    );
    assert!(
        app.on_line(accepted(
            &own_id,
            json!({"sessions": [exited_in("s_cccccccccccccccc", "c", "/w", "-w")], "total": 2}),
        ))
        .is_empty()
    );
    // Both projects hold exactly their loaded rows, so paging stays on
    // the unscoped cursor: the last unscoped row.
    let key = row_key(&app, "s_bbbbbbbbbbbbbbbb");
    let Some(Effect::Send(page)) = app.page_recent(key) else {
        panic!("the unscoped cursor pages");
    };
    let page: Value = serde_json::from_str(&page[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(page["command"], "recent");
    assert_eq!(page["args"]["before"], "s_bbbbbbbbbbbbbbbb");
    assert!(page["args"].get("project").is_none());
}
