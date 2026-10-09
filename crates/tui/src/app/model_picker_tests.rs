//! Tests for the model picker's reads on the app: a read error shows
//! once with the old catalogue kept, and each catalogue notice shows once
//! per read.

use super::super::{App, Effect};
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

use std::time::Instant;

use contract::clock::Clock;
use serde_json::json;

use crate::keys::{Edit, Key};
use crate::stroke::Stroke;

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app on home at 80x24, scoped to `scoped`.
fn scoped_home(scoped: &[&str]) -> App {
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

/// Three models over two providers: `acme/m1` with two levels,
/// `acme/m2` with none, `zeta/z1` with one.
fn three() -> Catalogue {
    Catalogue {
        models: vec![
            ModelEntry {
                reference: "acme/m1".to_owned(),
                provider: "acme".to_owned(),
                id: "m1".to_owned(),
                levels: levels(&["low", "high"]),
                default_level: Some("high".to_owned()),
                configured: None,
                roles: Vec::new(),
            },
            ModelEntry {
                reference: "acme/m2".to_owned(),
                provider: "acme".to_owned(),
                id: "m2".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
            },
            ModelEntry {
                reference: "zeta/z1".to_owned(),
                provider: "zeta".to_owned(),
                id: "z1".to_owned(),
                levels: levels(&["low"]),
                default_level: None,
                configured: Some("low".to_owned()),
                roles: Vec::new(),
            },
        ],
        notices: Vec::new(),
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
    for key in [
        Key::Char('x'),
        Key::Backspace,
        Key::Enter,
        Key::Char('s'),
        Key::F1,
    ] {
        assert_eq!(app.on_key(key.clone(), now()), Effect::None);
    }
    for edit in [Edit::Delete, Edit::WordLeft, Edit::CtrlJ] {
        assert_eq!(app.on_edit(edit), Effect::None);
    }
    // Enter and `s` choose in the next task; here the picker stays open,
    // the draft keeps nothing typed, and the selection never moved.
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
    let mut app = home();
    assert!(!app.model_picker.refreshing);
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    assert!(app.model_picker.refreshing);
    app.on_models(Ok(three()));
    assert!(!app.model_picker.refreshing);
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Every));
    assert!(app.model_picker.refreshing);
    app.on_models(Err("gone".to_owned()));
    assert!(!app.model_picker.refreshing);
}

#[test]
fn the_picker_is_an_overlay_context() {
    let mut app = App::new(PathBuf::from("/w"));
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"send": "ctrl+s"})).unwrap();
    app.set_keys(crate::KeysSetup { user });
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
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
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
                }
            })
            .collect(),
        notices: Vec::new(),
    }
}

#[test]
fn page_keys_move_by_a_page_clamped() {
    let mut app = home();
    app.on_models(Ok(thirty()));
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    // Home shows 24 rows less the header and footer: a page is 21.
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m21"));
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m29"));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(selected(&app).as_deref(), Some("acme/m08"));
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
fn the_header_shows_the_rebuild_size_of_the_session_on_screen() {
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
    assert_eq!(
        app.model_picker_frame(24).map(|frame| frame.title),
        Some("Models".to_owned())
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
    assert_eq!(
        app.model_picker_frame(24).map(|frame| frame.title),
        Some("Models · switching rebuilds the cache: about 201,034 tokens".to_owned())
    );
    // On home no session is on screen, so no size shows.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.go_home();
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert_eq!(
        app.model_picker_frame(24).map(|frame| frame.title),
        Some("Models".to_owned())
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
            0, 0
        ))),
        Effect::None
    );
    assert_eq!(app.take_reads(), Some(Refresh::Every));
    // The scope line toggles "show all", staying open either way.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            0, 1
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
            0, 1
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
