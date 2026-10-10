//! Tests for the model picker's frame: what it draws attached and on
//! home, the scope line, the status lines, scrolling, its targets and the
//! hidden cursor (`docs/tui.md`, "Swapped views").

use crate::app::{App, Effect};
use crate::catalogue::{Catalogue, ModelEntry};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::Instant;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    app.set_size(width, height);
    app
}

/// An app attached to [`SESSION`] at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = home(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// An app on home at 80x24 showing `scoped`.
fn scoped_home(scoped: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        scoped_models: scoped.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// Four models over two providers: `acme/m1` with two levels and roles,
/// `acme/m2` with none, `zeta/z1` with one, `zeta/z2` with two.
fn catalogue() -> Catalogue {
    Catalogue {
        models: vec![
            ModelEntry {
                reference: "acme/m1".to_owned(),
                provider: "acme".to_owned(),
                id: "m1".to_owned(),
                levels: vec!["low".to_owned(), "high".to_owned()],
                default_level: Some("high".to_owned()),
                configured: None,
                roles: vec!["deep".to_owned(), "review".to_owned()],
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
                levels: vec!["low".to_owned()],
                default_level: None,
                configured: Some("low".to_owned()),
                roles: Vec::new(),
            },
            ModelEntry {
                reference: "zeta/z2".to_owned(),
                provider: "zeta".to_owned(),
                id: "z2".to_owned(),
                levels: vec!["low".to_owned(), "high".to_owned()],
                default_level: Some("low".to_owned()),
                configured: Some("high".to_owned()),
                roles: vec!["chat".to_owned()],
            },
        ],
        notices: Vec::new(),
    }
}

/// One envelope on [`SESSION`].
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Renders `app` on a `width` by `height` screen, returning its targets.
fn drawn(app: &App, width: u16, height: u16) -> Vec<Target> {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None)
}

/// Opens the picker.
fn open(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlL, now()), Effect::None);
    assert!(app.model_picker_open());
}

#[test]
fn model_picker_attached_80x24() {
    let mut app = attached(80, 24);
    app.on_line(session_line(
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "acme/m1", "context_window": 200000,
            "trigger_at": 150000, "thinking": "high",
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    ));
    app.on_line(session_line(
        "usage_recorded",
        serde_json::json!({"generation_id": "g_1", "model": "acme/m1",
            "tokens": {"input": 1000, "cache_read": 200000,
                "cache_write": {"1h": 34}, "output": 5},
            "input_bytes": 1, "cost": null}),
    ));
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!("model_picker_attached_80x24", screen(&app, 80, 24));
}

#[test]
fn model_picker_scoped_with_the_toggle_line() {
    let mut app = scoped_home(&["acme/m1", "zeta/z2"]);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!(
        "model_picker_scoped_with_the_toggle_line",
        screen(&app, 80, 24)
    );
}

#[test]
fn model_picker_on_home() {
    let mut app = home(80, 24);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    insta::assert_snapshot!("model_picker_on_home", screen(&app, 80, 24));
}

#[test]
fn model_picker_reading() {
    let mut app = attached(80, 24);
    open(&mut app);
    insta::assert_snapshot!("model_picker_reading", screen(&app, 80, 24));
}

#[test]
fn model_picker_no_models() {
    let mut app = attached(80, 24);
    open(&mut app);
    assert_eq!(app.take_reads(), Some(crate::catalogue::Refresh::Stale));
    app.on_models(Ok(Catalogue::default()));
    insta::assert_snapshot!("model_picker_no_models", screen(&app, 80, 24));
}

#[test]
fn model_picker_long_list_scrolls_to_the_selection() {
    let mut app = attached(80, 24);
    app.on_models(Ok(Catalogue {
        models: (0..30)
            .map(|at| {
                let provider = ["acme", "zeta", "apex"][at % 3];
                ModelEntry {
                    reference: format!("{provider}/m{at:02}"),
                    provider: provider.to_owned(),
                    id: format!("m{at:02}"),
                    levels: vec!["low".to_owned(), "high".to_owned()],
                    default_level: Some("high".to_owned()),
                    configured: None,
                    roles: Vec::new(),
                }
            })
            .collect(),
        notices: Vec::new(),
    }));
    open(&mut app);
    for _ in 0..25 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let text = screen(&app, 80, 24);
    assert!(text.contains("m25"), "{text}");
    insta::assert_snapshot!("model_picker_long_list_scrolls_to_the_selection", text);
}

#[test]
fn model_picker_targets_cover_rows_chips_and_buttons() {
    let mut app = attached(80, 24);
    app.on_models(Ok(catalogue()));
    open(&mut app);
    let ids: Vec<TargetId> = drawn(&app, 80, 24).iter().map(|target| target.id).collect();
    // The ✕, the rows, the refresh button and a chip all take clicks.
    // Frame rows: 0 the buttons, 1 the `acme` heading, 2 `acme/m1` with
    // its name, roles and two chips.
    assert!(ids.contains(&TargetId::View(Spot::Close)));
    assert!(ids.contains(&TargetId::View(Spot::Row(2))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(0, 0))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(2, 0))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(2, 1))));
    assert!(ids.contains(&TargetId::View(Spot::Cell(2, 2))));
    // The heading takes none of its own.
    assert!(ids.iter().all(|id| *id != TargetId::View(Spot::Cell(1, 0))));
    assert!(ids.iter().all(|id| *id != TargetId::View(Spot::Cell(1, 1))));
}

#[test]
fn the_picker_hides_the_cursor() {
    for mut app in [home(80, 24), attached(80, 24)] {
        app.on_models(Ok(catalogue()));
        let area = Rect::new(0, 0, 80, 24);
        assert!(crate::view::cursor(&app, area).is_some());
        open(&mut app);
        assert_eq!(crate::view::cursor(&app, area), None);
        assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
        assert!(crate::view::cursor(&app, area).is_some());
    }
}

#[test]
fn model_picker_marking() {
    use ratatui::style::Modifier;

    let mut app = scoped_home(&["acme/m1", "zeta/z2"]);
    app.on_models(Ok(catalogue()));
    // Type the command rather than Ctrl+L: the checklist opens from it.
    for ch in "/scoped-models".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.model_picker_open());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    // Each row draws its mark; there are no level chips here.
    assert!(text.contains("[x] m1"), "{text}");
    assert!(text.contains("[ ] m2"), "{text}");
    assert!(!text.contains("[low]"), "{text}");
    assert!(
        text.contains("Space mark · Enter save · Esc back"),
        "{text}"
    );
    // A marked row draws bold, as a chosen multi-select option does;
    // an unmarked row draws plain.
    for (y, line) in text.lines().enumerate() {
        let row = u16::try_from(y).unwrap_or(u16::MAX);
        if line.contains("[x]") {
            assert!(
                (0..area.width).any(|x| buf[(x, row)].modifier.contains(Modifier::BOLD)),
                "no bold on {line:?}"
            );
        }
        if line.contains("[ ]") {
            assert!(
                (0..area.width).all(|x| !buf[(x, row)].modifier.contains(Modifier::BOLD)),
                "bold on {line:?}"
            );
        }
    }
    insta::assert_snapshot!("model_picker_marking", text);
}
