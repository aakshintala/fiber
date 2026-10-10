//! Tests for the chip row's keyboard: the chip stops, their clicks,
//! and every arrow, Enter and edit on them (`docs/tui.md`, "Home").

use super::super::super::{App, Effect};
use crate::catalogue::{Catalogue, ModelEntry};
use crate::home::{Launch, Spot};
use crate::keys::{Edit, Key};
use crate::model_picker::Mode;
use crate::mouse::TargetId;
use contract::clock::Clock;
use std::path::PathBuf;
use std::time::Instant;

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// Draws the home frame, so the stops match what is on screen.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

/// Types `text` without pressing Enter.
fn type_text(app: &mut App, text: &str) {
    let now = now();
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
}

/// An app on home at 80x24, with the model chip naming `model`:
/// inside git with the worktree switch, else three chips.
fn home_with(model: Option<&str>) -> App {
    home_git(true, model)
}

fn home_git(git: bool, model: Option<&str>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git,
        hover: true,
        version: "0.0.1".to_owned(),
        model: model.map(str::to_owned),
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

/// One catalogue entry for `reference` with `levels`.
fn entry(reference: &str, levels: &[&str]) -> ModelEntry {
    let (provider, id) = reference.split_once('/').unwrap();
    ModelEntry {
        reference: reference.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        levels: levels.iter().map(|level| (*level).to_owned()).collect(),
        default_level: None,
        configured: None,
        roles: Vec::new(),
        name: None,
    }
}

/// Two models: `acme/m1` with two levels, `acme/m2` with none.
fn catalogue() -> Catalogue {
    Catalogue {
        models: vec![entry("acme/m1", &["low", "high"]), entry("acme/m2", &[])],
        notices: Vec::new(),
    }
}

#[test]
fn clicking_the_model_chip_opens_the_picker_to_choose() {
    let mut app = home_with(Some("acme/m1"));
    app.on_models(Ok(catalogue()));
    assert_eq!(app.home_click(Spot::Model), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(Mode::Choose)
    );
}

#[test]
fn clicking_the_thinking_chip_opens_it_on_the_level_chips() {
    let mut app = home_with(Some("acme/m1"));
    app.on_models(Ok(catalogue()));
    assert_eq!(app.home_click(Spot::Thinking), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(Mode::Thinking)
    );
    // A model the catalogue does not list opens an ordinary choose.
    let mut app = home_with(Some("gone/x"));
    app.on_models(Ok(catalogue()));
    assert_eq!(app.home_click(Spot::Thinking), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(Mode::Choose)
    );
}

#[test]
fn is_chip_names_the_chip_row() {
    for chip in [Spot::Workspace, Spot::Worktree, Spot::Model, Spot::Thinking] {
        assert!(chip.is_chip(), "{chip:?} is a chip");
    }
    for stop in [
        Spot::Entry(0),
        Spot::Stop(0),
        Spot::Toggle,
        Spot::Pick(0),
        Spot::Quit(crate::home::QuitChoice::Stay),
    ] {
        assert!(!stop.is_chip(), "{stop:?} is no chip");
    }
}

#[test]
fn down_from_the_entry_bar_focuses_the_workspace_chip_at_first() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

/// The chip row's texts, left to right.
fn chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|(_, text)| text).collect())
        .unwrap_or_default()
}

#[test]
fn left_and_right_move_between_chips_and_stop_at_the_ends() {
    // Inside git the row holds four chips.
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    for chip in [Spot::Worktree, Spot::Model, Spot::Thinking] {
        assert_eq!(app.on_edit(Edit::Right), Effect::None);
        assert_eq!(app.focused(), Some(TargetId::Home(chip)));
    }
    // Past the last chip → stays.
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Thinking)));
    for chip in [Spot::Model, Spot::Worktree, Spot::Workspace] {
        assert_eq!(app.on_edit(Edit::Left), Effect::None);
        assert_eq!(app.focused(), Some(TargetId::Home(chip)));
    }
    // Before the first chip ← stays.
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    // Outside git the switch is hidden: three chips.
    let mut app = home_git(false, Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Thinking)));
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Thinking)));
}

#[test]
fn up_from_a_chip_returns_to_the_entry_bar_and_down_comes_back_to_the_same_chip() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
}

#[test]
fn down_keeps_the_draft_and_focuses_the_chip() {
    let mut app = home_with(Some("acme/m1"));
    type_text(&mut app, "x");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.input().expand(), "x");
}

#[test]
fn down_in_a_two_line_draft_moves_the_cursor_first() {
    let mut app = home_with(Some("acme/m1"));
    type_text(&mut app, "a");
    assert_eq!(app.on_edit(Edit::ShiftEnter), Effect::None);
    type_text(&mut app, "b");
    drawn(&mut app);
    // On the last wrapped row ↓ leaves for the chip row.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), None);
    // Above the last row ↑ moves the cursor first, then ↓ moves it
    // back down before leaving.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "a\nb");
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: serde_json::Value) -> crate::link::Line {
    crate::link::Line::Hub(contract::HubLine {
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

#[test]
fn down_while_browsing_a_recalled_prompt_stays_in_recall() {
    let mut app = home_with(Some("acme/m1"));
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    drawn(&mut app);
    // ↑ asks the first `prompt_history` page.
    let Effect::Send(lines) = app.on_key(Key::Up, now()) else {
        panic!("Up asks the first page");
    };
    let id: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let result = serde_json::json!({"prompts": [{"session_id": "s_aaaaaaaaaaaaaaaa",
        "content": [{"type": "text", "text": "hi"}]}]});
    assert!(
        app.on_line(accepted(id["id"].as_str().unwrap(), result))
            .is_empty()
    );
    assert_eq!(app.input().expand(), "hi");
    // ↓ browses newer, back to the empty draft, and stays in the box.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.input().expand().is_empty());
}

#[test]
fn down_with_the_completion_panel_open_moves_its_selection() {
    let mut app = home_with(Some("acme/m1"));
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.completions().is_some());
}

#[test]
fn a_hidden_remembered_chip_falls_back_to_the_workspace_chip() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
    // Outside git the switch hides: ↓ lands on the workspace chip.
    app.home.as_mut().expect("home").launch.git = false;
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn enter_on_the_workspace_chip_opens_the_picker_and_keeps_focus() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let picker = app.home.as_ref().and_then(|home| home.picker.clone());
    assert_eq!(picker, Some((vec!["/w".to_owned()], 0)));
    // The picker draws over home, so the chip stays focused.
    drawn(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    // Esc closes the picker with the chip still focused; a second Esc
    // returns to the entry bar.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.home.as_ref().is_some_and(|home| home.picker.is_none()));
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn enter_on_the_switch_toggles_it() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(chips(&app).contains(&"[x] new worktree".to_owned()));
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(chips(&app).contains(&"[ ] new worktree".to_owned()));
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
}

#[test]
fn enter_on_the_model_chip_opens_the_picker_and_keeps_focus() {
    let mut app = home_with(Some("acme/m1"));
    app.on_models(Ok(catalogue()));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(Mode::Choose)
    );
    // The swapped picker carries no home targets, yet the chip stays
    // focused behind it.
    drawn(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
    // Esc closes the picker and focus stays on the chip.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    drawn(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
}

#[test]
fn enter_on_the_thinking_chip_opens_the_level_chips() {
    let mut app = home_with(Some("acme/m1"));
    app.on_models(Ok(catalogue()));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    for _ in 0..3 {
        assert_eq!(app.on_edit(Edit::Right), Effect::None);
    }
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Thinking)));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(Mode::Thinking)
    );
    drawn(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Thinking)));
}

#[test]
fn enter_on_a_chip_with_a_draft_clicks_the_chip() {
    let mut app = home_with(None);
    type_text(&mut app, "hi");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    // The chip's click wins over `start`: the picker opens, nothing is
    // sent, and the draft stays.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.home.as_ref().is_some_and(|home| home.picker.is_some()));
    assert_eq!(app.input().expand(), "hi");
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

/// A `hub_hello` this terminal reads.
fn hello() -> crate::link::Line {
    crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A live `session_status` for `session`, named `name`, in `workspace` of
/// `project`, in `state`.
fn live(
    session: &str,
    name: &str,
    workspace: &str,
    project: &str,
    state: serde_json::Value,
) -> crate::link::Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": workspace,
        "project": project,
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    crate::link::Line::Session(contract::Envelope {
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

/// Links the app: the feed and recent ids, both waiting for their
/// answers.
fn linked(app: &mut App) -> (String, String) {
    let lines = app.on_line(hello());
    assert_eq!(lines.len(), 2);
    let lines: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (
        lines[0]["id"].as_str().unwrap().to_owned(),
        lines[1]["id"].as_str().unwrap().to_owned(),
    )
}

/// The rows home draws, as keys top to bottom.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

/// Two live rows, the first streaming and the second idle.
fn two_rows(app: &mut App) {
    linked(app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        serde_json::json!({"state": "streaming"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "tidy docs",
        "/w",
        "-w",
        serde_json::json!({"state": "idle"}),
    ));
}

/// Draws the home frame at `width` by `height`.
fn drawn_at(app: &mut App, width: u16, height: u16) {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

#[test]
fn second_down_focuses_the_first_row_and_down_moves_down_the_rows() {
    let mut app = home_with(Some("acme/m1"));
    two_rows(&mut app);
    drawn(&mut app);
    // The chip row comes first, then the rows, row to row, never a ✕.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    // Past the last row ↓ stays: the last answer was empty, so no page
    // is asked.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
}

#[test]
fn up_from_the_first_row_returns_to_the_chip_focused_last() {
    let mut app = home_with(Some("acme/m1"));
    two_rows(&mut app);
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
}

#[test]
fn arrows_from_a_cross_move_by_its_row() {
    let mut app = home_with(Some("acme/m1"));
    two_rows(&mut app);
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    // j steps onto the row's ✕, where ↓ moves by its row.
    assert_eq!(app.on_key(Key::Char('j'), now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Stop(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Char('j'), now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Stop(keys(&app)[1])))
    );
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn down_on_the_chip_row_with_no_session_does_nothing() {
    // No row at all.
    let mut app = home_with(Some("acme/m1"));
    linked(&mut app);
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    // Every row scoped away: the toggle shows, yet ↓ does nothing.
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "away",
        "/lens",
        "-other",
        serde_json::json!({"state": "idle"}),
    ));
    drawn(&mut app);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.toggle.is_some())
    );
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn down_on_a_chip_skips_the_toggle_to_the_first_row() {
    let mut app = home_with(Some("acme/m1"));
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "here",
        "/w",
        "-w",
        serde_json::json!({"state": "idle"}),
    ));
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        "away",
        "/lens",
        "-other",
        serde_json::json!({"state": "idle"}),
    ));
    drawn(&mut app);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.toggle.is_some())
    );
    // ↓ skips the toggle to the first row.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    // k from the first row reaches the toggle, ↓ skips back past it,
    // and ↑ on it returns to the chip.
    assert_eq!(app.on_key(Key::Char('k'), now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Toggle)));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(app.on_key(Key::Char('k'), now()), Effect::None);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn shift_tab_reaches_the_last_cross() {
    let mut app = home_with(Some("acme/m1"));
    two_rows(&mut app);
    drawn(&mut app);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Stop(keys(&app)[1])))
    );
}

#[test]
fn down_below_the_fold_and_up_while_scrolled_follow_the_rows() {
    let mut app = home_with(Some("acme/m1"));
    linked(&mut app);
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(live(
            &session,
            "fix the parser",
            "/w",
            "-w",
            serde_json::json!({"state": "idle"}),
        ));
    }
    assert_eq!(keys(&app).len(), 15);
    drawn(&mut app);
    // One chip step and nine row steps reach the ninth row; the next ↓
    // moves below the fold to the tenth, pending until the next frame.
    for _ in 0..10 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let shown = keys(&app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Entry(shown[8]))));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Entry(shown[9]))));
    drawn(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Entry(shown[9]))));
    // ↑ while scrolled steps back up the rows.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Entry(shown[8]))));
}

#[test]
fn row_moves_do_nothing_when_no_row_fits() {
    let mut app = home_with(Some("acme/m1"));
    app.set_size(80, 10);
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        "fix the parser",
        "/w",
        "-w",
        serde_json::json!({"state": "idle"}),
    ));
    drawn_at(&mut app, 80, 10);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn f1_over_the_model_picker_drops_chip_focus() {
    let mut app = home_with(Some("acme/m1"));
    app.on_models(Ok(catalogue()));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    // The key map opens above the picker; the next frame drops the chip
    // focus, as any other cover drops a row's.
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    // Closing the key map leaves the picker open with the entry bar
    // focused; closing the picker leaves the entry bar focused.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.keymap_top().is_none());
    assert!(app.model_picker_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(app.focused(), None);
}

#[test]
fn the_remembered_chip_survives_a_session() {
    let mut app = home_with(Some("acme/m1"));
    app.on_line(hello());
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    // Starting a session and leaving home keeps the remembered chip.
    type_text(&mut app, "hi");
    let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
        panic!("Enter sends the start");
    };
    let start: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let result = serde_json::json!({"session_id": "s_aaaaaaaaaaaaaaaa"});
    app.on_line(accepted(start["id"].as_str().unwrap(), result));
    assert!(!app.on_home());
    app.leave();
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Model)));
}

#[test]
fn typing_on_a_chip_goes_to_the_entry_bar() {
    for key in ['f', 'j', 'k', 'y', 'q'] {
        let mut app = home_with(Some("acme/m1"));
        drawn(&mut app);
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
        assert_eq!(
            app.focused(),
            Some(TargetId::Home(Spot::Workspace)),
            "{key} starts on the chip"
        );
        assert_eq!(app.on_key(Key::Char(key), now()), Effect::None);
        assert_eq!(app.focused(), None, "{key} returns to the entry bar");
        assert_eq!(app.input().expand(), key.to_string());
    }
}

#[test]
fn backspace_on_a_chip_goes_to_the_entry_bar() {
    let mut app = home_with(Some("acme/m1"));
    type_text(&mut app, "ab");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "a");
}

#[test]
fn delete_and_paste_on_a_chip_go_to_the_entry_bar() {
    let mut app = home_with(Some("acme/m1"));
    type_text(&mut app, "ab");
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    // Delete applies at the cursor once focus returns to the entry bar.
    assert_eq!(app.on_edit(Edit::Delete), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "ab");
    // A paste joins the draft the same way.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_edit(Edit::Paste("XY".to_owned())), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "abXY");
}

#[test]
fn esc_on_a_chip_returns_to_the_entry_bar() {
    let mut app = home_with(Some("acme/m1"));
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_chip_is_the_input_context() {
    let mut app = home_with(Some("acme/m1"));
    let mut user = serde_json::Map::new();
    user.insert("copy_focused".to_owned(), serde_json::json!("c"));
    app.set_keys(crate::KeysSetup { user });
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    // `y` types even though the person bound a `copy_focused` key: the
    // chip is the input box's keyboard, not the conversation's.
    let stroke = crate::stroke::Stroke::parse("y").unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
    assert_eq!(app.focused(), None);
    assert_eq!(app.input().expand(), "y");
}

/// Loads `entries` as the app's `keys`.
fn keyed(app: &mut App, entries: &[(&str, serde_json::Value)]) {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    app.set_keys(crate::KeysSetup { user });
}

/// Presses the stroke `name` names.
fn press(app: &mut App, name: &str) -> Effect {
    let stroke = crate::stroke::Stroke::parse(name).unwrap();
    app.on_press(stroke, now())
}

#[test]
fn rebound_recall_prompt_leaves_up_on_a_chip_to_home() {
    let mut app = home_with(Some("acme/m1"));
    keyed(&mut app, &[("recall_prompt", serde_json::json!("ctrl+p"))]);
    drawn(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    // ↑ on the chip reaches the entry bar, though the binding moved.
    assert_eq!(press(&mut app, "up"), Effect::None);
    assert_eq!(app.focused(), None);
    // ↑ on the entry bar still does nothing: the binding moved.
    assert_eq!(press(&mut app, "up"), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.input().expand().is_empty());
}

#[test]
fn rebound_focus_next_prev_leaves_down_on_a_row_to_home() {
    let mut app = home_with(Some("acme/m1"));
    keyed(
        &mut app,
        &[("focus_next_prev", serde_json::json!(["j", "k"]))],
    );
    two_rows(&mut app);
    drawn(&mut app);
    assert_eq!(press(&mut app, "down"), Effect::None);
    assert_eq!(press(&mut app, "down"), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    // ↓ and ↑ still walk the list from a row and back to the chip.
    assert_eq!(press(&mut app, "down"), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[1])))
    );
    assert_eq!(press(&mut app, "up"), Effect::None);
    assert_eq!(
        app.focused(),
        Some(TargetId::Home(Spot::Entry(keys(&app)[0])))
    );
    assert_eq!(press(&mut app, "up"), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
}

#[test]
fn left_and_right_on_a_chip_ignore_a_person_binding() {
    let mut app = home_with(Some("acme/m1"));
    keyed(
        &mut app,
        &[("move_word", serde_json::json!(["left", "right"]))],
    );
    drawn(&mut app);
    assert_eq!(press(&mut app, "down"), Effect::None);
    // ← → still move between chips, though the person bound them.
    assert_eq!(press(&mut app, "right"), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Worktree)));
    assert_eq!(press(&mut app, "left"), Effect::None);
    assert_eq!(app.focused(), Some(TargetId::Home(Spot::Workspace)));
    // On the entry bar they move by word.
    type_text(&mut app, "ab cd");
    assert_eq!(press(&mut app, "left"), Effect::None);
    assert_eq!(app.focused(), None);
}
