//! Tests for the repository offer on the app: what opens it, the keys and
//! edits it takes, and the reply it sends.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";
const BADGE_ONE: &str = "! 1 waiting · /approvals or ⌥A";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// A session line of `kind` from `session`.
fn session_line(session: &str, kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app connected to the hub and attached to [`S_A`].
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// An offer `r_1` of three MCP servers from `session`.
fn offer(session: &str) -> Line {
    let items: Vec<Value> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            json!({"kind": "mcp_server", "name": name, "hash": "h", "required": false,
                "summary": format!("MCP server: {name}")})
        })
        .collect();
    session_line(
        session,
        "repository_code_offered",
        json!({"request_id": "r_1", "items": items}),
        None,
    )
}

/// An app attached to [`S_A`] with its offer open.
fn offered() -> App {
    let mut app = attached();
    assert!(app.on_line(offer(S_A)).is_empty());
    assert!(app.offer_open());
    app
}

/// An approval request from [`S_A`].
fn approval(id: &str) -> Line {
    session_line(
        S_A,
        "permission_requested",
        json!({"request_id": id, "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "p"}}),
        Some("a_1"),
    )
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, now())
}

/// The lines an effect sends, parsed.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => Vec::new(),
    }
}

/// Chooses approve, skip, never with the keys, then Enter on Send.
fn answer_with_keys(app: &mut App) -> Vec<Value> {
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(press(app, Key::Down), Effect::None);
    assert_eq!(press(app, Key::Enter), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(press(app, Key::Down), Effect::None);
    sent(press(app, Key::Enter))
}

#[test]
fn an_attached_sessions_offer_opens_the_view() {
    let app = offered();
    let rows = app.offer_rows(80).map(|(rows, _)| rows).unwrap_or_default();
    assert!(rows.len() > 3);
}

#[test]
fn another_sessions_offer_does_not_show() {
    let mut app = attached();
    assert!(app.on_line(offer(S_B)).is_empty());
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn the_keys_build_the_decisions_and_send_one_reply() {
    let mut app = offered();
    let lines = answer_with_keys(&mut app);
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    assert!(id.starts_with("c_"));
    assert_eq!(
        lines[0],
        json!({"id": id, "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_1", "decisions": ["approve", "skip", "never"]}})
    );
    // Answered, the view closes and the badge does not count it.
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn no_reply_with_the_link_down() {
    let mut app = offered();
    app.disconnected();
    assert!(answer_with_keys(&mut app).is_empty());
    assert!(app.offer_open());
}

#[test]
fn a_rejected_reply_reopens_the_offer_with_its_choices() {
    let mut app = offered();
    let lines = answer_with_keys(&mut app);
    let rejected = session_line(
        S_A,
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "stale_request", "message": "Stale."}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("Stale."));
    assert!(app.offer_open());
    assert_eq!(app.draft(), "");
    let again = sent(press(&mut app, Key::Enter));
    assert_eq!(
        again[0]["args"]["decisions"],
        json!(["approve", "skip", "never"])
    );
}

#[test]
fn a_failed_write_reopens_the_offer() {
    let mut app = offered();
    app.on_edit(Edit::Left);
    let Effect::Send(lines) = app.on_click(crate::mouse::TargetId::Offer(crate::offer::Spot::Send))
    else {
        panic!("Send sends");
    };
    assert!(!app.offer_open());
    app.write_failed(&lines);
    assert!(app.offer_open());
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn another_clients_resolution_closes_it() {
    let mut app = offered();
    let resolved = session_line(
        S_A,
        "repository_code_resolved",
        json!({"request_id": "r_1", "decisions": ["skip", "skip", "skip"]}),
        None,
    );
    assert!(app.on_line(resolved).is_empty());
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn the_badge_counts_it_after_esc() {
    let mut app = offered();
    assert!(app.badge().is_none());
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.offer_open());
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn the_badge_counts_it_with_a_put_aside_approval() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    app.on_line(approval("r_9"));
    assert!(app.panel().is_some());
    press(&mut app, Key::Esc);
    assert_eq!(
        app.badge().as_deref(),
        Some("! 2 waiting · /approvals or ⌥A")
    );
    // ⌥A reopens the offer first.
    press(&mut app, Key::AltA);
    assert!(app.offer_open());
    assert!(app.panel().is_none());
}

#[test]
fn alt_a_reopens_it() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    assert_eq!(press(&mut app, Key::AltA), Effect::None);
    assert!(app.offer_open());
    assert!(app.notice().is_none());
}

#[test]
fn slash_approvals_reopens_the_offer() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    for ch in "/approvals".chars() {
        press(&mut app, Key::Char(ch));
    }
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(app.offer_open());
    assert!(app.notice().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn a_badge_click_reopens_it() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    app.on_click(crate::mouse::TargetId::Badge);
    assert!(app.offer_open());
}

#[test]
fn the_close_target_puts_it_aside() {
    let mut app = offered();
    app.on_click(crate::mouse::TargetId::Offer(crate::offer::Spot::Close));
    assert!(!app.offer_open());
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn esc_closes_the_notice_list_before_the_offer() {
    let mut app = offered();
    app.on_line(session_line(
        S_A,
        "notice",
        json!({"code": "internal", "message": "hello"}),
        None,
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    press(&mut app, Key::Esc);
    assert!(app.notice_overlay().is_none());
    assert!(app.offer_open());
    press(&mut app, Key::Esc);
    assert!(!app.offer_open());
}

#[test]
fn with_the_approval_panel_open_keys_go_to_the_panel() {
    let mut app = offered();
    app.on_line(approval("r_9"));
    assert!(app.panel().is_some());
    press(&mut app, Key::Char('n'));
    press(&mut app, Key::Char('o'));
    let lines = sent(press(&mut app, Key::Enter));
    assert_eq!(lines[0]["args"]["request_id"], "r_9");
    assert_eq!(lines[0]["args"]["feedback"], "no");
    // The offer is still open, and answered next.
    assert!(app.offer_open());
}

#[test]
fn a_paste_with_the_approval_panel_open_goes_to_its_feedback() {
    let mut app = offered();
    app.on_line(approval("r_9"));
    assert_eq!(app.on_edit(Edit::Paste("why".to_owned())), Effect::None);
    let lines = sent(press(&mut app, Key::Enter));
    assert_eq!(lines[0]["args"]["feedback"], "why");
}

#[test]
fn f1_opens_the_key_map_over_it() {
    let mut app = offered();
    press(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    press(&mut app, Key::Esc);
    assert!(app.keymap_top().is_none());
    assert!(app.offer_open());
}

#[test]
fn typed_characters_and_arrows_never_reach_the_draft() {
    let mut app = offered();
    for ch in "hi".chars() {
        press(&mut app, Key::Char(ch));
    }
    app.on_edit(Edit::Paste("x".to_owned()));
    app.on_edit(Edit::Left);
    press(&mut app, Key::Backspace);
    assert_eq!(app.draft(), "");
}

#[test]
fn going_home_drops_the_offer() {
    let mut app = offered();
    // The view takes typed keys, so it is put aside first.
    press(&mut app, Key::Esc);
    for ch in "/home".chars() {
        press(&mut app, Key::Char(ch));
    }
    press(&mut app, Key::Enter);
    assert!(app.session().is_none());
    assert!(app.badge().is_none());
    press(&mut app, Key::AltA);
    assert!(!app.offer_open());
}
