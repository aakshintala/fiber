//! Tests for the keyboard: the context on screen, actions, and the
//! person's bindings reaching every handler.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use contract::clock::Clock;

use super::super::{App, Effect};
use crate::home::{Launch, Spot};
use crate::keys::Key;
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
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
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
            "since": 0,
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
        .and_then(|screen| screen.rows.first().map(|row| row.0))
        .expect("one row");
    assert_eq!(app.home_click(Spot::Stop(key)), Effect::None);
    assert!(app.home_modal());
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
    assert!(!app.home_modal());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_delete_question_is_an_overlay() {
    let app = delete_question();
    assert!(app.home_modal());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn the_workspace_picker_is_an_overlay() {
    let mut app = home();
    assert_eq!(app.home_click(Spot::Workspace), Effect::None);
    assert!(app.home_modal());
    assert_eq!(app.key_context(), Context::Overlay);
}

#[test]
fn home_modal_needs_home_with_a_prompt_or_the_picker() {
    assert!(!home().home_modal());
    assert!(!attached(80, 24).home_modal());
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
    assert!(app.home_modal());
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
    assert!(panel.lines.iter().any(|line| line.contains("deny · n")));
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
    let total: usize = crate::keymap::lines(app.keys())
        .iter()
        .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), app.column_width()))
        .sum();
    let height = app.conversation_height();
    for _ in 0..80 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    assert_eq!(
        app.keymap_top(),
        Some(total.saturating_sub(height)),
        "pages the person's lines"
    );
}
