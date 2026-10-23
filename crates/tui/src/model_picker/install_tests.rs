//! Tests for the picker's install state, the rows it lists and the focus
//! over models and rows (`docs/tui.md`, "Swapped views").

use std::sync::Arc;

use contract::ErrorCode;

use super::{InstallStart, Installs, Landed, Progress, Stop};
use crate::catalogue::{Catalogue, Refresh};
use crate::configure::{Configure, ConfigureError, LoginNeed, ProviderInstall};
use crate::configure_fake::Fake;
use crate::keys::Key;
use crate::model_picker::fixtures::{catalogue, entry};
use crate::model_picker::{Mode, ModelPicker, PickerCtx, PickerView, RowAt};

/// An open picker over `models` with `installable` not installed.
fn picker(mode: Mode, models: &[&str], installable: &[&str]) -> ModelPicker {
    let mut picker = ModelPicker {
        catalogue: catalogue(models, installable),
        ..ModelPicker::default()
    };
    picker.open(mode, None);
    picker
}

fn start(provider: &str) -> InstallStart {
    InstallStart {
        provider: provider.to_owned(),
        workspace: "/w".into(),
        seam: Arc::new(Fake::new(Vec::new())) as Arc<dyn Configure>,
    }
}

fn done(summary: &[&str], login: Vec<LoginNeed>) -> Result<ProviderInstall, ConfigureError> {
    Ok(ProviderInstall {
        installed: Vec::new(),
        summary: summary.iter().map(|line| (*line).to_owned()).collect(),
        login,
    })
}

fn failed(message: &str) -> Result<ProviderInstall, ConfigureError> {
    Err(ConfigureError {
        code: ErrorCode::IoFailed,
        message: message.to_owned(),
    })
}

fn missing(provider: &str) -> LoginNeed {
    LoginNeed {
        provider: provider.to_owned(),
        error: None,
    }
}

fn view_of(picker: &ModelPicker) -> PickerView {
    picker
        .view(&PickerCtx {
            usage: None,
            wall_ms: 0,
            spinner: "⠋",
            current: None,
            home_only: None,
        })
        .expect("the picker is open")
}

fn row_of(picker: &ModelPicker, row: &RowAt) -> usize {
    picker
        .layout()
        .unwrap()
        .iter()
        .position(|at| at == row)
        .unwrap_or_else(|| panic!("no layout row {row:?}"))
}

fn install_row(name: &str) -> RowAt {
    RowAt::Install(name.to_owned())
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn start_marks_the_provider_installing_and_queues_one_job() {
    let mut installs = Installs::default();
    assert!(!installs.busy());
    assert!(installs.start(start("openai")));
    assert!(installs.busy());
    assert_eq!(installs.progress("openai"), Some(&Progress::Installing));
    let job = installs.take_start().expect("a job");
    assert_eq!(job.provider, "openai");
    assert_eq!(job.workspace, std::path::PathBuf::from("/w"));
    assert!(installs.take_start().is_none());
}

#[test]
fn a_second_start_while_one_installs_changes_nothing() {
    let mut installs = Installs::default();
    assert!(installs.start(start("openai")));
    assert!(!installs.start(start("zeta")));
    assert_eq!(installs.progress("zeta"), None);
    assert_eq!(
        installs.take_start().map(|job| job.provider).as_deref(),
        Some("openai")
    );
    assert!(installs.take_start().is_none());
}

#[test]
fn reset_keeps_a_running_install_and_drops_the_rest() {
    let mut installs = Installs::default();
    assert!(installs.start(start("zeta")));
    assert!(matches!(
        installs.land("zeta".to_owned(), done(&["Install zeta"], Vec::new()), true),
        Landed::Installed { .. }
    ));
    installs.fail("beta", "no network".to_owned());
    assert!(installs.start(start("openai")));
    installs.focus = Some("openai".to_owned());
    assert_eq!(installs.outcomes().len(), 1);

    installs.reset();

    assert_eq!(installs.progress("openai"), Some(&Progress::Installing));
    assert_eq!(installs.progress("beta"), None);
    assert!(installs.outcomes().is_empty());
    assert_eq!(installs.focus, None);
    assert!(installs.busy());
}

#[test]
fn a_result_for_a_provider_not_installing_is_ignored() {
    let mut installs = Installs::default();
    assert_eq!(
        installs.land("openai".to_owned(), done(&[], Vec::new()), true),
        Landed::Nothing
    );
    assert!(installs.outcomes().is_empty());

    assert!(installs.start(start("openai")));
    assert!(matches!(
        installs.land("openai".to_owned(), done(&[], Vec::new()), true),
        Landed::Installed { .. }
    ));
    assert_eq!(
        installs.land("openai".to_owned(), done(&[], Vec::new()), true),
        Landed::Nothing
    );
    assert_eq!(installs.outcomes().len(), 1);

    // A failed row is not a running install either.
    installs.fail("zeta", "boom".to_owned());
    assert_eq!(
        installs.land("zeta".to_owned(), done(&[], Vec::new()), true),
        Landed::Nothing
    );
    assert_eq!(
        installs.progress("zeta"),
        Some(&Progress::Failed("boom".to_owned()))
    );
}

#[test]
fn a_visible_success_stores_one_outcome_and_clears_the_progress() {
    let mut installs = Installs::default();
    assert!(installs.start(start("openai")));
    let landed = installs.land(
        "openai".to_owned(),
        done(&["Install openai", "Version 1.0"], vec![missing("openai")]),
        true,
    );
    assert_eq!(
        landed,
        Landed::Installed {
            notices: Vec::new()
        }
    );
    assert!(!installs.busy());
    assert_eq!(installs.progress("openai"), None);
    assert_eq!(installs.outcomes().len(), 1);
    let outcome = installs.outcomes().first().unwrap();
    assert_eq!(outcome.provider, "openai");
    assert_eq!(outcome.summary, names(&["Install openai", "Version 1.0"]));
    assert_eq!(outcome.login, vec![missing("openai")]);
}

#[test]
fn a_hidden_success_stores_no_outcome_and_returns_the_credential_notices() {
    let mut installs = Installs::default();
    assert!(installs.start(start("openai")));
    let landed = installs.land(
        "openai".to_owned(),
        done(
            &["Install openai"],
            vec![
                missing("openai"),
                LoginNeed {
                    provider: "zeta".to_owned(),
                    error: Some("no keyring".to_owned()),
                },
            ],
        ),
        false,
    );
    // `docs/tui.md` promises each notice, not their order: compare sorted.
    let Landed::Installed { mut notices } = landed else {
        panic!("a success lands as Installed: {landed:?}");
    };
    notices.sort();
    assert_eq!(
        notices,
        [
            (
                "credential_failed",
                "zeta's credential could not be read: no keyring".to_owned()
            ),
            (
                "credential_missing",
                "openai needs a credential: run `fiber login openai`".to_owned()
            ),
        ]
    );
    assert!(installs.outcomes().is_empty());
    assert!(!installs.busy());

    assert!(installs.start(start("zeta")));
    assert_eq!(
        installs.land("zeta".to_owned(), done(&[], Vec::new()), false),
        Landed::Installed {
            notices: Vec::new()
        }
    );
}

#[test]
fn a_visible_failure_keeps_the_row_with_its_error_and_a_retry_starts() {
    let mut installs = Installs::default();
    assert!(installs.start(start("openai")));
    let _ = installs.take_start();
    assert_eq!(
        installs.land("openai".to_owned(), failed("no network"), true),
        Landed::Nothing
    );
    assert_eq!(
        installs.progress("openai"),
        Some(&Progress::Failed("no network".to_owned()))
    );
    assert!(!installs.busy());
    assert!(installs.outcomes().is_empty());

    assert!(installs.start(start("openai")));
    assert_eq!(installs.progress("openai"), Some(&Progress::Installing));
    assert!(matches!(
        installs.land("openai".to_owned(), done(&[], Vec::new()), true),
        Landed::Installed { .. }
    ));
    assert_eq!(installs.progress("openai"), None);
    assert_eq!(installs.outcomes().len(), 1);
}

#[test]
fn a_hidden_failure_is_a_notice_and_clears_the_progress() {
    let mut installs = Installs::default();
    assert!(installs.start(start("openai")));
    let landed = installs.land(
        "openai".to_owned(),
        failed("no network\nsecond line"),
        false,
    );
    assert_eq!(
        landed,
        Landed::Notice("Installing openai failed: no network".to_owned())
    );
    assert_eq!(installs.progress("openai"), None);
    assert!(!installs.busy());
    assert!(installs.start(start("openai")));
}

#[test]
fn fail_stores_a_failure_without_a_job() {
    let mut installs = Installs::default();
    installs.fail("openai", "The hub is not connected.".to_owned());
    assert_eq!(
        installs.progress("openai"),
        Some(&Progress::Failed("The hub is not connected.".to_owned()))
    );
    assert!(!installs.busy());
    assert!(installs.take_start().is_none());
}

#[test]
fn text_from_an_install_is_stored_without_control_characters() {
    let mut installs = Installs::default();
    assert!(installs.start(start("op\u{1b}enai")));
    let landed = installs.land(
        "op\u{1b}enai".to_owned(),
        done(
            &["Install a\u{1b}[31m"],
            vec![LoginNeed {
                provider: "o\u{7}p".to_owned(),
                error: Some("bad\u{1b}[0m".to_owned()),
            }],
        ),
        true,
    );
    assert!(matches!(landed, Landed::Installed { .. }));
    let outcome = installs.outcomes().first().unwrap();
    assert_eq!(outcome.provider, "op enai");
    assert_eq!(outcome.summary, names(&["Install a [31m"]));
    let need = outcome.login.first().unwrap();
    assert_eq!(need.provider, "o p");
    assert_eq!(need.error.as_deref(), Some("bad [0m"));

    installs.fail("zeta", "bad\u{1b}[31m\nsecond".to_owned());
    assert_eq!(
        installs.progress("zeta"),
        Some(&Progress::Failed("bad [31m".to_owned()))
    );

    assert!(installs.start(start("a\u{1b}b")));
    assert_eq!(
        installs.land("a\u{1b}b".to_owned(), failed("x\u{1b}y"), false),
        Landed::Notice("Installing a b failed: x y".to_owned())
    );
}

#[test]
fn rows_list_in_the_choosing_picker_only() {
    let all = names(&["openai", "zeta"]);
    assert_eq!(
        picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]).shown_installs(),
        all
    );
    assert!(
        picker(Mode::Thinking, &["acme/m1"], &["openai", "zeta"])
            .shown_installs()
            .is_empty()
    );
    assert!(
        picker(Mode::Scope, &["acme/m1"], &["openai", "zeta"])
            .shown_installs()
            .is_empty()
    );
}

#[test]
fn a_query_filters_the_rows_by_provider_name() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    picker.push_query('z');
    picker.push_query('t');
    assert_eq!(picker.shown_installs(), names(&["zeta"]));
    picker.clear_query();
    for c in "zzz".chars() {
        picker.push_query(c);
    }
    assert!(picker.shown_installs().is_empty());
}

#[test]
fn a_landed_provider_leaves_the_rows_at_once_by_its_cleaned_name() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    assert!(picker.installs.start(start("openai")));
    assert!(matches!(
        picker.land_install("openai".to_owned(), done(&[], Vec::new())),
        Landed::Installed { .. }
    ));
    assert_eq!(picker.shown_installs(), names(&["zeta"]));
    assert_eq!(picker.catalogue.installable, names(&["zeta"]));
    assert_eq!(picker.want, Some(Refresh::Stale));

    let mut picker = self::picker(Mode::Choose, &["acme/m1"], &["op\u{1b}enai"]);
    assert!(picker.installs.start(start("op\u{1b}enai")));
    let _ = picker.land_install("op\u{1b}enai".to_owned(), done(&[], Vec::new()));
    assert!(picker.shown_installs().is_empty());
}

#[test]
fn a_failed_result_leaves_the_row_listed_and_asks_nothing() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai"]);
    picker.want = None;
    assert!(picker.installs.start(start("openai")));
    assert_eq!(
        picker.land_install("openai".to_owned(), failed("boom")),
        Landed::Nothing
    );
    assert_eq!(picker.shown_installs(), names(&["openai"]));
    assert_eq!(picker.want, None);
    // A result for a provider not installing changes nothing either.
    assert_eq!(
        picker.land_install("openai".to_owned(), done(&[], Vec::new())),
        Landed::Nothing
    );
    assert_eq!(picker.shown_installs(), names(&["openai"]));
    assert_eq!(picker.want, None);
}

#[test]
fn a_success_while_thinking_is_hidden_and_still_drops_the_row_and_asks() {
    let mut picker = picker(Mode::Thinking, &["acme/m1"], &["openai"]);
    picker.want = None;
    assert!(picker.installs.start(start("openai")));
    let landed = picker.land_install("openai".to_owned(), done(&[], vec![missing("openai")]));
    assert!(matches!(landed, Landed::Installed { ref notices } if notices.len() == 1));
    assert!(picker.installs.outcomes().is_empty());
    assert!(picker.catalogue.installable.is_empty());
    assert_eq!(picker.want, Some(Refresh::Stale));
}

#[test]
fn the_walk_crosses_from_the_last_model_to_the_rows_and_clamps() {
    let mut picker = picker(Mode::Choose, &["acme/m1", "acme/m2"], &["openai", "zeta"]);
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
    let walk = [
        (1, Stop::Model(1)),
        (1, Stop::Install("openai".to_owned())),
        (1, Stop::Install("zeta".to_owned())),
        (1, Stop::Install("zeta".to_owned())),
        (-1, Stop::Install("openai".to_owned())),
        (-1, Stop::Model(1)),
        (-1, Stop::Model(0)),
        (-1, Stop::Model(0)),
    ];
    for (delta, expected) in walk {
        picker.move_row(delta);
        assert_eq!(picker.focus(), Some(expected), "after {delta}");
    }
}

#[test]
fn a_page_walks_the_same_stops() {
    let mut picker = picker(Mode::Choose, &["acme/m1", "acme/m2"], &["openai", "zeta"]);
    picker.move_page(&Key::PageDown, 3);
    assert_eq!(picker.focus(), Some(Stop::Install("openai".to_owned())));
    picker.move_page(&Key::PageDown, 3);
    assert_eq!(picker.focus(), Some(Stop::Install("zeta".to_owned())));
    picker.move_page(&Key::PageUp, 3);
    assert_eq!(picker.focus(), Some(Stop::Model(1)));
    picker.move_page(&Key::Char('x'), 3);
    assert_eq!(picker.focus(), Some(Stop::Model(1)));
}

#[test]
fn with_no_model_nothing_is_focused_until_down() {
    let mut picker = picker(Mode::Choose, &[], &["openai", "zeta"]);
    assert_eq!(picker.focus(), None);
    assert_eq!(picker.install_target(), None);
    assert_eq!(view_of(&picker).focused, None);
    picker.move_row(-1);
    assert_eq!(picker.focus(), None);
    picker.move_row(1);
    assert_eq!(picker.focus(), Some(Stop::Install("openai".to_owned())));
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    let at = row_of(&picker, &install_row("openai"));
    assert_eq!(view_of(&picker).focused, Some(at));
}

#[test]
fn a_query_that_hides_every_model_focuses_nothing_until_down() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["zeta"]);
    picker.push_query('z');
    assert_eq!(picker.focus(), None);
    assert_eq!(picker.install_target(), None);
    assert!(picker.choice(false).is_none());
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("zeta"));
}

#[test]
fn a_query_that_some_model_matches_keeps_the_model_focused() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["zeta"]);
    picker.push_query('m');
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
    assert_eq!(picker.install_target(), None);
}

#[test]
fn a_query_never_moves_the_focus_onto_a_row_and_a_hidden_row_falls_back() {
    let mut picker = picker(Mode::Choose, &["acme/zz"], &["openai"]);
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    picker.push_query('z');
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
    picker.clear_query();
    assert_eq!(
        picker.focus(),
        Some(Stop::Model(0)),
        "the row does not take the focus back"
    );

    let mut picker = self::picker(Mode::Choose, &["acme/m1"], &["openai"]);
    picker.move_row(1);
    picker.push_query('z');
    assert_eq!(picker.focus(), None);
    picker.pop_query();
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
}

#[test]
fn clicking_a_model_clears_the_row_focus() {
    let mut picker = picker(Mode::Choose, &["acme/m1", "acme/m2"], &["openai"]);
    picker.move_row(1);
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    let model = row_of(&picker, &RowAt::Model(0));
    assert!(picker.click_cell(model, 5).is_none());
    assert_eq!(picker.install_target(), None);
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
}

#[test]
fn selecting_a_frame_row_focuses_a_row_and_a_model_clears_it() {
    let mut picker = picker(Mode::Choose, &["acme/m1", "acme/m2"], &["openai"]);
    let row = row_of(&picker, &install_row("openai"));
    picker.select_frame_row(row);
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    let model = row_of(&picker, &RowAt::Model(1));
    picker.select_frame_row(model);
    assert_eq!(picker.install_target(), None);
    assert_eq!(picker.focus(), Some(Stop::Model(1)));
    // The heading and the buttons are not stops.
    let heading = row_of(&picker, &RowAt::Heading("acme".to_owned()));
    picker.select_frame_row(heading);
    assert_eq!(picker.focus(), Some(Stop::Model(1)));
}

#[test]
fn an_install_click_answers_the_name_cell_and_focuses_the_row_either_way() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    let openai = row_of(&picker, &install_row("openai"));
    let zeta = row_of(&picker, &install_row("zeta"));
    assert_eq!(picker.install_click(zeta, 1), None);
    assert_eq!(picker.focus(), Some(Stop::Install("zeta".to_owned())));
    assert_eq!(picker.install_click(openai, 0).as_deref(), Some("openai"));
    assert_eq!(picker.focus(), Some(Stop::Install("openai".to_owned())));
    let model = row_of(&picker, &RowAt::Model(0));
    assert_eq!(picker.install_click(model, 0), None);
    assert_eq!(
        picker.focus(),
        Some(Stop::Install("openai".to_owned())),
        "a model row leaves the focus"
    );
    assert_eq!(picker.install_click(0, 0), None);
}

#[test]
fn a_focused_row_takes_no_choice_and_no_chip() {
    let mut picker = ModelPicker {
        catalogue: Catalogue {
            models: vec![entry("acme/m1", &["low", "high"])],
            installable: names(&["openai"]),
            ..Catalogue::default()
        },
        ..ModelPicker::default()
    };
    picker.open(Mode::Choose, None);
    assert!(picker.choice(false).is_some());
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    assert!(picker.choice(false).is_none());
    assert!(picker.choice(true).is_none());
    let chips = picker.open.as_ref().unwrap().chips.clone();
    picker.move_chip(1);
    picker.move_chip(-1);
    let open = picker.open.as_ref().unwrap();
    assert_eq!(open.chips, chips);
    assert_eq!(open.touched, [false]);
}

#[test]
fn the_focus_falls_back_after_its_row_lands_or_a_read_drops_it() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("openai"));
    assert!(picker.installs.start(start("openai")));
    let _ = picker.land_install("openai".to_owned(), done(&[], Vec::new()));
    assert_eq!(picker.focus(), Some(Stop::Model(0)), "never onto zeta");

    let mut picker = self::picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    picker.move_row(1);
    picker.move_row(1);
    assert_eq!(picker.install_target().as_deref(), Some("zeta"));
    picker.store(Ok(Catalogue {
        models: vec![entry("acme/m1", &[])],
        installable: names(&["openai", "zeta"]),
        ..Catalogue::default()
    }));
    assert_eq!(
        picker.install_target().as_deref(),
        Some("zeta"),
        "a read that keeps the row keeps the focus"
    );
    picker.store(Ok(Catalogue {
        models: vec![entry("acme/m1", &[])],
        installable: names(&["openai"]),
        ..Catalogue::default()
    }));
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
    picker.store(Ok(Catalogue {
        models: vec![entry("acme/m1", &[])],
        installable: names(&["openai", "zeta"]),
        ..Catalogue::default()
    }));
    assert_eq!(
        picker.focus(),
        Some(Stop::Model(0)),
        "a row that comes back is not focused"
    );
}

#[test]
fn the_layout_lists_the_rows_after_the_last_model() {
    let picker = picker(Mode::Choose, &["acme/m1", "zeta/z1"], &["openai", "zeta"]);
    assert_eq!(
        picker.layout().unwrap(),
        [
            RowAt::Filter,
            RowAt::Buttons,
            RowAt::Heading("acme".to_owned()),
            RowAt::Model(0),
            RowAt::Heading("zeta".to_owned()),
            RowAt::Model(1),
            install_row("openai"),
            install_row("zeta"),
        ]
    );
    let thinking = self::picker(Mode::Thinking, &["acme/m1"], &["openai"]);
    assert_eq!(
        thinking.layout().unwrap(),
        [
            RowAt::Filter,
            RowAt::Buttons,
            RowAt::Heading("acme".to_owned()),
            RowAt::Model(0)
        ]
    );
}

#[test]
fn the_view_carries_each_row_with_its_hits_and_progress() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    assert!(picker.installs.start(start("zeta")));
    let view = view_of(&picker);
    assert_eq!(view.installs.len(), 2);
    let first = view.installs.first().unwrap();
    assert_eq!(first.provider, "openai");
    assert_eq!(first.at, row_of(&picker, &install_row("openai")));
    assert_eq!(first.progress, None);
    assert_eq!(first.hits, vec![false; 6]);
    let second = view.installs.get(1).unwrap();
    assert_eq!(second.progress, Some(Progress::Installing));
    assert_eq!(view.count, "1 models");

    picker.push_query('z');
    let view = view_of(&picker);
    assert_eq!(view.installs.len(), 1);
    let only = view.installs.first().unwrap();
    assert_eq!(only.provider, "zeta");
    assert_eq!(only.hits, [true, false, false, false]);
    assert_eq!(view.count, "0 of 1 models");
}

#[test]
fn the_rows_drawn_are_the_catalogue_list_filtered_and_never_scripted() {
    let picker = picker(Mode::Choose, &["acme/m1"], &["anthropic", "openai", "zeta"]);
    let drawn: Vec<String> = view_of(&picker)
        .installs
        .into_iter()
        .map(|row| row.provider)
        .collect();
    assert_eq!(drawn, picker.catalogue.installable);
    assert!(!drawn.iter().any(|name| name == "scripted"));
}

#[test]
fn no_match_reads_the_rows_too() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["zeta"]);
    picker.push_query('z');
    let view = view_of(&picker);
    assert!(!view.no_match);
    assert!(!picker.layout().unwrap().contains(&RowAt::NoMatch));
    picker.push_query('z');
    picker.push_query('z');
    let view = view_of(&picker);
    assert!(view.no_match);
    assert!(view.installs.is_empty());
    assert!(picker.layout().unwrap().contains(&RowAt::NoMatch));
}

#[test]
fn an_install_never_widens_the_scope() {
    let after = Catalogue {
        models: vec![entry("acme/other", &[]), entry("openai/gpt", &[])],
        ..Catalogue::default()
    };
    let shown = |picker: &ModelPicker| -> Vec<String> {
        view_of(picker)
            .sections
            .iter()
            .flat_map(|section| section.models.iter().map(|model| model.id.clone()))
            .collect()
    };
    for mode in [Mode::Choose, Mode::Thinking] {
        let mut picker = picker(mode, &["acme/other"], &["openai"]);
        picker.scoped = names(&["acme/other"]);
        assert!(picker.installs.start(start("openai")));
        let _ = picker.land_install("openai".to_owned(), done(&[], Vec::new()));
        picker.store(Ok(after.clone()));
        assert_eq!(
            shown(&picker),
            names(&["other"]),
            "{mode:?}: hidden by the scope"
        );
        assert_eq!(picker.scoped, names(&["acme/other"]));
        picker.toggle_show_all();
        assert_eq!(
            shown(&picker),
            names(&["other", "gpt"]),
            "{mode:?}: after Tab"
        );
        assert_eq!(picker.scoped, names(&["acme/other"]));
    }
    let mut picker = picker(Mode::Choose, &["acme/other"], &["openai"]);
    assert!(picker.installs.start(start("openai")));
    let _ = picker.land_install("openai".to_owned(), done(&[], Vec::new()));
    picker.store(Ok(after));
    assert_eq!(shown(&picker), names(&["other", "gpt"]), "unscoped");
}

#[test]
fn open_keeps_a_running_install_and_drops_the_rest() {
    let mut picker = picker(Mode::Choose, &["acme/m1"], &["openai", "zeta"]);
    assert!(picker.installs.start(start("openai")));
    picker.installs.fail("zeta", "boom".to_owned());
    picker.move_row(1);
    picker.close();
    picker.open(Mode::Choose, None);
    assert_eq!(
        picker.installs.progress("openai"),
        Some(&Progress::Installing)
    );
    assert_eq!(picker.installs.progress("zeta"), None);
    assert_eq!(picker.focus(), Some(Stop::Model(0)));
}

#[test]
fn ask_stale_asks_stale_and_keeps_every() {
    let mut picker = ModelPicker::default();
    picker.ask_stale();
    assert_eq!(picker.want, Some(Refresh::Stale));
    picker.want = Some(Refresh::Every);
    picker.ask_stale();
    assert_eq!(picker.want, Some(Refresh::Every));
    picker.want = Some(Refresh::Cached);
    picker.ask_stale();
    assert_eq!(picker.want, Some(Refresh::Stale));
}
