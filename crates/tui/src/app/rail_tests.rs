//! Tests for the rail's state: card numbers, switching, keys and the
//! wheel.

use super::super::{App, Effect};
use super::Spot;
use crate::home::{Launch, Spot as HomeSpot};
use crate::keys::{Key, Mouse, MouseKind};
use crate::keyset::Context;
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::stroke::{Code, Mods, Stroke};
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        model: Some("test/model".to_owned()),
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

/// Links the app: the feed and recent ids. An open picker's listing
/// goes out beside them.
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

/// A live `session_status` for `session`, idle unless `fields` say
/// otherwise.
fn live(session: &str, fields: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "state": "idle", "started": 0, "calls": 0, "since": 0,
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
fn remote_rail_has_no_launch_project() {
    let mut app = home();
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        ..Default::default()
    });
    linked(&mut app);
    app.on_line(live(A, json!({"project":"other"})));
    app.on_line(live(B, json!({"project":"sentinel-project"})));
    let (cards, project) = app.rail_cards().unwrap();
    assert_eq!(project, None);
    assert_eq!(
        cards
            .iter()
            .map(|row| row.id.0.as_str())
            .collect::<Vec<_>>(),
        [A, B]
    );
}

#[test]
fn a_new_row_takes_the_smallest_free_number() {
    let app = with(&[A, B, C]);
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    assert_eq!(number(&app, C), Some(3));
}

#[test]
fn a_first_card_gets_one() {
    let app = with(&[A]);
    assert_eq!(number(&app, A), Some(1));
}

#[test]
fn a_fourth_card_gets_four() {
    let app = with(&[A, B, C, D]);
    assert_eq!(number(&app, A), Some(1));
    assert_eq!(number(&app, B), Some(2));
    assert_eq!(number(&app, C), Some(3));
    assert_eq!(number(&app, D), Some(4));
}

#[test]
fn a_new_card_takes_one_when_it_is_free() {
    let mut app = with(&[A, B, C]);
    app.on_line(left(A, "exited"));
    assert_eq!(number(&app, B), Some(2));
    assert_eq!(number(&app, C), Some(3));
    app.on_line(live(D, json!({})));
    assert_eq!(number(&app, D), Some(1));
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
            .is_some_and(|screen| crate::home::home_test_rows(&screen).len() == 1)
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

/// Accepts every subscribe in `out`, then the staged open's raise:
/// the first ack leaves the `full` raise due, and the raise's ack
/// ends the gate. With no open due nothing further goes out.
fn ack(app: &mut App, out: &[Value]) {
    for line in out.iter().filter(|line| line["command"] == "subscribe") {
        let session = line["session_id"].as_str().unwrap_or_default();
        let id = line["id"].as_str().unwrap_or_default();
        app.on_line(accepted(session, id));
    }
    if let Some(line) = app.take_raise() {
        let raise: Value =
            serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
        let session = raise["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a raise session"))
            .to_owned();
        let id = raise["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a raise id"))
            .to_owned();
        app.on_line(accepted(&session, &id));
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
    assert_eq!(out.len(), 2, "commands waits for acceptance: {out:?}");
    assert_eq!(
        out,
        vec![
            subscribe(&out[0]["id"], A, "summary"),
            subscribe(&out[1]["id"], B, "summary"),
        ]
    );
    let commands = commands(app.on_line(accepted(B, out[1]["id"].as_str().unwrap_or_default())));
    assert_eq!(commands, vec![asks_commands(&commands[0]["id"], B)]);
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
        ]
    );
    // The first stage answered, the `full` raise follows.
    for line in out.iter().filter(|line| line["command"] == "subscribe") {
        let target = line["session_id"].as_str().unwrap_or_default();
        let id = line["id"].as_str().unwrap_or_default();
        app.on_line(accepted(target, id));
    }
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["session_id"], B);
    assert_eq!(raise["args"]["level"], "full");
}

#[test]
fn alt_n_switches() {
    let ids = ten();
    let sessions: Vec<&str> = ids.iter().take(9).map(String::as_str).collect();
    let mut app = opened(&sessions, sessions[4]);
    let out = sent(press(&mut app, Key::AltDigit(1)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], sessions[0], "summary")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(sessions[0]));
    ack(&mut app, &out);
    let out = sent(press(&mut app, Key::AltDigit(9)));
    assert!(
        out.contains(&subscribe(&out[1]["id"], sessions[8], "summary")),
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
    assert_eq!(out, vec![subscribe(&out[0]["id"], B, "summary")]);
    let commands = commands(app.on_line(accepted(B, out[0]["id"].as_str().unwrap_or_default())));
    assert_eq!(commands, vec![asks_commands(&commands[0]["id"], B)]);
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
        out.contains(&subscribe(&out[1]["id"], B, "summary")),
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
        out.contains(&subscribe(&out[1]["id"], B, "summary")),
        "{out:?}"
    );
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn alt_r_with_the_key_map_open_still_hides() {
    let mut app = sized(200);
    press(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    assert!(rail_rect(&app).is_some());
    // ⌥R reaches past the key map to the rail, leaving the map open.
    assert_eq!(press(&mut app, Key::AltR), Effect::None);
    assert!(rail_rect(&app).is_none());
    assert!(app.keymap_top().is_some());
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
        Some("Cannot attach: its status did not parse.")
    );
    assert!(app.notices().iter().any(|line| matches!(
        line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "session_held"
            && text == "Cannot attach: its status did not parse."
    )));
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn an_unreadable_card_after_a_differing_hello_names_the_hubs_build() {
    let mut app = opened(&[A, B], A);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"fiber_version": "9.9.9", "fiber_commit": "2121f38d1e4f0a9b"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    app.on_line(live(C, json!({"state": 7})));
    assert_eq!(click(&mut app, C), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Cannot attach: this session runs Fiber 9.9.9 (2121f38).")
    );
}

#[test]
fn a_crashed_card_click_resumes_it() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(left(B, "crashed"));
    let out = sent(click(&mut app, B));
    let first = out
        .iter()
        .find(|line| line["command"] == "subscribe" && line["session_id"] == B)
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a first stage for {B}: {out:?}"))
        .to_owned();
    let commands = commands(app.on_line(accepted(B, &first)));
    assert_eq!(commands, vec![asks_commands(&commands[0]["id"], B)]);
    // The first frame releases the raise: the crashed session opens
    // at `full` after it.
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    let raise: Value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
    assert_eq!(raise, subscribe(&raise["id"], B, "full"));
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn after_a_switch_only_the_new_session_is_held_at_full() {
    let mut app = opened(&[A, B], A);
    let mut levels: Vec<(String, String)> = Vec::new();
    for session in [B, A] {
        let out = sent(click(&mut app, session));
        for line in out.iter().filter(|line| line["command"] == "subscribe") {
            let target = line["session_id"].as_str().unwrap_or_default();
            let id = line["id"].as_str().unwrap_or_default();
            app.on_line(accepted(target, id));
            let level = line["args"]["level"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            levels.retain(|(held, _)| held != target);
            levels.push((target.to_owned(), level));
        }
        // The first frame releases the raise; the switch ends holding
        // its level.
        let raise: Value =
            serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
                .unwrap_or_else(|err| panic!("{err}"));
        let target = raise["session_id"].as_str().unwrap_or_default().to_owned();
        let level = raise["args"]["level"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let id = raise["id"].as_str().unwrap_or_default().to_owned();
        levels.retain(|(held, _)| *held != target);
        levels.push((target.clone(), level));
        app.on_line(accepted(&target, &id));
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

#[test]
fn revealing_a_card_at_the_top_edge_keeps_the_scroll() {
    let mut app = tall();
    let down = wheel(MouseKind::WheelDown, 5, 10);
    app.on_wheel(&down);
    app.on_wheel(&down);
    assert_eq!(app.rail_state().scroll(), 7);
    // The second card starts on row 7, exactly at the top edge: the
    // scroll stays.
    press(&mut app, Key::AltDigit(2));
    assert_eq!(app.rail_state().scroll(), 7);
}

#[test]
fn revealing_a_card_ending_at_the_bottom_edge_keeps_the_scroll() {
    let mut app = tall();
    // A 12-row rail: cards start every 6 rows, so the third card's end
    // (19) is exactly the second card's start (7) plus the height.
    app.set_size(200, 12);
    let down = wheel(MouseKind::WheelDown, 5, 5);
    app.on_wheel(&down);
    app.on_wheel(&down);
    assert_eq!(app.rail_state().scroll(), 7);
    press(&mut app, Key::AltDigit(3));
    assert_eq!(app.rail_state().scroll(), 7);
}

#[test]
fn a_card_taller_than_the_rail_shows_its_bottom() {
    // A card is 6 rows and the floor keeps the rail at 10 or more, so
    // no drawn rail is shorter than a card. This pins the arithmetic
    // `sync_rail` uses when one is: the card's bottom edge sets the
    // scroll.
    let (start, height, scroll) = (10usize, 4usize, 20usize);
    let end = start + crate::view::rail::CARD_ROWS;
    assert_eq!(scroll.min(start).max(end.saturating_sub(height)), 12);
}

/// A waiting `session_status` field set: request `request` since `since`.
fn waiting(request: &str, since: u64) -> Value {
    json!({"state": "waiting", "started": 0, "calls": 0, "since": since, "waiting": {"request_id": request,
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
    // The header shows the feed row's name, shared by every row here.
    assert!(
        panel_text(&app).contains("fix the parser"),
        "{}",
        panel_text(&app)
    );
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
        out.contains(&subscribe(&out[1]["id"], C, "summary")),
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
    assert!(
        panel_text(&app).contains("fix the parser"),
        "{}",
        panel_text(&app)
    );
}

#[test]
fn alt_a_opens_a_waiting_question_on_arrival() {
    let mut app = two_waiting();
    let out = sent(press(&mut app, Key::AltA));
    ack(&mut app, &out);
    app.on_line(permission(B, "r_b"));
    put_aside(&mut app);
    app.on_line(question(C, "r_c"));
    assert!(
        panel_text(&app).contains("fix the parser"),
        "{}",
        panel_text(&app)
    );
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
    assert!(
        panel_text(&app).contains("fix the parser"),
        "{}",
        panel_text(&app)
    );
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
    assert!(
        panel_text(&app).contains("fix the parser"),
        "{}",
        panel_text(&app)
    );
}

#[test]
fn a_request_already_answered_does_not_reopen() {
    let mut app = two_waiting();
    // `C`'s request is answered while `A` is on screen; its reply waits
    // for the hub.
    app.on_line(permission(C, "r_c"));
    assert!(panel_text(&app).contains("fix the parser"));
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
        .and_then(|screen| screen.chips.first().map(|chip| chip.text.clone()));
    assert_eq!(chip.as_deref(), Some("[hub]"));
    assert_eq!(app.focused(), None);
}

// debt: kept until the mutant map stops naming
// clicking_done_opens_home_on_the_first_exited_row as the unique catcher of
// three mutants; re-propose deletion only with a replacement test that
// catches all three.
/// A header's "N ✓" sits in drawn order right after its "+": Up
/// from the card reaches the count, then the chip, and Down walks
/// back.
#[test]
fn done_sits_in_drawn_order_right_after_plus() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(B, "exited"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    // Down holds on the last stop.
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

/// A click on "N ✓" opens home with focus on the project's first
/// exited row.
#[test]
fn clicking_done_opens_home_on_the_first_exited_row() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(left(B, "exited"));
    app.on_line(left(C, "exited"));
    // The launch group's "N ✓" is keyed by its first card, as "+" is.
    let out = sent(app.on_click(TargetId::Rail(Spot::Done(key(&app, A)))));
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // The feed reversed lists the later row first, so C's row is the
    // project's first exited row.
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, C))))
    );
}

/// Enter on a focused "N ✓" does what a click does: home with focus
/// on the project's first exited row, clearing the rail's focus.
#[test]
fn enter_on_done_opens_home_on_the_first_exited_row() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(B, "exited"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    let effect = stroke_press(&mut app, plain(Code::Enter));
    assert!(matches!(effect, Effect::Send(_)), "{effect:?}");
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, B))))
    );
}

/// A click on "0 ✓" leaves home with no row focus: a plain `leave`
/// lands on the entry bar.
#[test]
fn clicking_done_with_no_exits_lands_home_unfocused() {
    let mut app = opened(&[A, B], A);
    let out = sent(app.on_click(TargetId::Rail(Spot::Done(key(&app, A)))));
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
}

/// A click on "N ✓" expands `more` hiding exited rows: the first
/// exited row lands focused even with four exits behind a collapsed
/// project.
#[test]
fn clicking_done_expands_more_around_the_first_exited_row() {
    const E: &str = "s_eeeeeeeeeeeeeeee";
    let mut app = opened(&[A, B, C, D, E], A);
    for session in [B, C, D, E] {
        app.on_line(left(session, "exited"));
    }
    // Four exits collapse the project to three shown behind `more`.
    let hidden = app.home.as_ref().and_then(|home| {
        home.sessions
            .sections(home.launch_project())
            .into_iter()
            .find(|section| section.project == "-w")
            .map(|section| section.hidden())
    });
    assert_eq!(hidden, Some(1));
    let out = sent(app.on_click(TargetId::Rail(Spot::Done(key(&app, A)))));
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // The feed reversed lists the latest row first, and the project
    // stands expanded.
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, E))))
    );
    let hidden = app.home.as_ref().and_then(|home| {
        home.sessions
            .sections(home.launch_project())
            .into_iter()
            .find(|section| section.project == "-w")
            .map(|section| section.hidden())
    });
    assert_eq!(hidden, Some(0));
}

/// The #2438 focus-table row this ticket adds back: `"N ✓" in a
/// rail project header → the project's first exited row`, entered by
/// click and by key, with Esc and `→` returning to the session left.
#[test]
fn done_lands_on_the_first_exited_row_and_the_back_key_returns() {
    let mut app = opened(&[A, B, C], A);
    app.on_line(left(B, "exited"));
    app.on_line(left(C, "exited"));
    // By click: home with the first exited row focused ...
    let out = sent(app.on_click(TargetId::Rail(Spot::Done(key(&app, A)))));
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    ack(&mut app, &out);
    assert!(app.on_home());
    framed(&mut app);
    framed(&mut app);
    // The feed reversed lists the later row first, so C's row is the
    // project's first exited row.
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, C))))
    );
    // ... and Esc returns to the session left, asking for its commands.
    let out = sent(press(&mut app, Key::Esc));
    assert_eq!(out, vec![asks_commands(&out[0]["id"], A)]);
    assert_eq!(on_screen(&app), Some(A));
    assert!(!app.on_home());
    // By key: the same landing through Enter on the focused "N ✓" ...
    framed(&mut app);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    let effect = stroke_press(&mut app, plain(Code::Enter));
    // A is only at `summary` after the round trip, so leaving sends
    // nothing: the landing is the focus it carries.
    assert_eq!(effect, Effect::None);
    assert!(app.on_home());
    framed(&mut app);
    framed(&mut app);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, C))))
    );
    // ... and → returns to the session left, asking for its commands.
    let out = sent(stroke_press(&mut app, plain(Code::Right)));
    assert_eq!(out, vec![asks_commands(&out[0]["id"], A)]);
    assert_eq!(on_screen(&app), Some(A));
    assert!(!app.on_home());
}

/// Focusing "N ✓" above a deep scroll shows its header row: the
/// settle reveals one row above the card's top for the header stop.
#[test]
fn focusing_done_above_a_deep_scroll_shows_its_header() {
    let mut app = tall_two_groups();
    let sessions = ten();
    let last = sessions.last().expect("ten sessions").clone();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    for _ in 0..11 {
        assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    }
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, &last))));
    for _ in 0..8 {
        assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    }
    let first_other = sessions.get(1).expect("a second session").clone();
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, &first_other))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, &first_other))));
    // The settle scrolled the header into view: the next frame draws it.
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let shown = crate::view::render(&app, area, &mut buf, None);
    assert!(
        shown
            .iter()
            .any(|target| target.id == TargetId::Rail(Spot::Done(key(&app, &first_other)))),
        "the settle reveals the focused done"
    );
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

/// The rail rect, while drawn.
fn rail_rect(app: &App) -> Option<ratatui::layout::Rect> {
    app.chrome().layout().and_then(|layout| layout.rail)
}

/// The panel rect, while drawn.
fn panel_rect(app: &App) -> Option<ratatui::layout::Rect> {
    app.chrome().layout().and_then(|layout| layout.panel)
}

/// Types `text` into the draft.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, Key::Char(ch));
    }
}

/// `A` and `B` live with `A` on screen, at `width` by 40.
fn sized(width: u16) -> App {
    let mut app = opened(&[A, B], A);
    app.set_size(width, 40);
    app
}

/// One exited `recent` row for `session` in the launch project.
fn exited_row(session: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": session,
        "how": "exited",
    })
}

/// Answers `recent` with exited rows for `sessions`, oldest first.
fn answer_recent(app: &mut App, recent: &str, sessions: &[&str]) {
    let result = json!({"sessions": sessions
        .iter()
        .map(|session| exited_row(session))
        .collect::<Vec<_>>()});
    let line = Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(recent.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    });
    assert!(app.on_line(line).is_empty());
}

/// Thirty live sessions in the launch project with `recent` waiting,
/// `live_027` on screen, drawn at 80x24: the app, the recent id, and the
/// row key of the session on screen.
fn thirty_live_on_screen() -> (App, String, u64) {
    let sessions: Vec<String> = (0..30).map(|i| format!("s_live_{i:03}")).collect();
    let refs: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut app = home();
    let (_, recent) = linked(&mut app);
    for session in &refs {
        app.on_line(live(session, json!({})));
    }
    app.set_size(80, 24);
    open(&mut app, refs[27]);
    let key = key(&app, refs[27]);
    (app, recent, key)
}

/// Renders home at 80x24; the targets of that frame.
fn render_home(app: &App, buf: &mut ratatui::buffer::Buffer) -> Vec<crate::mouse::Target> {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    crate::view::render(app, area, buf, None)
}

/// Whether `targets` draws `key` as a home list row.
fn draws_row(targets: &[crate::mouse::Target], key: u64) -> bool {
    targets
        .iter()
        .any(|target| target.id == TargetId::Home(HomeSpot::Entry(key)))
}

#[test]
fn focus_row_focuses_an_off_screen_live_row() {
    let (mut app, _, key) = thirty_live_on_screen();
    app.leave();
    assert!(app.on_home());
    app.focus_row(key);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut first_buf = ratatui::buffer::Buffer::empty(area);
    let first = render_home(&app, &mut first_buf);
    assert!(
        !draws_row(&first, key),
        "the row starts below the list window"
    );
    assert!(
        app.drawn(&first),
        "home_drawn runs, so the frame draws again"
    );
    let mut second_buf = ratatui::buffer::Buffer::empty(area);
    let second = render_home(&app, &mut second_buf);
    assert!(
        draws_row(&second, key),
        "the redraw scrolls the focused row into view"
    );
    assert!(!app.drawn(&second));
    assert_eq!(app.focused(), Some(TargetId::Home(HomeSpot::Entry(key))));
}

#[test]
fn resume_list_focuses_the_first_off_screen_exited_row() {
    let (mut app, recent, _) = thirty_live_on_screen();
    let exited = ["s_old_001", "s_old_002", "s_old_003"];
    answer_recent(&mut app, &recent, &exited);
    let first = app
        .home
        .as_ref()
        .and_then(|home| {
            home.sessions
                .row(&contract::SessionId(exited[0].to_owned()))
        })
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("the first exited row is listed"));
    app.resume_list();
    assert!(app.on_home());
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut first_buf = ratatui::buffer::Buffer::empty(area);
    let first_targets = render_home(&app, &mut first_buf);
    assert!(
        !draws_row(&first_targets, first),
        "the exited row starts below the list window"
    );
    assert!(app.drawn(&first_targets));
    let mut second_buf = ratatui::buffer::Buffer::empty(area);
    let second = render_home(&app, &mut second_buf);
    assert!(draws_row(&second, first));
    assert!(!app.drawn(&second));
    assert_eq!(app.focused(), Some(TargetId::Home(HomeSpot::Entry(first))));
}

#[test]
fn focus_row_expands_a_project_hiding_the_row_behind_more() {
    let sessions: Vec<String> = (0..5).map(|i| format!("s_old_{i:03}")).collect();
    let refs: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut app = home();
    app.set_size(80, 24);
    let (_, recent) = linked(&mut app);
    answer_recent(&mut app, &recent, &refs);
    let key = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.row(&contract::SessionId(refs[4].to_owned())))
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("the fifth exited row is listed"));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut before_buf = ratatui::buffer::Buffer::empty(area);
    let before = render_home(&app, &mut before_buf);
    assert!(
        !draws_row(&before, key),
        "the fifth exited row hides behind more"
    );
    app.focus_row(key);
    let hidden = app.home.as_ref().map(|home| {
        home.sessions
            .sections(home.launch_project())
            .into_iter()
            .find(|section| section.project == "-w")
            .map(|section| section.hidden())
    });
    assert_eq!(hidden, Some(Some(0)), "focus_row expands the project");
    let mut first_buf = ratatui::buffer::Buffer::empty(area);
    let first = render_home(&app, &mut first_buf);
    assert!(app.drawn(&first));
    let mut second_buf = ratatui::buffer::Buffer::empty(area);
    let second = render_home(&app, &mut second_buf);
    assert!(!app.drawn(&second));
    assert_eq!(app.focused(), Some(TargetId::Home(HomeSpot::Entry(key))));
    assert!(draws_row(&second, key));
}

#[test]
fn focus_row_with_an_unknown_key_focuses_nothing() {
    let mut app = with(&[A, B]);
    app.set_size(80, 24);
    assert!(app.on_home());
    app.focus_row(999_999);
    assert_eq!(app.focused(), None, "the box keeps focus");
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = render_home(&app, &mut buf);
    assert!(!app.drawn(&targets));
    assert_eq!(app.focused(), None);
}

#[test]
fn alt_r_hides_and_shows() {
    let mut app = sized(200);
    assert!(rail_rect(&app).is_some());
    assert_eq!(press(&mut app, Key::AltR), Effect::None);
    assert!(rail_rect(&app).is_none());
    // Hiding the rail is person-caused: it saves off.
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "false".to_owned())]
    );
    assert!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.grip)
            .is_some()
    );
    assert!(panel_rect(&app).is_some());
    assert_eq!(press(&mut app, Key::AltR), Effect::None);
    assert!(rail_rect(&app).is_some());
    // Showing it saves on.
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "true".to_owned())]
    );
    // The rail fits beside the panel, so the panel stays.
    assert!(panel_rect(&app).is_some());
}

#[test]
fn a_person_hidden_rail_stays_hidden_on_grow() {
    let mut app = sized(200);
    press(&mut app, Key::AltR);
    app.set_size(300, 40);
    assert!(rail_rect(&app).is_none());
}

#[test]
fn a_rail_hidden_for_width_returns_when_the_screen_grows() {
    let mut app = sized(130);
    assert!(rail_rect(&app).is_none());
    assert!(panel_rect(&app).is_some());
    app.set_size(136, 40);
    assert!(rail_rect(&app).is_some());
    assert!(panel_rect(&app).is_some());
    // Shedding for width is not person-caused: nothing saved.
    assert!(app.take_saves().is_empty());
}

#[test]
fn alt_r_on_a_rail_hidden_for_width_hides_the_panel_and_shows_the_rail() {
    let mut app = sized(130);
    assert_eq!(panel_rect(&app).map(|panel| panel.width), Some(30));
    press(&mut app, Key::AltR);
    assert_eq!(rail_rect(&app).map(|rail| rail.width), Some(22));
    assert!(panel_rect(&app).is_none());
    // The person asked to show: it saves on, even from a width shed.
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "true".to_owned())]
    );
}

#[test]
fn alt_r_where_even_the_rail_cannot_fit_leaves_the_panel() {
    let mut app = sized(100);
    assert!(panel_rect(&app).is_none());
    press(&mut app, Key::AltR);
    assert!(rail_rect(&app).is_none());
    assert!(panel_rect(&app).is_none());
    // The panel was never hidden: it shows once the screen has room.
    app.set_size(200, 40);
    assert!(panel_rect(&app).is_some());
}

#[test]
fn alt_p_after_that_brings_the_panel_back_and_the_rail_sheds() {
    let mut app = sized(130);
    press(&mut app, Key::AltR);
    assert!(panel_rect(&app).is_none());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "true".to_owned())]
    );
    press(&mut app, Key::AltP);
    assert!(panel_rect(&app).is_some());
    assert!(rail_rect(&app).is_none());
    // The shed for width saves nothing more.
    assert!(app.take_saves().is_empty());
}

#[test]
fn alt_r_on_home_does_nothing() {
    // ⌥R on home neither hides nor shows the rail of the next attach.
    let mut app = with(&[A, B]);
    assert_eq!(press(&mut app, Key::AltR), Effect::None);
    open(&mut app, A);
    assert!(rail_rect(&app).is_some());
    press(&mut app, Key::AltR);
    assert!(rail_rect(&app).is_none());
    for ch in "/home".chars() {
        press(&mut app, Key::Char(ch));
    }
    let lowering = sent(press(&mut app, Key::Enter));
    ack(&mut app, &lowering);
    assert!(app.home_screen().is_some());
    assert_eq!(press(&mut app, Key::AltR), Effect::None);
    open(&mut app, A);
    assert!(rail_rect(&app).is_none());
}

#[test]
fn n_waiting_shows_only_while_the_rail_is_not_drawn() {
    let mut app = sized(200);
    assert_eq!(app.rail_waiting(), None);
    press(&mut app, Key::AltR);
    assert_eq!(app.rail_waiting(), Some(0));
    let mut one = opened(&[A], A);
    one.set_size(200, 40);
    assert_eq!(one.rail_waiting(), None);
}

#[test]
fn n_waiting_counts_waiting_cards_but_not_the_attached_session() {
    let mut app = opened(&[A, B, C, D], A);
    app.on_line(live(A, waiting("r_a", 0)));
    app.on_line(live(B, waiting("r_b", 0)));
    app.on_line(live(D, waiting("r_d", 0)));
    app.on_line(left(D, "crashed"));
    press(&mut app, Key::AltR);
    assert_eq!(app.rail_waiting(), Some(1));
}

#[test]
fn a_click_on_n_waiting_shows_the_rail() {
    let mut app = sized(200);
    press(&mut app, Key::AltR);
    assert!(rail_rect(&app).is_none());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "false".to_owned())]
    );
    app.on_click(TargetId::Panel(crate::app::panel::Spot::Waiting));
    assert!(rail_rect(&app).is_some());
    // The click is person-caused: it saves on.
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "true".to_owned())]
    );
}

#[test]
fn slash_rail_hides_a_drawn_rail_and_saves_false() {
    let mut app = sized(200);
    assert!(rail_rect(&app).is_some());
    type_text(&mut app, "/rail");
    press(&mut app, Key::Enter);
    assert!(rail_rect(&app).is_none());
    assert!(app.input().is_empty());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "false".to_owned())]
    );
}

#[test]
fn slash_rail_shows_a_hidden_rail_and_saves_true() {
    let mut app = sized(200);
    press(&mut app, Key::AltR);
    assert!(rail_rect(&app).is_none());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "false".to_owned())]
    );
    type_text(&mut app, "/rail");
    press(&mut app, Key::Enter);
    assert!(rail_rect(&app).is_some());
    assert_eq!(
        app.take_saves(),
        vec![("tui.rail.shown", "true".to_owned())]
    );
}

#[test]
fn slash_rail_on_home_does_nothing() {
    let mut app = with(&[A, B]);
    type_text(&mut app, "/rail");
    press(&mut app, Key::Enter);
    assert!(app.take_saves().is_empty());
    open(&mut app, A);
    assert!(rail_rect(&app).is_some());
}

/// The rail's focused card key, if any.
fn rail_focus(app: &App) -> Option<Spot> {
    app.rail_state().focus
}

/// `stroke` pressed through the bindings.
fn stroke_press(app: &mut App, stroke: Stroke) -> Effect {
    app.on_press(stroke, fakes::clock::FakeClock::new().now())
}

fn plain(code: Code) -> Stroke {
    Stroke {
        code,
        mods: Mods::NONE,
    }
}

/// The home row key of `session`.
fn row_key(app: &App, session: &str) -> u64 {
    app.home
        .as_ref()
        .expect("home draws the rail")
        .sessions
        .row(&contract::SessionId(session.to_owned()))
        .expect("session has a row")
        .key
}

/// `A` and `B` live in the launch project, `C` live in another one, with
/// `A` on screen: feed order is `A`, `C`, `B`, but the rail draws the
/// launch project first.
fn two_projects() -> App {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(A, json!({})));
    app.on_line(live(C, json!({"project": "-other", "workspace": "/other"})));
    app.on_line(live(B, json!({})));
    open(&mut app, A);
    app
}

/// A session envelope of `kind` for `session` with a JSON `payload`.
fn turn(session: &str, kind: &str, payload: Value) -> Line {
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

#[test]
fn left_in_an_empty_box_focuses_the_on_screen_card() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
}

#[test]
fn down_moves_in_drawn_order_across_two_project_groups() {
    let mut app = two_projects();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    // Down stops at every stop, the next group's "+" and "N ✓"
    // included.
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
    // Down holds on the last stop.
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
}

#[test]
fn up_from_the_first_card_reaches_plus_and_down_on_the_last_holds() {
    let mut app = two_projects();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
}

#[test]
fn enter_on_another_card_switches_to_it() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    let effect = stroke_press(&mut app, plain(Code::Enter));
    assert!(matches!(effect, Effect::Send(_)), "{effect:?}");
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(B));
}

#[test]
fn enter_on_the_on_screen_card_sends_nothing() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Enter)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn esc_on_a_working_turn_clears_focus_without_an_interrupt() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    let input = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(turn(A, "turn_started", input));
    app.on_line(turn(
        A,
        "assistant_message_delta",
        json!({"text": "working"}),
    ));
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn with_focus_unset_arrows_enter_and_esc_reach_the_box() {
    let mut app = opened(&[A, B], A);
    let recall = stroke_press(&mut app, plain(Code::Up));
    assert!(matches!(recall, Effect::Send(_)), "{recall:?}");
    assert_eq!(rail_focus(&app), None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    type_text(&mut app, "ab");
    assert!(matches!(
        stroke_press(&mut app, plain(Code::Enter)),
        Effect::Send(_)
    ));
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn alt_digit_clears_focus_first() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("alt+1").unwrap()),
        Effect::None
    );
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn alt_a_clears_focus_with_no_waiting_request() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("alt+a").unwrap()),
        Effect::None
    );
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn alt_r_hiding_the_rail_clears_focus() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("alt+r").unwrap()),
        Effect::None
    );
    assert_eq!(rail_focus(&app), None);
    assert!(rail_rect(&app).is_none());
}

#[test]
fn slash_rail_hiding_clears_focus() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    type_text(&mut app, "/rail");
    press(&mut app, Key::Enter);
    assert_eq!(rail_focus(&app), None);
    assert!(rail_rect(&app).is_none());
}

#[test]
fn an_exit_of_the_focused_card_clears_it() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    app.on_line(left(B, "exited"));
    assert_eq!(rail_focus(&app), None);
    assert_eq!(on_screen(&app), Some(A));
}

#[test]
fn dismissing_the_focused_card_clears_it() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(B, "crashed"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    let out = sent(app.on_click(TargetId::Rail(Spot::Dismiss(key(&app, B)))));
    let id = out
        .iter()
        .find(|line| line["command"] == "dismiss")
        .expect("dismiss goes out")["id"]
        .as_str()
        .expect("dismiss has an id")
        .to_owned();
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    app.on_line(hub_answer("command_accepted", &id, ""));
    assert_eq!(rail_focus(&app), None);
}

/// `A` on screen but crashed, with `B` live: Tab walks the card, then
/// "+", "N ✓", then "×".
fn crashed_on_screen() -> App {
    let mut app = opened(&[A, B], A);
    app.on_line(left(A, "crashed"));
    app
}

#[test]
fn tab_walks_the_card_then_plus_then_cross() {
    use crate::app::rail::RAIL_HINT;
    let mut app = crashed_on_screen();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert!(
        frame_rows(&app, 200, 40).contains(&RAIL_HINT.to_owned()),
        "the hint follows the focused stop"
    );
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Dismiss(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    // Past the last stop Tab walks on from the rail.
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
}

#[test]
fn shift_tab_walks_the_walk_back() {
    let mut app = crashed_on_screen();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Dismiss(key(&app, A))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(app.focused(), None);
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(app.focused(), None);
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
    // From the walk's first stop Shift+Tab leaves the rail.
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
}

#[test]
fn shift_tab_past_a_held_end_moves_on() {
    let mut app = two_projects();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
    // Down holds on the last stop, marking it twice: one Shift+Tab
    // still walks past the hold.
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
}

#[test]
fn entering_the_rail_starts_the_walk_over() {
    let mut app = crashed_on_screen();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    app.set_rail_focus(Spot::Card(key(&app, B)));
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(
        rail_focus(&app),
        Some(Spot::Start(key(&app, A))),
        "the walk starts over, not past the old one"
    );
}

#[test]
fn enter_on_plus_goes_home_like_a_click() {
    fn hubbed() -> App {
        let mut app = opened(&[A, B], A);
        app.on_line(live(
            B,
            json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
        ));
        app
    }
    let mut clicked = hubbed();
    let click_out = sent(clicked.on_click(TargetId::Rail(Spot::Start(key(&clicked, B)))));
    let mut app = hubbed();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, B))));
    let out = sent(press(&mut app, Key::Enter));
    // The click and the key send the same commands.
    assert_eq!(out.len(), click_out.len());
    for (left, right) in out.iter().zip(click_out.iter()) {
        assert_eq!(left["command"], right["command"]);
        assert_eq!(left["args"], right["args"]);
    }
    assert_eq!(out, vec![subscribe(&out[0]["id"], A, "summary")]);
    assert_eq!(on_screen(&app), None);
    let chip = app
        .home_screen()
        .and_then(|screen| screen.chips.first().map(|chip| chip.text.clone()));
    assert_eq!(chip.as_deref(), Some("[hub]"));
    assert_eq!(app.focused(), None);
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn down_to_plus_enter_goes_home_like_a_click() {
    fn hubbed() -> App {
        let mut app = opened(&[A, B], A);
        app.on_line(live(
            B,
            json!({"workspace": "/Users/you/work/hub", "project": "-hub"}),
        ));
        app
    }
    let mut clicked = hubbed();
    let click_out = sent(clicked.on_click(TargetId::Rail(Spot::Start(key(&clicked, B)))));
    let mut app = hubbed();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    // Down stops at the next group's "+", the same stop Tab walks.
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, B))));
    let out = sent(press(&mut app, Key::Enter));
    assert_eq!(out.len(), click_out.len());
    for (left, right) in out.iter().zip(click_out.iter()) {
        assert_eq!(left["command"], right["command"]);
        assert_eq!(left["args"], right["args"]);
    }
    assert_eq!(on_screen(&app), None);
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn enter_on_cross_sends_dismiss_like_a_click_and_keeps_focus() {
    let mut app = crashed();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Dismiss(key(&app, B))));
    let out = sent(press(&mut app, Key::Enter));
    let mut clicked = crashed();
    let click_out = sent(clicked.on_click(TargetId::Rail(Spot::Dismiss(key(&clicked, B)))));
    assert_eq!(out.len(), click_out.len());
    for (left, right) in out.iter().zip(click_out.iter()) {
        assert_eq!(left["command"], right["command"]);
        assert_eq!(left["args"], right["args"]);
    }
    assert_eq!(
        out,
        vec![json!({"id": out[0]["id"], "command": "dismiss", "args": {"session": B}})]
    );
    // As with the click, the focus stays until the hub answers.
    assert_eq!(rail_focus(&app), Some(Spot::Dismiss(key(&app, B))));
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(hub_answer("command_accepted", &id, ""));
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn losing_a_plus_card_clears_its_plus_focus() {
    let mut app = two_projects();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, C))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, C))));
    app.on_line(left(C, "exited"));
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn a_rebound_navigate_leaves_shift_tab_walking_back() {
    let mut app = opened_in_conversation();
    app.on_line(left(A, "crashed"));
    app.set_keys(keyed_setup(&[("navigate", json!("f5"))]));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    // Without focus Shift+Tab is no navigate any more: the box keeps
    // it, instead of a rebound walk-back leaving the rail.
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    stroke_press(&mut app, shift_tab());
    assert_eq!(app.focused(), None);
    assert_eq!(rail_focus(&app), None);
}

/// Ten sessions, `A` on screen in the launch project and the rest in
/// another one: focusing the other group's "+" above a deep scroll
/// shows its header row.
fn tall_two_groups() -> App {
    let sessions = ten();
    let refs: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut app = opened(&refs, refs[0]);
    for session in refs.iter().skip(1) {
        app.on_line(live(
            session,
            json!({"workspace": "/other", "project": "-other"}),
        ));
    }
    app
}

#[test]
fn focusing_plus_above_a_deep_scroll_shows_its_header() {
    let mut app = tall_two_groups();
    let sessions = ten();
    let last = sessions.last().expect("ten sessions").clone();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    for _ in 0..11 {
        assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    }
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, &last))));
    for _ in 0..8 {
        assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    }
    let first_other = sessions.get(1).expect("a second session").clone();
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, &first_other))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, &first_other))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, &first_other))));
    // The settle scrolled the header into view: the next frame draws it.
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let shown = crate::view::render(&app, area, &mut buf, None);
    assert!(
        shown
            .iter()
            .any(|target| target.id == TargetId::Rail(Spot::Start(key(&app, &first_other)))),
        "the settle reveals the focused plus"
    );
}

#[test]
fn a_focused_plus_lifts_its_cell() {
    use crate::theme::hover_bg;
    let mut app = opened(&[A, B], A);
    let control = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    let targets = framed(&mut app);
    let plus = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Start(key(&app, A))))
        .map(|target| target.rect)
        .expect("the plus target");
    let at = usize::from(plus.y) * 200 + usize::from(plus.x);
    let bgs = frame_bgs(&app, 200, 40);
    let flats = frame_bgs(&control, 200, 40);
    assert_eq!(bgs[at], hover_bg(flats[at]), "the plus lifts");
    // The card's own tint stays flat: only the stop lifts.
    let card = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Card(key(&app, A))))
        .map(|target| target.rect)
        .expect("the card target");
    let tint = usize::from(card.y) * 200 + usize::from(card.x) + 2;
    assert_eq!(bgs[tint], flats[tint], "the card stays flat");
}

#[test]
fn a_focused_cross_lifts_its_cell() {
    use crate::theme::hover_bg;
    let mut app = opened(&[A, B], A);
    app.on_line(left(B, "crashed"));
    let mut control = opened(&[A, B], A);
    control.on_line(left(B, "crashed"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Dismiss(key(&app, B))));
    let targets = framed(&mut app);
    let cross = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Dismiss(key(&app, B))))
        .map(|target| target.rect)
        .expect("the cross target");
    let at = usize::from(cross.y) * 200 + usize::from(cross.x);
    let bgs = frame_bgs(&app, 200, 40);
    let flats = frame_bgs(&control, 200, 40);
    assert_eq!(bgs[at], hover_bg(flats[at]), "the cross lifts");
    let card = targets
        .iter()
        .find(|target| target.id == TargetId::Rail(Spot::Card(key(&app, A))))
        .map(|target| target.rect)
        .expect("the card target");
    let tint = usize::from(card.y) * 200 + usize::from(card.x) + 2;
    assert_eq!(bgs[tint], flats[tint], "the card stays flat");
}

#[test]
fn a_resize_that_sheds_the_rail_clears_it() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    app.set_size(60, 24);
    app.on_line(live(B, json!({})));
    assert!(rail_rect(&app).is_none());
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn leaving_clears_the_rail() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    app.leave();
    app.on_line(live(B, json!({})));
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn a_second_left_widens_home_on_the_on_screen_row_after_moving() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    let widened = stroke_press(&mut app, plain(Code::Left));
    assert!(matches!(widened, Effect::Send(_)), "{widened:?}");
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, A))))
    );
}

#[test]
fn a_second_left_widens_home_on_a_row_keyed_past_one() {
    let mut app = opened(&[A, B, C], C);
    assert!(row_key(&app, C) > 1, "C's row key: {}", row_key(&app, C));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, C))));
    let widened = stroke_press(&mut app, plain(Code::Left));
    assert!(matches!(widened, Effect::Send(_)), "{widened:?}");
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, C))))
    );
}

#[test]
fn a_focused_text_left_moves_the_cursor() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    type_text(&mut app, "ab");
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 3);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert!(!app.on_home());
}

#[test]
fn right_gives_focus_back_leaving_the_cursor_alone() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    type_text(&mut app, "ab");
    assert_eq!(stroke_press(&mut app, plain(Code::Right)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 4);
    assert!(!app.on_home());
}

#[test]
fn a_hidden_rail_left_goes_home_with_default_focus() {
    let mut app = opened(&[A, B], A);
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("alt+r").unwrap()),
        Effect::None
    );
    assert!(rail_rect(&app).is_none());
    let homeward = stroke_press(&mut app, plain(Code::Left));
    assert!(matches!(homeward, Effect::Send(_)), "{homeward:?}");
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
}

#[test]
fn an_exited_on_screen_session_widens_home_on_its_row() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(A, "exited"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, A))))
    );
}

#[test]
fn an_exited_on_screen_session_behind_more_widens_home_on_its_row() {
    let mut app = opened(&[A, B, C, D], A);
    app.on_line(left(B, "exited"));
    app.on_line(left(C, "exited"));
    app.on_line(left(D, "exited"));
    app.on_line(left(A, "exited"));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert!(app.on_home());
    assert_eq!(rail_focus(&app), None);
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(HomeSpot::Entry(row_key(&app, A))))
    );
}

#[test]
fn esc_closes_the_completion_panel_before_giving_focus_back() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert!(app.completions().is_none());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn esc_goes_to_an_open_approval_panel_first() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    app.on_line(permission(A, "r_esc"));
    assert!(app.panel().is_some());
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert!(app.panel().is_none());
}

#[test]
fn esc_goes_to_an_open_search_bar_first() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("ctrl+f").unwrap()),
        Effect::None
    );
    assert!(app.find_open());
    assert_eq!(stroke_press(&mut app, plain(Code::Esc)), Effect::None);
    assert!(!app.find_open());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

#[test]
fn unset_arrows_still_move_a_focused_rail_with_rebound_recall() {
    let mut app = opened(&[A, B], A);
    app.set_keys(crate::KeysSetup {
        user: [("recall_prompt".to_owned(), json!("ctrl+t"))]
            .into_iter()
            .collect(),
    });
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
}

/// Eight live sessions with `A` on screen, its card scrolled out of view
/// and focus back in the input box: entry, seven downs and Right.
fn eight_scrolled_past_on_screen() -> App {
    let mut app = opened(&[A, B, C, D, "s_e1", "s_e2", "s_e3", "s_e4"], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    for _ in 0..7 {
        assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    }
    assert!(app.rail_state().scroll() > 0);
    assert_eq!(stroke_press(&mut app, plain(Code::Right)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    app
}

#[test]
fn a_focus_rail_key_rebound_to_up_moves_a_focused_rail() {
    let mut app = opened(&[A, B], B);
    // `up` alone would clash with the default recall and revert: move the
    // recall aside so `focus_rail` holds it.
    app.set_keys(keyed_setup(&[
        ("recall_prompt", json!("ctrl+t")),
        ("focus_rail", json!("up")),
    ]));
    // Entry still reaches the rail from the input box, not the recall.
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert!(!app.on_home());
    // A focused rail takes Up as a rail move, not a widen home.
    assert_eq!(stroke_press(&mut app, plain(Code::Up)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert!(!app.on_home());
}

#[test]
fn tab_lands_on_the_on_screen_card_scrolled_out_of_view() {
    let mut app = eight_scrolled_past_on_screen();
    let targets = framed(&mut app);
    assert!(
        !targets
            .iter()
            .any(|target| target.id == TargetId::Rail(Spot::Card(key(&app, A)))),
        "the on-screen card scrolled out of view"
    );
    let panel = targets
        .iter()
        .find(|target| target.id == TargetId::Notice(5))
        .expect("a panel stop")
        .id;
    app.focus = Some(panel);
    app.drawn(&targets);
    assert_eq!(stroke_press(&mut app, plain(Code::Tab)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
    // The Tab's settle reveals the card: the next frame draws it.
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let shown = crate::view::render(&app, area, &mut buf, None);
    assert!(
        shown
            .iter()
            .any(|target| target.id == TargetId::Rail(Spot::Card(key(&app, A)))),
        "the settle reveals the on-screen card"
    );
}

#[test]
fn left_entry_reveals_a_scrolled_off_on_screen_card_in_the_same_frame() {
    let mut app = eight_scrolled_past_on_screen();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    // No further key or hub line: the entry's settle already revealed it.
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let shown = crate::view::render(&app, area, &mut buf, None);
    assert!(
        shown
            .iter()
            .any(|target| target.id == TargetId::Rail(Spot::Card(key(&app, A)))),
        "the lifted card is drawn in the same frame"
    );
}

fn keyed_setup(entries: &[(&str, Value)]) -> crate::KeysSetup {
    crate::KeysSetup {
        user: entries
            .iter()
            .map(|(id, value)| ((*id).to_owned(), value.clone()))
            .collect(),
    }
}

#[test]
fn an_unbound_focus_rail_key_never_focuses() {
    let mut app = opened(&[A, B], A);
    app.set_keys(keyed_setup(&[("focus_rail", json!([]))]));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 2);
    type_text(&mut app, "ab");
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 3);
    assert_eq!(rail_focus(&app), None);
    app.open_keymap();
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    assert!(crate::view::text(&buf).contains("unbound"));
}

#[test]
fn a_rebound_focus_rail_key_focuses_an_empty_box() {
    let mut app = opened(&[A, B], A);
    app.set_keys(keyed_setup(&[("focus_rail", json!("alt+l"))]));
    let alt_l = Stroke::parse("alt+l").unwrap();
    assert_eq!(stroke_press(&mut app, alt_l), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
}

#[test]
fn a_rebound_left_is_still_the_cursor_with_focus_unset() {
    let mut app = opened(&[A, B], A);
    app.set_keys(keyed_setup(&[("focus_rail", json!("alt+l"))]));
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 2);
}

#[test]
fn a_rebound_focus_rail_key_leaves_text_alone() {
    let mut app = opened(&[A, B], A);
    app.set_keys(keyed_setup(&[("focus_rail", json!("alt+l"))]));
    type_text(&mut app, "ab");
    let alt_l = Stroke::parse("alt+l").unwrap();
    assert_eq!(stroke_press(&mut app, alt_l), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 3);
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn a_default_keyset_leaves_word_left_alone() {
    let mut app = opened(&[A, B], A);
    type_text(&mut app, "ab cd");
    let word_left = Stroke::parse("alt+left").unwrap();
    assert_eq!(stroke_press(&mut app, word_left), Effect::None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 5);
    assert_eq!(rail_focus(&app), None);
}

/// The `sessions` command id waiting after a dropped and re-helloed link.
fn reconnected_sessions(app: &mut App) -> String {
    app.disconnected();
    let out: Vec<Value> = app
        .on_line(hello())
        .into_iter()
        .map(|line| serde_json::from_str(&line).expect("hub line parses"))
        .collect();
    out.iter()
        .find(|line| line["command"] == "sessions")
        .expect("reconnect sends sessions")["id"]
        .as_str()
        .expect("sessions has an id")
        .to_owned()
}

/// The `sessions` answer listing `running` live and none exited.
fn sessions_result(app: &mut App, id: &str, running: &[&str]) {
    let live: Vec<Value> = running
        .iter()
        .map(|session| json!({"session_id": session}))
        .collect();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "result": {"live": live, "exited": []}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
}

#[test]
fn a_dropped_link_keeps_a_focused_card() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    app.disconnected();
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert!(app.panel().is_none());
}

#[test]
fn enter_on_a_focused_card_with_the_link_down_sends_nothing() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    app.disconnected();
    assert_eq!(stroke_press(&mut app, plain(Code::Enter)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

#[test]
fn down_still_moves_a_focused_rail_with_the_link_down() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    app.disconnected();
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
}

#[test]
fn a_reconnect_dropping_the_focused_card_clears_it() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    let id = reconnected_sessions(&mut app);
    sessions_result(&mut app, &id, &[A]);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_reconnect_listing_the_focused_card_keeps_it() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    let id = reconnected_sessions(&mut app);
    sessions_result(&mut app, &id, &[A, B]);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

/// `A` and `B` live with `A` on screen, after one finished turn, so the
/// conversation has a stop to navigate to.
fn opened_in_conversation() -> App {
    let mut app = opened(&[A, B], A);
    let input = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(turn(A, "turn_started", input));
    app.on_line(turn(A, "text_completed", json!({"text": "done"})));
    app.on_line(turn(A, "turn_completed", json!({"outcome": "completed"})));
    app
}

/// The real targets plus a notice standing in for the panel's cards, with
/// the regions the layout drew.
fn framed(app: &mut App) -> Vec<Target> {
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let mut targets = crate::view::render(app, area, &mut buf, None);
    let regions = app.chrome().regions();
    if let Some(panel) = regions.panel {
        targets.push(Target {
            id: TargetId::Notice(5),
            rect: ratatui::layout::Rect::new(panel.x, panel.y + 2, 20, 3),
        });
    }
    app.set_regions(regions);
    app.drawn(&targets);
    targets
}

/// Rail focus and `App` focus are never both on the rail.
fn assert_single_focus(app: &App) {
    let rail = rail_focus(app);
    let rail_target = app
        .focused()
        .is_some_and(|id| matches!(id, TargetId::Rail(_)));
    assert!(!(rail.is_some() && rail_target), "both focuses set");
}

fn shift_tab() -> Stroke {
    Stroke::parse("shift+tab").unwrap()
}

#[test]
fn tab_walks_the_conversation_the_panel_and_the_rail() {
    let mut app = opened_in_conversation();
    framed(&mut app);
    stroke_press(&mut app, shift_tab());
    assert!(matches!(app.focused(), Some(TargetId::Turn(_))));
    assert_single_focus(&app);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(app.focused(), Some(TargetId::Notice(5)));
    assert_single_focus(&app);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
    // Tab walks the rail's stops in drawn order from the on-screen
    // card: the "+" and "N ✓" above it, then the next card.
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), Some(Spot::Start(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), Some(Spot::Done(key(&app, A))));
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
    // Past the last stop Tab walks on from the rail.
    stroke_press(&mut app, plain(Code::Tab));
    assert!(matches!(app.focused(), Some(TargetId::Turn(_))));
    assert_eq!(rail_focus(&app), None);
    assert_single_focus(&app);
}

#[test]
fn typing_on_a_focused_rail_types_into_the_box() {
    let mut app = opened_in_conversation();
    framed(&mut app);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    let x = Stroke {
        code: Code::Char('x'),
        mods: Mods::NONE,
    };
    assert_eq!(stroke_press(&mut app, x), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_single_focus(&app);
    assert_eq!(app.input().cursor(app.draft_width()).1, 3);
}

#[test]
fn shift_tab_from_the_rail_returns_to_the_conversation() {
    let mut app = opened_in_conversation();
    framed(&mut app);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    stroke_press(&mut app, shift_tab());
    assert!(matches!(app.focused(), Some(TargetId::Turn(_))));
    assert_eq!(rail_focus(&app), None);
    assert_single_focus(&app);
}

#[test]
fn shift_tab_with_no_stop_clears_the_rail_leaving_no_focus() {
    let mut app = opened(&[A, B], A);
    framed(&mut app);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    stroke_press(&mut app, shift_tab());
    assert_eq!(app.focused(), None);
    assert_eq!(rail_focus(&app), None);
    assert_single_focus(&app);
}

#[test]
fn tab_with_the_on_screen_session_exited_lands_on_the_first_card() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(A, "exited"));
    let targets = framed(&mut app);
    let panel_stop = targets
        .iter()
        .find(|target| target.id == TargetId::Notice(5))
        .expect("a panel stop")
        .id;
    app.focus = Some(panel_stop);
    app.drawn(&targets);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
}

#[test]
fn tab_with_no_card_stop_skips_the_rail() {
    let mut app = opened(&[A, B], A);
    app.on_line(left(A, "exited"));
    app.on_line(left(B, "exited"));
    let targets = framed(&mut app);
    let panel_stop = targets
        .iter()
        .find(|target| target.id == TargetId::Notice(5))
        .expect("a panel stop")
        .id;
    app.focus = Some(panel_stop);
    app.drawn(&targets);
    stroke_press(&mut app, plain(Code::Tab));
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), Some(panel_stop));
    assert_single_focus(&app);
}

#[test]
fn focusing_a_card_past_the_rail_height_scrolls_it_into_view() {
    let mut app = opened(&[A, B, C, D], A);
    for extra in ["s_e1", "s_e2", "s_e3", "s_e4"] {
        app.on_line(live(extra, json!({})));
    }
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    for _ in 0..7 {
        assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    }
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, "s_e4"))));
    app.on_line(live(A, json!({})));
    assert!(app.rail_state().scroll() > 0);
}

#[test]
fn clicking_a_conversation_target_clears_a_focused_rail() {
    let mut app = opened_in_conversation();
    let targets = framed(&mut app);
    let turn = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Turn(_)))
        .expect("a conversation stop")
        .id;
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    assert_eq!(app.on_click(turn), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.focused(), None);
    assert_single_focus(&app);
}

/// The full frame's background colours on a `width` by `height` screen.
fn frame_bgs(app: &App, width: u16, height: u16) -> Vec<ratatui::style::Color> {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf.content.iter().map(|cell| cell.bg).collect()
}

/// The full frame's trimmed text rows on a `width` by `height` screen.
fn frame_rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    // The hint row sits in the conversation's column, indented past
    // the rail: the check reads its content exactly.
    crate::view::text(&buf)
        .lines()
        .map(|line| line.trim().to_owned())
        .collect()
}

#[test]
fn the_foot_row_reads_the_rail_hint_while_focused() {
    use crate::app::rail::RAIL_HINT;
    let mut app = opened(&[A, B], A);
    assert!(
        !frame_rows(&app, 200, 40).contains(&RAIL_HINT.to_owned()),
        "no hint without focus"
    );
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert!(rail_focus(&app).is_some());
    assert!(
        frame_rows(&app, 200, 40).contains(&RAIL_HINT.to_owned()),
        "the hint row"
    );
    assert_eq!(stroke_press(&mut app, plain(Code::Right)), Effect::None);
    assert!(
        !frame_rows(&app, 200, 40).contains(&RAIL_HINT.to_owned()),
        "no hint after"
    );
}

#[test]
fn the_armed_quit_hint_wins_over_the_rail_hint() {
    use crate::app::rail::RAIL_HINT;
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(press(&mut app, Key::CtrlC), Effect::None);
    assert!(app.hint());
    let rows = frame_rows(&app, 200, 40);
    assert!(
        rows.contains(&crate::app::QUIT_HINT.to_owned()),
        "the quit hint"
    );
    assert!(!rows.contains(&RAIL_HINT.to_owned()), "no rail hint");
}

#[test]
fn below_rows_grows_by_one_while_the_rail_is_focused() {
    let mut app = opened(&[A, B], A);
    let room = app.below_rows();
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(app.below_rows(), room.saturating_add(1));
    assert_eq!(stroke_press(&mut app, plain(Code::Right)), Effect::None);
    assert_eq!(app.below_rows(), room);
}

#[test]
fn a_shed_rail_frame_shows_no_lift_and_no_hint() {
    use crate::app::rail::RAIL_HINT;
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    app.set_size(60, 24);
    app.on_line(live(B, json!({})));
    assert!(rail_rect(&app).is_none());
    let mut control = opened(&[A, B], A);
    control.set_size(60, 24);
    control.on_line(live(B, json!({})));
    // The shed frame matches one never focused: no lifted card, and
    // the hint row is gone with the rail.
    assert_eq!(frame_bgs(&app, 60, 24), frame_bgs(&control, 60, 24));
    assert!(
        !frame_rows(&app, 60, 24).contains(&RAIL_HINT.to_owned()),
        "no hint without the rail"
    );
}

/// A `notice` the overlay can show, for `session`.
fn notice(session: &str, message: &str) -> Line {
    turn(
        session,
        "notice",
        json!({"code": "extension_failed", "message": message}),
    )
}

#[test]
fn a_plain_left_with_the_find_bar_open_leaves_rail_focus_unset() {
    let mut app = opened(&[A, B], A);
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("ctrl+f").unwrap()),
        Effect::None
    );
    assert!(app.find_open());
    assert_eq!(app.key_context(), Context::Search);
    type_text(&mut app, "ab");
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert!(app.find_open());
}

#[test]
fn a_plain_left_with_an_item_open_leaves_rail_focus_unset() {
    let mut app = opened(&[A, B], A);
    app.on_line(turn(
        A,
        "job_started",
        json!({"job_id": "j_1", "description": "build", "output_path": "/tmp/out"}),
    ));
    let _ = app.open_item(&contract::JobId("j_1".to_owned()));
    assert!(app.item_open());
    assert_eq!(app.key_context(), Context::Input);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn a_plain_left_with_the_notice_overlay_open_leaves_rail_focus_unset() {
    let mut app = opened(&[A, B], A);
    app.on_line(notice(A, "The fiber is quiet."));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), None);
    assert!(app.notice_overlay().is_some());
}

#[test]
fn with_the_notice_overlay_open_up_leaves_a_focused_card_alone() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    app.on_line(notice(A, "The fiber is quiet."));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    let _ = stroke_press(&mut app, plain(Code::Up));
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert!(app.notice_overlay().is_some());
}

#[test]
fn a_rebound_plain_focus_rail_key_in_a_non_empty_box_leaves_the_text_alone() {
    let mut app = opened(&[A, B], A);
    app.set_keys(keyed_setup(&[("focus_rail", json!("f5"))]));
    type_text(&mut app, "ab");
    assert_eq!(app.input().cursor(app.draft_width()).1, 4);
    assert_eq!(
        stroke_press(&mut app, Stroke::parse("f5").unwrap()),
        Effect::None
    );
    assert_eq!(rail_focus(&app), None);
    assert_eq!(app.input().cursor(app.draft_width()).1, 4);
}

#[test]
fn a_plain_up_with_an_item_open_leaves_a_focused_card_alone() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Down)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    app.on_line(turn(
        A,
        "job_started",
        json!({"job_id": "j_1", "description": "build", "output_path": "/tmp/out"}),
    ));
    let _ = app.open_item(&contract::JobId("j_1".to_owned()));
    assert!(app.item_open());
    let _ = stroke_press(&mut app, plain(Code::Up));
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, B))));
    assert!(app.item_open());
}

#[test]
fn down_moves_a_focused_rail_with_nothing_on_top() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    let before = rail_focus(&app);
    assert!(before.is_some(), "the rail has focus");
    assert_eq!(press(&mut app, Key::Down), Effect::None);
    assert_ne!(rail_focus(&app), before, "the focus moved to the next stop");
}

#[test]
fn down_leaves_a_focused_rail_alone_with_the_approval_panel_open() {
    let mut app = opened(&[A, B], A);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    app.on_line(permission(A, "r_1"));
    assert_eq!(app.top_surface(), crate::app::Surface::Panel);
    let before = rail_focus(&app);
    assert!(before.is_some(), "the rail keeps its focus");
    press(&mut app, Key::Down);
    assert_eq!(rail_focus(&app), before);
}

/// Rebinds `delete_session` to `keys` on `app`.
fn rebind_delete_session(app: &mut App, keys: Value) {
    let mut user = serde_json::Map::new();
    user.insert("delete_session".to_owned(), keys);
    app.set_keys(crate::KeysSetup { user });
}

/// `A` and `B` live with `A` on screen, the rail focused on `A`'s card.
fn rail_on_a() -> App {
    let mut app = opened_in_conversation();
    framed(&mut app);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
    app
}

/// The frame at the rail tests' size.
fn shown(app: &App) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 200, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn delete_on_a_focused_live_card_asks_to_stop_it_over_the_conversation() {
    let mut app = rail_on_a();
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    assert!(app.quit_open());
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, false))
    );
    // The same frame draws the question: nothing else was pressed.
    let frame = shown(&app);
    assert!(frame.contains("Stop fix the parser?"), "{frame}");
    assert!(frame.contains("click a choice"), "{frame}");
    assert_eq!(app.session().map(|id| id.0.as_str()), Some(A));
}

#[test]
fn delete_on_a_focused_card_asks_with_a_draft_and_leaves_it_alone() {
    let mut app = rail_on_a();
    for ch in "hi".chars() {
        assert_eq!(stroke_press(&mut app, plain(Code::Char(ch))), Effect::None);
    }
    assert_eq!(app.draft(), "hi");
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    assert!(app.stop_question().is_some());
    assert_eq!(app.draft(), "hi");
    // Esc cancels, keeping the draft and the rail's focus.
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.quit_open());
    assert_eq!(app.draft(), "hi");
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

#[test]
fn enter_on_the_on_screen_card_stops_it_and_goes_home() {
    let mut app = rail_on_a();
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    let out = sent(press(&mut app, Key::Enter));
    let closes: Vec<&Value> = out
        .iter()
        .filter(|line| line["command"] == "close")
        .collect();
    assert_eq!(closes.len(), 1, "{out:?}");
    assert_eq!(closes[0]["session_id"], A);
    assert_eq!(closes[0]["args"]["now"], json!(true));
    assert!(app.session().is_none(), "home");
    assert!(!app.quit_open());
    assert_eq!(rail_focus(&app), None);
}

#[test]
fn enter_on_another_card_stops_it_and_stays_on_the_screen() {
    let mut app = rail_on_a();
    app.set_rail_focus(Spot::Card(key(&app, B)));
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    let out = sent(press(&mut app, Key::Enter));
    let closes: Vec<&Value> = out
        .iter()
        .filter(|line| line["command"] == "close")
        .collect();
    assert_eq!(closes.len(), 1, "{out:?}");
    assert_eq!(closes[0]["session_id"], B);
    assert_eq!(closes[0]["args"]["now"], json!(true));
    assert_eq!(on_screen(&app), Some(A));
    assert_eq!(rail_focus(&app), None);
    assert!(!app.quit_open());
}

#[test]
fn delete_on_a_crashed_card_a_plus_or_a_done_count_opens_nothing() {
    let mut app = rail_on_a();
    app.on_line(left(B, "crashed"));
    framed(&mut app);
    let b = key(&app, B);
    for spot in [
        Spot::Card(b),
        Spot::Dismiss(b),
        Spot::Start(key(&app, A)),
        Spot::Done(key(&app, A)),
    ] {
        app.set_rail_focus(spot);
        assert_eq!(stroke_press(&mut app, plain(Code::Char('h'))), Effect::None);
        assert_eq!(stroke_press(&mut app, plain(Code::Backspace)), Effect::None);
        let draft = app.draft().to_owned();
        assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
        assert!(!app.quit_open(), "{spot:?}");
        assert_eq!(app.draft(), draft, "{spot:?}");
        assert_eq!(rail_focus(&app), Some(spot));
    }
}

#[test]
fn backspace_on_a_focused_rail_edits_the_draft_and_stops_nothing() {
    let mut app = rail_on_a();
    for ch in "ab".chars() {
        assert_eq!(stroke_press(&mut app, plain(Code::Char(ch))), Effect::None);
    }
    assert_eq!(stroke_press(&mut app, plain(Code::Backspace)), Effect::None);
    assert_eq!(app.draft(), "a");
    assert!(!app.quit_open());
    assert_eq!(rail_focus(&app), Some(Spot::Card(key(&app, A))));
}

#[test]
fn delete_with_the_box_focused_edits_the_draft_and_stops_nothing() {
    let mut app = opened_in_conversation();
    framed(&mut app);
    for ch in "abc".chars() {
        assert_eq!(stroke_press(&mut app, plain(Code::Char(ch))), Effect::None);
    }
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Left)), Effect::None);
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    assert_eq!(app.draft(), "ac");
    assert!(!app.quit_open());
}

#[test]
fn every_rebound_delete_session_key_stops_the_card_and_the_old_one_does_not() {
    for bound in ["f7", "f8"] {
        let mut app = rail_on_a();
        rebind_delete_session(&mut app, json!(["f7", "f8"]));
        let stroke = Stroke::parse(bound).unwrap_or_else(|err| panic!("{bound}: {err:?}"));
        assert_eq!(stroke_press(&mut app, stroke), Effect::None);
        assert!(app.quit_open(), "{bound} stops");
        assert_eq!(press(&mut app, Key::Esc), Effect::None);
        assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
        assert!(!app.quit_open(), "physical Delete is no longer bound");
    }
}

#[test]
fn a_rebound_backspace_never_stops_the_card() {
    let mut app = rail_on_a();
    rebind_delete_session(&mut app, json!(["backspace"]));
    for ch in "ab".chars() {
        assert_eq!(stroke_press(&mut app, plain(Code::Char(ch))), Effect::None);
    }
    assert_eq!(stroke_press(&mut app, plain(Code::Backspace)), Effect::None);
    assert_eq!(app.draft(), "a");
    assert!(!app.quit_open());
}

#[test]
fn a_rebound_character_types_on_the_rail_and_stops_on_a_home_row() {
    let mut app = rail_on_a();
    rebind_delete_session(&mut app, json!(["x"]));
    assert_eq!(stroke_press(&mut app, plain(Code::Char('x'))), Effect::None);
    assert_eq!(app.draft(), "x");
    assert!(!app.quit_open());
    // On home's list the same key stops the row.
    let mut app = with(&[A]);
    rebind_delete_session(&mut app, json!(["x"]));
    let row = row_key(&app, A);
    app.focus = Some(TargetId::Home(HomeSpot::Entry(row)));
    assert_eq!(stroke_press(&mut app, plain(Code::Char('x'))), Effect::None);
    assert!(app.quit_open());
}

#[test]
fn unbinding_delete_session_stops_nothing_on_the_rail() {
    let mut app = rail_on_a();
    rebind_delete_session(&mut app, json!([]));
    assert_eq!(stroke_press(&mut app, plain(Code::Delete)), Effect::None);
    assert!(!app.quit_open());
}

#[test]
fn the_rail_hint_names_delete() {
    use crate::app::rail::RAIL_HINT;
    let app = rail_on_a();
    assert!(RAIL_HINT.contains("Delete stop"), "{RAIL_HINT}");
    assert!(shown(&app).contains(RAIL_HINT));
}

#[test]
fn a_rebound_shifted_letter_or_space_types_on_the_rail() {
    for (bound, typed) in [("X", "X"), ("space", " ")] {
        let mut app = rail_on_a();
        rebind_delete_session(&mut app, json!([bound]));
        let stroke = Stroke::parse(bound).unwrap_or_else(|err| panic!("{bound}: {err:?}"));
        assert_eq!(stroke_press(&mut app, stroke), Effect::None);
        assert!(!app.quit_open(), "{bound} types");
        assert_eq!(app.draft(), typed, "{bound}");
    }
}
