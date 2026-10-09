//! Tests for the rail's state: card numbers, switching, keys and the
//! wheel.

use super::super::{App, Effect};
use super::Spot;
use crate::home::{Launch, Spot as HomeSpot};
use crate::keys::{Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const C: &str = "s_cccccccccccccccc";
const D: &str = "s_dddddddddddddddd";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app on home at 200x40, drawing the default card list.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.set_size(200, 40);
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

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Links the app: the feed and recent ids.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
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

/// A live `session_status` for `session`, idle unless `fields` say
/// otherwise.
fn live(session: &str, fields: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "state": "idle", "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in fields.as_object().cloned().unwrap_or_default() {
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

/// A hub `session_left` for `session`, ended `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The row key of `session`'s card.
fn key(app: &App, session: &str) -> u64 {
    app.rail_cards()
        .and_then(|(cards, _)| {
            cards
                .iter()
                .find(|row| row.id.0 == session)
                .map(|row| row.key)
        })
        .unwrap_or_else(|| panic!("a card for {session}"))
}

/// The card number of `session`, if it is a card with one.
fn number(app: &App, session: &str) -> Option<usize> {
    let key = app.rail_cards().and_then(|(cards, _)| {
        cards
            .iter()
            .find(|row| row.id.0 == session)
            .map(|row| row.key)
    })?;
    app.rail_state().number(key)
}

/// A linked app on home with `sessions` live and idle, in order.
fn with(sessions: &[&str]) -> App {
    let mut app = home();
    linked(&mut app);
    for session in sessions {
        app.on_line(live(session, json!({})));
    }
    app
}

#[test]
fn a_new_row_takes_the_smallest_free_number() {
    let app = with(&[A, B, C]);
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    assert_eq!(number(&app, C), Some(3));
}

#[test]
fn numbers_stay_through_waiting_and_back() {
    let mut app = with(&[A, B]);
    let waiting = json!({"state": "waiting", "waiting": {"request_id": "r_1",
        "kind": "approval", "summary": "shell ls"}});
    app.on_line(live(B, waiting));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    app.on_line(live(B, json!({})));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn an_exited_card_leaves_a_gap_until_a_new_session_fills_it() {
    let mut app = with(&[A, B, C]);
    app.on_line(left(B, "exited"));
    assert_eq!(number(&app, C), Some(3));
    app.on_line(live(D, json!({})));
    assert_eq!(number(&app, D), Some(2));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, C), Some(3));
}

#[test]
fn an_exited_row_gives_up_its_number() {
    let mut app = with(&[A, B]);
    let a = key(&app, A);
    app.on_line(left(A, "exited"));
    assert_eq!(app.rail_state().number(a), None);
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn a_crashed_row_keeps_its_number() {
    let mut app = with(&[A, B]);
    app.on_line(left(A, "crashed"));
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
}

#[test]
fn a_recent_row_gets_no_number() {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": recent, "result": {"sessions": [
            {"session_id": A, "project": "-w", "workspace": "/w", "name": "old",
             "how": "crashed"}]}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.rows.len() == 1)
    );
    assert_eq!(app.rail_cards().map(|(cards, _)| cards.len()), Some(0));
    app.on_line(live(B, json!({})));
    assert_eq!(number(&app, B), Some(1));
}

#[test]
fn numbers_are_kept_without_the_rail() {
    let app = with(&[A]);
    assert_eq!(number(&app, A), Some(1));
}

#[test]
fn no_home_no_numbers() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(A.to_owned()));
    app.on_line(hello());
    app.on_line(live(A, json!({})));
    assert!(app.rail_cards().is_none());
    assert_eq!(app.rail_state().number(0), None);
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

/// The lines an effect sends, parsed; none for any other effect.
fn sent(effect: Effect) -> Vec<Value> {
    if let Effect::Send(lines) = effect {
        commands(lines)
    } else {
        Vec::new()
    }
}

/// A session `command_accepted` for `id` from `session`.
fn accepted(session: &str, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"command_id": id})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// Accepts every subscribe in `out`.
fn ack(app: &mut App, out: &[Value]) {
    for line in out.iter().filter(|line| line["command"] == "subscribe") {
        let session = line["session_id"].as_str().unwrap_or_default();
        let id = line["id"].as_str().unwrap_or_default();
        app.on_line(accepted(session, id));
    }
}

/// Opens `session` from home and accepts its subscribes.
fn open(app: &mut App, session: &str) -> Vec<Value> {
    let key = key(app, session);
    let out = sent(app.on_click(TargetId::Home(HomeSpot::Entry(key))));
    assert!(!out.is_empty(), "opening {session} sends");
    ack(app, &out);
    out
}

/// A linked app with `sessions` live, showing `on`.
fn opened(sessions: &[&str], on: &str) -> App {
    let mut app = with(sessions);
    open(&mut app, on);
    assert_eq!(app.session().map(|id| id.0.as_str()), Some(on));
    app
}

/// Clicks `session`'s card.
fn click(app: &mut App, session: &str) -> Effect {
    let key = key(app, session);
    app.on_click(TargetId::Rail(Spot::Card(key)))
}

/// A `subscribe` line with `id` for `session` at `level`.
fn subscribe(id: &Value, session: &str, level: &str) -> Value {
    json!({"id": id, "command": "subscribe", "session_id": session,
        "args": {"level": level}})
}

/// A `commands` line with `id` for `session`.
fn asks_commands(id: &Value, session: &str) -> Value {
    json!({"id": id, "command": "commands", "session_id": session})
}

/// The session on screen.
fn on_screen(app: &App) -> Option<&str> {
    app.session().map(|id| id.0.as_str())
}

/// Ten sessions' ids, `s_` and the number in 16 hex digits.
fn ten() -> Vec<String> {
    (1..=10).map(|n| format!("s_{n:016x}")).collect()
}

#[test]
fn a_click_switches_lowering_the_old_and_opening_the_new() {
    let mut app = opened(&[A, B], A);
    let out = sent(click(&mut app, B));
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(
        out,
        vec![
            subscribe(&out[0]["id"], A, "summary"),
            subscribe(&out[1]["id"], B, "full"),
            asks_commands(&out[2]["id"], B),
        ]
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn a_switch_to_a_session_held_at_full_sends_summary_then_full() {
    // `B` is held at `full` when it crashes, so leaving it lowers
    // nothing.
    let mut app = opened(&[A, B, C], B);
    app.on_line(left(B, "crashed"));
    let out = sent(click(&mut app, A));
    ack(&mut app, &out);
    let out = sent(click(&mut app, B));
    assert_eq!(
        out,
        vec![
            subscribe(&out[0]["id"], A, "summary"),
            subscribe(&out[1]["id"], B, "summary"),
            subscribe(&out[2]["id"], B, "full"),
            asks_commands(&out[3]["id"], B),
        ]
    );
}

#[test]
fn alt_n_switches() {
    let ids = ten();
    let sessions: Vec<&str> = ids.iter().take(9).map(String::as_str).collect();
    let mut app = opened(&sessions, sessions[4]);
    let out = sent(press(&mut app, Key::AltDigit(1)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], sessions[0], "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(sessions[0]));
    ack(&mut app, &out);
    let out = sent(press(&mut app, Key::AltDigit(9)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], sessions[8], "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(sessions[8]));
}

#[test]
fn alt_n_with_no_such_card_does_nothing() {
    let mut app = opened(&[A, B], A);
    assert_eq!(press(&mut app, Key::AltDigit(3)), Effect::None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn alt_n_on_home_opens_the_card() {
    let mut app = with(&[A, B]);
    let out = sent(press(&mut app, Key::AltDigit(2)));
    assert_eq!(
        out,
        vec![
            subscribe(&out[0]["id"], B, "full"),
            asks_commands(&out[1]["id"], B),
        ]
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn alt_n_without_home_does_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(A.to_owned()));
    app.on_line(hello());
    app.on_line(live(A, json!({})));
    app.on_line(live(B, json!({})));
    assert_eq!(press(&mut app, Key::AltDigit(1)), Effect::None);
    assert_eq!(press(&mut app, Key::AltDigit(2)), Effect::None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn a_click_on_the_session_on_screen_does_nothing() {
    let mut app = opened(&[A, B], A);
    assert_eq!(click(&mut app, A), Effect::None);
    assert_eq!(on_screen(&app), Some(A));
}

/// A `permission_requested` from `session` for `request`.
fn permission(session: &str, request: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "permission_requested".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: json!({"request_id": request, "effects": ["executes"],
            "reversible": true, "step": "review"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn a_switch_with_the_approval_panel_open_still_opens() {
    let mut app = opened(&[A, B], A);
    app.on_line(permission(A, "r_1"));
    assert!(app.panel().is_some());
    let out = sent(press(&mut app, Key::AltDigit(2)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], B, "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn a_switch_with_the_key_map_open_still_opens() {
    let mut app = opened(&[A, B], A);
    press(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    let out = sent(press(&mut app, Key::AltDigit(2)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], B, "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn a_switch_with_the_link_down_does_nothing() {
    let mut app = opened(&[A, B], A);
    app.disconnected();
    assert_eq!(click(&mut app, B), Effect::None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn a_switch_while_a_start_is_pending_does_nothing() {
    let mut app = with(&[A, B]);
    for ch in "hi".chars() {
        press(&mut app, Key::Char(ch));
    }
    let start = sent(press(&mut app, Key::Enter));
    assert_eq!(start[0]["command"], "start");
    assert_eq!(press(&mut app, Key::AltDigit(2)), Effect::None);
    assert_eq!(on_screen(&app), None);
}

#[test]
fn an_unreadable_card_gives_the_notice_and_sends_nothing() {
    let mut app = opened(&[A, B], A);
    let Line::Session(mut envelope) = live(C, json!({})) else {
        panic!("a session line");
    };
    envelope.schema_version = contract::SCHEMA_VERSION + 1;
    app.on_line(Line::Session(envelope));
    assert_eq!(click(&mut app, C), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Cannot attach: this session's schema is newer than this terminal reads.")
    );
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn a_crashed_card_click_resumes_it() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(left(B, "crashed"));
    let out = sent(click(&mut app, B));
    assert!(
        out.contains(&subscribe(&out[1]["id"], B, "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn after_a_switch_only_the_new_session_is_held_at_full() {
    let mut app = opened(&[A, B], A);
    let mut levels: Vec<(String, String)> = Vec::new();
    for session in [B, A] {
        let out = sent(click(&mut app, session));
        ack(&mut app, &out);
        for line in out.iter().filter(|line| line["command"] == "subscribe") {
            let id = line["session_id"].as_str().unwrap_or_default().to_owned();
            let level = line["args"]["level"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            levels.retain(|(held, _)| *held != id);
            levels.push((id, level));
        }
    }
    levels.sort();
    assert_eq!(
        levels,
        vec![
            (A.to_owned(), "full".to_owned()),
            (B.to_owned(), "summary".to_owned()),
        ]
    );
}

/// A wheel `kind` at 0-based `col`, `row`.
fn wheel(kind: MouseKind, col: u16, row: u16) -> Mouse {
    Mouse { kind, col, row }
}

/// Ten live sessions, the first on screen: one group of ten cards is
/// 61 rows, 21 past a 40-row rail, with cards starting on rows 1, 7, 13,
/// 19 and 25.
fn tall() -> App {
    let ids = ten();
    let sessions: Vec<&str> = ids.iter().map(String::as_str).collect();
    opened(&sessions, sessions[0])
}

#[test]
fn the_wheel_scrolls_the_rail_by_one_card_and_clamps() {
    let mut app = tall();
    let down = wheel(MouseKind::WheelDown, 5, 10);
    for expected in [1, 7, 13, 19, 21, 21] {
        app.on_wheel(&down);
        assert_eq!(app.rail_state().scroll(), expected);
    }
    let up = wheel(MouseKind::WheelUp, 5, 10);
    for expected in [19, 13, 7, 1, 0, 0] {
        app.on_wheel(&up);
        assert_eq!(app.rail_state().scroll(), expected);
    }
}

#[test]
fn the_wheel_over_the_conversation_does_not_scroll_the_rail() {
    let mut app = tall();
    app.on_wheel(&wheel(MouseKind::WheelDown, 100, 10));
    assert_eq!(app.rail_state().scroll(), 0);
}

#[test]
fn a_scroll_past_the_end_after_a_resize_moves_on_the_first_wheel_up() {
    let mut app = tall();
    let down = wheel(MouseKind::WheelDown, 5, 10);
    for _ in 0..5 {
        app.on_wheel(&down);
    }
    assert_eq!(app.rail_state().scroll(), 21);
    // At 50 rows the end is 11: up from there is the card on row 7.
    app.set_size(200, 50);
    app.on_wheel(&wheel(MouseKind::WheelUp, 5, 10));
    assert_eq!(app.rail_state().scroll(), 7);
}

#[test]
fn switching_scrolls_the_card_into_view() {
    let mut app = tall();
    // The ninth card starts on row 49 and ends on row 54: the least
    // scroll showing it in 40 rows is 15.
    press(&mut app, Key::AltDigit(9));
    assert_eq!(app.rail_state().scroll(), 15);
    press(&mut app, Key::AltDigit(1));
    assert_eq!(app.rail_state().scroll(), 1);
}

/// A waiting `session_status` field set: request `request` since `since`.
fn waiting(request: &str, since: u64) -> Value {
    json!({"state": "waiting", "since": since, "waiting": {"request_id": request,
        "kind": "approval", "summary": "shell ls"}})
}

/// A linked app showing `A`, with `B` and `C` waiting on `r_b` since 200
/// and `r_c` since 100.
fn two_waiting() -> App {
    let mut app = opened(&[A, B, C], A);
    app.on_line(live(B, waiting("r_b", 200)));
    app.on_line(live(C, waiting("r_c", 100)));
    app
}

/// The approval panel's text, or nothing while it is closed.
fn panel_text(app: &App) -> String {
    app.panel()
        .map(|panel| panel.lines.join("\n"))
        .unwrap_or_default()
}

/// Puts aside the request the approval panel shows.
fn put_aside(app: &mut App) {
    assert!(app.panel().is_some());
    press(app, Key::Esc);
    assert!(app.panel().is_none());
}

/// An `interaction_requested` form from `session` for `request`.
fn question(session: &str, request: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "interaction_requested".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"request_id": request, "kind": "form", "action_ids": ["a_1"],
            "fields": [{"header": "Name", "question": "What name?"}]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn alt_a_opens_the_queue_first() {
    let mut app = two_waiting();
    app.on_line(permission(A, "r_a"));
    put_aside(&mut app);
    assert_eq!(press(&mut app, Key::AltA), Effect::None);
    assert!(panel_text(&app).contains(A), "{}", panel_text(&app));
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn alt_a_reopens_a_put_aside_offer_first() {
    let mut app = two_waiting();
    let items = [json!({"kind": "mcp_server", "name": "a", "hash": "h",
        "required": false, "summary": "MCP server: a"})];
    app.on_line(Line::Session(contract::Envelope {
        kind: "repository_code_offered".to_owned(),
        session_id: contract::SessionId(A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"request_id": "r_o", "items": items})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    assert!(app.offer_open());
    press(&mut app, Key::Esc);
    assert!(!app.offer_open());
    assert_eq!(press(&mut app, Key::AltA), Effect::None);
    assert!(app.offer_open());
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn alt_a_switches_to_the_oldest_waiting_row_and_opens_its_request_on_arrival() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    assert!(
        out.contains(&subscribe(&out[1]["id"], C, "full")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(C));
    // The replay follows the subscribe's acknowledgement.
    ack(&mut app, &out);
    // Another session's request is put aside, so the queue would not
    // surface the replay by itself.
    app.on_line(permission(B, "r_b"));
    put_aside(&mut app);
    app.on_line(permission(C, "r_c"));
    assert!(panel_text(&app).contains(C), "{}", panel_text(&app));
}

#[test]
fn alt_a_opens_a_waiting_question_on_arrival() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    ack(&mut app, &out);
    app.on_line(permission(B, "r_b"));
    put_aside(&mut app);
    app.on_line(question(C, "r_c"));
    assert!(panel_text(&app).contains(C), "{}", panel_text(&app));
}

#[test]
fn alt_a_with_equal_since_takes_the_earlier_card() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(live(B, waiting("r_b", 100)));
    app.on_line(live(C, waiting("r_c", 100)));
    press(&mut app, Key::AltA);
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn alt_a_skips_the_attached_session() {
    let mut app = opened(&[A, B], A);
    app.on_line(live(A, waiting("r_a", 0)));
    app.on_line(live(B, waiting("r_b", 100)));
    press(&mut app, Key::AltA);
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn alt_a_skips_a_crashed_card() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(live(B, waiting("r_b", 0)));
    app.on_line(left(B, "crashed"));
    app.on_line(live(C, waiting("r_c", 100)));
    press(&mut app, Key::AltA);
    assert_eq!(on_screen(&app), Some(C));
}

#[test]
fn alt_a_with_nothing_waiting_says_so() {
    let mut app = opened(&[A, B], A);
    assert_eq!(press(&mut app, Key::AltA), Effect::None);
    assert_eq!(app.notice(), Some("No requests waiting."));
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn the_badge_and_slash_approvals_do_what_alt_a_does() {
    let mut app = two_waiting();
    app.on_click(TargetId::Badge);
    assert_eq!(on_screen(&app), Some(C));
    let mut app = two_waiting();
    for ch in "/approvals".chars() {
        press(&mut app, Key::Char(ch));
    }
    press(&mut app, Key::Enter);
    assert_eq!(on_screen(&app), Some(C));
}

#[test]
fn a_request_from_another_session_with_the_same_id_does_not_open() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    ack(&mut app, &out);
    app.on_line(permission(A, "r_a"));
    put_aside(&mut app);
    app.on_line(permission(B, "r_c"));
    assert!(app.panel().is_none());
    app.on_line(permission(C, "r_c"));
    assert!(panel_text(&app).contains(C), "{}", panel_text(&app));
}

#[test]
fn another_request_from_that_session_does_not_open() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    ack(&mut app, &out);
    app.on_line(permission(A, "r_a"));
    put_aside(&mut app);
    app.on_line(permission(C, "r_other"));
    assert!(app.panel().is_none());
    app.on_line(permission(C, "r_c"));
    assert!(panel_text(&app).contains(C), "{}", panel_text(&app));
}

#[test]
fn a_request_already_answered_does_not_reopen() {
    let mut app = two_waiting();
    // `C`'s request is answered while `A` is on screen; its reply waits
    // for the hub.
    app.on_line(permission(C, "r_c"));
    assert!(panel_text(&app).contains(C));
    press(&mut app, Key::Enter);
    assert!(app.panel().is_none());
    let out = sent(press(&mut app, Key::AltA));
    assert_eq!(on_screen(&app), Some(C));
    ack(&mut app, &out);
    app.on_line(permission(C, "r_c"));
    assert!(app.panel().is_none(), "{}", panel_text(&app));
}

/// A hub answer of `kind` for command `id`, refusing with `message`.
fn hub_answer(kind: &str, id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "code": "invalid_arguments", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// A linked app showing `A`, with `B` crashed and `C` live.
fn crashed() -> App {
    let mut app = opened(&[A, B, C], A);
    app.on_line(left(B, "crashed"));
    app
}

/// Clicks `session`'s ✕.
fn cross(app: &mut App, session: &str) -> Vec<Value> {
    let key = key(app, session);
    sent(app.on_click(TargetId::Rail(Spot::Dismiss(key))))
}

/// Whether `session` has a card.
fn carded(app: &App, session: &str) -> bool {
    app.rail_cards()
        .is_some_and(|(cards, _)| cards.iter().any(|row| row.id.0 == session))
}

#[test]
fn x_sends_dismiss_and_an_accept_removes_the_card() {
    let mut app = crashed();
    let out = cross(&mut app, B);
    assert_eq!(
        out,
        vec![json!({"id": out[0]["id"], "command": "dismiss", "args": {"session": B}})]
    );
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    assert!(
        app.on_line(hub_answer("command_accepted", &id, ""))
            .is_empty()
    );
    assert!(!carded(&app, B));
    assert!(carded(&app, C));
}

#[test]
fn a_refused_dismiss_notes_the_row_and_adds_a_notice() {
    let mut app = crashed();
    let out = cross(&mut app, B);
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(hub_answer("command_rejected", &id, "still running"));
    assert!(carded(&app, B));
    assert_eq!(app.notice(), Some("still running"));
    let note = app.rail_cards().and_then(|(cards, _)| {
        cards
            .iter()
            .find(|row| row.id.0 == B)
            .and_then(|row| row.note.clone())
    });
    assert_eq!(note.as_deref(), Some("still running"));
    // The answer is taken: a second one is no dismiss's.
    app.on_line(hub_answer("command_accepted", &id, ""));
    assert!(carded(&app, B));
}

#[test]
fn an_unrelated_hub_answer_is_not_a_dismiss() {
    let mut app = crashed();
    let out = cross(&mut app, B);
    app.on_line(hub_answer("command_accepted", "c_unrelated", ""));
    assert!(carded(&app, B));
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(hub_answer("command_accepted", &id, ""));
    assert!(!carded(&app, B));
}

#[test]
fn x_with_the_link_down_sends_nothing() {
    let mut app = crashed();
    app.disconnected();
    assert!(cross(&mut app, B).is_empty());
}

#[test]
fn x_on_a_live_card_sends_nothing() {
    let mut app = crashed();
    assert!(cross(&mut app, C).is_empty());
}

#[test]
fn a_dismissed_card_leaves_a_gap_until_a_new_session_fills_it() {
    let mut app = crashed();
    let out = cross(&mut app, B);
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(hub_answer("command_accepted", &id, ""));
    assert_eq!(number(&app, C), Some(3));
    app.on_line(live(D, json!({})));
    assert_eq!(number(&app, D), Some(2));
    assert_eq!(number(&app, A), Some(1));
}

#[test]
fn plus_goes_home_with_that_projects_workspace() {
    let mut app = opened(&[A, B], A);
    app.on_line(live(
        B,
        json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
    ));
    let key = key(&app, B);
    let out = sent(app.on_click(TargetId::Rail(Spot::Start(key))));
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    assert_eq!(on_screen(&app), None);
    let chip = app
        .home_screen()
        .and_then(|screen| screen.chips.first().map(|(_, chip)| chip.clone()));
    assert_eq!(chip.as_deref(), Some("[hub]"));
    assert_eq!(app.focused(), None);
}

#[test]
fn a_resolved_request_ends_the_wait() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    ack(&mut app, &out);
    app.on_line(permission(A, "r_a"));
    put_aside(&mut app);
    let Line::Session(mut resolved) = permission(C, "r_c") else {
        panic!("a session line");
    };
    resolved.kind = "permission_resolved".to_owned();
    app.on_line(Line::Session(resolved));
    app.on_line(permission(C, "r_c"));
    assert!(app.panel().is_none(), "{}", panel_text(&app));
}
