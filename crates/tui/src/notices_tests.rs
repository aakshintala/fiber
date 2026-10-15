//! Tests for the notice stack, its rows and overlays.

use std::path::PathBuf;

use super::{NoticeLine, Notices};
use crate::app::App;
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// Notices of `texts` under `code`, oldest first.
fn stack(code: &str, texts: &[&str]) -> Notices {
    let mut notices = Notices::default();
    for text in texts {
        notices.push(code, (*text).to_owned());
    }
    notices
}

/// Each line's notice id, or the `More` count as a negative.
fn ids(lines: &[NoticeLine]) -> Vec<i64> {
    lines
        .iter()
        .map(|line| match line {
            NoticeLine::Notice { id, .. } => *id as i64,
            NoticeLine::More(n) => -(*n as i64),
        })
        .collect()
}

#[test]
fn two_notices_draw_two_rows_and_no_more() {
    assert_eq!(ids(&stack("extension_failed", &["a", "b"]).lines()), [0, 1]);
}

#[test]
fn three_oldest_then_more() {
    // At the cap three rows show, oldest first, and no "+N more".
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c"]).lines()),
        [0, 1, 2]
    );
    // Past it the three oldest show, then one "+N more" row last.
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c", "d"]).lines()),
        [0, 1, 2, -1]
    );
    assert_eq!(
        ids(&stack("extension_failed", &["a", "b", "c", "d", "e"]).lines()),
        [0, 1, 2, -2]
    );
    assert!(Notices::default().lines().is_empty());
}

#[test]
fn dismiss_reveals_the_next_oldest() {
    let mut notices = stack("extension_failed", &["a", "b", "c", "d"]);
    notices.dismiss(0);
    // Dismissing a shown row reveals the next oldest; the "+N more" row
    // goes, since three show again.
    assert_eq!(ids(&notices.lines()), [1, 2, 3]);
    notices.dismiss(3);
    assert_eq!(ids(&notices.lines()), [1, 2]);
}

#[test]
fn dismissing_a_hidden_notice_drops_the_more_count() {
    let mut notices = stack("extension_failed", &["a", "b", "c", "d", "e"]);
    notices.dismiss(4);
    assert_eq!(ids(&notices.lines()), [0, 1, 2, -1]);
}

#[test]
fn code_is_kept() {
    let lines = stack("extension_failed", &["x"]).lines();
    assert_eq!(
        lines,
        [NoticeLine::Notice {
            id: 0,
            code: "extension_failed".to_owned(),
            text: "x".to_owned(),
        }]
    );
}

#[test]
fn empty_has_no_lines() {
    assert!(Notices::default().lines().is_empty());
}

#[test]
fn dismiss_and_the_overlays() {
    let mut notices = stack("extension_failed", &["one", "two", "three", "four"]);
    assert_eq!(notices.overlay(), None);
    notices.open(1);
    assert_eq!(notices.overlay(), Some(vec!["two".to_owned()]));
    notices.dismiss(1);
    assert_eq!(notices.overlay(), None);
    assert_eq!(
        notices
            .lines()
            .iter()
            .map(|line| match line {
                NoticeLine::Notice { text, .. } => text.clone(),
                NoticeLine::More(n) => format!("+{n} more"),
            })
            .collect::<Vec<_>>(),
        ["one", "three", "four"]
    );
    // Opening a notice that is gone does nothing.
    notices.open(1);
    assert_eq!(notices.overlay(), None);
    notices.open_all();
    assert_eq!(
        notices.overlay(),
        Some(vec![
            "four".to_owned(),
            "three".to_owned(),
            "one".to_owned()
        ])
    );
    // Dismissing another notice keeps the list open.
    notices.dismiss(0);
    assert_eq!(
        notices.overlay(),
        Some(vec!["four".to_owned(), "three".to_owned()])
    );
    assert!(notices.close());
    assert!(!notices.close());
    let mut empty = Notices::default();
    empty.open_all();
    assert_eq!(empty.overlay(), None);
}

#[test]
fn held_notices_wait_for_the_release() {
    let mut notices = stack("extension_failed", &["one", "two", "three"]);
    notices.hold();
    notices.push("extension_failed", "during".to_owned());
    notices.push("extension_failed", "later".to_owned());
    // Held notices are not drawn, not listed by "+N more" and not in the
    // overlay; the ones from before the hold still show.
    let first = |notices: &Notices| -> Vec<String> {
        notices
            .lines()
            .iter()
            .map(|line| match line {
                NoticeLine::Notice { text, .. } => text.clone(),
                NoticeLine::More(n) => format!("+{n} more"),
            })
            .collect()
    };
    assert_eq!(first(&notices), ["one", "two", "three"]);
    notices.open_all();
    assert_eq!(
        notices.overlay(),
        Some(vec![
            "three".to_owned(),
            "two".to_owned(),
            "one".to_owned()
        ])
    );
    notices.close();
    // A second hold keeps where the first began.
    notices.hold();
    notices.release();
    assert_eq!(first(&notices), ["one", "two", "three", "+2 more"]);
}

/// An app attached to [`S_A`], connected, 100 columns wide.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(100, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::default(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// A `notice` line for `session`.
fn notice(session: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "notice".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"code": "extension_failed", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The first line's code and text.
fn first(app: &App) -> (String, String) {
    match app.notices().into_iter().next() {
        Some(NoticeLine::Notice { code, text, .. }) => (code, text),
        line => panic!("a notice row, not {line:?}"),
    }
}

/// A hub `command_rejected` for `id`, with `code` when `Some`.
fn hub_refused(id: &str, code: Option<&str>, message: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "command_id".to_owned(),
        serde_json::Value::String(id.to_owned()),
    );
    if let Some(code) = code {
        payload.insert(
            "code".to_owned(),
            serde_json::Value::String(code.to_owned()),
        );
    }
    payload.insert(
        "message".to_owned(),
        serde_json::Value::String(message.to_owned()),
    );
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload,
    })
}

/// A session `command_rejected` for `id` on `session` with `code`.
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
            ("command_id".to_owned(), serde_json::json!(id)),
            ("code".to_owned(), serde_json::json!(code)),
            ("message".to_owned(), serde_json::json!(message)),
        ]
        .into_iter()
        .collect(),
    })
}

#[test]
fn connection_lost_is_connection_failed() {
    let mut app = attached();
    app.disconnected();
    assert_eq!(
        first(&app),
        (
            "connection_failed".to_owned(),
            "Connection lost.".to_owned()
        )
    );
}

#[test]
fn connect_failed_is_connection_failed() {
    let mut app = attached();
    app.connect_failed("Could not reach the hub: refused.".to_owned());
    assert_eq!(
        first(&app),
        (
            "connection_failed".to_owned(),
            "Could not reach the hub: refused.".to_owned()
        )
    );
}

#[test]
fn session_notice_keeps_its_code() {
    let mut app = attached();
    app.on_line(notice(S_A, "An extension failed to load."));
    assert_eq!(
        first(&app),
        (
            "extension_failed".to_owned(),
            "An extension failed to load.".to_owned()
        )
    );
    assert_eq!(app.notice(), Some("An extension failed to load."));
}

#[test]
fn the_sessions_notices_and_the_terminals_own_share_the_stack() {
    let mut app = attached();
    app.on_line(notice(S_A, "An extension failed to load."));
    app.on_line(notice("s_bbbbbbbbbbbbbbbb", "Not ours."));
    app.on_key(Key::AltA, fakes::clock::FakeClock::new().now());
    app.disconnected();
    let firsts: Vec<String> = app
        .notices()
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { text, .. } => Some(text.clone()),
            NoticeLine::More(_) => None,
        })
        .collect();
    assert_eq!(
        firsts,
        [
            "An extension failed to load.",
            "No requests waiting.",
            "Connection lost.",
        ]
    );
}

#[test]
fn a_hub_refusal_keeps_its_code() {
    let mut app = attached();
    // A refused `reply` carries its draft back with the hub's code.
    app.pending.insert(
        "c_1".to_owned(),
        (
            crate::app::Kind::Reply,
            crate::input::Draft::default(),
        ),
    );
    app.on_line(hub_refused("c_1", Some("busy"), "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn a_hub_refusal_with_no_code_is_malformed() {
    let mut app = attached();
    app.pending.insert(
        "c_1".to_owned(),
        (
            crate::app::Kind::Reply,
            crate::input::Draft::default(),
        ),
    );
    app.on_line(hub_refused("c_1", None, "No code."));
    assert_eq!(first(&app), ("malformed".to_owned(), "No code.".to_owned()));
}

#[test]
fn a_session_refusal_keeps_its_code() {
    let mut app = attached();
    app.pending.insert(
        "c_1".to_owned(),
        (
            crate::app::Kind::Reply,
            crate::input::Draft::default(),
        ),
    );
    app.on_line(session_refused(S_A, "c_1", "busy", "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn a_history_refusal_keeps_its_code() {
    // Up sends `prompt_history`; its refusal is the notice with the
    // refusal's code.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(100, 24);
    app.set_project("key".to_owned());
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::default(),
    }));
    let now = fakes::clock::FakeClock::new().now();
    let sent = match app.on_key(Key::Up, now) {
        crate::app::Effect::Send(lines) => lines,
        effect => panic!("a send, not {effect:?}"),
    };
    let id = sent
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap_or_default())
        .find(|line| line["command"] == "prompt_history")
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .expect("a prompt_history send");
    app.on_line(hub_refused(&id, Some("busy"), "A turn is running."));
    assert_eq!(
        first(&app),
        ("busy".to_owned(), "A turn is running.".to_owned())
    );
}

#[test]
fn keys_notices_are_config_invalid() {
    let mut app = attached();
    // An action Fiber has no name for is ignored with a notice.
    let setup = crate::keyset::KeysSetup {
        user: [("no_action".to_owned(), serde_json::json!("ctrl+a"))]
            .into_iter()
            .collect(),
    };
    app.set_keys(setup);
    let codes: Vec<String> = app
        .notices()
        .iter()
        .filter_map(|line| match line {
            NoticeLine::Notice { code, .. } => Some(code.clone()),
            NoticeLine::More(_) => None,
        })
        .collect();
    assert!(!codes.is_empty());
    assert!(codes.iter().all(|code| code == "config_invalid"), "{codes:?}");
}

#[test]
fn catalogue_notices_are_model_invalid_and_a_read_error_is_io_failed() {
    let mut app = attached();
    app.on_models(Ok(crate::catalogue::Catalogue {
        models: Vec::new(),
        notices: vec!["a cached provider is gone".to_owned()],
        lists: Vec::new(),
    }));
    assert_eq!(
        first(&app),
        (
            "model_invalid".to_owned(),
            "a cached provider is gone".to_owned()
        )
    );
    app.on_models(Err("the lists could not be read".to_owned()));
    assert_eq!(
        app.notices()[1],
        NoticeLine::Notice {
            id: 1,
            code: "io_failed".to_owned(),
            text: "the lists could not be read".to_owned(),
        }
    );
}

#[test]
fn the_app_dismisses_and_opens_notices_and_esc_closes_the_overlay() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    for n in 0..4 {
        app.on_line(notice(S_A, &format!("notice {n}")));
    }
    app.open_notice(2);
    assert_eq!(app.notice_overlay(), Some(vec!["notice 2".to_owned()]));
    assert_eq!(app.on_key(Key::Esc, now), crate::app::Effect::None);
    assert_eq!(app.notice_overlay(), None);
    app.open_more_notices();
    assert_eq!(app.notice_overlay().map(|all| all.len()), Some(4));
    app.on_key(Key::Esc, now);
    app.dismiss_notice(3);
    assert_eq!(app.notice(), Some("notice 2"));
    assert_eq!(app.notices().len(), 3);
}
