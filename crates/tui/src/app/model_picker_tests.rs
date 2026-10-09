//! Tests for the model picker's reads on the app: a read error shows
//! once with the old catalogue kept, and each catalogue notice shows once
//! per read.

use super::super::App;
use crate::catalogue::{Catalogue, ModelEntry};
use crate::home::Launch;
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
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
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

fn entry(reference: &str) -> ModelEntry {
    let (provider, id) = reference.split_once('/').unwrap();
    ModelEntry {
        reference: reference.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        levels: Vec::new(),
        default_level: None,
        configured: None,
        roles: Vec::new(),
    }
}

fn catalogue() -> Catalogue {
    Catalogue {
        models: vec![entry("acme/m1")],
        notices: vec!["a cached provider is gone".to_owned()],
    }
}

#[test]
fn a_read_error_is_one_notice_and_keeps_the_catalogue() {
    let mut app = home();
    app.on_models(Ok(catalogue()));
    assert_eq!(app.model_picker.catalogue.models.len(), 1);
    app.on_models(Err("the lists could not be read".to_owned()));
    assert_eq!(app.notice(), Some("the lists could not be read"));
    assert_eq!(app.notices().len(), 2);
    assert_eq!(app.model_picker.catalogue.models.len(), 1);
    assert_eq!(
        app.model_picker.error.as_deref(),
        Some("the lists could not be read")
    );
    // A later read replaces the catalogue and clears the error.
    app.on_models(Ok(Catalogue::default()));
    assert!(app.model_picker.catalogue.models.is_empty());
    assert_eq!(app.model_picker.error, None);
}

#[test]
fn catalogue_notices_are_pushed_once_per_read() {
    let mut app = home();
    let catalogue = Catalogue {
        models: vec![entry("acme/m1")],
        notices: vec!["first".to_owned(), "second".to_owned()],
    };
    app.on_models(Ok(catalogue.clone()));
    assert_eq!(app.notices().len(), 2);
    assert_eq!(app.notice(), Some("second"));
    app.on_models(Ok(catalogue));
    assert_eq!(app.notices().len(), 4);
}
