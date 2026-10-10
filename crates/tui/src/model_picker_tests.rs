//! Tests for the model picker's state: the rows in scope, the preselected
//! chips, and the movement clamps. The app's keys are tested beside them.

use super::{
    Choice, Fresh, Mode, ModelPicker, PickerCtx, PickerView, age, command_args, preselect,
    rebuild_cost, saves, scoped_save, shown_in, start_args, visible,
};
use crate::catalogue::{Catalogue, ModelEntry};
use crate::keys::Key;

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
        name: None,
        price: None,
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

/// The draw context: no prompt size, the wall time `wall_ms`, and the
/// on-screen model `current`.
fn ctx(usage: Option<u64>, wall_ms: u64, current: Option<&str>) -> PickerCtx<'_> {
    PickerCtx {
        usage,
        wall_ms,
        spinner: "⠋",
        current,
    }
}

/// `picker`'s view with no prompt size, at wall time 0, and no current
/// model.
fn view_of(picker: &ModelPicker) -> PickerView {
    picker
        .view(&ctx(None, 0, None))
        .expect("the picker is open")
}

#[test]
fn rows_group_by_provider_in_catalogue_order() {
    let models = catalogue();
    assert_eq!(visible(&models, &[], false), [0, 1, 2, 3, 4]);
}

#[test]
fn scoped_shows_only_listed_models_until_show_all() {
    let models = catalogue();
    let scoped = [
        "acme/m1".to_owned(),
        "zeta/z3".to_owned(),
        "gone/x".to_owned(),
    ];
    assert_eq!(visible(&models, &scoped, false), [0, 4]);
    assert_eq!(visible(&models, &scoped, true), [0, 1, 2, 3, 4]);
}

#[test]
fn an_empty_scoped_list_shows_every_model() {
    let models = catalogue();
    assert_eq!(visible(&models, &[], true), [0, 1, 2, 3, 4]);
}

#[test]
fn a_scoped_list_with_nothing_installed_lists_no_rows_until_show_all() {
    let models = catalogue();
    let scoped = ["gone/x".to_owned()];
    assert!(visible(&models, &scoped, false).is_empty());
    assert_eq!(visible(&models, &scoped, true), [0, 1, 2, 3, 4]);
}

#[test]
fn thinking_inserts_the_current_model_in_catalogue_order_without_duplicates() {
    let models = catalogue();
    let scoped = ["acme/m1".to_owned(), "zeta/z2".to_owned()];
    let rows = shown_in(
        &models,
        &scoped,
        false,
        Mode::Thinking,
        Some(&("zeta/z1".to_owned(), None)),
        "",
    );
    assert_eq!(rows, [0, 2, 3]);

    let scoped = ["acme/m1".to_owned(), "acme/m2".to_owned()];
    let rows = shown_in(
        &models,
        &scoped,
        false,
        Mode::Thinking,
        Some(&("zeta/z3".to_owned(), None)),
        "",
    );
    assert_eq!(rows, [0, 1, 4]);

    let scoped = [
        "acme/m1".to_owned(),
        "zeta/z1".to_owned(),
        "zeta/z2".to_owned(),
    ];
    let rows = shown_in(
        &models,
        &scoped,
        false,
        Mode::Thinking,
        Some(&("zeta/z1".to_owned(), None)),
        "",
    );
    assert_eq!(rows, [0, 2, 3]);
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
            lists: Vec::new(),
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
            lists: Vec::new(),
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
            lists: Vec::new(),
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
            lists: Vec::new(),
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

/// An open picker over `models`: the selection on the first row.
fn open_over(models: Vec<ModelEntry>) -> ModelPicker {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models,
            notices: Vec::new(),
            lists: Vec::new(),
        },
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    picker
}

#[test]
fn a_replacement_inserting_before_the_selection_keeps_its_model() {
    let mut picker = open_over(catalogue());
    picker.move_row(1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(picker.catalogue.models[open.selected].reference, "acme/m2");
    let mut models = vec![entry("acme/m0", &["low"], Some("low"), None)];
    models.extend(catalogue());
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    // The selection follows `acme/m2` past the inserted row.
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.selected, 2);
    assert_eq!(picker.catalogue.models[open.selected].reference, "acme/m2");
    // A newly discovered row preselects its default level, and an
    // untouched row keeps its preselection.
    assert_eq!(open.chips.first().copied().flatten(), Some(0));
    assert_eq!(open.chips.get(2).copied().flatten(), Some(1));
}

#[test]
fn a_replacement_removing_the_selected_model_clamps() {
    // The selected last row answered away: the selection clamps to the
    // new last row.
    let mut picker = open_over(catalogue());
    picker.move_row(10);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(4));
    let mut models = catalogue();
    models.pop();
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.selected, 3);
    assert_eq!(picker.catalogue.models[open.selected].reference, "zeta/z2");
    // A removed middle row clamps the index into the answered list.
    let mut picker = open_over(catalogue());
    picker.move_row(1);
    let mut models = catalogue();
    models.remove(1);
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.selected, 1);
    assert_eq!(picker.catalogue.models[open.selected].reference, "zeta/z1");
}

#[test]
fn chips_follow_the_level_name_across_a_replacement() {
    let mut picker = open_over(catalogue());
    // Touch `acme/m1` on `low`: its chip sits at index 0.
    picker.move_chip(-1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.first().copied().flatten(), Some(0));
    // The replacement reorders both rows' levels: the touched chip
    // follows `low` to index 1, while untouched `acme/m2` follows its
    // configured `high` to index 0.
    let mut models = catalogue();
    models[0].levels = vec!["high".to_owned(), "low".to_owned()];
    models[1].levels = vec!["high".to_owned(), "low".to_owned()];
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.first().copied().flatten(), Some(1));
    assert_eq!(open.chips.get(1).copied().flatten(), Some(0));
    assert!(open.touched.first().copied().unwrap_or(false));
    assert!(!open.touched.get(1).copied().unwrap_or(true));
}

#[test]
fn an_open_before_the_first_answer_preselects_the_current_model() {
    let mut picker = ModelPicker::default();
    picker.open(Mode::Choose, Some(("acme/m2", Some("low"))));
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    picker.store(Ok(Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    let open = picker.open.as_ref().unwrap();
    assert_eq!(picker.catalogue.models[open.selected].reference, "acme/m2");
    // The current level wins over the row's configured one; the other
    // rows preselect as usual.
    assert_eq!(open.chips.get(1).copied().flatten(), Some(0));
    assert_eq!(open.chips.first().copied().flatten(), Some(1));
}

#[test]
fn page_keys_move_by_the_height_less_one_clamped() {
    let mut picker = open_over(catalogue());
    // A page is the height less one, as the list moves.
    picker.move_page(&Key::PageDown, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(2));
    picker.move_page(&Key::PageDown, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(4));
    picker.move_page(&Key::PageUp, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(2));
    picker.move_page(&Key::PageUp, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // A one-row page still moves by one.
    picker.move_page(&Key::PageDown, 1);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
    // Any other key leaves the selection where it was.
    picker.move_page(&Key::Enter, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
}

#[test]
fn page_keys_do_nothing_closed_empty_or_without_height() {
    let mut closed = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
            lists: Vec::new(),
        },
        ..ModelPicker::default()
    };
    closed.move_page(&Key::PageDown, 3);
    assert!(closed.open.is_none());

    // A scope matching nothing lists no rows to page over.
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
            lists: Vec::new(),
        },
        scoped: vec!["gone/x".to_owned()],
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    picker.move_page(&Key::PageDown, 3);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));

    // With no height nothing moves.
    let mut picker = open_over(catalogue());
    picker.move_page(&Key::PageDown, 0);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
}

#[test]
fn the_status_says_reading_until_the_first_answer() {
    let mut picker = open_over(Vec::new());
    // Opening asks `Stale`, so a read is owed.
    assert_eq!(
        view_of(&picker).status,
        ["Reading the model lists…".to_owned()]
    );
    // Taken but unanswered, a read is out: still reading.
    assert_eq!(picker.take_read(), Some(crate::catalogue::Refresh::Stale));
    assert_eq!(
        view_of(&picker).status,
        ["Reading the model lists…".to_owned()]
    );
    // A running refresh beside a catalogue shows on its own line.
    picker.store(Ok(Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(picker.take_read(), None);
    picker.refresh();
    assert_eq!(picker.take_read(), Some(crate::catalogue::Refresh::Every));
    assert_eq!(view_of(&picker).status, ["refreshing…".to_owned()]);
    picker.store(Err("gone".to_owned()));
    assert!(view_of(&picker).status.is_empty());
}

#[test]
fn a_read_error_with_no_catalogue_shows_the_error() {
    let mut picker = open_over(Vec::new());
    assert_eq!(picker.take_read(), Some(crate::catalogue::Refresh::Stale));
    picker.store(Err("the lists could not be read".to_owned()));
    assert_eq!(
        view_of(&picker).status,
        ["the lists could not be read".to_owned()]
    );
    // With a catalogue kept, the error shows as a notice instead: the
    // app pushes it, so the view says nothing.
    let mut picker = open_over(catalogue());
    picker.store(Err("the lists could not be read".to_owned()));
    assert!(view_of(&picker).status.is_empty());
}

#[test]
fn an_answered_empty_catalogue_names_the_install() {
    let mut picker = open_over(Vec::new());
    assert_eq!(picker.take_read(), Some(crate::catalogue::Refresh::Stale));
    picker.store(Ok(Catalogue::default()));
    assert_eq!(
        view_of(&picker).status,
        ["No models. Install a provider: fiber extension install <name>.".to_owned()]
    );
}

#[test]
fn frame_row_clicks_select_without_choosing() {
    let mut models = catalogue();
    models[0].roles = vec!["review".to_owned()];
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models,
            notices: Vec::new(),
            lists: Vec::new(),
        },
        scoped: vec!["acme/m1".to_owned(), "zeta/z3".to_owned()],
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    // Layout rows: 0 the filter, 1 the buttons, 2 the `acme` heading,
    // 3 `acme/m1` with its roles cell, 4 the `zeta` heading, 5 `zeta/z3`.
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // A heading selects nothing.
    picker.select_frame_row(2);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // The filter and the buttons rows select nothing either.
    picker.select_frame_row(0);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    picker.select_frame_row(1);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // A model row is a selection stop.
    picker.select_frame_row(5);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(4));
    // The refresh button asks `Every`; the scope line toggles.
    let _ = picker.click_cell(1, 0);
    assert_eq!(picker.want, Some(crate::catalogue::Refresh::Every));
    assert!(picker.open.as_ref().is_some_and(|open| !open.show_all));
    let _ = picker.click_cell(1, 1);
    assert!(picker.open.as_ref().is_some_and(|open| open.show_all));
}

#[test]
fn the_first_chip_click_follows_the_roles_cell() {
    let mut model = entry("acme/m1", &["low", "high"], None, None);
    model.roles.push("review".to_owned());
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: vec![model],
            notices: Vec::new(),
            lists: Vec::new(),
        },
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    // The roles take cell 1, so the first chip is cell 2: choosing it
    // carries `low`, picked out.
    let choice = picker.click_cell(3, 2).expect("the first chip chooses");
    assert_eq!(choice.reference, "acme/m1");
    assert_eq!(choice.level.as_deref(), Some("low"));
    assert!(choice.level_chosen);
}

#[test]
fn each_chip_click_advances_one_cell() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: vec![entry("acme/m1", &["low", "high"], None, None)],
            notices: Vec::new(),
            lists: Vec::new(),
        },
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    // With no roles the chips start at cell 1: cell 2 is `high`.
    let choice = picker.click_cell(3, 2).expect("the second chip chooses");
    assert_eq!(choice.level.as_deref(), Some("high"));
    assert!(choice.level_chosen);
}

/// A choice of `reference` at `level`, picked out or not.
fn choice(reference: &str, level: Option<&str>, level_chosen: bool) -> Choice {
    Choice {
        reference: reference.to_owned(),
        level: level.map(str::to_owned),
        level_chosen,
        save_model: true,
        session_only: false,
    }
}

#[test]
fn saves_are_model_then_thinking_when_a_level_was_chosen() {
    assert_eq!(
        saves(&choice("acme/m1", Some("high"), true)),
        [
            ("model".to_owned(), "acme/m1".to_owned()),
            ("models.\"acme/m1\".thinking".to_owned(), "high".to_owned()),
        ]
    );
}

#[test]
fn an_untouched_chip_saves_model_only() {
    // The chip's level rides the command, but without `level_chosen`
    // only the model is saved.
    assert_eq!(
        saves(&choice("acme/m1", Some("high"), false)),
        [("model".to_owned(), "acme/m1".to_owned())]
    );
}

#[test]
fn a_chip_moved_away_and_back_still_saves_the_level() {
    // Touching marks the row: moving back onto the preselected level
    // keeps `level_chosen`, so the level is saved.
    let mut picker = open_over(catalogue());
    picker.move_chip(-1);
    picker.move_chip(1);
    let choice = picker.choice(false).expect("a choice");
    assert!(choice.level_chosen);
    assert_eq!(choice.level.as_deref(), Some("high"));
    assert_eq!(saves(&choice).len(), 2);
}

#[test]
fn a_model_without_levels_saves_model_only() {
    assert_eq!(
        saves(&choice("zeta/z1", None, false)),
        [("model".to_owned(), "zeta/z1".to_owned())]
    );
    assert_eq!(
        saves(&choice("zeta/z1", None, true)),
        [("model".to_owned(), "zeta/z1".to_owned())]
    );
}

#[test]
fn save_model_false_saves_only_the_level() {
    let choice = Choice {
        save_model: false,
        ..choice("acme/m1", Some("high"), true)
    };
    assert_eq!(
        saves(&choice),
        [("models.\"acme/m1\".thinking".to_owned(), "high".to_owned())]
    );
}

#[test]
fn session_only_saves_nothing() {
    let choice = Choice {
        session_only: true,
        ..choice("acme/m1", Some("high"), true)
    };
    assert!(saves(&choice).is_empty());
}

#[test]
fn command_args_carry_thinking_exactly_when_there_is_a_level() {
    assert_eq!(
        command_args(&choice("acme/m1", Some("high"), false)),
        serde_json::json!({"model": "acme/m1", "thinking": "high"})
    );
    assert_eq!(
        command_args(&choice("zeta/z1", None, false)),
        serde_json::json!({"model": "zeta/z1"})
    );
}

#[test]
fn start_args_carry_the_model_exactly_with_a_level_override() {
    // No `:level` suffix: the session tries the exact string first, and
    // the per-run override outranks every file.
    assert_eq!(
        start_args(&choice("acme/m1", Some("high"), true)),
        (
            "acme/m1".to_owned(),
            Some("models.\"acme/m1\".thinking=high".to_owned())
        )
    );
    assert_eq!(
        start_args(&choice("zeta/z1", None, false)),
        ("zeta/z1".to_owned(), None)
    );
}

#[test]
fn choice_is_none_while_closed_or_with_no_scoped_row() {
    let closed = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
            lists: Vec::new(),
        },
        ..ModelPicker::default()
    };
    assert!(closed.choice(false).is_none());
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
            lists: Vec::new(),
        },
        scoped: vec!["gone/x".to_owned()],
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    assert!(picker.choice(false).is_none());
}

#[test]
fn clicks_choose_at_the_row_and_chip() {
    let mut models = catalogue();
    models[0].roles = vec!["review".to_owned()];
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models,
            notices: Vec::new(),
            lists: Vec::new(),
        },
        scoped: vec!["acme/m1".to_owned(), "zeta/z3".to_owned()],
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    // Layout rows: 0 the filter, 1 the buttons, 2 the `acme` heading,
    // 3 `acme/m1` with its roles cell, 4 the `zeta` heading, 5 `zeta/z3`.
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // A heading chooses nothing.
    assert_eq!(picker.click_cell(2, 0), None);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // The filter and the buttons choose nothing either.
    assert_eq!(picker.click_cell(0, 0), None);
    assert_eq!(picker.click_cell(1, 0), None);
    // A name cell chooses its row at its chip: untouched, so only the
    // model is saved.
    let choice = picker.click_cell(5, 0).expect("a choice");
    assert_eq!(choice.reference, "zeta/z3");
    assert!(!choice.level_chosen);
    assert!(choice.save_model);
    assert!(!choice.session_only);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(4));
    // A roles cell selects its row without choosing.
    assert_eq!(picker.click_cell(3, 1), None);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    assert!(
        picker
            .open
            .as_ref()
            .is_some_and(|open| open.touched.iter().all(|touched| !touched))
    );
    // A chip chooses its row at that level: `acme/m1` declares `low`
    // then `high`, past its roles cell.
    let choice = picker.click_cell(3, 3).expect("a choice");
    assert_eq!(choice.reference, "acme/m1");
    assert_eq!(choice.level.as_deref(), Some("high"));
    assert!(choice.level_chosen);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.selected, 0);
    assert_eq!(open.chips.first().copied().flatten(), Some(1));
    assert!(open.touched.first().copied().unwrap_or(false));
    // The first cell past the last chip only selects the row.
    assert_eq!(picker.click_cell(3, 4), None);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // A row past the frame chooses nothing.
    assert_eq!(picker.click_cell(40, 0), None);
}

#[test]
fn scope_opens_over_every_model_whatever_the_scope() {
    let models = catalogue();
    for scoped in [Vec::new(), vec!["acme/m1".to_owned(), "gone/x".to_owned()]] {
        assert_eq!(
            shown_in(&models, &scoped, false, Mode::Scope, None, ""),
            [0, 1, 2, 3, 4]
        );
    }
}

#[test]
fn an_open_checklist_starts_marked_from_the_list() {
    let mut picker = ModelPicker {
        scoped: vec!["acme/m2".to_owned(), "gone/x".to_owned()],
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Scope, None);
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![false, true, false, false, false])
    );
    // Any other open keeps no marks.
    picker.open(Mode::Choose, None);
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(Vec::new())
    );
}

#[test]
fn toggle_mark_flips_only_the_selected_row() {
    let mut picker = ModelPicker {
        scoped: vec!["acme/m1".to_owned()],
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Scope, None);
    picker.move_row(1);
    picker.toggle_mark();
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, true, false, false, false])
    );
    picker.toggle_mark();
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    // With no marks kept, toggling changes nothing.
    picker.open(Mode::Choose, None);
    picker.toggle_mark();
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(Vec::new())
    );
}

#[test]
fn scope_clicks_toggle_the_mark_cell_and_select_on_any_other() {
    let mut picker = ModelPicker {
        scoped: Vec::new(),
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Scope, None);
    // Layout rows: 0 the buttons, 1 the `acme` heading, 2 `acme/m1`.
    assert_eq!(picker.click_cell(2, 0), None);
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // The name cell selects without toggling.
    assert_eq!(picker.click_cell(3, 1), None);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    // A heading toggles nothing.
    assert_eq!(picker.click_cell(1, 0), None);
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    // The checklist never chooses, on any cell.
    assert_eq!(picker.click_cell(2, 0), None);
    assert!(picker.choice(false).is_none());
    assert!(picker.choice(true).is_none());
}

#[test]
fn scoped_save_orders_marked_then_keeps_uninstalled_old_entries() {
    let models = catalogue();
    let old = vec![
        "gone/x".to_owned(),
        "zeta/z3".to_owned(),
        "acme/m2".to_owned(),
    ];
    // `zeta/z3` is installed but unmarked: it stays dropped, while the
    // uninstalled `gone/x` is kept in its old order.
    assert_eq!(
        scoped_save(&models, &[false, false, true, false, false], &old),
        vec!["zeta/z1".to_owned(), "gone/x".to_owned()]
    );
    // Marked references come in catalogue order whatever the old order.
    assert_eq!(
        scoped_save(&models, &[false, false, false, true, true], &old),
        vec![
            "zeta/z2".to_owned(),
            "zeta/z3".to_owned(),
            "gone/x".to_owned()
        ]
    );
}

#[test]
fn scoped_save_with_every_marked_keeps_the_old_order_behind() {
    let models = catalogue();
    assert_eq!(
        scoped_save(
            &models,
            &[true, true, true, true, true],
            &["gone/y".to_owned(), "gone/x".to_owned()]
        ),
        vec![
            "acme/m1".to_owned(),
            "acme/m2".to_owned(),
            "zeta/z1".to_owned(),
            "zeta/z2".to_owned(),
            "zeta/z3".to_owned(),
            "gone/y".to_owned(),
            "gone/x".to_owned()
        ]
    );
}

#[test]
fn scoped_save_with_none_marked_clears() {
    let models = catalogue();
    assert!(scoped_save(&models, &[false, false, false, false, false], &[]).is_empty());
    // Even an unavailable old entry is dropped: written as `[]`, an
    // empty list reads as every model.
    assert!(
        scoped_save(
            &models,
            &[false, false, false, false, false],
            &["gone/x".to_owned(), "acme/m1".to_owned()]
        )
        .is_empty()
    );
}

#[test]
fn a_read_keeps_checklist_marks_by_reference() {
    let mut picker = ModelPicker {
        scoped: vec!["acme/m1".to_owned(), "zeta/z9".to_owned()],
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Scope, None);
    // Unmark the installed `acme/m1`.
    picker.toggle_mark();
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![false, false, false, false, false])
    );
    // The answer drops `acme/m2` and adds `zeta/z9`: the kept rows hold
    // their marks, and the new row starts marked from the saved list.
    let mut models = catalogue();
    models.remove(1);
    models.push(entry("zeta/z9", &["low"], Some("low"), None));
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![false, false, false, false, true])
    );
}

#[test]
fn a_read_leaves_a_choose_open_without_marks() {
    let mut picker = ModelPicker {
        scoped: vec!["acme/m1".to_owned()],
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Choose, None);
    picker.store(Ok(Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(Vec::new())
    );
}

#[test]
fn a_read_through_an_empty_catalogue_marks_from_the_list_again() {
    let mut picker = ModelPicker {
        scoped: vec!["acme/m1".to_owned(), "gone/x".to_owned()],
        ..ModelPicker::default()
    };
    picker.catalogue = Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    };
    picker.open(Mode::Scope, None);
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    // A refresh that briefly empties the catalogue leaves no rows, so
    // no marks either.
    picker.store(Ok(Catalogue {
        models: Vec::new(),
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(Vec::new())
    );
    // The next answer marks every row from the saved list again, so
    // toggling and saving work.
    picker.store(Ok(Catalogue {
        models: catalogue(),
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(
        picker.open.as_ref().map(|open| open.marks.clone()),
        Some(vec![true, false, false, false, false])
    );
    picker.move_row(1);
    picker.toggle_mark();
    let marks = picker.open.as_ref().expect("open").marks.clone();
    assert_eq!(marks, vec![true, true, false, false, false]);
    assert_eq!(
        scoped_save(&picker.catalogue.models, &marks, &picker.scoped),
        vec![
            "acme/m1".to_owned(),
            "acme/m2".to_owned(),
            "gone/x".to_owned()
        ]
    );
}

#[test]
fn typing_narrows_the_shown_rows_in_catalogue_order() {
    let models = catalogue();
    let rows = shown_in(&models, &[], false, Mode::Choose, None, "z1 z2");
    assert!(rows.is_empty());
    let rows = shown_in(&models, &[], false, Mode::Choose, None, "zeta");
    assert_eq!(rows, [2, 3, 4]);
    let rows = shown_in(&models, &[], false, Mode::Choose, None, "m1");
    assert_eq!(rows, [0]);
}

#[test]
fn typing_keeps_the_selection_on_a_shown_model() {
    let mut picker = open_over(catalogue());
    picker.move_row(1);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
    // `acme/m2` matches "m2": the selection stays on its model.
    picker.push_query('m');
    picker.push_query('2');
    assert_eq!(picker.query(), "m2");
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
}

#[test]
fn typing_moves_the_selection_to_the_first_shown_row() {
    let mut picker = open_over(catalogue());
    picker.move_row(1);
    // `acme/m2` hides under "z": the selection moves to `zeta/z1`.
    picker.push_query('z');
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(2));
}

#[test]
fn typing_with_no_match_keeps_the_hidden_selection() {
    let mut picker = open_over(catalogue());
    picker.push_query('q');
    picker.push_query('q');
    picker.push_query('q');
    let rows = shown_in(
        &picker.catalogue.models,
        &picker.scoped,
        false,
        Mode::Choose,
        None,
        picker.query(),
    );
    assert!(rows.is_empty());
    // With no row shown the selection stays where it was, hidden.
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    // Clearing the query shows every row again, on the same selection.
    picker.clear_query();
    assert_eq!(picker.query(), "");
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
}

#[test]
fn typing_keeps_chips_and_touched() {
    let mut picker = open_over(catalogue());
    picker.move_chip(-1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.first().copied().flatten(), Some(0));
    assert!(open.touched.first().copied().unwrap_or(false));
    // A level chosen with the arrows survives typing.
    picker.push_query('m');
    picker.push_query('1');
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips.first().copied().flatten(), Some(0));
    assert!(open.touched.first().copied().unwrap_or(false));
}

#[test]
fn backspace_on_an_empty_query_changes_nothing() {
    let mut picker = open_over(catalogue());
    picker.pop_query();
    assert_eq!(picker.query(), "");
    assert!(picker.is_open());
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(0));
    picker.clear_query();
    assert_eq!(picker.query(), "");
    assert!(picker.is_open());
}

#[test]
fn backspace_drops_the_last_letter_and_widens_the_filter() {
    let mut picker = open_over(catalogue());
    picker.push_query('m');
    picker.push_query('2');
    let narrow = shown_in(
        &picker.catalogue.models,
        &picker.scoped,
        false,
        Mode::Choose,
        None,
        picker.query(),
    );
    assert_eq!(narrow, [1]);
    picker.pop_query();
    assert_eq!(picker.query(), "m");
    // `acme/m1` shows again; the selection stays on `acme/m2`.
    let wide = shown_in(
        &picker.catalogue.models,
        &picker.scoped,
        false,
        Mode::Choose,
        None,
        picker.query(),
    );
    assert_eq!(wide, [0, 1]);
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
}

#[test]
fn with_no_row_shown_choice_is_none_and_chips_do_not_move() {
    let mut picker = open_over(catalogue());
    picker.push_query('q');
    picker.push_query('q');
    picker.push_query('q');
    assert_eq!(picker.choice(false), None);
    assert_eq!(picker.choice(true), None);
    let before = picker.open.as_ref().expect("open").chips.clone();
    picker.move_chip(-1);
    picker.move_chip(1);
    assert_eq!(picker.open.as_ref().expect("open").chips, before);
}

#[test]
fn the_scope_toggle_keeps_the_query_and_refilters() {
    let mut picker = open_over(catalogue());
    picker.scoped = vec!["acme/m1".to_owned(), "acme/m2".to_owned()];
    picker.push_query('z');
    assert_eq!(picker.query(), "z");
    // Under the scope nothing matches: showing all refilters the same
    // query onto every installed model.
    picker.toggle_show_all();
    assert_eq!(picker.query(), "z");
    let open = picker.open.as_ref().unwrap();
    assert!(open.show_all);
    assert_eq!(open.selected, 2);
    picker.toggle_show_all();
    assert_eq!(picker.query(), "z");
}

#[test]
fn a_replacement_with_a_query_keeps_the_selection_on_the_shown_rows() {
    let mut picker = open_over(catalogue());
    picker.move_row(1);
    picker.push_query('a');
    assert_eq!(picker.open.as_ref().map(|open| open.selected), Some(1));
    // The answer removes `acme/m2`: the old index clamps onto `zeta/z1`,
    // which matches "a" and shows, so it stays selected.
    let mut models = catalogue();
    models.remove(1);
    picker.store(Ok(Catalogue {
        models,
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    let open = picker.open.as_ref().unwrap();
    assert_eq!(picker.catalogue.models[open.selected].reference, "zeta/z1");
    assert_eq!(picker.query(), "a");
}

#[test]
fn thinking_drops_its_target_row_on_a_non_matching_query() {
    let models = catalogue();
    let target = Some(("acme/m1".to_owned(), None));
    let rows = shown_in(&models, &[], false, Mode::Thinking, target.as_ref(), "");
    assert!(rows.contains(&0));
    let rows = shown_in(&models, &[], false, Mode::Thinking, target.as_ref(), "zeta");
    assert!(!rows.contains(&0));
    assert_eq!(rows, [2, 3, 4]);
}

#[test]
fn the_checklist_ignores_the_query() {
    let models = catalogue();
    assert_eq!(
        shown_in(&models, &[], false, Mode::Scope, None, "zzz"),
        [0, 1, 2, 3, 4]
    );
}

/// An open picker over `catalogue()` with `lists`, for the view tests.
fn open_listed(lists: Vec<crate::catalogue::ListAge>) -> ModelPicker {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: catalogue(),
            notices: Vec::new(),
            lists,
        },
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    picker
}

#[test]
fn the_view_lists_every_provider_with_its_models() {
    let picker = open_over(catalogue());
    let view = view_of(&picker);
    assert_eq!(view.filter, "");
    assert_eq!(view.count, "5 models");
    assert_eq!(view.toggle, None);
    assert_eq!(view.buttons_at, 1);
    assert!(!view.no_match);
    assert!(view.status.is_empty());
    let providers: Vec<&str> = view
        .sections
        .iter()
        .map(|section| section.provider.as_str())
        .collect();
    assert_eq!(providers, ["acme", "zeta"]);
    // No list times read: every section is unknown.
    assert!(
        view.sections
            .iter()
            .all(|section| matches!(section.state, Fresh::Unknown))
    );
    let ids: Vec<&str> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter().map(|model| model.id.as_str()))
        .collect();
    assert_eq!(ids, ["m1", "m2", "z1", "z2", "z3"]);
    // The selection opens on the first row, whose layout index is 3:
    // the filter is 0, the buttons 1, the heading 2.
    assert_eq!(view.focused, Some(3));
    assert_eq!(view.sections[0].models[0].at, 3);
}

#[test]
fn the_view_marks_chips_saved_levels_and_the_current_model() {
    let mut picker = open_over(catalogue());
    picker.open(Mode::Choose, Some(("acme/m2", Some("high"))));
    let laid = picker
        .view(&ctx(Some(84_000), 0, Some("acme/m2")))
        .expect("open");
    let m2 = &laid.sections[0].models[1];
    // `acme/m2` configures `high` (index 1) and the open preselects it.
    assert_eq!(m2.saved, Some(1));
    assert_eq!(m2.chip, Some(1));
    assert!(m2.current);
    // The current model shows "—", whatever its price.
    assert_eq!(m2.cost.as_deref(), Some("—"));
    // `acme/m1` configures nothing but declares `high` (index 1).
    let m1 = &laid.sections[0].models[0];
    assert_eq!(m1.saved, None);
    assert_eq!(m1.chip, Some(1));
    assert!(!m1.current);
    // No price names no rebuild money: the size alone.
    assert_eq!(m1.cost.as_deref(), Some("~84k tokens"));
    // With no prompt size the cost column stays out everywhere.
    let plain = view_of(&picker);
    assert!(
        plain
            .sections
            .iter()
            .flat_map(|section| section.models.iter())
            .all(|model| model.cost.is_none())
    );
}

#[test]
fn the_view_counts_the_scope_and_marks_its_models_when_shown_all() {
    let mut picker = open_over(catalogue());
    picker.scoped = vec!["acme/m1".to_owned(), "zeta/z3".to_owned()];
    picker.open(Mode::Choose, None);
    let scoped = view_of(&picker);
    assert_eq!(scoped.count, "scoped · 2 of 5");
    assert_eq!(scoped.toggle.as_deref(), Some("[show all]"));
    assert!(
        scoped
            .sections
            .iter()
            .flat_map(|section| section.models.iter())
            .all(|model| !model.scoped)
    );
    picker.toggle_show_all();
    let all = view_of(&picker);
    assert_eq!(all.count, "5 models");
    assert_eq!(all.toggle.as_deref(), Some("[show scoped]"));
    let scoped_ids: Vec<&str> = all
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .filter(|model| model.scoped)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(scoped_ids, ["m1", "z3"]);
}

#[test]
fn the_checklist_view_marks_rows_and_keeps_its_footer() {
    let mut picker = open_over(catalogue());
    picker.open(Mode::Scope, None);
    let view = view_of(&picker);
    // Every installed model shows, with no filter, count or toggle.
    assert_eq!(view.count, "5 models");
    assert_eq!(view.toggle, None);
    assert_eq!(view.buttons_at, 0);
    assert_eq!(
        view.footer,
        [("Space", "mark"), ("Enter", "save"), ("Esc", "back")]
    );
    let marks: Vec<bool> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .map(|model| model.mark.unwrap_or(false))
        .collect();
    assert_eq!(marks, [false; 5]);
    picker.toggle_mark();
    let view = view_of(&picker);
    assert!(
        view.sections[0].models[0].mark.unwrap_or(false),
        "the first row marks"
    );
}

#[test]
fn the_view_shows_stale_lists_refreshing() {
    let mut picker = open_listed(vec![
        crate::catalogue::ListAge {
            provider: "acme".to_owned(),
            updated_ms: Some(1_000),
            stale: true,
        },
        crate::catalogue::ListAge {
            provider: "zeta".to_owned(),
            updated_ms: Some(1_000),
            stale: false,
        },
    ]);
    picker.take_read();
    let view = picker.view(&ctx(None, 61_000, None)).expect("open");
    assert!(matches!(view.sections[0].state, Fresh::Refreshing));
    // A fresh list still shows its age: 60 seconds is "1m".
    assert!(matches!(view.sections[1].state, Fresh::Updated(ref age) if age == "1m"));
    // `Every` refreshes every list, whatever the cache holds.
    picker.refreshing = Some(crate::catalogue::Refresh::Every);
    let view = view_of(&picker);
    assert!(
        view.sections
            .iter()
            .all(|section| matches!(section.state, Fresh::Refreshing))
    );
}

#[test]
fn the_view_marks_the_session_only_choice() {
    use contract::SessionId;
    let mut picker = open_over(catalogue());
    let session = SessionId("s_1".to_owned());
    picker.session_only = Some((session, "zeta/z1".to_owned()));
    let view = view_of(&picker);
    let only: Vec<&str> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .filter(|model| model.session_only)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(only, ["z1"]);
}

#[test]
fn the_view_filters_ids_and_marks_the_hits() {
    let mut picker = open_over(catalogue());
    picker.push_query('m');
    picker.push_query('2');
    let view = view_of(&picker);
    assert_eq!(view.filter, "m2");
    assert_eq!(view.count, "1 of 5 models");
    assert!(!view.no_match);
    let ids: Vec<&str> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter().map(|model| model.id.as_str()))
        .collect();
    assert_eq!(ids, ["m2"]);
    assert_eq!(view.sections[0].models[0].hits, [true, true]);
}

#[test]
fn the_view_with_no_match_holds_one_line_and_no_focus() {
    let mut picker = open_over(catalogue());
    picker.push_query('z');
    picker.push_query('z');
    picker.push_query('z');
    let view = view_of(&picker);
    assert!(view.no_match);
    assert!(view.sections.is_empty());
    assert_eq!(view.focused, None);
    assert_eq!(view.count, "0 of 5 models");
}

#[test]
fn rebuild_cost_rounds_sizes_and_picks_tiers() {
    // Under 1000 tokens the size stays whole; at 1000 it kilos.
    assert_eq!(rebuild_cost(999, None), "~999 tokens");
    assert_eq!(rebuild_cost(1000, None), "~1k tokens");
    // Half up: 1499 rounds down, 1500 rounds up.
    assert_eq!(rebuild_cost(1499, None), "~1k tokens");
    assert_eq!(rebuild_cost(1500, None), "~2k tokens");
    let price = crate::catalogue::Price {
        micros_per_mtok: 2_000_000,
        tiers: vec![(100_000, 3_000_000)],
    };
    // At and below the tier's edge the base price holds; above it the
    // tier's does. 80k tokens at $3.75 is $0.30.
    assert_eq!(
        rebuild_cost(
            80_000,
            Some(&crate::catalogue::Price {
                micros_per_mtok: 3_750_000,
                tiers: Vec::new(),
            })
        ),
        "~80k tokens · $0.30"
    );
    assert!(rebuild_cost(99_999, Some(&price)).ends_with("· $0.20"));
    assert!(rebuild_cost(100_000, Some(&price)).ends_with("· $0.20"));
    assert!(rebuild_cost(100_001, Some(&price)).ends_with("· $0.30"));
}

#[test]
fn age_says_seconds_minutes_hours_then_days() {
    assert_eq!(age(0), "0s");
    assert_eq!(age(999), "0s");
    assert_eq!(age(59_000), "59s");
    assert_eq!(age(60_000), "1m");
    assert_eq!(age(59 * 60_000), "59m");
    assert_eq!(age(60 * 60_000), "1h");
    assert_eq!(age(23 * 60 * 60_000), "23h");
    assert_eq!(age(24 * 60 * 60_000), "1d");
    assert_eq!(age(9 * 24 * 60 * 60_000), "9d");
}
