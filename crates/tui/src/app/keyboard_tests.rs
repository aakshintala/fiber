//! Tests for the keyboard: the context on screen, actions, and the
//! person's bindings reaching every handler.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use contract::clock::Clock;

use super::super::commands::commands_testkit::{
    attached as hub_attached, connected, enter, key, turn_starts, type_any,
};
use super::super::{App, Effect, Surface};
use crate::home::{Launch, Spot};
use crate::keys::{Edit, Key};
use crate::keyset::Context;
use crate::link::Line;
use crate::stroke::{Code, Mods, Stroke};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// The launch description: `/w`, outside git, default shares.
fn launch() -> Launch {
    Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    }
}

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.set_size(80, 24);
    app
}

/// An app attached to [`SESSION`] at `width` by `height`, with no home.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// An app with home state, attached, at `width` by `height`.
fn homed(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
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

/// One session envelope.
fn envelope(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app with one turn of `count` read groups, closed.
fn groups(count: usize, width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    let input = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    for i in 1..=count {
        app.on_line(envelope(
            "tool_call_requested",
            json!({"name": "read", "arguments": {"path": format!("src/{i}.rs")}}),
            Some(format!("a_{i}").as_str()),
        ));
        app.on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "ok"}]}),
            Some(format!("a_{i}").as_str()),
        ));
        app.on_line(envelope(
            "assistant_message_delta",
            json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
        app.on_line(envelope(
            "text_completed",
            json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
    }
    app.on_line(envelope(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    app
}

/// Renders `app` and takes the frame's click targets.
fn frame(app: &mut App) {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

/// The screen as text at the app's size.
fn screen(app: &App) -> String {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Presses the stroke `name` names.
fn press(app: &mut App, name: &str) -> Effect {
    let stroke = Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"));
    app.on_press(stroke, now())
}

/// Loads `entries` as the app's `keys`.
fn keyed(app: &mut App, entries: &[(&str, Value)]) {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    app.set_keys(crate::KeysSetup { user });
}

/// Types `text` into the app, one character per key.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// A live `session_status` for `session`, streaming.
fn live(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model",
            "delegates": 0,
            "jobs": 0,
            "clients": 0,
            "state": "streaming",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// An approval request, which opens the approval panel.
fn approval() -> Line {
    envelope(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "npm"}}),
        Some("a_1"),
    )
}

/// An offer of one MCP server, which opens the repository offer's view.
fn offer() -> Line {
    envelope(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [
            {"kind": "mcp_server", "name": "a", "hash": "h",
             "required": false, "summary": "MCP server: a"},
        ]}),
        None,
    )
}

/// The steering queue with one row.
fn steering_queue() -> Line {
    envelope(
        "steering_queue",
        json!({"messages": [{
            "content": [{"type": "text", "text": "steer me"}],
            "source": "driver",
            "command_id": "c_1",
        }]}),
        None,
    )
}

/// Links a home app: the feed and recent ids.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
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

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// One exited `recent` row.
fn exited(name: &str) -> Value {
    json!({
        "session_id": SESSION,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "exited",
    })
}

/// Home with one exited row, its delete question open.
fn delete_question() -> App {
    let mut app = home();
    let (_, recent) = linked(&mut app);
    app.on_line(accepted(&recent, json!({"sessions": [exited("old work")]})));
    let key = app
        .home_screen()
        .and_then(|screen| {
            crate::home::home_test_rows(&screen)
                .first()
                .map(|row| row.0)
        })
        .expect("one row");
    assert_eq!(app.home_click(Spot::Stop(key)), Effect::None);
    assert_eq!(app.top_surface(), Surface::DeleteQuestion);
    app
}

#[test]
fn the_quit_question_is_an_overlay_on_its_own() {
    let mut app = homed(80, 24);
    app.on_line(live("s_bbbbbbbbbbbbbbbb"));
    app.quit();
    assert!(app.quit_open());
    assert!(app.keymap_top().is_none());
    assert!(app.panel().is_none());
    assert!(!app.offer_open());
    assert!(app.search_panel().is_none());
    assert_eq!(app.top_surface(), Surface::Quit);
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_delete_question_is_an_overlay() {
    let app = delete_question();
    assert_eq!(app.top_surface(), Surface::DeleteQuestion);
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_workspace_picker_is_an_overlay() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert_eq!(app.top_surface(), Surface::WorkspacePicker);
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn nothing_open_leaves_the_draft_on_top_of_home_and_a_session() {
    assert_eq!(home().top_surface(), Surface::Draft);
    assert_eq!(attached(80, 24).top_surface(), Surface::Draft);
}

#[test]
fn the_key_map_is_an_overlay() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_approval_panel_is_an_overlay() {
    let mut app = homed(80, 24);
    app.on_line(approval());
    assert!(app.panel().is_some());
    assert!(!app.offer_open());
    assert!(app.find_bar().is_none());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_offer_is_an_overlay() {
    let mut app = homed(80, 24);
    app.on_line(offer());
    assert!(app.offer_open());
    assert!(app.panel().is_none());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_ctrl_r_panel_is_an_overlay_with_the_find_bar_open() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.search_panel().is_some());
    assert!(app.find_bar().is_some());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_slash_panel_is_an_overlay() {
    let mut app = homed(80, 24);
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    assert!(app.search_panel().is_none());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_open_search_bar_is_search() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_some());
    assert_eq!(app.key_context(), Context::Search);
}

#[test]
fn the_results_view_is_search() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_results().is_some());
    assert_eq!(app.key_context(), Context::Search);
}

#[test]
fn the_search_bar_wins_over_focus() {
    let mut app = groups(1, 80, 24);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    frame(&mut app);
    // No key reaches focus while the bar is open, so the selection is
    // made directly: what the context prioritises is under test.
    app.navigate();
    assert!(app.focused().is_some());
    assert!(app.find_bar().is_some());
    assert_eq!(app.key_context(), Context::Search);
}

#[test]
fn focus_in_the_conversation_is_conversation() {
    let mut app = groups(1, 80, 24);
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    assert_eq!(app.key_context(), Context::Conversation);
}

#[test]
fn a_steering_selection_is_steering() {
    let mut app = homed(80, 24);
    app.on_line(steering_queue());
    app.select_steering(0);
    assert!(app.focused().is_none());
    assert!(app.completions().is_none());
    assert_eq!(app.key_context(), Context::Steering);
}

#[test]
fn the_plain_input_box_is_input() {
    assert_eq!(homed(80, 24).key_context(), Context::Input);
    assert_eq!(home().key_context(), Context::Input);
}

#[test]
fn alt_0_goes_home() {
    let mut app = homed(80, 24);
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn alt_0_goes_home_on_the_on_screen_sessions_row() {
    let mut app = homed(80, 24);
    app.on_line(live(SESSION));
    app.on_line(live("s_bbbbbbbbbbbbbbbb"));
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.session().is_none());
    frame(&mut app);
    let key = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.row(&contract::SessionId(SESSION.to_owned())))
        .map(|row| row.key)
        .expect("SESSION row");
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(key)))
    );
}

/// A `session_left` hub line for `session`, exited.
fn left(session: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": "exited"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// Home left from [`SESSION`] with its live row fed: the back key is
/// live for it.
fn backed() -> App {
    let mut app = homed(80, 24);
    app.on_line(hello());
    app.on_line(live(SESSION));
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    app
}

/// The sent lines parsed.
fn parsed(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{err}")))
        .collect()
}

/// The session `press` attached, from an `Effect::Send` of `lines`.
fn attached_session(lines: &[String]) -> String {
    let lines = parsed(lines);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], SESSION);
    lines[0]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("session"))
        .to_owned()
}

#[test]
fn back_right_after_home_goes_back_to_the_session() {
    let mut app = backed();
    let Effect::Send(lines) = press(&mut app, "right") else {
        panic!("→ goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(!app.on_home());
}

#[test]
fn back_esc_after_home_goes_back_to_the_session() {
    let mut app = backed();
    let Effect::Send(lines) = press(&mut app, "esc") else {
        panic!("Esc goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(!app.on_home());
}

#[test]
fn back_esc_with_the_row_focused_goes_back() {
    let mut app = backed();
    frame(&mut app);
    assert!(matches!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(_)))
    ));
    let Effect::Send(lines) = press(&mut app, "esc") else {
        panic!("Esc goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn back_right_with_the_row_focused_goes_back() {
    let mut app = backed();
    frame(&mut app);
    assert!(matches!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(_)))
    ));
    let Effect::Send(lines) = press(&mut app, "right") else {
        panic!("→ goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn back_esc_with_a_chip_focused_goes_back() {
    let mut app = backed();
    frame(&mut app);
    app.focus = None;
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert!(matches!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(
            Spot::Workspace | Spot::Worktree | Spot::Model | Spot::Thinking
        ))
    ));
    let Effect::Send(lines) = press(&mut app, "esc") else {
        panic!("Esc goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn right_with_a_chip_focused_moves_between_chips() {
    let mut app = backed();
    frame(&mut app);
    app.focus = None;
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let first = app.focused().expect("a chip focused");
    assert_eq!(press(&mut app, "right"), Effect::None);
    let moved = app.focused().expect("still focused");
    assert_ne!(moved, first);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn text_typed_keeps_right_and_esc_from_going_back() {
    let mut app = backed();
    type_text(&mut app, "x");
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert_eq!(app.draft(), "x");
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn open_completions_keep_right_and_esc_from_going_back() {
    let mut app = backed();
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn an_open_key_map_keeps_right_and_esc_from_going_back() {
    let mut app = backed();
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
}

#[test]
fn an_open_workspace_picker_keeps_right_and_esc_from_going_back() {
    let mut app = backed();
    app.home_click(Spot::Workspace);
    assert_eq!(app.top_surface(), Surface::WorkspacePicker);
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn an_open_delete_question_keeps_right_and_esc_from_going_back() {
    let mut app = homed(80, 24);
    let (_, recent) = linked(&mut app);
    app.on_line(live(SESSION));
    app.on_line(accepted(
        &recent,
        json!({"sessions": [{
            "session_id": "s_bbbbbbbbbbbbbbbb",
            "ts": 0,
            "project": "-w",
            "workspace": "/w",
            "name": "old work",
            "how": "exited",
        }]}),
    ));
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.on_home());
    let key = app
        .home_screen()
        .and_then(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .find(|(_, text)| text.contains("old work"))
                .map(|row| row.0)
        })
        .expect("the exited row");
    assert_eq!(app.home_click(Spot::Stop(key)), Effect::None);
    assert_eq!(app.top_surface(), Surface::DeleteQuestion);
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn esc_closes_a_notice_without_going_back() {
    let mut app = backed();
    app.notices.push("extension_failed", "A notice.".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.notice_overlay().is_some());
    assert!(app.session().is_none());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn an_open_workspace_picker_keeps_go_back_empty() {
    let mut app = backed();
    app.home_click(Spot::Workspace);
    assert_eq!(app.top_surface(), Surface::WorkspacePicker);
    assert!(app.completions().is_none());
    assert_eq!(app.draft(), "");
    assert_eq!(app.key_context(), Context::Overlay);
    // The picker takes Esc ahead of the back key, so no press reaches
    // the gate: call it directly. Overlay context with no completions
    // must not go back; `&&` in place of the `||` below would open
    // SESSION here.
    assert!(app.go_back().is_none());
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn a_selected_steering_row_keeps_right_and_esc_from_going_back() {
    let mut app = homed(80, 24);
    app.on_line(live(SESSION));
    app.on_line(steering_queue());
    app.select_steering(0);
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.on_home());
    // The selection returns on home: the rows stayed while leaving.
    app.select_steering(0);
    assert_eq!(app.draft(), "steer me");
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn back_keys_with_an_exited_came_from_do_nothing() {
    let mut app = backed();
    app.on_line(left(SESSION));
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn back_keys_with_no_came_from_row_do_nothing() {
    let mut app = homed(80, 24);
    app.on_line(hello());
    app.on_line(live("s_bbbbbbbbbbbbbbbb"));
    assert_eq!(press(&mut app, "alt+0"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn back_keys_with_the_link_down_are_a_no_op() {
    let mut app = backed();
    app.disconnected();
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn back_keys_while_a_start_waits_are_a_no_op() {
    let mut app = backed();
    // With a model on the chip Enter starts instead of opening the
    // picker, as on a launch that names one.
    if let Some(home) = app.home.as_mut() {
        home.launch.model = Some("test/model".to_owned());
    }
    type_text(&mut app, "hi");
    let Effect::Send(_) = app.on_key(Key::Enter, now()) else {
        panic!("Enter starts");
    };
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn a_rebound_back_key_answers_its_new_key() {
    let mut app = backed();
    keyed(&mut app, &[("close_or_interrupt", json!("f5"))]);
    let Effect::Send(lines) = press(&mut app, "f5") else {
        panic!("F5 goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    let mut app = backed();
    keyed(&mut app, &[("close_or_interrupt", json!("f5"))]);
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
}

#[test]
fn an_unbound_back_key_still_answers_right() {
    let mut app = backed();
    keyed(&mut app, &[("close_or_interrupt", json!([]))]);
    let Effect::Send(lines) = press(&mut app, "right") else {
        panic!("→ goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn ctrl_n_then_right_returns_to_the_session() {
    let mut app = homed(80, 24);
    app.on_line(hello());
    app.on_line(live(SESSION));
    app.on_line(steering_queue());
    app.select_steering(0);
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    let Effect::Send(lines) = press(&mut app, "right") else {
        panic!("→ goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn ctrl_n_then_esc_returns_to_the_session() {
    let mut app = homed(80, 24);
    app.on_line(hello());
    app.on_line(live(SESSION));
    app.on_line(steering_queue());
    app.select_steering(0);
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.on_home());
    let Effect::Send(lines) = press(&mut app, "esc") else {
        panic!("Esc goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn ctrl_n_from_a_focused_item_then_right_returns_to_the_session() {
    let mut app = groups(1, 80, 24);
    app.set_home(launch());
    app.on_line(hello());
    app.on_line(live(SESSION));
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    let Effect::Send(lines) = press(&mut app, "right") else {
        panic!("→ goes back");
    };
    assert_eq!(attached_session(&lines), SESSION);
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
}

#[test]
fn an_unknown_action_does_nothing() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_action("no_such_action"), Effect::None);
    assert!(app.session().is_some());
}

#[test]
fn ctrl_n_goes_home_with_an_empty_draft_as_slash_new() {
    let mut app = homed(80, 24);
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(app.draft(), "");
    let mut slash = homed(80, 24);
    type_text(&mut slash, "/new");
    slash.on_key(Key::Enter, now());
    assert!(slash.session().is_none());
    assert_eq!(slash.draft(), "");
}

#[test]
fn an_action_settles_once_from_the_press_entry() {
    let mut app = homed(80, 24);
    // A key settles the attached layout first, so the action below has
    // a stale layout to clear.
    type_text(&mut app, "x");
    assert!(app.chrome().layout().is_some());
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(app.draft(), "");
    // The entry settles the action's `leave`: the layout is home's,
    // not the attached conversation's.
    assert_eq!(app.chrome().layout(), None);
}

#[test]
fn ctrl_n_from_a_focused_item_lands_on_the_entry_bar() {
    let mut app = groups(1, 80, 24);
    app.set_home(launch());
    type_text(&mut app, "hi");
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.focused().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn ctrl_n_from_a_steering_row_lands_on_the_entry_bar() {
    let mut app = homed(80, 24);
    app.on_line(steering_queue());
    app.select_steering(0);
    assert_eq!(app.draft(), "steer me");
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.focused().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn ctrl_n_with_the_rail_focused_leaves_nothing_focused() {
    let mut app = homed(80, 24);
    app.on_line(live(SESSION));
    let key = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.row(&contract::SessionId(SESSION.to_owned())))
        .map(|row| row.key)
        .expect("SESSION row");
    app.set_rail_focus(crate::app::rail::Spot::Card(key));
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.rail_state().focused().is_none());
    assert!(app.focused().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn ctrl_n_from_the_conversation_leaves_focus_unset() {
    let mut app = groups(1, 80, 24);
    app.set_home(launch());
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert!(app.focused().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn ctrl_n_from_steering_leaves_no_row_selected() {
    let mut app = homed(80, 24);
    app.on_line(steering_queue());
    app.select_steering(0);
    assert_eq!(app.draft(), "steer me");
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(app.draft(), "");
    app.on_line(steering_queue());
    assert_eq!(app.draft(), "");
}

#[test]
fn ctrl_n_under_an_overlay_does_nothing() {
    let mut app = homed(80, 24);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_some());
    assert!(app.keymap_top().is_some());
}

#[test]
fn a_rebound_new_session_answers_its_new_key() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("new_session", json!("ctrl+t"))]);
    assert_eq!(press(&mut app, "ctrl+n"), Effect::None);
    assert!(app.session().is_some());
    assert_eq!(press(&mut app, "ctrl+t"), Effect::None);
    assert!(app.session().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn enter_sends_nothing_once_send_moves_while_ctrl_s_sends() {
    let mut app = homed(80, 24);
    app.on_line(hello());
    keyed(&mut app, &[("send", json!("ctrl+s"))]);
    type_text(&mut app, "hi");
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(app.draft(), "hi");
    assert!(matches!(press(&mut app, "ctrl+s"), Effect::Send(_)));
}

#[test]
fn enter_on_a_focused_item_still_opens_it() {
    let mut app = groups(1, 80, 24);
    keyed(&mut app, &[("send", json!("ctrl+s"))]);
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    let before = screen(&app);
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_ne!(screen(&app), before);
}

#[test]
fn enter_on_the_delete_question_still_deletes() {
    let mut app = delete_question();
    keyed(&mut app, &[("send", json!("ctrl+s"))]);
    assert_eq!(app.top_surface(), Surface::DeleteQuestion);
    assert!(matches!(press(&mut app, "enter"), Effect::Send(_)));
}

#[test]
fn enter_in_the_slash_panel_still_runs_the_command() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("send", json!("ctrl+s"))]);
    type_text(&mut app, "/new");
    assert!(app.completions().is_some());
    press(&mut app, "enter");
    assert!(app.session().is_none());
}

#[test]
fn a_rebound_copy_key_copies_in_the_conversation_and_types_in_the_box() {
    let mut app = groups(1, 80, 24);
    keyed(&mut app, &[("copy_focused", json!("c"))]);
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(matches!(press(&mut app, "c"), Effect::Copy(_)));
    let mut boxed = homed(80, 24);
    keyed(&mut boxed, &[("copy_focused", json!("c"))]);
    assert_eq!(press(&mut boxed, "c"), Effect::None);
    assert_eq!(boxed.draft(), "c");
}

#[test]
fn the_moved_off_y_types_in_the_box_and_does_nothing_focused() {
    let mut app = groups(1, 80, 24);
    keyed(&mut app, &[("copy_focused", json!("c"))]);
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert_eq!(press(&mut app, "y"), Effect::None);
    assert_eq!(app.draft(), "");
    let mut boxed = homed(80, 24);
    keyed(&mut boxed, &[("copy_focused", json!("c"))]);
    assert_eq!(press(&mut boxed, "y"), Effect::None);
    assert_eq!(boxed.draft(), "y");
}

#[test]
fn unbinding_search_results_leaves_both_aliases_closing_nothing() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("search_results", json!([]))]);
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert!(app.find_bar().is_some());
    assert!(app.find_results().is_none());
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert!(app.find_results().is_none());
    assert_eq!(press(&mut app, "super+f"), Effect::None);
    assert!(app.find_results().is_none());
}

#[test]
fn a_rebound_search_results_opens_on_its_key_only() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("search_results", json!("ctrl+s"))]);
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert!(app.find_results().is_none());
    assert_eq!(press(&mut app, "ctrl+s"), Effect::None);
    assert!(app.find_results().is_some());
    assert_eq!(press(&mut app, "super+f"), Effect::None);
    assert!(app.find_results().is_some());
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert!(app.find_results().is_some());
}

#[test]
fn unbinding_search_leaves_both_aliases_shut() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("search", json!([]))]);
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert_eq!(press(&mut app, "super+f"), Effect::None);
    assert!(app.find_bar().is_none());
}

#[test]
fn an_unbound_drop_key_drops_nothing() {
    let mut app = groups(1, 80, 24);
    app.set_home(launch());
    app.on_line(hello());
    keyed(&mut app, &[("drop_steering", json!([]))]);
    app.on_line(steering_queue());
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    assert_eq!(press(&mut app, "alt+x"), Effect::None);
    assert_eq!(app.steering().len(), 1);
}

#[test]
fn a_rebound_drop_key_drops() {
    let mut app = groups(1, 80, 24);
    app.set_home(launch());
    app.on_line(hello());
    keyed(&mut app, &[("drop_steering", json!("ctrl+d"))]);
    app.on_line(steering_queue());
    frame(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(app.focused().is_some());
    let effect = press(&mut app, "ctrl+d");
    assert!(matches!(effect, Effect::Send(_)), "{effect:?}");
}

#[test]
fn an_unbound_select_key_selects_nothing() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("select_steering", json!([]))]);
    app.on_line(steering_queue());
    assert_eq!(press(&mut app, "alt+up"), Effect::None);
    assert_eq!(app.draft(), "");
    let mut bound = homed(80, 24);
    bound.on_line(steering_queue());
    assert_eq!(bound.on_key(Key::AltUp, now()), Effect::None);
    assert_eq!(bound.draft(), "steer me");
}

#[test]
fn an_unbound_navigate_key_leaves_focus_in_the_box() {
    let mut app = groups(1, 80, 24);
    keyed(&mut app, &[("navigate", json!([]))]);
    frame(&mut app);
    assert_eq!(press(&mut app, "shift+tab"), Effect::None);
    assert!(app.focused().is_none());
    let mut bound = groups(1, 80, 24);
    frame(&mut bound);
    assert_eq!(bound.on_key(Key::BackTab, now()), Effect::None);
    assert!(bound.focused().is_some());
}

#[test]
fn rebound_search_steps_move_through_the_matches() {
    let mut app = attached(80, 24);
    keyed(&mut app, &[("search_next_prev", json!(["n", "p"]))]);
    app.on_line(envelope(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(envelope(
        "text_completed",
        json!({"text": "aa aa"}),
        Some("a_m"),
    ));
    app.on_line(envelope(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    assert_eq!(press(&mut app, "ctrl+f"), Effect::None);
    assert!(app.find_bar().is_some());
    let mut generation = 0;
    for ch in "aa".chars() {
        let stroke = Stroke {
            code: Code::Char(ch),
            mods: Mods::NONE,
        };
        let Effect::FindPause {
            generation: next, ..
        } = app.on_press(stroke, now())
        else {
            panic!("typing {ch} started no pause");
        };
        generation = next;
    }
    app.find_due(generation);
    assert_eq!(app.find.current_index(), Some(0));
    assert_eq!(press(&mut app, "n"), Effect::None);
    assert_eq!(app.find.current_index(), Some(1));
    assert_eq!(press(&mut app, "p"), Effect::None);
    assert_eq!(app.find.current_index(), Some(0));
}

#[test]
fn rebound_search_steps_type_into_the_approval_feedback() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("search_next_prev", json!(["n", "p"]))]);
    app.on_line(approval());
    assert!(app.panel().is_some());
    assert_eq!(press(&mut app, "n"), Effect::None);
    let panel = app.panel().expect("the panel stays open");
    assert!(panel.lines.iter().any(|line| line.contains("Deny  n")));
}

#[test]
fn set_keys_with_a_clashing_entry_shows_the_notice() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("search", json!("ctrl+o"))]);
    assert_eq!(
        app.notice(),
        Some(
            "keys.search: Ctrl+O is also Open or close the ledgers (toggle_ledgers); search keeps its default."
        )
    );
}

#[test]
fn the_key_map_shows_the_person_lines_and_pages_them() {
    let mut app = homed(80, 24);
    keyed(&mut app, &[("send", json!(["ctrl+s", "alt+s"]))]);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(screen(&app).contains("Ctrl+S, ⌥S"));
    for _ in 0..80 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    // Paged past the end, the last binding is focused and drawn.
    let map = app.keymap().expect("open");
    let total = map.visible(app.keys()).len();
    assert_eq!(map.focus(), total - 1);
    assert!(
        screen(&app).contains("Open the key map"),
        "{}",
        screen(&app)
    );
}

#[test]
fn the_key_map_over_the_model_picker_is_the_overlay_context() {
    let mut app = home();
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(app.key_context(), Context::Picker);
    // The key map opens above the picker: Esc closes whatever is on top.
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(app.key_context(), Context::Overlay);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.keymap_top().is_none());
    assert!(app.model_picker_open());
    assert_eq!(app.key_context(), Context::Picker);
}

#[test]
fn slash_question_mark_slash_help_and_f1_open_the_key_map() {
    for open in ["/?", "/help", "F1"] {
        let mut app = connected();
        if open == "F1" {
            assert_eq!(app.on_key(Key::F1, now()), Effect::None);
        } else {
            assert_eq!(enter(&mut app, open), Effect::None, "{open}");
        }
        assert_eq!(app.keymap_top(), Some(0), "{open}");
        assert_eq!(app.draft(), "", "{open}");
        assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
        assert_eq!(app.keymap_top(), None, "{open}");
    }
}

#[test]
fn the_key_map_scrolls_by_binding_and_searches() {
    let mut app = hub_attached();
    app.set_size(80, 24);
    turn_starts(&mut app);
    app.on_key(Key::F1, now());
    // The focus moves by binding; the scroll follows whole bindings.
    assert_eq!(app.keymap_top(), Some(0));
    app.on_key(Key::Up, now());
    assert_eq!(app.keymap_top(), Some(0));
    app.on_key(Key::Down, now());
    assert_eq!(app.keymap_top(), Some(0));
    // A page moves by the bindings that fit less one.
    app.on_key(Key::PageDown, now());
    let map = app.keymap().expect("open");
    assert_eq!((map.focus(), map.top()), (2, 1));
    app.on_key(Key::PageUp, now());
    let map = app.keymap().expect("open");
    assert_eq!((map.focus(), map.top()), (1, 1));
    app.on_key(Key::PageUp, now());
    assert_eq!(app.keymap_top(), Some(0));
    // Down past the end holds on the last binding.
    for _ in 0..80 {
        app.on_key(Key::Down, now());
    }
    let map = app.keymap().expect("open");
    let total = map.visible(app.keys()).len();
    assert_eq!(map.focus(), total - 1);
    let last = app.keymap_top();
    app.on_key(Key::Down, now());
    assert_eq!(app.keymap_top(), last);
    // Typing narrows the rows to the query; the draft keeps nothing.
    app.on_key(Key::Esc, now());
    app.on_key(Key::F1, now());
    app.on_key(Key::Char('x'), now());
    let map = app.keymap().expect("open");
    assert_eq!(map.query(), "x");
    assert!(map.visible(app.keys()).len() < total);
    assert_eq!(app.draft(), "");
    app.on_key(Key::Backspace, now());
    assert_eq!(app.keymap().expect("open").query(), "");
    // Enter, Tab, End and F1 do nothing behind the map.
    for key in [Key::Enter, Key::Tab, Key::End, Key::F1] {
        assert_eq!(app.on_key(key, now()), Effect::None);
    }
    assert_eq!(app.draft(), "");
    assert!(app.keymap_top().is_some());
    // Ctrl+C still arms the quit past the map.
    app.on_key(Key::CtrlC, now());
    assert!(app.hint());
    assert!(app.keymap_top().is_some());
}

#[test]
fn editing_keys_do_nothing_while_the_key_map_is_open() {
    let mut app = connected();
    type_any(&mut app, "ab");
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(app.on_edit(Edit::Paste("x".to_owned())), Effect::None);
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    app.on_key(Key::Esc, now());
    key(&mut app, '!');
    assert_eq!(app.draft(), "ab!");
}
/// The open import report is an overlay: strokes resolve through the
/// overlay context while it is open, so nothing types into the draft,
/// the chip row or the conversation behind it.
#[test]
fn the_open_report_is_an_overlay() {
    let mut app = homed(80, 24);
    assert_eq!(app.key_context(), Context::Input);
    app.open_import_report(None, contract::events::ImportedNames::default(), Vec::new());
    assert!(app.import_report_open());
    assert_eq!(app.key_context(), Context::Overlay);
}

/// A paste and home-owned arrows change neither the draft nor the
/// report while the report is open: the paste never reaches the draft
/// behind it, and the arrows never reach the chip row or the draft.
/// The draft is byte-identical after.
#[test]
fn paste_and_home_arrows_change_neither_the_draft_nor_the_report() {
    let mut app = home();
    app.home.as_mut().expect("home").launch.model = Some("acme/m1".to_owned());
    type_text(&mut app, "ab");
    frame(&mut app);
    assert_eq!(press(&mut app, "down"), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    app.open_import_report(None, contract::events::ImportedNames::default(), Vec::new());
    assert!(app.import_report_open());
    // A paste behind the report changes neither the draft nor the report.
    assert_eq!(app.on_edit(Edit::Paste("XY".to_owned())), Effect::None);
    // Home-owned arrows through the bindings path change neither either.
    assert_eq!(press(&mut app, "left"), Effect::None);
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert_eq!(app.draft(), "ab");
    let report = app.import_report().expect("the report stays open");
    assert_eq!((report.copied_count(), report.left_count()), (0, 0));
    assert_eq!(report.top(), 0);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Workspace))
    );
    assert!(app.notices().is_empty());
}

#[test]
fn ctrl_u_deletes_the_draft_line_back_to_its_start() {
    let mut app = attached(80, 24);
    type_text(&mut app, "first");
    assert_eq!(app.on_edit(Edit::ShiftEnter), Effect::None);
    type_text(&mut app, "second line");
    assert_eq!(press(&mut app, "ctrl+u"), Effect::None);
    assert_eq!(app.draft(), "first\n");
    // Rebound, the old key types nothing and the new one deletes.
    keyed(&mut app, &[("delete_to_line_start", json!("ctrl+k"))]);
    type_text(&mut app, "again");
    assert_eq!(press(&mut app, "ctrl+u"), Effect::None);
    assert_eq!(app.draft(), "first\nagain");
    assert_eq!(press(&mut app, "ctrl+k"), Effect::None);
    assert_eq!(app.draft(), "first\n");
}

#[test]
fn ctrl_u_deletes_in_a_draft_typed_during_a_turn() {
    let mut app = attached(80, 24);
    turn_starts(&mut app);
    type_text(&mut app, "steer this way");
    assert_eq!(press(&mut app, "ctrl+u"), Effect::None);
    assert_eq!(app.draft(), "");
}
