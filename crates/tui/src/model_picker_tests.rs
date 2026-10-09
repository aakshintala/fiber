//! Tests for the model picker's state: the rows in scope, the preselected
//! chips, and the movement clamps. The app's keys are tested beside them.

use super::{Mode, ModelPicker, preselect, visible};
use crate::catalogue::{Catalogue, ModelEntry};

fn entry(
    reference: &str,
    levels: &[&str],
    default: Option<&str>,
    configured: Option<&str>,
) -> ModelEntry {
    let (provider, id) = reference.split_once('/').unwrap();
    ModelEntry {
        reference: reference.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        levels: levels.iter().map(|level| (*level).to_owned()).collect(),
        default_level: default.map(str::to_owned),
        configured: configured.map(str::to_owned),
        roles: Vec::new(),
    }
}

/// The catalogue's `n`th entry.
fn at(models: &[ModelEntry], n: usize) -> &ModelEntry {
    models.get(n).unwrap_or_else(|| panic!("entry {n}"))
}

/// Five models over two providers, in catalogue order.
fn catalogue() -> Vec<ModelEntry> {
    vec![
        entry("acme/m1", &["low", "high"], Some("high"), None),
        entry("acme/m2", &["low", "high"], Some("low"), Some("high")),
        entry("zeta/z1", &[], None, None),
        entry("zeta/z2", &["low"], Some("low"), Some("high")),
        entry("zeta/z3", &["low", "high"], Some("max"), None),
    ]
}

#[test]
fn rows_group_by_provider_in_catalogue_order() {
    let models = catalogue();
    let (rows, line) = visible(&models, &[], false);
    assert_eq!(rows, [0, 1, 2, 3, 4]);
    assert_eq!(line, None);
}

#[test]
fn scoped_shows_only_listed_models_until_show_all() {
    let models = catalogue();
    let scoped = [
        "acme/m1".to_owned(),
        "zeta/z3".to_owned(),
        "gone/x".to_owned(),
    ];
    let (rows, line) = visible(&models, &scoped, false);
    assert_eq!(rows, [0, 4]);
    assert_eq!(
        line.as_deref(),
        Some("scoped_models: 2 of 5 · Tab shows all")
    );
    let (rows, line) = visible(&models, &scoped, true);
    assert_eq!(rows, [0, 1, 2, 3, 4]);
    assert_eq!(line.as_deref(), Some("all 5 · Tab shows scoped_models"));
}

#[test]
fn an_empty_scoped_list_shows_every_model() {
    let models = catalogue();
    let (rows, line) = visible(&models, &[], true);
    assert_eq!(rows, [0, 1, 2, 3, 4]);
    assert_eq!(line, None);
}

#[test]
fn a_scoped_list_with_nothing_installed_lists_no_rows_until_show_all() {
    let models = catalogue();
    let scoped = ["gone/x".to_owned()];
    let (rows, line) = visible(&models, &scoped, false);
    assert!(rows.is_empty());
    assert_eq!(
        line.as_deref(),
        Some("None of scoped_models is installed. Tab shows all 5.")
    );
    let (rows, _) = visible(&models, &scoped, true);
    assert_eq!(rows, [0, 1, 2, 3, 4]);
}

#[test]
fn the_on_screen_model_preselects_its_level() {
    let models = catalogue();
    // The current level, when the model declares it.
    assert_eq!(
        preselect(at(&models, 0), Some(("acme/m1", Some("low")))),
        Some(0)
    );
    // Unset, the row falls to its configured level, then its default.
    assert_eq!(preselect(at(&models, 0), Some(("acme/m1", None))), Some(1));
    assert_eq!(preselect(at(&models, 1), Some(("acme/m2", None))), Some(1));
}

#[test]
fn other_rows_preselect_configured_then_default() {
    let models = catalogue();
    // Declared configured levels win over the default.
    assert_eq!(preselect(at(&models, 1), None), Some(1));
    // An undeclared configured level falls to the default without a notice.
    assert_eq!(preselect(at(&models, 3), None), Some(0));
    // Without either, the default wins.
    assert_eq!(
        preselect(
            &entry("acme/m9", &["low", "high"], Some("high"), None),
            None
        ),
        Some(1)
    );
    // Without any of them, no chip.
    assert_eq!(
        preselect(&entry("acme/m9", &["low", "high"], None, None), None),
        None
    );
}

#[test]
fn an_undeclared_configured_level_falls_to_the_default() {
    let models = catalogue();
    assert_eq!(preselect(at(&models, 3), None), Some(0));
}

#[test]
fn a_model_with_no_levels_has_no_chip() {
    let models = catalogue();
    assert_eq!(preselect(at(&models, 2), None), None);
    assert_eq!(
        preselect(at(&models, 2), Some(("zeta/z1", Some("high")))),
        None
    );
}

#[test]
fn the_selection_stays_on_its_model_across_the_toggle() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
        },
        scoped: vec!["acme/m1".to_owned(), "zeta/z3".to_owned()],
        ..ModelPicker::default()
    };
    let selected = |picker: &ModelPicker| {
        picker
            .open
            .as_ref()
            .map(|open| picker.catalogue.models[open.selected].reference.clone())
    };
    picker.open(Mode::Choose, None);
    picker.move_row(1);
    assert_eq!(selected(&picker).as_deref(), Some("zeta/z3"));
    picker.toggle_show_all();
    assert_eq!(selected(&picker).as_deref(), Some("zeta/z3"));
    picker.move_row(10);
    picker.toggle_show_all();
    // `zeta/z3` is still shown, so the selection stays; off the list it
    // would move to the first row.
    assert_eq!(selected(&picker).as_deref(), Some("zeta/z3"));
}

#[test]
fn move_chip_from_no_chip_takes_the_near_end_and_marks_touched() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
        },
        ..ModelPicker::default()
    };
    // `zeta/z3` declares levels but preselects none.
    picker.scoped = vec!["zeta/z3".to_owned()];
    picker.open(Mode::Choose, None);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.selected, 4);
    assert_eq!(open.chips.get(4).copied().flatten(), None);
    picker.move_chip(1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.get(4).copied().flatten(), Some(0));
    assert!(open.touched.get(4).copied().unwrap_or(false));
    picker.open(Mode::Choose, None);
    picker.move_chip(-1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.get(4).copied().flatten(), Some(1));
}

#[test]
fn move_chip_on_a_model_with_no_levels_does_nothing() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
        },
        ..ModelPicker::default()
    };
    picker.scoped = vec!["zeta/z1".to_owned()];
    picker.open(Mode::Choose, None);
    picker.move_chip(1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.get(2).copied().flatten(), None);
    assert!(!open.touched.get(2).copied().unwrap_or(true));
}

#[test]
fn toggle_moves_the_selection_to_the_first_row_when_its_model_hides() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
        },
        scoped: vec!["acme/m1".to_owned()],
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    picker.toggle_show_all();
    picker.move_row(2);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(2));
    picker.toggle_show_all();
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
}
