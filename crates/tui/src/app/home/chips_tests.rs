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
