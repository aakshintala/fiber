//! Tests for the model picker's reads on the app: a read error shows
//! once with the old catalogue kept, and catalogue notices show grouped
//! by code, each shown group once per terminal run.

use super::super::{App, Effect};
use crate::catalogue::{Catalogue, ModelEntry};
use crate::home::Launch;
use contract::ErrorCode;
use contract::events::Notice;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
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
        name: None,
        price: None,
    }
}

fn catalogue() -> Catalogue {
    Catalogue {
        installable: Vec::new(),
        models: vec![entry("acme/m1")],
        notices: vec![Notice {
            code: ErrorCode::ModelInvalid,
            message: "a cached provider is gone".to_owned(),
            extension: None,
        }],
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    }
}

#[test]
fn a_read_error_is_one_notice_and_keeps_the_catalogue() {
    let mut app = home();
    app.on_models(Ok(catalogue()));
    assert_eq!(app.model_picker.catalogue.models.len(), 1);
    app.on_models(Err("the lists could not be read".to_owned()));
    assert_eq!(app.notice(), Some("the lists could not be read"));
    assert_eq!(newest_code(&app), "io_failed");
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
fn catalogue_notices_show_under_their_own_codes() {
    let mut app = home();
    let catalogue = Catalogue {
        installable: Vec::new(),
        models: vec![entry("acme/m1")],
        notices: vec![
            Notice {
                code: ErrorCode::ModelUnconfigured,
                message: "a host is missing".to_owned(),
                extension: None,
            },
            Notice {
                code: ErrorCode::ExtensionFailed,
                message: "the refresh failed".to_owned(),
                extension: Some("acme".to_owned()),
            },
            Notice {
                code: ErrorCode::ModelInvalid,
                message: "a cached list is bad".to_owned(),
                extension: None,
            },
        ],
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    };
    app.on_models(Ok(catalogue.clone()));
    assert_eq!(app.notices().len(), 3);
    let codes: Vec<String> = app
        .notices()
        .iter()
        .map(|line| match line {
            crate::app::NoticeLine::Notice { code, .. } => code.clone(),
            line @ crate::app::NoticeLine::More(_) => {
                panic!("a more row, not a notice: {line:?}")
            }
        })
        .collect();
    // Catalogue notices keep their push order, one row each, oldest first
    // (`docs/tui.md`, "Notices").
    assert_eq!(
        codes,
        ["model_unconfigured", "extension_failed", "model_invalid"]
    );
    assert_eq!(app.notice(), Some("a cached list is bad"));
    // A second identical read adds no rows: the same code with the same
    // text while stacked dedupes (#2843: event repeats fade as one row).
    app.on_models(Ok(catalogue));
    assert_eq!(app.notices().len(), 3);
}

#[test]
fn catalogue_notices_group_by_code() {
    // Two `model_invalid` notices in one catalogue read stack one grouped
    // row; a second identical read adds none, since the run already
    // showed it (`docs/errors.md`, "Notices": one per code;
    // `docs/tui.md`, "Notices": no repeat in the same run).
    let mut app = home();
    let catalogue = Catalogue {
        installable: Vec::new(),
        models: vec![entry("acme/m1")],
        notices: vec![
            Notice {
                code: ErrorCode::ModelInvalid,
                message: "a first cached list is bad".to_owned(),
                extension: None,
            },
            Notice {
                code: ErrorCode::ModelInvalid,
                message: "a second cached list is bad".to_owned(),
                extension: None,
            },
        ],
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    };
    app.on_models(Ok(catalogue.clone()));
    let rows: Vec<(String, String)> = app
        .notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => Some((code, text)),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "model_invalid");
    assert!(
        rows[0]
            .1
            .starts_with("2 `model_invalid` notices: a first cached list is bad\n"),
        "{}",
        rows[0].1
    );
    assert!(
        rows[0].1.contains("- a first cached list is bad"),
        "{}",
        rows[0].1
    );
    assert!(
        rows[0].1.contains("- a second cached list is bad"),
        "{}",
        rows[0].1
    );
    app.on_models(Ok(catalogue));
    assert_eq!(app.notices().len(), 1);
}

use std::time::Instant;

use contract::clock::Clock;
use serde_json::json;

use crate::keys::{Edit, Key, Mouse, MouseKind};
use crate::stroke::Stroke;

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// Presses Ctrl+S through the bindings, the `session_only` action's key.
fn ctrl_s(app: &mut App) -> Effect {
    app.on_press(Stroke::parse("ctrl+s").unwrap(), now())
}

/// An app on home at 80x24, scoped to `scoped`.
fn scoped_home(scoped: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
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
        scoped_models: scoped.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// An app attached to a session at 80x24, with no home.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

fn levels(levels: &[&str]) -> Vec<String> {
    levels.iter().map(|level| (*level).to_owned()).collect()
}

/// The first notice row's code.
fn first_code(app: &App) -> String {
    match app.notices().into_iter().next() {
        Some(crate::app::NoticeLine::Notice { code, .. }) => code,
        line => panic!("a notice row, not {line:?}"),
    }
}

/// The newest notice row's code.
fn newest_code(app: &App) -> String {
    match app.notices().into_iter().last() {
        Some(crate::app::NoticeLine::Notice { code, .. }) => code,
        line => panic!("a notice row, not {line:?}"),
    }
}

/// Three models over two providers: `acme/m1` with two levels,
/// `acme/m2` with none, `zeta/z1` with one.
fn three() -> Catalogue {
    Catalogue {
        installable: Vec::new(),
        models: vec![
            ModelEntry {
                reference: "acme/m1".to_owned(),
                provider: "acme".to_owned(),
                id: "m1".to_owned(),
                levels: levels(&["low", "high"]),
                default_level: Some("high".to_owned()),
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            },
            ModelEntry {
                reference: "acme/m2".to_owned(),
                provider: "acme".to_owned(),
                id: "m2".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            },
            ModelEntry {
                reference: "zeta/z1".to_owned(),
                provider: "zeta".to_owned(),
                id: "z1".to_owned(),
                levels: levels(&["low"]),
                default_level: None,
                configured: Some("low".to_owned()),
                roles: Vec::new(),
                name: None,
                price: None,
            },
        ],
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    }
}

/// The picker's selected reference, if it is open.
fn selected(app: &App) -> Option<String> {
    app.model_picker.open.as_ref().and_then(|open| {
        app.model_picker
            .catalogue
            .models
            .get(open.selected)
            .map(|entry| entry.reference.clone())
    })
}

/// The selected row's chip level, if it has one.
fn chip(app: &App) -> Option<String> {
    app.model_picker.open.as_ref().and_then(|open| {
        let entry = app.model_picker.catalogue.models.get(open.selected)?;
        open.chips
            .get(open.selected)
            .copied()
            .flatten()
            .and_then(|chip| entry.levels.get(chip).cloned())
    })
}

#[test]
fn ctrl_l_opens_the_picker_attached_and_on_home() {
    for mut app in [home(), attached()] {
        assert!(!app.model_picker_open());
        assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
        assert!(app.model_picker_open());
    }
}

#[test]
fn slash_model_opens_the_picker() {
    let mut app = home();
    for ch in "/model".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.input().expand().is_empty());
}

#[test]
fn esc_closes_it_sending_nothing() {
    let mut app = home();
    app.on_models(Ok(three()));
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(app.input().expand(), "hi");
}

#[test]
fn arrows_move_rows_and_chips_clamped() {
    let mut app = home();
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    // Rows clamp at both ends.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    // Chips clamp at both ends, and mark the row touched.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(chip(&app), Some("high".to_owned()));
    app.on_edit(Edit::Right);
    assert_eq!(chip(&app), Some("high".to_owned()));
    app.on_edit(Edit::Left);
    assert_eq!(chip(&app), Some("low".to_owned()));
    app.on_edit(Edit::Left);
    assert_eq!(chip(&app), Some("low".to_owned()));
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| open.touched[0])
    );
    // A model with no levels has no chip to move.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(chip(&app), None);
    app.on_edit(Edit::Right);
    assert_eq!(chip(&app), None);
}

#[test]
fn headings_are_not_stops() {
    let mut app = home();
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    let mut seen = vec![selected(&app)];
    for _ in 0..4 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
        seen.push(selected(&app));
    }
    let seen: Vec<Option<String>> = seen.into_iter().collect();
    assert_eq!(
        seen.as_slice(),
        [
            Some("acme/m1".to_owned()),
            Some("acme/m2".to_owned()),
            Some("zeta/z1".to_owned()),
            Some("zeta/z1".to_owned()),
            Some("zeta/z1".to_owned()),
        ]
    );
}

#[test]
fn tab_toggles_show_all_only_when_scoped() {
    let mut plain = home();
    plain.on_models(Ok(three()));
    assert_eq!(plain.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(plain.on_key(Key::Tab, now()), Effect::None);
    assert!(
        plain
            .model_picker
            .open
            .as_ref()
            .is_some_and(|open| !open.show_all)
    );

    let mut app = scoped_home(&["acme/m1", "zeta/z1"]);
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| open.show_all)
    );
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| !open.show_all)
    );
}

#[test]
fn other_keys_and_edits_do_nothing() {
    let mut app = home();
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    let before = selected(&app);
    for key in [Key::Char('x'), Key::Backspace, Key::Char('S'), Key::CtrlF] {
        assert_eq!(app.on_key(key.clone(), now()), Effect::None);
    }
    for edit in [Edit::Delete, Edit::WordLeft, Edit::CtrlJ] {
        assert_eq!(app.on_edit(edit), Effect::None);
    }
    // Enter and `s` choose; any other key keeps the picker open, the
    // draft keeps nothing typed, and the selection never moved.
    assert!(app.model_picker_open());
    assert!(app.input().expand().is_empty());
    assert_eq!(selected(&app), before);
}

#[test]
fn ctrl_c_passes_through() {
    let mut app = home();
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.hint());
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::Quit);
}

#[test]
fn each_open_starts_fresh() {
    let mut app = scoped_home(&["acme/m1", "zeta/z1"]);
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    app.on_edit(Edit::Right);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(chip(&app), Some("high".to_owned()));
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| !open.show_all)
    );
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| open.touched.iter().all(|touched| !touched))
    );
}

#[test]
fn opening_asks_stale_and_refresh_asks_every() {
    let mut app = home();
    assert_eq!(app.take_reads(), None);
    // No picker open, no read owed, whatever the key.
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.take_reads(), None);
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    assert_eq!(app.take_reads(), None);
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    assert_eq!(app.take_reads(), None);
    // An `Every` owed survives the next open.
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
}

#[test]
fn refreshing_shows_until_the_answer() {
    use crate::catalogue::Refresh;
    let mut app = home();
    assert_eq!(app.model_picker.refreshing, None);
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(app.model_picker.refreshing, Some(Refresh::Stale));
    app.on_models(Ok(three()));
    assert_eq!(app.model_picker.refreshing, None);
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(Refresh::Every));
    assert_eq!(app.model_picker.refreshing, Some(Refresh::Every));
    app.on_models(Err("gone".to_owned()));
    assert_eq!(app.model_picker.refreshing, None);
    // The startup `Cached` take never shows refreshing.
    app.model_picker.want = Some(Refresh::Cached);
    assert_eq!(app.take_reads(), Some(Refresh::Cached));
    assert_eq!(app.model_picker.refreshing, None);
}

#[test]
fn startup_read_keeps_a_meanwhile_openers_mark() {
    use crate::catalogue::Refresh;
    let mut app = home();
    // The startup ask counts as owed, so its answer never drops a
    // meanwhile opened picker's mark.
    app.note_catalogue_read();
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(app.model_picker.refreshing, Some(Refresh::Stale));
    // The startup read answers first, while the open read still runs:
    // one owed read stays, so the mark stays.
    app.on_models(Err("gone".to_owned()));
    assert_eq!(app.model_picker.refreshing, Some(Refresh::Stale));
    // Its own answer clears the mark.
    app.on_models(Err("gone".to_owned()));
    assert_eq!(app.model_picker.refreshing, None);
    assert_eq!(app.take_reads(), None);
}

#[test]
fn the_picker_is_a_picker_context() {
    let mut app = App::new(PathBuf::from("/w"));
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"send": "ctrl+s"})).unwrap();
    app.set_keys(crate::KeysSetup { user });
    app.set_home(Launch {
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
    });
    app.set_size(80, 24);
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // With `send` rebound, Enter still reaches the picker, and a letter
    // never reaches the draft: the picker's keys survive rebinding.
    let stroke = Stroke::parse("x").unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
    assert!(app.input().expand().is_empty());
    assert!(app.model_picker_open());
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

/// A live `session_status` for `session`, streaming.
fn streaming(session: &str) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
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

/// A home app with a working session, the picker open, and the quit
/// question up.
fn quitting() -> App {
    let mut app = home();
    app.on_line(hello());
    app.on_line(streaming("s_aaaaaaaaaaaaaaaa"));
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.quit_open());
    app
}

#[test]
fn the_quit_question_takes_keys_over_the_picker() {
    // Enter leaves working sessions running.
    let before = selected(&quitting());
    assert_eq!(quitting().on_key(Key::Enter, now()), Effect::Quit);
    assert_eq!(selected(&quitting()), before);
    // `c` closes them; with the hub up it sends the closes, then quits.
    assert!(matches!(
        quitting().on_key(Key::Char('c'), now()),
        Effect::Exit(_)
    ));
    // Esc stays, keeping the picker open with its selection.
    let mut app = quitting();
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.quit_open());
    assert!(app.model_picker_open());
    assert_eq!(selected(&app), before);
    // Ctrl+L never reaches the picker while the question is up.
    let mut app = quitting();
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app), before);
    // Neither does an edit: the draft keeps it, not the picker.
    let mut app = quitting();
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(selected(&app), before);
}

#[test]
fn open_starts_on_the_home_chips_model_and_level() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("zeta/z1".to_owned()),
        thinking: Some("low".to_owned()),
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(chip(&app).as_deref(), Some("low"));
}

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// One envelope of `session`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `preamble_built` naming `model` with `thinking`.
fn preamble_line(model: &str, thinking: Option<&str>) -> crate::link::Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": model, "context_window": 200000,
            "trigger_at": 150000, "thinking": thinking,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    )
}

/// A `model_changed` to `model` with `thinking`.
fn changed_line(model: &str, thinking: Option<&str>) -> crate::link::Line {
    session_line(
        SESSION,
        "model_changed",
        serde_json::json!({
            "before": {"model": "old/model", "cache_lifetime": "5m"},
            "after": {"model": model, "thinking": thinking, "cache_lifetime": "5m"},
            "source": "driver",
        }),
    )
}

/// A `usage_recorded` of `input`, `cache_read` and `cache_write` tokens on
/// `session`, with `extra` merged in: an origin or extension marker.
fn usage_line(
    session: &str,
    input: u64,
    cache_read: u64,
    cache_write: serde_json::Value,
    extra: serde_json::Value,
) -> crate::link::Line {
    let mut payload = serde_json::json!({
        "generation_id": "g_1", "model": "acme/m1",
        "tokens": {"input": input, "cache_read": cache_read,
            "cache_write": cache_write, "output": 5},
        "input_bytes": 1, "cost": null,
    });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    session_line(session, "usage_recorded", payload)
}

/// Thirty models on one provider, without levels.
fn thirty() -> Catalogue {
    Catalogue {
        installable: Vec::new(),
        models: (0..30)
            .map(|at| {
                let reference = format!("acme/m{at:02}");
                ModelEntry {
                    reference: reference.clone(),
                    provider: "acme".to_owned(),
                    id: format!("m{at:02}"),
                    levels: Vec::new(),
                    default_level: None,
                    configured: None,
                    roles: Vec::new(),
                    name: None,
                    price: None,
                }
            })
            .collect(),
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    }
}

#[test]
fn page_keys_move_by_a_page_clamped() {
    let mut app = home();
    app.on_models(Ok(thirty()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // The picker draws below its one-row swapped-view header. Page steps
    // use that exact body height, so each page moves six models here.
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m06"));
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m12"));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m06"));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m00"));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m00"));
}

#[test]
fn the_current_model_preselects_its_level_after_model_changed() {
    let mut app = attached();
    app.on_models(Ok(three()));
    app.on_line(preamble_line("zeta/z1", Some("low")));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // The fold's model wins over catalogue order, and its level over the
    // row's configured one.
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.on_line(changed_line("acme/m2", None));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    assert_eq!(chip(&app), None);
}

#[test]
fn the_cross_closes_it() {
    let mut app = home();
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Close)),
        Effect::None
    );
    assert!(!app.model_picker_open());
}

#[test]
fn the_rows_show_the_rebuild_size_of_the_session_on_screen() {
    let mut app = attached();
    app.on_models(Ok(three()));
    // Another session's call, a copy of this one's, and an extension's
    // count nothing.
    app.on_line(usage_line(
        OTHER,
        1000,
        200000,
        json!({"1h": 34}),
        json!({}),
    ));
    app.on_line(usage_line(
        SESSION,
        1,
        0,
        json!({}),
        json!({"origin_session_id": OTHER}),
    ));
    app.on_line(usage_line(
        SESSION,
        1,
        0,
        json!({}),
        json!({"extension": "x"}),
    ));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // With no prompt size on screen no cost column shows.
    let view = app.model_picker_view().expect("open");
    assert!(
        view.sections
            .iter()
            .flat_map(|section| section.models.iter())
            .all(|model| model.cost.is_none())
    );
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.on_line(usage_line(
        SESSION,
        1000,
        200000,
        json!({"1h": 34}),
        json!({}),
    ));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // The session's own call sizes every row: no row is current here.
    let view = app.model_picker_view().expect("open");
    let costs: Vec<(&str, Option<&str>)> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .map(|model| (model.id.as_str(), model.cost.as_deref()))
        .collect();
    assert_eq!(
        costs,
        [
            ("m1", Some("~201k tokens")),
            ("m2", Some("~201k tokens")),
            ("z1", Some("~201k tokens")),
        ]
    );
    // On home no session is on screen, so no size shows.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.go_home();
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    let view = app.model_picker_view().expect("open");
    assert!(
        view.sections
            .iter()
            .flat_map(|section| section.models.iter())
            .all(|model| model.cost.is_none())
    );
}

#[test]
fn clicks_on_refresh_and_the_scope_line_act_as_their_keys() {
    use crate::catalogue::Refresh;

    let mut app = scoped_home(&["acme/m1", "zeta/z1"]);
    app.on_models(Ok(three()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    // The refresh button asks `Every`.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            1, 0
        ))),
        Effect::None
    );
    assert_eq!(app.take_reads(), Some(Refresh::Every));
    // The scope line toggles "show all", staying open either way.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            1, 1
        ))),
        Effect::None
    );
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| open.show_all)
    );
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            1, 1
        ))),
        Effect::None
    );
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| !open.show_all)
    );
    assert!(app.model_picker_open());
}

#[test]
fn opening_the_picker_closes_a_config_view() {
    use std::sync::Arc;

    let mut app = home();
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(vec![])) as Arc<dyn crate::Configure>
    ));
    app.open_config_view(super::super::ConfigView::Settings);
    assert!(app.config_view_open());
    app.open_model_picker(crate::model_picker::Mode::Choose);
    assert!(!app.config_view_open());
    assert!(app.model_picker_open());
}

#[test]
fn opening_a_config_view_closes_the_picker() {
    use std::sync::Arc;

    let mut app = home();
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(vec![])) as Arc<dyn crate::Configure>
    ));
    app.open_model_picker(crate::model_picker::Mode::Choose);
    assert!(app.model_picker_open());
    app.open_config_view(super::super::ConfigView::Settings);
    assert!(!app.model_picker_open());
    assert!(app.config_view_open());
}

use std::sync::Arc;

use crate::link::Line;

/// An app attached to [`SESSION`] with the hub up, the seam `seam`
/// recording writes, and the three-model catalogue read.
fn choosing_app() -> (App, Arc<crate::configure_fake::Fake>) {
    let mut app = attached();
    app.on_line(hello());
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_models(Ok(three()));
    (app, seam)
}

/// Opens the picker.
fn open(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
}

/// The `model` line an Enter or click sent, parsed.
fn sent(effect: Effect) -> serde_json::Value {
    let Effect::Send(lines) = effect else {
        panic!("a choice sends one line, got {effect:?}");
    };
    assert_eq!(lines.len(), 1);
    serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("a line: {err}"))
}

/// A `command_accepted` for `id` on `session`.
fn accepted(session: &str, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), json!(id))].into_iter().collect(),
    })
}

/// A `command_rejected` for `id` on `session`, with `code` and `message`.
fn refused(session: &str, id: &str, code: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), json!(id)),
            ("code".to_owned(), json!(code)),
            ("message".to_owned(), json!(message)),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn hub_refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), json!(id)),
            ("message".to_owned(), json!(message)),
        ]
        .into_iter()
        .collect(),
    })
}

#[test]
fn enter_sends_one_model_command_and_closes() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["command"], json!("model"));
    assert_eq!(line["session_id"], json!(SESSION));
    // Untouched, the chip's level rides the command but only the model
    // waits to be saved.
    assert_eq!(
        line["args"],
        json!({"model": "acme/m1", "thinking": "high"})
    );
    assert!(!app.model_picker_open());
    // Nothing is written before the session accepts.
    assert!(seam.writes().is_empty());
    assert_eq!(app.model_picker.awaiting.len(), 1);
    // The command is pending as a built-in, carrying an empty draft, so
    // a rejection is the existing notice with nothing to put back.
    let id = line["id"].as_str().expect("an id");
    app.on_line(refused(
        SESSION,
        id,
        "invalid_arguments",
        "acme/m1 takes low, high.",
    ));
    assert_eq!(app.notice(), Some("acme/m1 takes low, high."));
    assert!(app.input().expand().is_empty());
}

#[test]
fn choosing_a_model_without_levels_sends_no_thinking() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
    let id = line["id"].as_str().expect("an id");
    app.on_line(accepted(SESSION, id));
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("model".to_owned(), "acme/m2".to_owned())]
    );
}

#[test]
fn choosing_a_level_writes_models_ref_thinking_after_acceptance() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    // Touch the row: the level is picked out, so it is saved too.
    app.on_edit(Edit::Left);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert_eq!(line["args"], json!({"model": "acme/m1", "thinking": "low"}));
    assert!(seam.writes().is_empty());
    app.on_line(accepted(SESSION, &id));
    // The model first, then the level, both to the global file.
    assert_eq!(
        seam.writes(),
        vec![
            (
                PathBuf::from("/w"),
                crate::configure::Layer::Global,
                "model".to_owned(),
                "acme/m1".to_owned()
            ),
            (
                PathBuf::from("/w"),
                crate::configure::Layer::Global,
                "models.\"acme/m1\".thinking".to_owned(),
                "low".to_owned()
            ),
        ]
    );
    // The catalogue's configured level follows the write.
    assert_eq!(
        app.model_picker
            .catalogue
            .models
            .first()
            .and_then(|entry| entry.configured.clone()),
        Some("low".to_owned())
    );
}

#[test]
fn a_rejection_writes_nothing_and_shows_the_message() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let first = sent(app.on_key(Key::Enter, now()));
    let first_id = first["id"].as_str().expect("an id").to_owned();
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let second = sent(app.on_key(Key::Enter, now()));
    let second_id = second["id"].as_str().expect("an id").to_owned();
    assert_eq!(app.model_picker.awaiting.len(), 2);
    app.on_line(refused(SESSION, &second_id, "invalid_arguments", "gone."));
    assert_eq!(app.notice(), Some("gone."));
    // The refused id leaves `awaiting` while the other's entry stays.
    assert!(!app.model_picker.awaiting.contains_key(&second_id));
    assert!(app.model_picker.awaiting.contains_key(&first_id));
    // A later acceptance carrying the refused id writes nothing.
    app.on_line(accepted(SESSION, &second_id));
    assert!(seam.writes().is_empty());
    // The other choice still writes on its own acceptance.
    app.on_line(accepted(SESSION, &first_id));
    assert_eq!(seam.writes().len(), 1);
}

#[test]
fn a_hub_level_rejection_also_drops_the_writes() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(hub_refused(&id, "gone."));
    assert_eq!(app.notice(), Some("gone."));
    assert!(!app.model_picker.awaiting.contains_key(&id));
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().is_empty());
}

#[test]
fn a_duplicate_rejection_drops_the_writes() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    let notices = app.notices().len();
    // A resent copy the session already holds closes with no notice.
    app.on_line(refused(SESSION, &id, "duplicate_command", "twice."));
    assert_eq!(app.notices().len(), notices);
    assert!(!app.model_picker.awaiting.contains_key(&id));
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().is_empty());
}

#[test]
fn an_acceptance_after_switching_sessions_still_writes() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    // Choosing may span a switch: the answer's session need not be on
    // screen.
    app.attach(contract::SessionId(OTHER.to_owned()));
    app.on_line(accepted(SESSION, &id));
    assert_eq!(seam.writes().len(), 1);
}

#[test]
fn two_choices_in_flight_each_write_their_own() {
    for first in [true, false] {
        let (mut app, seam) = choosing_app();
        open(&mut app);
        let one = sent(app.on_key(Key::Enter, now()));
        let one_id = one["id"].as_str().expect("an id").to_owned();
        open(&mut app);
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
        let two = sent(app.on_key(Key::Enter, now()));
        let two_id = two["id"].as_str().expect("an id").to_owned();
        let (early, late) = if first {
            (one_id, two_id)
        } else {
            (two_id, one_id)
        };
        app.on_line(accepted(SESSION, &early));
        app.on_line(accepted(SESSION, &late));
        // Each acceptance writes its own choice, in acceptance order.
        let models: Vec<String> = seam
            .writes()
            .iter()
            .filter(|(_, _, key, _)| key == "model")
            .map(|(_, _, _, text)| text.clone())
            .collect();
        if first {
            assert_eq!(models, ["acme/m1".to_owned(), "zeta/z1".to_owned()]);
        } else {
            assert_eq!(models, ["zeta/z1".to_owned(), "acme/m1".to_owned()]);
        }
    }
}

#[test]
fn the_rebuild_notice_shows_only_for_a_change_with_a_known_usage() {
    // The fold names `acme/m1` at `high`: choosing it untouched is no
    // change.
    let mut same = choosing_app().0;
    same.on_line(preamble_line("acme/m1", Some("high")));
    same.on_line(usage_line(
        SESSION,
        1000,
        200000,
        json!({"1h": 34}),
        json!({}),
    ));
    open(&mut same);
    sends_one(same.on_key(Key::Enter, now()));
    assert!(same.notices().is_empty());

    // A changed level with a known last call names its size.
    let (mut app, _) = choosing_app();
    app.on_line(preamble_line("acme/m1", Some("high")));
    app.on_line(usage_line(
        SESSION,
        1000,
        200000,
        json!({"1h": 34}),
        json!({}),
    ));
    open(&mut app);
    app.on_edit(Edit::Left);
    sends_one(app.on_key(Key::Enter, now()));
    assert_eq!(
        app.notice(),
        Some("switching rebuilds the cache: about 201,034 tokens")
    );

    // A change with no known last call says nothing.
    let (mut app, _) = choosing_app();
    app.on_line(preamble_line("acme/m1", Some("high")));
    open(&mut app);
    app.on_edit(Edit::Left);
    sends_one(app.on_key(Key::Enter, now()));
    assert!(app.notices().is_empty());

    // The same model with no known last call says nothing either.
    let (mut app, _) = choosing_app();
    app.on_line(preamble_line("acme/m1", Some("high")));
    open(&mut app);
    sends_one(app.on_key(Key::Enter, now()));
    assert!(app.notices().is_empty());
}

/// A choice sends exactly one line.
fn sends_one(effect: Effect) {
    let Effect::Send(lines) = effect else {
        panic!("a choice sends one line");
    };
    assert_eq!(lines.len(), 1);
}

#[test]
fn with_the_link_down_nothing_is_sent_and_it_stays_open() {
    let mut app = attached();
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        app.notice(),
        Some("The hub is not connected; nothing changed.")
    );
    assert!(app.model_picker_open());
}

#[test]
fn a_failed_write_is_a_notice() {
    let (mut app, seam) = choosing_app();
    *seam.refuse.lock().expect("the refusal") = Some("locked".to_owned());
    open(&mut app);
    app.on_edit(Edit::Left);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    let shown = shown_notices(&app);
    assert!(shown.contains("Savingmodelfailed:locked"), "{shown}");
    assert!(
        shown.contains("models.\"acme/m1\".thinkingfailed:locked"),
        "{shown}"
    );
}

/// Every notice row showing, joined with its wrapping and padding cut:
/// what the person reads, each with its code.
fn shown_notices(app: &App) -> String {
    app.notices()
        .iter()
        .map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => format!("{code} {text}"),
            crate::app::NoticeLine::More(n) => format!("+{n} more"),
        })
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect()
}

#[test]
fn with_no_seam_the_switch_says_it_is_not_saved() {
    let mut app = attached();
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    sends_one(app.on_key(Key::Enter, now()));
    assert_eq!(
        app.notice(),
        Some("Saving is not available; the switch is for this session only.")
    );
    assert!(!app.model_picker_open());
}

#[test]
fn a_chip_click_chooses_that_level() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    // Layout rows: 0 the filter, 1 the buttons, 2 the `acme` heading,
    // 3 `acme/m1` with two chips past its name cell.
    let line = sent(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            3, 2,
        ))),
    );
    assert_eq!(
        line["args"],
        json!({"model": "acme/m1", "thinking": "high"})
    );
    assert!(!app.model_picker_open());
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    // A click on a chip carries the level picked out: both are saved.
    assert_eq!(seam.writes().len(), 2);
}

#[test]
fn a_click_on_the_preselected_chip_saves_the_level() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    // `acme/m1` preselects `high` at cell (3, 2): clicking it chooses the
    // level, saving `models."acme/m1".thinking`.
    let line = sent(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            3, 2,
        ))),
    );
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().iter().any(|(_, layer, key, text)| {
        *layer == crate::configure::Layer::Global
            && key == "models.\"acme/m1\".thinking"
            && text == "high"
    }));
}

#[test]
fn a_name_click_chooses_its_chip_saving_model_only() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    // The name cell chooses the row at its untouched chip: only the
    // model is saved.
    let line = sent(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            3, 0,
        ))),
    );
    assert_eq!(
        line["args"],
        json!({"model": "acme/m1", "thinking": "high"})
    );
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("model".to_owned(), "acme/m1".to_owned())]
    );
}

#[test]
fn esc_or_the_cross_after_moving_chips_writes_nothing() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    app.on_edit(Edit::Left);
    app.on_edit(Edit::Right);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.awaiting.is_empty());

    open(&mut app);
    app.on_edit(Edit::Left);
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Close)),
        Effect::None
    );
    assert!(!app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn enter_on_the_empty_scoped_view_sends_nothing() {
    let mut app = scoped_home(&["gone/x"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_models(Ok(three()));
    open(&mut app);
    // No scoped row is installed: Enter keeps the picker open, sending
    // nothing.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
}

#[test]
fn choosing_on_home_writes_at_once_and_sets_the_chips() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_models(Ok(three()));
    open(&mut app);
    app.on_edit(Edit::Left);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(!app.model_picker_open());
    // Saved choices on home write at once, the model first.
    assert_eq!(
        seam.writes(),
        vec![
            (
                PathBuf::from("/w"),
                crate::configure::Layer::Global,
                "model".to_owned(),
                "acme/m1".to_owned()
            ),
            (
                PathBuf::from("/w"),
                crate::configure::Layer::Global,
                "models.\"acme/m1\".thinking".to_owned(),
                "low".to_owned()
            ),
        ]
    );
    // The home chips and the catalogue's configured level follow.
    let launch = &app.home.as_ref().expect("home").launch;
    assert_eq!(launch.model.as_deref(), Some("acme/m1"));
    assert_eq!(launch.thinking.as_deref(), Some("low"));
    assert_eq!(
        app.model_picker
            .catalogue
            .models
            .first()
            .and_then(|entry| entry.configured.clone()),
        Some("low".to_owned())
    );
}

#[test]
fn a_home_choice_that_needs_saving_reports_when_the_configure_seam_is_missing() {
    let mut app = home();
    app.on_models(Ok(three()));
    open(&mut app);

    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);

    assert_eq!(
        app.notice(),
        Some("Saving is not available; the switch is for this session only.")
    );
    assert!(!app.model_picker_open());
    assert!(app.model_picker.awaiting.is_empty());
    assert!(app.model_picker.start_model.is_none());
    let launch = &app.home.as_ref().expect("home").launch;
    assert_eq!(launch.model, None);
    assert_eq!(launch.thinking, None);
}

#[test]
fn a_home_choice_with_nothing_to_save_needs_no_configure_seam() {
    let mut app = home();
    app.home.as_mut().expect("home").launch.model = Some("acme/m2".to_owned());
    app.on_models(Ok(three()));

    assert_eq!(
        app.open_model_picker(crate::model_picker::Mode::Thinking),
        Effect::None
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);

    assert!(app.notices().is_empty());
    assert!(!app.model_picker_open());
    assert!(app.model_picker.awaiting.is_empty());
    assert!(app.model_picker.start_model.is_none());
    let launch = &app.home.as_ref().expect("home").launch;
    assert_eq!(launch.model.as_deref(), Some("acme/m2"));
    assert_eq!(launch.thinking, None);
}

#[test]
fn s_chooses_for_this_session_only_and_writes_nothing() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(ctrl_s(&mut app));
    assert_eq!(line["command"], json!("model"));
    assert_eq!(
        line["args"],
        json!({"model": "acme/m1", "thinking": "high"})
    );
    assert!(!app.model_picker_open());
    // Nothing waits to be written, before or after the acceptance.
    assert!(app.model_picker.awaiting.is_empty());
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().is_empty());
}

#[test]
fn s_on_a_touched_row_sends_the_level() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    app.on_edit(Edit::Left);
    let line = sent(ctrl_s(&mut app));
    assert_eq!(line["args"], json!({"model": "acme/m1", "thinking": "low"}));
}

#[test]
fn s_does_nothing_on_the_empty_scoped_view() {
    let mut app = scoped_home(&["gone/x"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker_open());
}

/// A scoped app attached to [`SESSION`] with the picker open on
/// `acme/m1`, after a refresh answer that drops it for an unscoped row
/// in its place.
fn dropped_selection_app() -> App {
    let mut app = scoped_home(&["acme/m1", "acme/m2"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    let mut answered = three();
    answered.models[0] = entry("other/u");
    app.on_models(Ok(answered));
    app
}

#[test]
fn a_refresh_that_drops_the_selected_model_moves_the_selection_onto_the_shown_rows() {
    let mut app = dropped_selection_app();
    // Index 0 now hides the unscoped `other/u`: the selection moves to
    // the first shown row, `acme/m2`.
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    // The view highlights that same row: the filter, the buttons, the
    // heading, then `m2` at layout row 3.
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.focused, Some(3));
    assert_eq!(view.sections[0].models[0].id, "m2");
    // Enter takes the highlighted row, not the hidden model.
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
}

#[test]
fn choosing_with_a_hidden_selection_takes_the_highlighted_row() {
    let mut app = dropped_selection_app();
    // Park the selection back on the hidden row: the view still
    // highlights the first shown row.
    app.model_picker.open.as_mut().expect("open").selected = 0;
    let view = app.model_picker_view().expect("open");
    assert_eq!(view.focused, Some(3));
    assert_eq!(view.sections[0].models[0].id, "m2");
    // `s` takes the highlighted row for this session only, leaving
    // nothing to save.
    let line = sent(ctrl_s(&mut app));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn the_footer_names_the_picker_keys() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(
        app.model_picker_view().map(|view| view.footer),
        Some(vec![
            ("↑↓", "move"),
            ("←→", "levels"),
            ("Enter", "choose"),
            ("Tab", "all"),
            ("Ctrl+S", "session"),
            ("Ctrl+R", "refresh"),
            ("Esc", "close"),
        ])
    );
}

#[test]
fn session_only_rebinds() {
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"session_only": "x"})).unwrap();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_keys(crate::KeysSetup { user });
    app.set_home(Launch {
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
    });
    app.set_size(80, 24);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_models(Ok(three()));
    open(&mut app);
    // `x` chooses for this session only, sending the `model` command.
    let stroke = Stroke::parse("x").unwrap();
    let Effect::Send(lines) = app.on_press(stroke, now()) else {
        panic!("a rebound `s` sends");
    };
    assert_eq!(lines.len(), 1);
    assert!(!app.model_picker_open());
    // `s` moved off the action: it types into the filter instead,
    // sending nothing.
    open(&mut app);
    let stroke = Stroke::parse("s").unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
    assert_eq!(app.model_picker.query(), "s");
    assert!(app.model_picker_open());
}

#[test]
fn typing_s_in_an_overlay_is_unaffected_by_rebinding_session_only() {
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"session_only": "x"})).unwrap();
    let mut app = App::new(PathBuf::from("/w"));
    app.set_keys(crate::KeysSetup { user });
    app.set_home(Launch {
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
    });
    app.set_size(80, 24);
    // The `/` panel is open: `s` types into the draft, reaching no
    // picker, which is never open here.
    assert_eq!(app.on_key(Key::Char('/'), now()), Effect::None);
    let stroke = Stroke::parse("s").unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
    assert_eq!(app.input().expand(), "/s");
    assert!(!app.model_picker_open());
}

#[test]
fn esc_closes_the_topmost_overlay_first() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    // F1 opens the key map above the open picker: the picker's own key
    // check passes the global actions' keys on.
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert!(app.model_picker_open());
    // Esc closes whatever is on top: the key map first, leaving the
    // picker open.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.keymap_top().is_none());
    assert!(app.model_picker_open());
    // The next Esc closes the picker underneath.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
}

#[test]
fn edits_reach_nothing_under_the_key_map() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    // The arrows would move the selected row's chip: above it, the key
    // map takes every edit, and the chip stays where it was.
    assert_eq!(chip(&app).as_deref(), Some("high"));
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(chip(&app).as_deref(), Some("high"));
    assert!(app.model_picker_open());
}

#[test]
fn ctrl_s_sends_one_model_command_and_waits_on_nothing() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    let line = sent(ctrl_s(&mut app));
    assert_eq!(line["command"], json!("model"));
    assert!(!app.model_picker_open());
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn ctrl_s_with_the_quit_question_up_sends_nothing_from_a_choosable_row() {
    let mut app = quitting();
    // A row is there to choose, so only the quit question keeps it back.
    // The question takes Ctrl+S before the keymap, so this runs the
    // action directly.
    assert!(app.model_picker.choice(true).is_some());
    assert_eq!(app.on_action("session_only"), Effect::None);
    assert!(app.quit_open());
    assert!(app.model_picker_open());
    assert!(app.model_picker.start_model.is_none());
}

/// Types one letter into the open picker.
fn type_letter(app: &mut App, c: char) {
    let stroke = Stroke::parse(&c.to_string()).unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
}

#[test]
fn backspace_shortens_the_query_in_every_mode() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    type_letter(&mut app, 'm');
    type_letter(&mut app, '2');
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.model_picker.query(), "m");
    // Backspace shortens the checklist query too.
    let _ = app.scoped_models_command();
    assert!(app.model_picker.is_scope());
    type_letter(&mut app, 'm');
    assert_eq!(app.model_picker.query(), "m");
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.model_picker.query(), "");
    assert!(app.model_picker_open());
}

#[test]
fn ctrl_s_under_the_key_map_sends_nothing_and_stays_open() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(app.on_key(Key::F1, now()), Effect::None);
    assert!(app.keymap_top().is_some());
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker_open());
}

#[test]
fn ctrl_s_with_the_quit_question_up_sends_nothing() {
    let mut app = quitting();
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.quit_open());
}

#[test]
fn ctrl_s_on_home_holds_the_choice_and_saves_nothing() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert!(app.model_picker.start_model.is_some());
    assert!(seam.writes().is_empty());
}

/// Types `text` into the draft.
fn type_draft(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// The `start` line an Enter on home sent, parsed.
fn started(effect: Effect) -> serde_json::Value {
    let Effect::Send(lines) = effect else {
        panic!("Enter on home sends the start, got {effect:?}");
    };
    let start = lines
        .iter()
        .find_map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).ok()?;
            (line["command"] == json!("start")).then(|| line.clone())
        })
        .expect("a start line");
    start["args"].clone()
}

#[test]
fn s_on_home_rides_the_next_start() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    app.on_edit(Edit::Left);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    // Nothing is written: the choice rides the next `start`.
    assert!(seam.writes().is_empty());
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(args["workspace"], json!("/w"));
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(
        args["overrides"],
        json!(["models.\"acme/m1\".thinking=low"])
    );
}

#[test]
fn a_suffix_named_model_is_not_chosen_by_mistake() {
    // The catalogue holds `p/m` and a `p/m:high` of its own: choosing
    // `p/m` at `high` still names the exact reference, with the level as
    // an override.
    let catalogue = Catalogue {
        installable: Vec::new(),
        models: vec![
            ModelEntry {
                reference: "p/m".to_owned(),
                provider: "p".to_owned(),
                id: "m".to_owned(),
                levels: vec!["high".to_owned()],
                default_level: Some("high".to_owned()),
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            },
            ModelEntry {
                reference: "p/m:high".to_owned(),
                provider: "p".to_owned(),
                id: "m:high".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            },
        ],
        notices: Vec::new(),
        lists: Vec::new(),
        left_out: Vec::new(),
        ..Default::default()
    };
    let mut app = home();
    app.on_line(hello());
    app.on_models(Ok(catalogue));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(args["model"], json!("p/m"));
    assert_eq!(args["overrides"], json!(["models.\"p/m\".thinking=high"]));
}

#[test]
fn a_model_without_levels_rides_start_with_no_override() {
    let mut app = home();
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(args["model"], json!("acme/m2"));
    assert!(args.get("overrides").is_none());
}

/// A session-only choice for `reference` at `level`: what Ctrl+S holds
/// for the next `start`.
fn held(reference: &str, level: Option<&str>) -> crate::model_picker::Choice {
    crate::model_picker::Choice {
        reference: reference.to_owned(),
        level: level.map(str::to_owned),
        level_chosen: false,
        save_model: false,
        session_only: true,
    }
}

/// The `start` args with `layer` stored and `choice` held, merged.
fn merged(
    app: &mut App,
    layer: Vec<String>,
    choice: Option<crate::model_picker::Choice>,
) -> serde_json::Value {
    app.launch_overrides = layer;
    app.model_picker.start_model = choice;
    let mut args = app.start_args();
    app.with_start_model(&mut args);
    args
}

#[test]
fn a_session_only_choice_rides_beside_unrelated_layer_keys() {
    let mut app = home();
    // The layer's `model=` yields to the choice naming the model; the
    // unrelated key and the choice's level ride on.
    let args = merged(
        &mut app,
        vec!["model=cli/m".to_owned(), "retry.attempts=2".to_owned()],
        Some(held("acme/m1", Some("low"))),
    );
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(
        args["overrides"],
        json!(["retry.attempts=2", "models.\"acme/m1\".thinking=low"])
    );
}

#[test]
fn a_session_only_choice_without_a_level_keeps_unrelated_layer_keys() {
    let mut app = home();
    // No level, no thinking override: the choice supersedes only the
    // layer's `model=`.
    let args = merged(
        &mut app,
        vec!["model=cli/m".to_owned(), "budget.usd=10".to_owned()],
        Some(held("acme/m1", None)),
    );
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(args["overrides"], json!(["budget.usd=10"]));
    // Nothing left to merge, the key is absent, as without a layer.
    let args = merged(
        &mut app,
        vec!["model=cli/m".to_owned()],
        Some(held("acme/m1", None)),
    );
    assert_eq!(args["model"], json!("acme/m1"));
    assert!(args.get("overrides").is_none());
}

#[test]
fn a_launch_thinking_key_for_the_chosen_model_yields_to_the_choice() {
    let mut app = home();
    // The choice's level wins over the layer's thinking key for the
    // same model; another key rides on. The layer holds canonical keys,
    // as `main` hands them: `acme/m1` needs no quotes.
    let args = merged(
        &mut app,
        vec![
            "models.acme/m1.thinking=high".to_owned(),
            "retry.attempts=2".to_owned(),
        ],
        Some(held("acme/m1", Some("low"))),
    );
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(
        args["overrides"],
        json!(["retry.attempts=2", "models.\"acme/m1\".thinking=low"])
    );
    // Without a chosen level the layer's thinking key stands.
    let args = merged(
        &mut app,
        vec!["models.acme/m1.thinking=high".to_owned()],
        Some(held("acme/m1", None)),
    );
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(args["overrides"], json!(["models.acme/m1.thinking=high"]));
}

#[test]
fn a_canonical_thinking_key_for_a_dotted_reference_yields_to_the_choice() {
    let mut app = home();
    // A reference holding a dot keeps its quotes in canonical spelling,
    // so the layer's thinking key for the chosen model yields to the
    // choice's level, while an unrelated key rides on.
    let layer = vec![
        "models.\"acme/m.1\".thinking=high".to_owned(),
        "retry.attempts=2".to_owned(),
    ];
    let args = merged(&mut app, layer.clone(), Some(held("acme/m.1", Some("low"))));
    assert_eq!(args["model"], json!("acme/m.1"));
    assert_eq!(
        args["overrides"],
        json!(["retry.attempts=2", "models.\"acme/m.1\".thinking=low"]),
        "{layer:?}"
    );
}

#[test]
fn a_canonical_thinking_key_with_a_quoted_value_yields_to_the_choice() {
    let mut app = home();
    // The key is what yields: the layer's canonical thinking key for
    // the chosen model, even with a quoted value, gives way to the
    // choice's level.
    let layer = vec!["models.acme/m1.thinking=\"high\"".to_owned()];
    let args = merged(&mut app, layer.clone(), Some(held("acme/m1", Some("low"))));
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(
        args["overrides"],
        json!(["models.\"acme/m1\".thinking=low"]),
        "{layer:?}"
    );
}

#[test]
fn with_no_choice_the_layer_rides_untouched() {
    let mut app = home();
    let args = merged(&mut app, vec!["model=cli/m".to_owned()], None);
    assert!(args.get("model").is_none());
    assert_eq!(args["overrides"], json!(["model=cli/m"]));
}

#[test]
fn the_start_model_clears_once_start_is_accepted() {
    let mut app = home();
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker.start_model.is_some());
    type_draft(&mut app, "hi");
    let Effect::Send(lines) = app.on_key(Key::Enter, now()) else {
        panic!("Enter on home sends the start");
    };
    let id = lines
        .iter()
        .find_map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).ok()?;
            (line["command"] == json!("start"))
                .then(|| line["id"].as_str().map(str::to_owned).expect("an id"))
        })
        .expect("a start line");
    // The hub accepts the `start`: the choice rode it, and clears.
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), json!(id)),
            (
                "result".to_owned(),
                json!({"session_id": "s_aaaaaaaaaaaaaaaa"}),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    assert!(app.model_picker.start_model.is_none());
}

#[test]
fn enter_after_s_on_home_forgets_the_session_only_choice() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    // `s` holds `acme/m1` for the next `start`, saving nothing.
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker.start_model.is_some());
    assert!(seam.writes().is_empty());
    // Enter saves `acme/m2` as the default, superseding the held choice.
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker.start_model.is_none());
    let launch = &app.home.as_ref().expect("home").launch;
    assert_eq!(launch.model.as_deref(), Some("acme/m2"));
    // The next `start` carries neither a model nor an override: the
    // forgotten choice leaves no trace, and the saved default stands
    // in the config file.
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert!(args.get("model").is_none());
    assert!(args.get("overrides").is_none());
}

#[test]
fn enter_on_home_supersedes_a_pending_choice_for_the_same_model() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));

    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    open(&mut app);
    app.on_edit(Edit::Left);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);

    assert!(app.model_picker.start_model.is_none());
    let launch = &app.home.as_ref().expect("home").launch;
    assert_eq!(launch.model.as_deref(), Some("acme/m1"));
    assert_eq!(launch.thinking.as_deref(), Some("low"));
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [
            ("model".to_owned(), "acme/m1".to_owned()),
            ("models.\"acme/m1\".thinking".to_owned(), "low".to_owned())
        ]
    );
}

#[test]
fn s_after_enter_on_home_still_rides_the_next_start() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    // Enter saves `acme/m2` as the default, holding nothing back.
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker.start_model.is_none());
    // A later `s` holds `acme/m1` at `low` for the next `start`, over
    // the saved default.
    open(&mut app);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    app.on_edit(Edit::Left);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(seam.writes().len(), 1);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(args["model"], json!("acme/m1"));
    assert_eq!(
        args["overrides"],
        json!(["models.\"acme/m1\".thinking=low"])
    );
}

#[test]
fn acceptance_clears_a_pending_home_choice() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    // A session-only choice on home rides the next `start`.
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker.start_model.is_some());
    // Attached instead, an Enter choice saves its model once the session
    // accepts it, superseding the pending choice.
    app.attach(contract::SessionId(SESSION.to_owned()));
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert!(app.model_picker.start_model.is_some());
    app.on_line(accepted(SESSION, &id));
    assert!(app.model_picker.start_model.is_none());
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("model".to_owned(), "acme/m2".to_owned())]
    );
}

#[test]
fn enter_on_home_with_no_model_opens_the_picker_and_keeps_the_draft() {
    let mut app = home();
    app.on_line(hello());
    type_draft(&mut app, "hi");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(app.input().expand(), "hi");
    // With a model on the chip Enter starts instead.
    let mut app = home();
    app.home.as_mut().expect("home").launch.model = Some("p/m".to_owned());
    app.on_line(hello());
    type_draft(&mut app, "hi");
    let effect = app.on_key(Key::Enter, now());
    assert!(!app.model_picker_open());
    started(effect);
}

/// Runs the draft as a built-in command.
fn run_draft(app: &mut App, text: &str) -> Effect {
    type_draft(app, text);
    let now = now();
    app.on_key(Key::Enter, now)
}

/// An app attached to [`SESSION`] with the fold naming `model` at
/// `thinking`, and the three-model catalogue read.
fn thinking_app(model: &str, thinking: Option<&str>) -> (App, Arc<crate::configure_fake::Fake>) {
    let (mut app, seam) = choosing_app();
    app.on_line(preamble_line(model, thinking));
    (app, seam)
}

#[test]
fn thinking_high_acts_as_the_chip() {
    let (mut app, seam) = thinking_app("acme/m1", Some("high"));
    let line = sent(run_draft(&mut app, "/thinking high"));
    assert_eq!(line["command"], json!("model"));
    assert_eq!(
        line["args"],
        json!({"model": "acme/m1", "thinking": "high"})
    );
    assert!(app.input().expand().is_empty());
    // After acceptance only the level is written: `/thinking` never
    // saves the model.
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("models.\"acme/m1\".thinking".to_owned(), "high".to_owned())]
    );
}

#[test]
fn an_undeclared_level_is_refused_with_the_supported_list() {
    let (mut app, seam) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking max"), Effect::None);
    assert_eq!(app.notice(), Some("acme/m1 takes low, high."));
    assert!(app.model_picker.awaiting.is_empty());
    assert!(seam.writes().is_empty());
}

#[test]
fn a_model_without_levels_refuses_every_level() {
    let (mut app, _) = thinking_app("acme/m2", None);
    assert_eq!(run_draft(&mut app, "/thinking low"), Effect::None);
    assert_eq!(app.notice(), Some("acme/m2 takes no thinking level."));
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn an_unknown_word_is_refused_with_the_seven_levels() {
    let (mut app, _) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking turbo"), Effect::None);
    assert_eq!(
        app.notice(),
        Some(
            "Unknown thinking level \"turbo\"; the levels are off, minimal, low, medium, high, xhigh, max."
        )
    );
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn a_second_word_is_refused() {
    let (mut app, _) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking high extra"), Effect::None);
    assert_eq!(
        app.notice(),
        Some(
            "Unknown thinking level \"extra\"; the levels are off, minimal, low, medium, high, xhigh, max."
        )
    );
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn with_no_entry_attached_the_session_decides() {
    let (mut app, seam) = thinking_app("gone/x", None);
    let line = sent(run_draft(&mut app, "/thinking high"));
    assert_eq!(line["args"], json!({"model": "gone/x", "thinking": "high"}));
    // The session's rejection is the refusal, writing nothing.
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(refused(SESSION, &id, "invalid_arguments", "no such model."));
    assert_eq!(app.notice(), Some("no such model."));
    assert!(seam.writes().is_empty());
}

#[test]
fn with_no_entry_on_home_it_asks_for_a_read() {
    let mut app = home();
    app.home.as_mut().expect("home").launch.model = Some("gone/x".to_owned());
    assert_eq!(run_draft(&mut app, "/thinking high"), Effect::None);
    assert_eq!(
        app.notice(),
        Some("The model list is not read yet; try again in a moment.")
    );
    assert_eq!(first_code(&app), "models_loading");
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Cached));
}

#[test]
fn with_no_model_it_says_to_choose_one() {
    let (mut app, _) = choosing_app();
    assert_eq!(run_draft(&mut app, "/thinking high"), Effect::None);
    assert_eq!(app.notice(), Some("Choose a model first: Ctrl+L."));
    assert_eq!(first_code(&app), "no_model");
    assert!(app.model_picker.awaiting.is_empty());

    let mut app = home();
    assert_eq!(run_draft(&mut app, "/thinking high"), Effect::None);
    assert_eq!(app.notice(), Some("Choose a model first: Ctrl+L."));
}

#[test]
fn bare_thinking_opens_the_picker_on_the_current_models_chips() {
    let (mut app, _) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.input().expand().is_empty());
    let open = app.model_picker.open.as_ref().expect("open");
    assert_eq!(open.mode, crate::model_picker::Mode::Thinking);
    // The selection sits on the current model's row, touched, at the
    // current level.
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(chip(&app).as_deref(), Some("high"));
    assert!(open.touched.first().copied().unwrap_or(false));
    assert_eq!(
        app.model_picker_view().map(|view| view.footer),
        Some(vec![
            ("↑↓", "move"),
            ("←→", "levels"),
            ("Enter", "choose"),
            ("Tab", "all"),
            ("Ctrl+S", "session"),
            ("Ctrl+R", "refresh"),
            ("Esc", "close"),
        ])
    );
}

#[test]
fn enter_there_writes_only_the_level() {
    let (mut app, seam) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("models.\"acme/m1\".thinking".to_owned(), "high".to_owned())]
    );
}

#[test]
fn s_there_writes_nothing() {
    let (mut app, seam) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    let line = sent(ctrl_s(&mut app));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().is_empty());
}

#[test]
fn choosing_another_row_there_is_an_ordinary_choice() {
    let (mut app, seam) = thinking_app("acme/m1", Some("high"));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert!(
        seam.writes()
            .iter()
            .any(|(_, _, key, text)| { key == "model" && text == "acme/m2" })
    );
}

#[test]
fn thinking_uses_the_pending_home_choice() {
    let mut app = home();
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(vec![])) as Arc<dyn crate::Configure>
    ));
    app.on_models(Ok(three()));
    // `s` holds `acme/m1` for the next `start`: the chips still name no
    // model.
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(app.home.as_ref().expect("home").launch.model, None);
    // Bare `/thinking` opens on the pending choice's chips, not as an
    // ordinary choose.
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    assert!(app.model_picker_open());
    let open = app.model_picker.open.as_ref().expect("open");
    assert_eq!(open.mode, crate::model_picker::Mode::Thinking);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(chip(&app).as_deref(), Some("high"));
    assert!(open.touched.first().copied().unwrap_or(false));
}

#[test]
fn thinking_with_a_level_saves_for_the_pending_home_choice() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    // `/thinking high` saves only the level, for the pending model.
    assert_eq!(run_draft(&mut app, "/thinking high"), Effect::None);
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("models.\"acme/m1\".thinking".to_owned(), "high".to_owned())]
    );
}

#[test]
fn thinking_low_updates_a_pending_home_choice_and_the_next_start() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    // `s` holds `acme/m1` at high for this start only.
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(
        app.model_picker
            .start_model
            .as_ref()
            .map(|choice| choice.level.as_deref()),
        Some(Some("high"))
    );

    // `/thinking low` saves a different level without saving the model.
    assert_eq!(run_draft(&mut app, "/thinking low"), Effect::None);
    assert_eq!(
        seam.writes()
            .iter()
            .map(|(_, _, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        [("models.\"acme/m1\".thinking".to_owned(), "low".to_owned())]
    );

    // Reopening draws low as the selected chip; the same level rides the
    // pending model into the next start.
    open(&mut app);
    let view = app.model_picker_view().expect("picker view");
    let drawn_levels: Vec<&str> = view.sections[0].models[0]
        .levels
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(drawn_levels, ["low", "high"]);
    assert_eq!(view.sections[0].models[0].chip, Some(0));
    let chips = chip(&app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(
        (
            chips.as_deref(),
            drawn_levels,
            args["model"].clone(),
            args["overrides"].clone()
        ),
        (
            Some("low"),
            vec!["low", "high"],
            json!("acme/m1"),
            json!(["models.\"acme/m1\".thinking=low"])
        )
    );
}

#[test]
fn an_accepted_level_updates_only_the_matching_pending_start_model() {
    let mut app = home();
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    let held = crate::model_picker::Choice {
        reference: "acme/m1".to_owned(),
        level: Some("high".to_owned()),
        level_chosen: false,
        save_model: false,
        session_only: true,
    };
    app.model_picker.start_model = Some(held.clone());

    app.model_picker.awaiting.insert(
        "other".to_owned(),
        vec![("models.\"acme/m2\".thinking".to_owned(), "low".to_owned())],
    );
    app.model_picker_accepted("other", &contract::SessionId("s_1".to_owned()));
    assert_eq!(app.model_picker.start_model, Some(held.clone()));

    app.model_picker.awaiting.insert(
        "same".to_owned(),
        vec![("models.\"acme/m1\".thinking".to_owned(), "low".to_owned())],
    );
    app.model_picker_accepted("same", &contract::SessionId("s_1".to_owned()));
    assert_eq!(
        app.model_picker
            .start_model
            .as_ref()
            .and_then(|choice| choice.level.as_deref()),
        Some("low")
    );
}

#[test]
fn the_picker_marks_the_pending_home_choice_current() {
    let mut app = home();
    app.home.as_mut().expect("home").launch.model = Some("zeta/z1".to_owned());
    app.home.as_mut().expect("home").launch.thinking = Some("low".to_owned());
    app.on_models(Ok(three()));
    // `s` holds `acme/m1` for the next `start`, over the chips' `zeta/z1`.
    open(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    // Reopening lands on the pending choice, not the chips' model.
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(chip(&app).as_deref(), Some("high"));
}

#[test]
fn thinking_shows_the_current_row_outside_a_partial_scope() {
    let mut app = scoped_home(&["acme/m2"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(preamble_line("acme/m1", Some("high")));
    app.on_models(Ok(three()));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    // The scoped rows plus the current model's row: two rows, the
    // current one selected.
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
}

#[test]
fn thinking_shows_only_the_current_row_when_no_scoped_entry_is_installed() {
    let mut app = scoped_home(&["gone/x"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(preamble_line("acme/m1", Some("high")));
    app.on_models(Ok(three()));
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
}

#[test]
fn thinking_with_no_catalogue_entry_opens_as_choose() {
    let (mut app, _) = thinking_app("gone/x", None);
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    let open = app.model_picker.open.as_ref().expect("open");
    assert_eq!(open.mode, crate::model_picker::Mode::Choose);

    let mut app = home();
    assert_eq!(run_draft(&mut app, "/thinking"), Effect::None);
    let open = app.model_picker.open.as_ref().expect("open");
    assert_eq!(open.mode, crate::model_picker::Mode::Choose);
}

/// An app attached to [`SESSION`] with the hub up, the seam recording
/// writes, scoped to `scoped`, and the three-model catalogue read.
fn scope_app(scoped: &[&str]) -> (App, Arc<crate::configure_fake::Fake>) {
    let mut app = attached();
    app.on_line(hello());
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.model_picker.scoped = scoped.iter().map(|name| (*name).to_owned()).collect();
    app.on_models(Ok(three()));
    (app, seam)
}

/// The checklist marks, if a picker is open.
fn marks(app: &App) -> Option<Vec<bool>> {
    app.model_picker
        .open
        .as_ref()
        .map(|open| open.marks.clone())
}

#[test]
fn scoped_models_opens_marking_over_every_model() {
    let (mut app, _) = scope_app(&["acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.input().expand().is_empty());
    let open = app.model_picker.open.as_ref().expect("open");
    assert_eq!(open.mode, crate::model_picker::Mode::Scope);
    // Every installed model shows whatever the scope, with no scope
    // line and no level chips: arrows walk all three rows.
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m2"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(
        app.model_picker_view().map(|view| view.footer),
        Some(vec![
            ("↑↓", "move"),
            ("Enter", "toggle"),
            ("Ctrl+S", "save"),
            ("Esc", "close")
        ])
    );
}

#[test]
fn rows_start_marked_from_the_list() {
    let (mut app, _) = scope_app(&["acme/m1", "zeta/z1", "gone/x"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, true]));
    // With nothing saved, every row starts unmarked.
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
}

#[test]
fn enter_and_a_click_toggle_a_mark() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    // Layout rows: 0 the filter, 1 the buttons, 2 the `acme` heading,
    // 3 `acme/m1`, 5 the `zeta` heading, 6 `zeta/z1`.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    // A click on the mark toggles its row.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            3, 0
        ))),
        Effect::None
    );
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    // A click on the name selects without toggling.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            6, 1
        ))),
        Effect::None
    );
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert!(app.model_picker_open());
}

#[test]
fn ctrl_s_saves_marked_in_order_then_unlisted_old_entries() {
    let (mut app, seam) = scope_app(&["gone/x", "zeta/z1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, true]));
    // Mark `acme/m1`: the save lists marked references in catalogue
    // order, then the old list's uninstalled entries in their order.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "scoped_models".to_owned(),
            "[\"acme/m1\",\"zeta/z1\",\"gone/x\"]".to_owned()
        )]
    );
    assert_eq!(
        app.model_picker.scoped,
        vec![
            "acme/m1".to_owned(),
            "zeta/z1".to_owned(),
            "gone/x".to_owned()
        ]
    );
}

#[test]
fn ctrl_s_saves_marks_the_query_hides() {
    let (mut app, seam) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    // Mark `acme/m1`, then filter it out: the save still takes every
    // mark, shown or hidden.
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert_eq!(press(&mut app, "z"), Effect::None);
    assert_eq!(app.model_picker.query(), "z");
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, true]));
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "scoped_models".to_owned(),
            "[\"acme/m1\",\"zeta/z1\"]".to_owned()
        )]
    );
}

#[test]
fn enter_with_no_match_in_the_checklist_toggles_nothing() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(app.model_picker.query(), "qqq");
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert!(app.model_picker_open());
    assert!(seam.writes().is_empty());
}

#[test]
fn esc_clears_the_checklist_query_then_closes_saving_nothing() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press(&mut app, "z"), Effect::None);
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, true]));
    // The first Esc clears the query, keeping the picker open and the
    // marks held.
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert_eq!(app.model_picker.query(), "");
    assert!(app.model_picker_open());
    assert_eq!(marks(&app), Some(vec![true, false, true]));
    // The next Esc closes it, saving nothing.
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(!app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
}

#[test]
fn a_rebound_session_only_saves_the_checklist() {
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"session_only": "x"})).unwrap();
    let (mut app, seam) = scope_app(&[]);
    app.set_keys(crate::KeysSetup { user });
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    // The displaced Ctrl+S does nothing on the checklist now.
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.scoped.is_empty());
    // The rebound key saves the marked list and closes.
    assert_eq!(press(&mut app, "x"), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "scoped_models".to_owned(),
            "[\"acme/m1\"]".to_owned()
        )]
    );
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
}

#[test]
fn ctrl_r_refreshes_in_the_checklist() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press(&mut app, "z"), Effect::None);
    // The refresh key still asks `Every`, keeping the query.
    assert_eq!(press(&mut app, "ctrl+r"), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    assert_eq!(app.model_picker.query(), "z");
    assert!(app.model_picker_open());
}

#[test]
fn marking_none_clears_even_with_an_unavailable_old_entry() {
    // The old list holds `gone/x`, which is not installed, and
    // `acme/m1`, which is: with neither marked the save writes `[]`,
    // dropping every old entry.
    let (mut app, seam) = scope_app(&["gone/x", "acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "scoped_models".to_owned(),
            "[]".to_owned()
        )]
    );
    assert!(app.model_picker.scoped.is_empty());
}

#[test]
fn esc_saves_nothing() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
}

#[test]
fn text_after_the_command_is_ignored() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(
        run_draft(&mut app, "/scoped-models extra words"),
        Effect::None
    );
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(crate::model_picker::Mode::Scope)
    );
    assert!(app.input().expand().is_empty());
}

#[test]
fn the_saved_list_scopes_the_next_open() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
    // Choosing next lists only the saved entry until "show all".
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    // The checklist next starts marked from the saved list.
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
}

#[test]
fn saving_on_home_writes_at_once_and_sets_the_saved_list() {
    let mut app = scoped_home(&[]);
    app.set_configure(Some(
        Arc::new(crate::configure_fake::Fake::new(vec![])) as Arc<dyn crate::Configure>
    ));
    app.on_models(Ok(three()));
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
    assert_eq!(
        app.home
            .as_ref()
            .map(|home| home.launch.scoped_models.clone()),
        Some(vec!["acme/m1".to_owned()])
    );
}

#[test]
fn space_does_nothing_outside_the_checklist() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let before = selected(&app);
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(selected(&app), before);
    assert_eq!(marks(&app), Some(Vec::new()));
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn tab_and_chips_do_nothing_in_the_checklist() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| !open.show_all)
    );
    // The rows have no chips to move.
    app.on_edit(Edit::Left);
    app.on_edit(Edit::Right);
    assert!(
        app.model_picker
            .open
            .as_ref()
            .is_some_and(|open| open.touched.iter().all(|touched| !touched))
    );
    // Tab and the chip keys choose nothing: the checklist stays open,
    // sending and writing nothing.
    assert!(app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn a_failed_scope_write_is_a_notice_and_keeps_the_list() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    *seam.refuse.lock().expect("the refusal") = Some("locked".to_owned());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(app.notice(), Some("Saving scoped_models failed: locked"));
    assert_eq!(app.model_picker.scoped, vec!["acme/m1".to_owned()]);
    assert!(!app.model_picker_open());
}

#[test]
fn saving_without_a_seam_says_nothing_changed() {
    let mut app = attached();
    app.on_line(hello());
    app.on_models(Ok(three()));
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Saving is not available; nothing changed.")
    );
    assert!(app.model_picker.scoped.is_empty());
    assert!(!app.model_picker_open());
}

#[test]
fn opening_before_the_first_read_marks_toggles_and_saves() {
    let mut app = attached();
    app.on_line(hello());
    let seam = Arc::new(crate::configure_fake::Fake::new(vec![]));
    app.set_configure(Some(seam.clone() as Arc<dyn crate::Configure>));
    app.model_picker.scoped = vec!["acme/m1".to_owned(), "gone/x".to_owned()];
    // Opening before any catalogue arrives shows no rows yet, and the
    // draft is cleared.
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert!(app.model_picker_open());
    assert!(app.input().expand().is_empty());
    assert_eq!(marks(&app), Some(Vec::new()));
    // The first read marks the rows from the saved list.
    app.on_models(Ok(three()));
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    // Enter toggles the selected row; Ctrl+S saves the marked list,
    // keeping the old list's uninstalled entries.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(!app.model_picker_open());
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "scoped_models".to_owned(),
            "[\"acme/m1\",\"gone/x\"]".to_owned()
        )]
    );
    assert_eq!(
        app.model_picker.scoped,
        vec!["acme/m1".to_owned(), "gone/x".to_owned()]
    );
}

/// Presses `name` through the bindings.
fn press(app: &mut App, name: &str) -> Effect {
    app.on_press(Stroke::parse(name).unwrap(), now())
}

#[test]
fn bare_letters_extend_the_query() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(press(&mut app, "s"), Effect::None);
    assert_eq!(press(&mut app, "r"), Effect::None);
    assert_eq!(press(&mut app, "a"), Effect::None);
    assert_eq!(app.model_picker.query(), "sra");
    assert!(app.model_picker_open());
}

#[test]
fn esc_with_a_query_clears_it_and_stays_open() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(press(&mut app, "s"), Effect::None);
    assert_eq!(app.model_picker.query(), "s");
    // The first Esc clears the query, keeping the picker open.
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert_eq!(app.model_picker.query(), "");
    assert!(app.model_picker_open());
    // The next Esc closes it.
    assert_eq!(press(&mut app, "esc"), Effect::None);
    assert!(!app.model_picker_open());
}

#[test]
fn enter_with_no_match_stays_open_and_sends_nothing() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(press(&mut app, "q"), Effect::None);
    assert_eq!(press(&mut app, "enter"), Effect::None);
    assert!(app.model_picker_open());
}

#[test]
fn ctrl_r_with_a_query_asks_every_and_keeps_the_query() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(press(&mut app, "m"), Effect::None);
    assert_eq!(press(&mut app, "ctrl+r"), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    assert_eq!(app.model_picker.query(), "m");
    assert!(app.model_picker_open());
}

#[test]
fn typing_in_the_checklist_filters_with_space_in_the_query() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    // Typing filters the checklist, Space included in the query.
    assert_eq!(press(&mut app, "m"), Effect::None);
    assert_eq!(app.model_picker.query(), "m");
    assert_eq!(press(&mut app, "space"), Effect::None);
    assert_eq!(app.model_picker.query(), "m ");
    assert_eq!(press(&mut app, "1"), Effect::None);
    assert_eq!(app.model_picker.query(), "m 1");
    // Only `acme/m1` matches both words: "m" matches the `acme` rows
    // but not `zeta/z1`, while "a" alone would also match `zeta`.
    let ids: Vec<String> = app
        .model_picker_view()
        .expect("open")
        .sections
        .iter()
        .flat_map(|section| section.models.iter().map(|model| model.id.clone()))
        .collect();
    assert_eq!(ids, ["m1".to_owned()]);
    assert!(app.model_picker_open());
    // Backspace shortens the query, keeping the trailing Space.
    assert_eq!(press(&mut app, "backspace"), Effect::None);
    assert_eq!(app.model_picker.query(), "m ");
    assert!(app.model_picker_open());
}

/// The session-only `model` command a Ctrl+S sends, with its id.
fn session_only_id(app: &mut App) -> String {
    open(app);
    let line = sent(ctrl_s(app));
    assert_eq!(line["command"], json!("model"));
    line["id"].as_str().expect("a command id").to_owned()
}

#[test]
fn a_session_only_accept_sets_the_row() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    assert!(app.model_picker.only_pending.contains_key(&id));
    app.on_line(accepted(SESSION, &id));
    assert_eq!(
        app.model_picker.session_only,
        Some((
            contract::SessionId(SESSION.to_owned()),
            "acme/m1".to_owned()
        ))
    );
    assert!(!app.model_picker.only_pending.contains_key(&id));
    // The row draws its third row.
    open(&mut app);
    let view = app.model_picker_view().expect("open");
    let only: Vec<&str> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .filter(|model| model.session_only)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(only, ["m1"]);
}

#[test]
fn a_session_only_rejection_drops_the_wait() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    app.on_line(refused(SESSION, &id, "invalid_arguments", "gone."));
    assert!(!app.model_picker.only_pending.contains_key(&id));
    // The late accept answers nothing now.
    app.on_line(accepted(SESSION, &id));
    assert_eq!(app.model_picker.session_only, None);
}

#[test]
fn a_saved_choice_accepted_clears_the_session_only_mark() {
    let (mut app, _) = choosing_app();
    let first = session_only_id(&mut app);
    app.on_line(accepted(SESSION, &first));
    assert!(app.model_picker.session_only.is_some());
    // Enter saves the model: its accept clears the mark.
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let second = line["id"].as_str().expect("a command id").to_owned();
    assert_ne!(first, second);
    app.on_line(accepted(SESSION, &second));
    assert_eq!(app.model_picker.session_only, None);
}

#[test]
fn a_switch_before_the_accept_sets_nothing() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    app.go_home();
    app.attach(contract::SessionId(OTHER.to_owned()));
    app.on_line(accepted(SESSION, &id));
    assert_eq!(app.model_picker.session_only, None);
    assert!(!app.model_picker.only_pending.contains_key(&id));
}

#[test]
fn another_sessions_accept_for_the_wait_is_ignored() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    app.on_line(accepted(OTHER, &id));
    assert_eq!(app.model_picker.session_only, None);
    assert!(app.model_picker.only_pending.contains_key(&id));
    // Its own session's accept still names the row.
    app.on_line(accepted(SESSION, &id));
    assert!(app.model_picker.session_only.is_some());
}

#[test]
fn the_home_session_only_choice_marks_its_row() {
    let mut app = home();
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(ctrl_s(&mut app), Effect::None);
    assert!(app.model_picker.start_model.is_some());
    // Reopening marks only the choice the next start carries.
    open(&mut app);
    let view = app.model_picker_view().expect("open");
    let only: Vec<&str> = view
        .sections
        .iter()
        .flat_map(|section| section.models.iter())
        .filter(|model| model.session_only)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(only, ["m1"]);
}

#[test]
fn leaving_a_session_drops_its_session_only_wait_and_mark() {
    let (mut app, _) = choosing_app();
    let first = session_only_id(&mut app);
    app.on_line(accepted(SESSION, &first));
    assert!(app.model_picker.session_only.is_some());
    let second = session_only_id(&mut app);
    app.go_home();
    assert!(app.model_picker.only_pending.is_empty());
    assert_eq!(app.model_picker.session_only, None);
    app.on_line(accepted(SESSION, &second));
    assert_eq!(app.model_picker.session_only, None);
}

#[test]
fn attaching_another_session_drops_its_session_only_wait_and_mark() {
    let (mut app, _) = choosing_app();
    let first = session_only_id(&mut app);
    app.on_line(accepted(SESSION, &first));
    let second = session_only_id(&mut app);
    app.attach(contract::SessionId(OTHER.to_owned()));
    assert!(app.model_picker.only_pending.is_empty());
    assert_eq!(app.model_picker.session_only, None);
    app.on_line(accepted(SESSION, &second));
    assert_eq!(app.model_picker.session_only, None);
}

#[test]
fn reattaching_the_same_session_keeps_its_session_only_wait() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(app.model_picker.only_pending.contains_key(&id));
    app.on_line(accepted(SESSION, &id));
    assert!(app.model_picker.session_only.is_some());
}

#[test]
fn reattaching_the_same_session_keeps_its_session_only_mark() {
    let (mut app, _) = choosing_app();
    let id = session_only_id(&mut app);
    app.on_line(accepted(SESSION, &id));
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert_eq!(
        app.model_picker.session_only,
        Some((
            contract::SessionId(SESSION.to_owned()),
            "acme/m1".to_owned()
        ))
    );
}

#[test]
fn take_reads_records_the_workspace_in_use() {
    use crate::catalogue::Refresh;
    use crate::configure_fake::Fake;
    use crate::model_picker::Mode;
    use std::sync::Arc;
    let mut app = home();
    let router = crate::remote::Router::new();
    let remote = Arc::new(crate::remote::Remote::new(
        Arc::new(Fake::new(vec![])),
        PathBuf::from("/client"),
        router,
    ));
    app.set_remote(Arc::clone(&remote));
    app.open_model_picker(Mode::Choose);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(remote.reading(), Some(PathBuf::from("/w")));
    // Without an owed read the recorded workspace is left alone.
    assert_eq!(app.take_reads(), None);
    assert_eq!(remote.reading(), Some(PathBuf::from("/w")));
    // After a workspace choice the next read uses it.
    app.choose_workspace("/hub/p".to_owned());
    app.open_model_picker(Mode::Choose);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(remote.reading(), Some(PathBuf::from("/hub/p")));
}

#[test]
fn take_reads_without_a_remote_is_unchanged() {
    use crate::catalogue::Refresh;
    use crate::model_picker::Mode;
    let mut app = home();
    app.open_model_picker(Mode::Choose);
    assert_eq!(app.take_reads(), Some(Refresh::Stale));
    assert_eq!(app.take_reads(), None);
}

// The hub-backed chip read and start pick: a scripted hub answers the
// wire while the app folds on its own thread, so a desync hangs loudly
// instead of passing silently.

use crate::configure_fake::Fake;
use crate::remote::{Remote, Router};
use fakes::Deadline;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

/// A remote app on home at 80x24, with no remote installed yet.
fn remote_home_named(model: Option<&str>, thinking: Option<&str>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: model.map(|model| model.to_owned()),
        thinking: thinking.map(|thinking| thinking.to_owned()),
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

fn hub_hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A remote app whose hub answers `script` on its own thread: each
/// `(command, args)` read off the wire must match, and is answered with
/// the result, or rejected with the code and message. The end of the
/// script arrives on `done` once the app has asked everything. The
/// watcher's hub end proves quiet waits: reads on it time out while
/// the app writes nothing. The player waits for `start` before its
/// first read, so it never parks on the shared socket while a quiet
/// wait holds a read timeout: the timeout could otherwise fire on the
/// parked player instead.
struct Hub {
    done: mpsc::Receiver<()>,
    watcher: UnixStream,
    start: mpsc::Sender<()>,
}

impl Hub {
    /// Releases the player thread: after the quiet waits before the
    /// first fold, and before that fold.
    fn start(&self) {
        self.start.send(()).unwrap_or(());
    }
}

fn scripted_remote(script: Vec<(String, Value, Result<Value, Rejection>)>) -> (App, Hub) {
    scripted_remote_named(script, None, None)
}

/// [`scripted_remote`] with the chips starting named, as flags name them.
fn scripted_remote_named(
    script: Vec<(String, Value, Result<Value, Rejection>)>,
    model: Option<&str>,
    thinking: Option<&str>,
) -> (App, Hub) {
    let router = Router::new();
    let (client, hub) = UnixStream::pair().unwrap();
    let watcher = hub.try_clone().unwrap();
    router.attach(client);
    let remote = Arc::new(Remote::new(
        Arc::new(Fake::new(vec![])),
        PathBuf::from("/client"),
        Arc::clone(&router),
    ));
    let (served, done) = mpsc::channel();
    let (start, release) = mpsc::channel();
    thread::spawn(move || {
        release.recv().unwrap_or(());
        let mut reader = BufReader::new(hub);
        for (command, args, answer) in script {
            let mut text = String::new();
            reader.read_line(&mut text).unwrap();
            let line: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(line["command"], json!(command));
            assert_eq!(line["args"], args);
            let accepted = answer.is_ok();
            let payload = match answer {
                Ok(result) => json!({"command_id": line["id"], "result": result}),
                Err((code, message)) => json!({
                    "command_id": line["id"],
                    "code": code,
                    "message": message,
                }),
            };
            let answer = Line::Hub(contract::HubLine {
                kind: if accepted {
                    "command_accepted".to_owned()
                } else {
                    "command_rejected".to_owned()
                },
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                payload: payload.as_object().cloned().unwrap_or_default(),
            });
            assert!(router.route(&answer));
        }
        served.send(()).unwrap();
    });
    let mut app = remote_home_named(model, thinking);
    app.set_remote(Arc::clone(&remote));
    app.set_models(Some(remote.catalogue_reader()));
    (
        app,
        Hub {
            done,
            watcher,
            start,
        },
    )
}

/// Folds `line` on a thread of its own: the chip and start reads block
/// for the scripted hub, so the test thread cannot fold them directly.
fn fold(mut app: App, line: Line) -> (App, Vec<String>) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outgoing = app.on_line(line);
        done.send((app, outgoing)).unwrap();
    });
    Deadline::start().recv_or_fail(&finished, "hub fold")
}

/// Saves the checklist on a thread of its own: the save blocks for
/// the scripted hub, so the test thread cannot save it directly.
fn saving(mut app: App) -> (App, Effect) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let effect = ctrl_s(&mut app);
        done.send((app, effect)).unwrap();
    });
    Deadline::start().recv_or_fail(&finished, "scope save")
}

/// Chooses `workspace` on a thread of its own: with the link up the
/// choice reads the chips at once, blocking for the scripted hub.
fn choosing(mut app: App, workspace: &str) -> App {
    let workspace = workspace.to_owned();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        app.choose_workspace(workspace);
        done.send(app).unwrap();
    });
    Deadline::start().recv_or_fail(&finished, "workspace choice")
}

fn chip_rows() -> Value {
    json!({"rows": [
        {"key": "model", "shown": "value", "value": "\"hub/m\"",
         "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
        {"key": "models.hub/m.thinking", "shown": "value", "value": "\"high\"",         "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
        {"key": "scoped_models", "shown": "value", "value": "[\"hub/m\"]",
         "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
    ]})
}

/// The hub's import-pending answer: the offer resolve asks it
/// first on every remote hello, so every scripted wire opens
/// with it. Denied stays down without touching the pick.
fn pending() -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "import_pending"}),
        Ok(json!({"pending": false})),
    )
}

fn global_file() -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "global_file"}),
        Ok(json!({"file": "/hub/config.json"})),
    )
}

fn chip_settings_at(workspace: &str) -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "settings", "workspace": workspace}),
        Ok(chip_rows()),
    )
}

fn chip_settings() -> (String, Value, Result<Value, Rejection>) {
    chip_settings_at("/hub")
}

fn press_key(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

fn home_chips(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.chips.into_iter().map(|chip| chip.text).collect())
        .unwrap_or_default()
}

use std::io::Read as _;

/// A hub rejection's code and message.
type Rejection = (String, String);

/// Asserts the hub reads nothing: the app wrote no line. The fold,
/// save or choice the test just took joined its thread, so every
/// synchronous write finished; a nonblocking read proves the wire is
/// empty without waiting on the clock. The watcher shares its socket
/// with the scripted player, so the read is restored to blocking
/// after: the player's reads never inherit it.
fn quiet(watcher: &UnixStream) {
    watcher.set_nonblocking(true).unwrap();
    let mut byte = [0; 1];
    let idle = matches!(
        (&*watcher).read(&mut byte),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock
    );
    watcher.set_nonblocking(false).unwrap();
    assert!(idle, "the app wrote a line it owes nothing");
}

/// A `settings` answer naming only the scoped list: the chip has no
/// model, so an owed start pick opens.
fn chip_settings_scoped(scoped: &str) -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "settings", "workspace": "/hub"}),
        Ok(json!({"rows": [
            {"key": "scoped_models", "shown": "value", "value": scoped,
             "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
        ]})),
    )
}

/// A `settings` rejection with the hub's code and message.
fn chip_rejection(code: &str, message: &str) -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "settings", "workspace": "/hub"}),
        Err((code.to_owned(), message.to_owned())),
    )
}

/// A `settings` answer at `workspace` naming only the scoped list:
/// the chip has no model, so an owed start pick opens.
fn chip_settings_scoped_at(
    workspace: &str,
    scoped: &str,
) -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({"operation": "settings", "workspace": workspace}),
        Ok(json!({"rows": [
            {"key": "scoped_models", "shown": "value", "value": scoped,
             "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
        ]})),
    )
}

/// A `models` answer holding `references`, each with one level.
fn models_with(references: &[&str]) -> (String, Value, Result<Value, Rejection>) {
    models_with_at("/hub", references)
}

/// A `models` answer at `workspace` holding `references`, each with
/// one level.
fn models_with_at(
    workspace: &str,
    references: &[&str],
) -> (String, Value, Result<Value, Rejection>) {
    let models: Vec<Value> = references
        .iter()
        .map(|reference| {
            let (_, id) = reference.split_once('/').unwrap();
            json!({"reference": reference, "provider": "hub", "id": id,
                   "levels": ["high"], "roles": ["main"]})
        })
        .collect();
    (
        "configure".to_owned(),
        json!({"operation": "models", "workspace": workspace, "refresh": "stale"}),
        Ok(json!({"models": models, "notices": [], "lists": []})),
    )
}

/// The `set` answer a scope save decodes.
fn set_saved() -> (String, Value, Result<Value, Rejection>) {
    (
        "configure".to_owned(),
        json!({
            "operation": "set", "workspace": "/hub", "layer": "global",
            "key": "scoped_models", "text": "[\"acme/m1\",\"gone/x\"]",
        }),
        Ok(json!({"file": "/hub/config.json", "warnings": []})),
    )
}

/// The open picker's mode, if one is open.
fn open_mode(app: &App) -> Option<crate::model_picker::Mode> {
    app.model_picker.open.as_ref().map(|open| open.mode)
}

#[test]
fn the_chip_read_runs_before_the_start_pick_and_not_twice() {
    let (mut app, hub) = scripted_remote(vec![pending(), global_file(), chip_settings()]);
    // Before the link the start pick owes but writes nothing.
    app.start_model_pick();
    assert!(app.model_picker.open.is_none());
    quiet(&hub.watcher);
    // The first hello reads the chips: the hub named a model, so the
    // owed pick drops and no `models` line goes out.
    hub.start();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    let chips = home_chips(&app);
    assert!(chips.contains(&"[hub/m]".to_owned()), "{chips:?}");
    assert!(chips.contains(&"[thinking: high]".to_owned()), "{chips:?}");
    assert_eq!(app.model_picker.scoped, vec!["hub/m".to_owned()]);
    // A second hello reads nothing further.
    let (app, _) = fold(app, hub_hello());
    assert!(!app.chip_read_owed());
    quiet(&hub.watcher);
    quiet(&hub.watcher);
}

#[test]
fn the_start_pick_drops_when_the_chip_names_a_model() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings(),
        chip_settings(),
    ]);
    app.owe_start_pick();
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    assert!(app.model_picker.open.is_none());
    // Closing nothing and reconnecting opens nothing either: the pick
    // was taken, while the chips read again over the cached global.
    app.disconnected();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert!(app.model_picker.open.is_none());
    quiet(&hub.watcher);
}

#[test]
fn the_start_pick_opens_once_when_the_chip_names_no_model() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings_scoped("[\"hub/m\",\"hub/n\"]"),
        models_with(&["hub/m", "hub/n"]),
        chip_settings_scoped("[\"hub/m\",\"hub/n\"]"),
    ]);
    app.owe_start_pick();
    hub.start();
    let (app, _) = fold(app, hub_hello());
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    assert_eq!(selected(&app).as_deref(), Some("hub/m"));
    // A later hello neither reads nor reopens: the pick was taken.
    let selected_before = app.model_picker.open.as_ref().map(|open| open.selected);
    let (mut app, _) = fold(app, hub_hello());
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.selected),
        selected_before
    );
    // Closing it and reconnecting opens nothing: the flag stays taken.
    assert_eq!(press_key(&mut app, Key::Esc), Effect::None);
    assert!(app.model_picker.open.is_none());
    app.disconnected();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert!(app.model_picker.open.is_none());
    quiet(&hub.watcher);
}

#[test]
fn explicit_opens_run_at_once_while_the_chip_read_is_owed() {
    for mode in [
        crate::model_picker::Mode::Thinking,
        crate::model_picker::Mode::Scope,
        crate::model_picker::Mode::Choose,
    ] {
        let (mut app, hub) = scripted_remote(vec![
            pending(),
            global_file(),
            chip_settings_scoped("[\"hub/m\"]"),
        ]);
        // The seeded catalogue holds the chip's model, so a thinking
        // open lands on its row instead of falling back to Choose.
        app.on_models(Ok(three()));
        app.open_model_picker(mode);
        assert_eq!(open_mode(&app), Some(mode));
        let selected_before = selected(&app);
        quiet(&hub.watcher);
        // The hello's answer, with no model, neither closes the open
        // picker nor opens a second one.
        hub.start();
        let (app, _) = fold(app, hub_hello());
        Deadline::start().recv_or_fail(&hub.done, "wire script");
        assert_eq!(open_mode(&app), Some(mode));
        // The open is unchanged: its mode, selection and touched set.
        assert_eq!(selected(&app), selected_before);
        let touched = if mode == crate::model_picker::Mode::Scope {
            vec![false, false, false]
        } else {
            Vec::new()
        };
        assert_eq!(
            app.model_picker
                .open
                .as_ref()
                .map(|open| open.toggled.clone()),
            Some(touched)
        );
        drop(hub);
        drop(app);
    }
}

#[test]
fn explicit_opens_by_key_and_click_run_at_once_while_owed() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings_scoped("[\"hub/m\"]"),
    ]);
    app.on_models(Ok(three()));
    assert_eq!(press_key(&mut app, Key::CtrlL), Effect::None);
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    quiet(&hub.watcher);
    hub.start();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    quiet(&hub.watcher);
}

#[test]
fn an_explicit_open_drops_the_owed_start_pick() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings_scoped("[\"hub/m\"]"),
    ]);
    app.owe_start_pick();
    app.open_model_picker(crate::model_picker::Mode::Choose);
    // The hello answers with no model, but the pick is gone: the open
    // picker keeps its mode, and two Esc closes leave no picker.
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    assert_eq!(press_key(&mut app, Key::Esc), Effect::None);
    assert_eq!(press_key(&mut app, Key::Esc), Effect::None);
    assert!(app.model_picker.open.is_none());
    quiet(&hub.watcher);
}

#[test]
fn a_rejected_chip_read_notices_once_and_waits_for_reconnect() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_rejection("invalid_arguments", "bad settings"),
        // The reconnect reads again over the cached global file.
        chip_settings_scoped("[\"hub/m\"]"),
        models_with(&["hub/m"]),
    ]);
    app.owe_start_pick();
    hub.start();
    let (app, _) = fold(app, hub_hello());
    // Exactly one notice, with the hub's code and text: the chip keeps
    // its text, and the failed pick stays owed.
    let lines = app.notices.lines();
    assert_eq!(lines.len(), 1);
    let crate::app::NoticeLine::Notice { code, text, .. } = &lines[0] else {
        panic!("one notice line");
    };
    assert_eq!(code, "invalid_arguments");
    assert_eq!(text, "bad settings");
    assert!(app.chip_read_owed());
    assert!(app.model_picker.open.is_none());
    // No second `settings` on the next hello: the failure waits.
    let (mut app, _) = fold(app, hub_hello());
    assert_eq!(app.notices.lines().len(), 1);
    // A reconnect reads again, and with no model the owed pick opens.
    app.disconnected();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    quiet(&hub.watcher);
}

#[test]
fn choosing_a_workspace_rereads_the_chips_at_once() {
    let (app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings(),
        chip_settings_at("/hub/p"),
    ]);
    hub.start();
    let (app, _) = fold(app, hub_hello());
    assert!(home_chips(&app).contains(&"[hub/m]".to_owned()));
    // With the link up the choice reads at once at the new workspace:
    // nothing stays owed, and the chips are the hub's. Both fire before
    // the hang guard below under the inverted link check.
    let app = choosing(app, "/hub/p");
    assert!(!app.chip_read_owed());
    assert!(home_chips(&app).contains(&"[hub/m]".to_owned()));
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    quiet(&hub.watcher);
}

#[test]
fn choosing_a_workspace_while_down_reads_at_the_next_hello() {
    let (mut app, hub) = scripted_remote(vec![pending(), chip_settings_at("/hub/p")]);
    // The player runs before the choice, so a wrong-state choice answers
    // at once instead of parking the test thread: the asserts below fail
    // before any hang guard. With the link down the choice writes nothing
    // and only owes; the chosen workspace reads directly, with no
    // `global_file`.
    hub.start();
    app.choose_workspace("/hub/p".to_owned());
    assert!(app.chip_read_owed());
    assert!(!app.chip_read_failed());
    let (app, _) = fold(app, hub_hello());
    assert!(home_chips(&app).contains(&"[hub/m]".to_owned()));
    assert!(!app.chip_read_owed());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    quiet(&hub.watcher);
}

#[test]
fn flagged_keys_survive_the_chip_read() {
    // The layer names `model=flag/m`: the answer never replaces it,
    // while thinking, unnamed, is replaced.
    let (mut app, hub) = scripted_remote_named(
        vec![pending(), global_file(), chip_settings()],
        Some("flag/m"),
        None,
    );
    app.set_launch_overrides(vec!["model=flag/m".to_owned()]);
    hub.start();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    let chips = home_chips(&app);
    assert!(chips.contains(&"[flag/m]".to_owned()), "{chips:?}");
    assert!(chips.contains(&"[thinking: high]".to_owned()), "{chips:?}");
    quiet(&hub.watcher);
    // With `thinking=low` named, thinking is kept too.
    let (mut app, hub) = scripted_remote_named(
        vec![pending(), global_file(), chip_settings()],
        None,
        Some("low"),
    );
    app.set_launch_overrides(vec!["thinking=low".to_owned()]);
    hub.start();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    let chips = home_chips(&app);
    assert!(chips.contains(&"[hub/m]".to_owned()), "{chips:?}");
    assert!(!chips.iter().any(|chip| chip.contains("high")), "{chips:?}");
    quiet(&hub.watcher);
}

#[test]
fn a_local_start_pick_opens_at_once() {
    let mut app = home();
    app.start_model_pick();
    assert!(app.model_picker_open());
    // With no hub, hellos and choices write nothing extra; the hello
    // answers the owed chip read locally, so none stays owed.
    let (app, _) = fold(app, hub_hello());
    assert!(app.model_picker_open());
    assert!(!app.chip_read_owed());
    drop(app);
}

#[test]
fn the_start_pick_opens_over_the_workspace_picker() {
    let (app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings_scoped("[\"hub/m\",\"hub/n\"]"),
        models_with(&["hub/m", "hub/n"]),
    ]);
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    // The workspace picker opens first: Down focuses the chip row,
    // Enter opens it.
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    let focus_before = app
        .home_screen()
        .and_then(|screen| screen.picker)
        .and_then(|picker| picker.focus);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_some())
    );
    // The owed pick opens over it once the next hello runs it.
    app.owe_start_pick();
    let (mut app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    assert_eq!(selected(&app).as_deref(), Some("hub/m"));
    // Down moves the model picker's row, not the workspace picker's.
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("hub/n"));
    assert_eq!(
        app.home_screen()
            .and_then(|screen| screen.picker)
            .and_then(|picker| picker.focus),
        focus_before
    );
    // Esc closes the model picker with the workspace picker behind it.
    assert_eq!(press_key(&mut app, Key::Esc), Effect::None);
    assert!(!app.model_picker_open());
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_some())
    );
    quiet(&hub.watcher);
}

#[test]
fn a_failed_read_then_a_reconnect_opens_the_pick_once() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_rejection("invalid_arguments", "bad settings"),
        chip_settings_scoped("[\"hub/m\"]"),
        models_with(&["hub/m"]),
    ]);
    app.owe_start_pick();
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    assert!(app.model_picker.open.is_none());
    app.disconnected();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    quiet(&hub.watcher);
}

#[test]
fn the_deferred_start_pick_reads_models_at_the_chosen_workspace() {
    // The first chip read fails, so the pick stays owed; choosing a
    // workspace answers the next chip read with no model, and the
    // owed pick opens over its catalogue.
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_rejection("invalid_arguments", "bad settings"),
        chip_settings_scoped_at("/hub/p", "[\"hub/m\"]"),
        models_with_at("/hub/p", &["hub/m"]),
    ]);
    app.owe_start_pick();
    hub.start();
    let (app, _) = fold(app, hub_hello());
    assert!(app.model_picker.open.is_none());
    assert!(app.chip_read_failed());
    // With the link up the choice reads the chips at once; the player
    // checks each line's workspace, so a stale catalogue workspace
    // fails the script here and below.
    let app = choosing(app, "/hub/p");
    assert!(!app.chip_read_owed());
    // The next hello runs the owed pick: it opens over the catalogue
    // read at the chosen workspace.
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    assert_eq!(selected(&app).as_deref(), Some("hub/m"));
    assert_eq!(
        app.remote().map(|remote| remote.reading()),
        Some(Some(std::path::PathBuf::from("/hub/p")))
    );
    quiet(&hub.watcher);
}

#[test]
fn a_failed_read_then_an_explicit_open_stays_single() {
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_rejection("invalid_arguments", "bad settings"),
        chip_settings_scoped("[\"hub/m\"]"),
    ]);
    app.owe_start_pick();
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    assert!(app.model_picker.open.is_none());
    // Ctrl+L opens the picker; the re-read answers with no model, but
    // the pick is gone, so no second picker opens.
    assert_eq!(press_key(&mut app, Key::CtrlL), Effect::None);
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    app.disconnected();
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(open_mode(&app), Some(crate::model_picker::Mode::Choose));
    quiet(&hub.watcher);
}

#[test]
fn the_chip_answer_keeps_each_toggle() {
    // The chosen workspace reads directly, with no `global_file`.
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        (
            "configure".to_owned(),
            json!({"operation": "settings", "workspace": "/hub"}),
            Ok(json!({"rows": [
                {"key": "scoped_models", "shown": "value",
                 "value": "[\"acme/m1\",\"acme/m2\",\"gone/x\"]",
                 "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
            ]})),
        ),
        set_saved(),
    ]);
    // The seeded catalogue holds rows 1, 2 and 3; the placeholder
    // names row 2 only, so row 2 opens marked.
    app.on_models(Ok(three()));
    app.model_picker.scoped = vec!["acme/m2".to_owned()];
    // The remote home opens with the workspace picker showing; choosing
    // the hub's workspace closes it, owing the chips without writing.
    // The player runs first, so a wrong-state choice answers at once and
    // the asserts below fail before any hang guard.
    hub.start();
    app.choose_workspace("/hub".to_owned());
    assert!(!app.chip_read_failed());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, true, false]));
    // The person moves to row 2 and toggles it off: it joins the
    // toggled rows.
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    // The hello's answer lists rows 1 and 2, with an uninstalled
    // `gone/x`: row 1 adopts the answer, row 2 keeps the person's
    // mark, row 3 stays unmarked.
    let (app, _) = fold(app, hub_hello());
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    let (app, effect) = saving(app);
    assert_eq!(effect, Effect::None);
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert!(!app.model_picker_open());
    quiet(&hub.watcher);
}

#[test]
fn a_twice_toggled_row_keeps_its_mark_against_an_answer_omitting_it() {
    // The chosen workspace reads directly, with no `global_file`.
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        (
            "configure".to_owned(),
            json!({"operation": "settings", "workspace": "/hub"}),
            Ok(json!({"rows": [
                {"key": "scoped_models", "shown": "value", "value": "[\"acme/m1\"]",
                 "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
            ]})),
        ),
    ]);
    app.on_models(Ok(three()));
    app.model_picker.scoped = vec!["acme/m2".to_owned()];
    // The remote home opens with the workspace picker showing; choosing
    // the hub's workspace closes it, owing the chips without writing.
    // The player runs first, so a wrong-state choice answers at once and
    // the asserts below fail before any hang guard.
    hub.start();
    app.choose_workspace("/hub".to_owned());
    assert!(!app.chip_read_failed());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    // Toggling row 2 twice leaves it marked; the answer omits row 2,
    // but the touched row keeps its mark.
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, true, false]));
    let (app, _) = fold(app, hub_hello());
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert_eq!(marks(&app), Some(vec![true, true, false]));
    quiet(&hub.watcher);
}

#[test]
fn a_failed_chip_read_keeps_the_placeholders_marks() {
    // The chosen workspace reads directly, with no `global_file`;
    // the save writes the marked list, here empty.
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        chip_rejection("invalid_arguments", "bad settings"),
        (
            "configure".to_owned(),
            json!({
                "operation": "set", "workspace": "/hub", "layer": "global",
                "key": "scoped_models", "text": "[]",
            }),
            Ok(json!({"file": "/hub/config.json", "warnings": []})),
        ),
    ]);
    app.on_models(Ok(three()));
    app.model_picker.scoped = vec!["acme/m2".to_owned()];
    // The player runs first, so a wrong-state choice answers at once and
    // the assert below fails before any hang guard.
    hub.start();
    app.choose_workspace("/hub".to_owned());
    assert!(!app.chip_read_failed());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    // The rejection notices once and leaves the marks as they were.
    let (app, _) = fold(app, hub_hello());
    let lines = app.notices.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    // Ctrl+S saves only what is marked: nothing.
    let (app, effect) = saving(app);
    assert_eq!(effect, Effect::None);
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert!(!app.model_picker_open());
    quiet(&hub.watcher);
}

#[test]
fn a_catalogue_answer_after_the_toggle_keeps_it_by_reference() {
    // The hello reads the chips with the placeholder scope, and the
    // link-up choice re-reads them at once at the new workspace: the
    // placeholder survives both reads.
    let (app, hub) = scripted_remote(vec![
        pending(),
        global_file(),
        chip_settings_scoped("[\"acme/m2\"]"),
        chip_settings_scoped_at("/hub", "[\"acme/m2\"]"),
    ]);
    hub.start();
    let (mut app, _) = fold(app, hub_hello());
    assert!(!app.chip_read_owed());
    app.on_models(Ok(three()));
    app.model_picker.scoped = vec!["acme/m2".to_owned()];
    // With the link up the choice reads at once: nothing stays owed.
    // Under the inverted link check this fails before the hang guard.
    let app = choosing(app, "/hub");
    assert!(!app.chip_read_owed());
    let mut app = app;
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(press_key(&mut app, Key::Down), Effect::None);
    assert_eq!(press_key(&mut app, Key::Enter), Effect::None);
    // A reordered catalogue carries the toggle by reference: row 2 is
    // touched at its new first slot, and keeps its mark.
    let mut reordered = three();
    reordered.models.swap(0, 1);
    app.on_models(Ok(reordered));
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    assert_eq!(
        app.model_picker
            .open
            .as_ref()
            .map(|open| open.toggled.clone()),
        Some(vec![true, false, false])
    );
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    quiet(&hub.watcher);
}

#[test]
fn a_mark_cell_click_joins_the_toggled_set() {
    // The chosen workspace reads directly, with no `global_file`.
    let (mut app, hub) = scripted_remote(vec![
        pending(),
        (
            "configure".to_owned(),
            json!({"operation": "settings", "workspace": "/hub"}),
            Ok(json!({"rows": [
                {"key": "scoped_models", "shown": "value",
                 "value": "[\"acme/m1\",\"acme/m2\",\"gone/x\"]",
                 "layer": "global", "file": "/hub/config.json", "scope": "global_only"},
            ]})),
        ),
        set_saved(),
    ]);
    app.on_models(Ok(three()));
    app.model_picker.scoped = vec!["acme/m2".to_owned()];
    // The player runs first, so a wrong-state choice answers at once and
    // the asserts below fail before any hang guard.
    hub.start();
    app.choose_workspace("/hub".to_owned());
    assert!(!app.chip_read_failed());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, true, false]));
    // The mark cell click toggles row 2 the way Enter does: layout
    // rows 0 the filter, 1 the buttons, 2 the heading, 3 row 1.
    assert_eq!(app.model_picker.click_cell(4, 0), None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    let (app, _) = fold(app, hub_hello());
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    let (app, effect) = saving(app);
    assert_eq!(effect, Effect::None);
    Deadline::start().recv_or_fail(&hub.done, "wire script");
    assert!(!app.model_picker_open());
    quiet(&hub.watcher);
}

/// Marks the attached session busy, as a running turn does.
fn busy_mid_turn(app: &mut App) {
    app.phase = super::super::Phase::Attached {
        session: contract::SessionId(SESSION.to_owned()),
        busy: true,
    };
}

/// Folds the attached session's pass with a running turn, as a raise's
/// pass does mid-turn: the fold's content never matters here, only its
/// busyness.
fn folding_mid_turn(app: &mut App) {
    app.overlay = super::super::Overlay::Folding {
        session: contract::SessionId(SESSION.to_owned()),
        fold: Box::new(crate::window::Pages::new(80)),
        busy: Some(true),
        held: false,
        landing: false,
    };
}

/// Whether any notice row carries `code`.
fn has_code(app: &App, code: &str) -> bool {
    app.notices().into_iter().any(
        |line| matches!(line, crate::app::NoticeLine::Notice { code: found, .. } if found == code),
    )
}

#[test]
fn a_send_while_busy_pushes_the_pending_notice() {
    let (mut app, seam) = choosing_app();
    // A real running turn: the send's own fold must not end it, or the
    // turn-end fold would clear the row just pushed.
    app.on_line(turn_started());
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["command"], json!("model"));
    // The notice names the stashed choice, not the chips: the model with
    // its untouched level rides along.
    assert_eq!(newest_code(&app), "switch_pending");
    assert_eq!(
        app.notice(),
        Some("acme/m1 · high applies when this turn ends")
    );
    // The acceptance writes the choice and adds no notice of its own:
    // the pending one already said it would apply.
    let id = line["id"].as_str().expect("an id").to_owned();
    let notices = app.notices().len();
    app.on_line(accepted(SESSION, &id));
    assert_eq!(app.notices().len(), notices);
    assert_eq!(seam.writes().len(), 1);
}

#[test]
fn a_send_while_busy_in_a_folded_pass_pushes_the_pending_notice() {
    let (mut app, _) = choosing_app();
    folding_mid_turn(&mut app);
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["command"], json!("model"));
    assert_eq!(newest_code(&app), "switch_pending");
    assert_eq!(
        app.notice(),
        Some("acme/m1 · high applies when this turn ends")
    );
}

#[test]
fn an_idle_send_is_silent() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["command"], json!("model"));
    assert!(
        app.notices().is_empty(),
        "idle sends stay silent: {:?}",
        app.notices()
    );
}

#[test]
fn an_accept_while_busy_adds_no_notice() {
    let (mut app, _) = choosing_app();
    // A real running turn: the acceptance changes the model without
    // ending the turn, so the pending row stands on, silent.
    app.on_line(turn_started());
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert_eq!(newest_code(&app), "switch_pending");
    app.on_line(accepted(SESSION, &id));
    // Only the pending notice stands: the acceptance is silent.
    assert_eq!(newest_code(&app), "switch_pending");
    assert_eq!(
        app.notice(),
        Some("acme/m1 · high applies when this turn ends")
    );
}

#[test]
fn an_idle_acceptance_writes_with_no_notice() {
    let (mut app, seam) = choosing_app();
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    app.on_line(accepted(SESSION, &id));
    assert_eq!(seam.writes().len(), 1);
    assert!(
        app.notices().is_empty(),
        "idle choices write with no notice: {:?}",
        app.notices()
    );
}

#[test]
fn a_rejection_after_acceptance_pushes_without_replacing() {
    let (mut app, _) = choosing_app();
    // A real running turn: the folds must not end it, or the turn-end
    // fold would clear the rows this test reasons about.
    app.on_line(turn_started());
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    // The acceptance drains the stash; the pending notice stands as the
    // record of the applied choice.
    app.on_line(accepted(SESSION, &id));
    // A late rejection for the drained id changes nothing: the pending
    // row is gone, and with it the generic path's push.
    app.on_line(refused(SESSION, &id, "credential_missing", "no key."));
    assert_eq!(newest_code(&app), "switch_pending");
    assert!(!has_code(&app, "credential_missing"));
}

#[test]
fn a_rejection_replaces_the_pending_notice() {
    let (mut app, _) = choosing_app();
    // A real running turn, so only the rejection path clears.
    app.on_line(turn_started());
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert_eq!(
        app.notice(),
        Some("acme/m1 · high applies when this turn ends")
    );
    app.on_line(refused(SESSION, &id, "credential_missing", "no key."));
    // The rejection takes the pending notice's place: its code and
    // message, with no `switch_pending` row left.
    assert_eq!(newest_code(&app), "credential_missing");
    assert_eq!(app.notice(), Some("no key."));
    assert!(!has_code(&app, "switch_pending"));
}

#[test]
fn a_duplicate_rejection_mid_turn_is_silent_and_drops_the_writes() {
    let (mut app, seam) = choosing_app();
    // A real running turn, so the stranded row stands on.
    app.on_line(turn_started());
    open(&mut app);
    // Session-only, so the wait names the row and no write waits.
    let line = sent(ctrl_s(&mut app));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert_eq!(newest_code(&app), "switch_pending");
    let notices = app.notices().len();
    // A resent copy the session already holds closes with no notice of
    // its own; the first send is still in flight, so its pending notice
    // stands.
    app.on_line(refused(SESSION, &id, "duplicate_command", "twice."));
    assert_eq!(app.notices().len(), notices);
    assert_eq!(newest_code(&app), "switch_pending");
    assert!(!app.model_picker.awaiting.contains_key(&id));
    // A later acceptance carrying the dropped id writes nothing.
    app.on_line(accepted(SESSION, &id));
    assert!(seam.writes().is_empty());
}

#[test]
fn a_rejection_after_leaving_drops_the_stash_without_a_notice() {
    let (mut app, _) = choosing_app();
    // A real running turn, so the send's own fold ends nothing.
    app.on_line(turn_started());
    open(&mut app);
    // Session-only, so leaving drops its whole wait.
    let line = sent(ctrl_s(&mut app));
    let id = line["id"].as_str().expect("an id").to_owned();
    assert_eq!(newest_code(&app), "switch_pending");
    app.leave_session(Some(contract::SessionId(SESSION.to_owned())));
    // The late rejection for the dropped choice is neither written nor
    // notified: its pending notice goes with it, since the choice can
    // never apply.
    app.on_line(refused(SESSION, &id, "credential_missing", "no key."));
    assert!(
        app.notices().is_empty(),
        "a dropped choice stays silent: {:?}",
        app.notices()
    );
}

#[test]
fn a_dismiss_leaves_a_newer_other_code_standing() {
    let (mut app, _) = choosing_app();
    busy_mid_turn(&mut app);
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    // A newer notice with another code sits on top of the pending one:
    // the dismissal keys on the stashed choice's exact wording, so the
    // newest row survives while the stashed choice's own older notice
    // is still taken.
    app.push_notice("credential_missing", "no key.".to_owned());
    app.dismiss_pending_notice("acme/m1", Some("high"));
    assert_eq!(newest_code(&app), "credential_missing");
    assert_eq!(app.notice(), Some("no key."));
    assert!(!has_code(&app, "switch_pending"));
    // The stash is untouched, so the later reject still replaces the
    // pending notice with the rejection notice.
    app.on_line(refused(SESSION, &id, "credential_missing", "no key."));
    assert_eq!(newest_code(&app), "credential_missing");
    assert!(!has_code(&app, "switch_pending"));
}

#[test]
fn a_dismiss_leaves_a_newer_other_choice_standing() {
    let (mut app, _) = choosing_app();
    // A real running turn, so the late rejection takes only its share.
    app.on_line(turn_started());
    open(&mut app);
    let line = sent(app.on_key(Key::Enter, now()));
    let id = line["id"].as_str().expect("an id").to_owned();
    // A newer pending notice names another choice: same code, other
    // text. The dismissal keys on the stashed choice's exact wording,
    // so the newest row survives it.
    app.push_notice(
        "switch_pending",
        "other/om · low applies when this turn ends".to_owned(),
    );
    app.dismiss_pending_notice("acme/m1", Some("high"));
    assert_eq!(
        app.notice(),
        Some("other/om · low applies when this turn ends")
    );
    assert!(has_code(&app, "switch_pending"));
    // The stash is untouched, so the later reject still takes only the
    // stashed choice's own notice, leaving the other's standing.
    app.on_line(refused(SESSION, &id, "credential_missing", "no key."));
    assert_eq!(newest_code(&app), "credential_missing");
    assert_eq!(app.notice(), Some("no key."));
    assert!(
        app.notices().into_iter().any(|line| matches!(
            line,
            crate::app::NoticeLine::Notice { text: shown, .. }
            if shown == "other/om · low applies when this turn ends"
        )),
        "the other choice's pending notice stands: {:?}",
        app.notices()
    );
}

/// Counts the stacked rows under `code`.
fn count_code(app: &App, code: &str) -> usize {
    app.notices()
        .into_iter()
        .filter(
            |line| matches!(line, crate::app::NoticeLine::Notice { code: found, .. } if found == code),
        )
        .count()
}

/// A `turn_started` envelope for the attached session: a real running
/// turn, so refusing a choice mid-turn ends no turn and the rejection
/// path alone clears.
fn turn_started() -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [(
            "input".to_owned(),
            json!([{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]),
        )]
        .into_iter()
        .collect(),
    })
}

/// A `turn_completed` envelope for the attached session: the running
/// turn ends, so the turn-end fold clears the applied shares.
fn turn_completed() -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_completed".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("outcome".to_owned(), json!("completed"))]
            .into_iter()
            .collect(),
    })
}

#[test]
fn two_identical_pending_choices_stack_and_one_rejection_leaves_one() {
    let (mut app, _) = choosing_app();
    app.on_line(turn_started());
    open(&mut app);
    let first = sent(app.on_key(Key::Enter, now()));
    let first_id = first["id"].as_str().expect("an id").to_owned();
    // The same choice sent again is a second notice worth of state, not
    // a repeat: two rows, one text.
    open(&mut app);
    let second = sent(app.on_key(Key::Enter, now()));
    let second_id = second["id"].as_str().expect("an id").to_owned();
    assert_ne!(first_id, second_id);
    assert_eq!(count_code(&app, "switch_pending"), 2);
    assert_eq!(
        app.notice(),
        Some("acme/m1 · high applies when this turn ends")
    );
    // Rejecting the second command voids only its share: the turn still
    // runs, so only the rejection path clears, and the surviving share
    // keeps its text under the rejection's own notice.
    app.on_line(refused(
        SESSION,
        &second_id,
        "credential_missing",
        "no key.",
    ));
    assert_eq!(count_code(&app, "switch_pending"), 1);
    assert!(app.notices().into_iter().any(|line| matches!(line,
        crate::app::NoticeLine::Notice { code, text, .. }
        if code == "switch_pending" && text == "acme/m1 · high applies when this turn ends")));
    // The turn ending applies the last share: the turn-end fold clears
    // it, with no rejection in play.
    app.on_line(turn_completed());
    assert!(!has_code(&app, "switch_pending"));
}

#[test]
fn on_models_arrival_clears_models_loading() {
    let (mut app, _) = choosing_app();
    app.push_notice(
        "models_loading",
        "The model list is not read yet; try again in a moment.".to_owned(),
    );
    assert!(has_code(&app, "models_loading"));
    // The load landed, answered or not: its wait is over.
    app.on_models(Ok(three()));
    assert!(!has_code(&app, "models_loading"));
    app.push_notice(
        "models_loading",
        "The model list is not read yet; try again in a moment.".to_owned(),
    );
    app.on_models(Err("gone".to_owned()));
    assert!(!has_code(&app, "models_loading"));
}

/// A catalogue with five models, so three steps fit without clamping.
fn five() -> Catalogue {
    let mut catalogue = three();
    for reference in ["acme/m3", "acme/m4"] {
        let (provider, id) = reference.split_once('/').unwrap();
        catalogue.models.push(ModelEntry {
            reference: reference.to_owned(),
            provider: provider.to_owned(),
            id: id.to_owned(),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        });
    }
    catalogue
}

/// A wheel report at (`col`, `row`): down steps toward the tail, over the
/// picker's view area.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// Renders `app` at its size so slabs are noted for the wheel points.
fn rendered(app: &App) {
    let (width, height) = app.screen_size();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let _ = crate::view::render(app, area, &mut buf, None);
}

/// The wheel inside the picker slab moves the picker, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_inside_the_picker_slab_moves_the_picker_not_the_conversation() {
    let mut app = attached();
    app.on_models(Ok(five()));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    rendered(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Picker)
        .expect("the picker notes a slab");
    let top = app.top();
    let picker_top = app.model_picker_view().unwrap().top;
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(app.model_picker_view().unwrap().top, picker_top + 3);
    assert_eq!(
        selected(&app).as_deref(),
        Some("acme/m2"),
        "focus clamps to the nearest visible model after the viewport moves"
    );
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, false));
    assert_eq!(app.model_picker_view().unwrap().top, picker_top);
    assert_eq!(
        selected(&app).as_deref(),
        Some("acme/m2"),
        "focus stays when scrolling back makes its row visible"
    );
}

/// The wheel outside the picker slab but over its view area moves the
/// picker too, never the conversation under it (`docs/tui.md`, "Swapped
/// views"; see #3578).
#[test]
fn wheel_outside_the_picker_slab_moves_the_picker_not_the_conversation() {
    let mut app = attached();
    app.on_models(Ok(five()));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
    rendered(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Picker)
        .expect("the picker notes a slab");
    let top = app.top();
    let picker_top = app.model_picker_view().unwrap().top;
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    assert_eq!(selected(&app).as_deref(), Some("acme/m1"));
    // The top swapped-view header sits outside the picker panel slab but
    // remains in the replacement area, so it still routes to the picker.
    let col = 40;
    let row = 0;
    assert!((row as usize) < app.conversation_height());
    assert!(!slab.contains(ratatui::layout::Position { x: col, y: row }));
    app.on_wheel(&wheel(col, row, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(app.model_picker_view().unwrap().top, picker_top + 3);
    assert_eq!(
        selected(&app).as_deref(),
        Some("acme/m2"),
        "focus clamps to the nearest visible model after the viewport moves"
    );
}

/// A Backspace with an empty query keeps the wheel position, while one
/// that shortens the query resets to the focus window, both with rendered
/// rows (`docs/tui.md:576`).
#[test]
fn backspace_empty_keeps_and_shortening_resets_the_window() {
    let (mut app, _) = choosing_app();
    app.on_models(Ok(five()));
    open(&mut app);
    rendered(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Picker)
        .expect("the picker notes a slab");
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
    let moved = app.model_picker_view().unwrap().top;
    assert_ne!(moved, 0, "the wheel moves the picker top");
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(
        app.model_picker_view().unwrap().top,
        moved,
        "an empty Backspace keeps the wheel position"
    );
    type_letter(&mut app, 'm');
    assert_eq!(app.model_picker.query(), "m");
    rendered(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Picker)
        .expect("the picker notes a slab");
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
    let moved = app.model_picker_view().unwrap().top;
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.model_picker.query(), "");
    let view = app.model_picker_view().expect("open");
    assert_eq!(
        view.top,
        crate::view::model_picker::focus_window_top(&view, app.screen_size().1),
        "shortening resets to the focus window"
    );
    let _ = (moved, slab);
}

/// Tab with nothing to show keeps the wheel position with rendered rows
/// (`docs/tui.md:576`).
#[test]
fn tab_with_nothing_to_show_keeps_the_window() {
    let (mut app, _) = choosing_app();
    app.on_models(Ok(five()));
    open(&mut app);
    rendered(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Picker)
        .expect("the picker notes a slab");
    app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
    let moved = app.model_picker_view().unwrap().top;
    assert_ne!(moved, 0, "the wheel moves the picker top");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(
        app.model_picker_view().unwrap().top,
        moved,
        "a no-op Tab keeps the wheel position"
    );
}

/// The picker on home-with-a-session sizes from the conversation, like
/// attached, not from the screen like home, with rendered rows
/// (`docs/tui.md:576`).
#[test]
fn picker_on_home_with_a_session_sizes_from_the_conversation() {
    use std::path::PathBuf;
    let mut home_only = home();
    home_only.on_models(Ok(five()));
    assert_eq!(home_only.on_key(Key::CtrlL, now()), Effect::None);
    assert!(home_only.model_picker_open());
    let home_height = home_only.model_picker_area_height();
    let mut attached_home = attached();
    attached_home.set_home(Launch {
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
    });
    attached_home.on_models(Ok(five()));
    assert_eq!(attached_home.on_key(Key::CtrlL, now()), Effect::None);
    assert!(attached_home.model_picker_open());
    let session_height = attached_home.model_picker_area_height();
    let mut plain = attached();
    plain.on_models(Ok(five()));
    assert_eq!(plain.on_key(Key::CtrlL, now()), Effect::None);
    let plain_height = plain.model_picker_area_height();
    assert_eq!(
        session_height, plain_height,
        "home-with-session sizes like attached"
    );
    assert_ne!(
        session_height, home_height,
        "home-with-session never sizes from the screen"
    );
}

/// A wheel outside the conversation with a swapped view open on
/// home-with-a-session reaches neither the picker nor the conversation,
/// while on home alone the swapped view owns it (`docs/tui.md:576`).
#[test]
fn wheel_outside_with_swapped_open_home_session_vs_home() {
    use std::path::PathBuf;
    // Home-with-a-session: the swapped view owns only the visible content
    // outside a covering panel, so a wheel on the input line moves neither.
    let mut both = attached();
    both.set_home(crate::home::Launch {
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
    });
    both.on_models(Ok(five()));
    assert_eq!(both.on_key(Key::CtrlL, now()), Effect::None);
    assert!(both.model_picker_open());
    rendered(&both);
    let picker_top = both.model_picker_view().unwrap().top;
    let top = both.top();
    both.on_wheel(&wheel(10, 23, true));
    assert_eq!(
        both.model_picker_view().unwrap().top,
        picker_top,
        "outside owns neither"
    );
    assert_eq!(both.top(), top, "the wheel left the conversation");
    // Home alone: the swapped view takes the whole screen, so the same
    // outside wheel moves the picker (with enough models to scroll).
    let mut alone = home();
    let mut many = five();
    for at in 5..20 {
        many.models.push(crate::catalogue::ModelEntry {
            reference: format!("acme/m{at}"),
            provider: "acme".to_owned(),
            id: format!("m{at}"),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        });
    }
    alone.on_models(Ok(many));
    assert_eq!(alone.on_key(Key::CtrlL, now()), Effect::None);
    assert!(alone.model_picker_open());
    rendered(&alone);
    let picker_top = alone.model_picker_view().unwrap().top;
    alone.on_wheel(&wheel(10, 23, true));
    assert_eq!(
        alone.model_picker_view().unwrap().top,
        picker_top + 3,
        "on home the swapped view owns the outside wheel"
    );
}
