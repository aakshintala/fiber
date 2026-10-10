//! Tests for the chip row's keyboard: the chip stops, their clicks,
//! and every arrow, Enter and edit on them (`docs/tui.md`, "Home").

use super::super::super::{App, Effect};
use crate::catalogue::{Catalogue, ModelEntry};
use crate::home::{Launch, Spot};
use crate::model_picker::Mode;
use std::path::PathBuf;

/// An app on home at 80x24, inside git with the model chip naming
/// `model`.
fn home_with(model: Option<&str>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: true,
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
