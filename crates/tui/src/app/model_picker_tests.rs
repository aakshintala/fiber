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
fn the_picker_is_a_picker_context() {
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
/// what the person reads.
fn shown_notices(app: &App) -> String {
    app.notices()
        .iter()
        .flat_map(|notice| notice.rows.clone())
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
    // Frame rows: 0 the buttons, 1 the `acme` heading, 2 `acme/m1` with
    // two chips past its name cell.
    let line = sent(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            2, 2,
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
    // `acme/m1` preselects `high` at cell (2, 2): clicking it chooses the
    // level, saving `models."acme/m1".thinking`.
    let line = sent(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            2, 2,
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
            2, 0,
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
    let line = sent(app.on_key(Key::Char('s'), now()));
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
    let line = sent(app.on_key(Key::Char('s'), now()));
    assert_eq!(line["args"], json!({"model": "acme/m1", "thinking": "low"}));
}

#[test]
fn s_does_nothing_on_the_empty_scoped_view() {
    let mut app = scoped_home(&["gone/x"]);
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    // The frame highlights that same row: the buttons, the heading,
    // then `m2`.
    let frame = app.model_picker_frame(24).expect("open");
    assert_eq!(frame.rows[2][0].0, "m2   ".to_owned());
    assert_eq!(frame.list.selected(), 2);
    // Enter takes the highlighted row, not the hidden model.
    let line = sent(app.on_key(Key::Enter, now()));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
}

#[test]
fn choosing_with_a_hidden_selection_takes_the_highlighted_row() {
    let mut app = dropped_selection_app();
    // Park the selection back on the hidden row: the frame still
    // highlights the first shown row.
    app.model_picker.open.as_mut().expect("open").selected = 0;
    let frame = app.model_picker_frame(24).expect("open");
    assert_eq!(frame.rows[2][0].0, "m2   ".to_owned());
    assert_eq!(frame.list.selected(), 2);
    // `s` takes the highlighted row for this session only, leaving
    // nothing to save.
    let line = sent(app.on_key(Key::Char('s'), now()));
    assert_eq!(line["args"], json!({"model": "acme/m2"}));
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn the_footer_reads_enter_set_as_default_s_this_session_only() {
    let (mut app, _) = choosing_app();
    open(&mut app);
    assert_eq!(
        app.model_picker_frame(24).map(|frame| frame.footer),
        Some(
            "Enter set as default · s this session only · ↑↓ move · ←→ level · PageUp PageDown page · Tab scope · Ctrl+R refresh · Esc close"
                .to_owned()
        )
    );
}

#[test]
fn session_only_rebinds() {
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"session_only": "x"})).unwrap();
    let mut app = App::new(PathBuf::from("/w"));
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
    // `s` moved off the action: it does nothing in the picker.
    open(&mut app);
    let stroke = Stroke::parse("s").unwrap();
    assert_eq!(app.on_press(stroke, now()), Effect::None);
    assert!(app.model_picker_open());
}

#[test]
fn typing_s_in_an_overlay_is_unaffected_by_rebinding_session_only() {
    let user: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(json!({"session_only": "x"})).unwrap();
    let mut app = App::new(PathBuf::from("/w"));
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
        models: vec![
            ModelEntry {
                reference: "p/m".to_owned(),
                provider: "p".to_owned(),
                id: "m".to_owned(),
                levels: vec!["high".to_owned()],
                default_level: Some("high".to_owned()),
                configured: None,
                roles: Vec::new(),
            },
            ModelEntry {
                reference: "p/m:high".to_owned(),
                provider: "p".to_owned(),
                id: "m:high".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
            },
        ],
        notices: Vec::new(),
    };
    let mut app = home();
    app.on_line(hello());
    app.on_models(Ok(catalogue));
    open(&mut app);
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(args["model"], json!("acme/m2"));
    assert!(args.get("overrides").is_none());
}

#[test]
fn the_start_model_clears_once_start_is_accepted() {
    let mut app = home();
    app.on_line(hello());
    app.on_models(Ok(three()));
    open(&mut app);
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Cached));
}

#[test]
fn with_no_model_it_says_to_choose_one() {
    let (mut app, _) = choosing_app();
    assert_eq!(run_draft(&mut app, "/thinking high"), Effect::None);
    assert_eq!(app.notice(), Some("Choose a model first: Ctrl+L."));
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
        app.model_picker_frame(24).map(|frame| frame.footer),
        Some(
            "Enter set as default · s this session only · ↑↓ move · ←→ level · PageUp PageDown page · Tab scope · Ctrl+R refresh · Esc close"
                .to_owned()
        )
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
    let line = sent(app.on_key(Key::Char('s'), now()));
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
    let frame = app.model_picker_frame(24).expect("picker frame");
    let chip_cells = frame.rows[2]
        .iter()
        .map(|(text, _, _)| text.as_str())
        .collect::<Vec<_>>();
    let chips = chip(&app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    type_draft(&mut app, "hi");
    let args = started(app.on_key(Key::Enter, now()));
    assert_eq!(
        (
            chips.as_deref(),
            chip_cells,
            args["model"].clone(),
            args["overrides"].clone()
        ),
        (
            Some("low"),
            vec!["m1   ", "[low] ", "high "],
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
    app.model_picker_accepted("other");
    assert_eq!(app.model_picker.start_model, Some(held.clone()));

    app.model_picker.awaiting.insert(
        "same".to_owned(),
        vec![("models.\"acme/m1\".thinking".to_owned(), "low".to_owned())],
    );
    app.model_picker_accepted("same");
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
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
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
        app.model_picker_frame(24).map(|frame| frame.footer),
        Some("Space mark · Enter save · Esc back".to_owned())
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
fn space_and_a_click_toggle_a_mark() {
    let (mut app, _) = scope_app(&[]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    // Frame rows: 0 the buttons, 1 the `acme` heading, 2 `acme/m1`,
    // 4 the `zeta` heading, 5 `zeta/z1`.
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    // A click on the mark toggles its row.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            2, 0
        ))),
        Effect::None
    );
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    // A click on the name selects without toggling.
    assert_eq!(
        app.on_click(crate::mouse::TargetId::View(crate::swapped::Spot::Cell(
            5, 1
        ))),
        Effect::None
    );
    assert_eq!(selected(&app).as_deref(), Some("zeta/z1"));
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert!(app.model_picker_open());
}

#[test]
fn enter_saves_marked_in_order_then_unlisted_old_entries() {
    let (mut app, seam) = scope_app(&["gone/x", "zeta/z1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, true]));
    // Mark `acme/m1`: the save lists marked references in catalogue
    // order, then the old list's uninstalled entries in their order.
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
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
fn marking_none_clears_even_with_an_unavailable_old_entry() {
    // The old list holds `gone/x`, which is not installed, and
    // `acme/m1`, which is: with neither marked the save writes `[]`,
    // dropping every old entry.
    let (mut app, seam) = scope_app(&["gone/x", "acme/m1"]);
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(marks(&app), Some(vec![true, false, false]));
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(marks(&app), Some(vec![false, false, false]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
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
fn tab_chips_and_s_do_nothing_in_the_checklist() {
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
    // `s` never chooses from the checklist: it stays open, sending and
    // writing nothing.
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
    assert!(app.model_picker_open());
    assert!(seam.writes().is_empty());
    assert!(app.model_picker.awaiting.is_empty());
}

#[test]
fn a_failed_scope_write_is_a_notice_and_keeps_the_list() {
    let (mut app, seam) = scope_app(&["acme/m1"]);
    *seam.refuse.lock().expect("the refusal") = Some("locked".to_owned());
    assert_eq!(run_draft(&mut app, "/scoped-models"), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
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
    assert_eq!(app.on_key(Key::Char(' '), now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Saving is not available; nothing changed.")
    );
    assert!(app.model_picker.scoped.is_empty());
    assert!(!app.model_picker_open());
}
