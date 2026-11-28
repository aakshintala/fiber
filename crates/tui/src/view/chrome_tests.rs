//! Snapshots of the session screen's chrome: the floor line, the
//! conversation's gutter and the panel's region.

use super::super::{cursor, render, text};
use crate::app::App;
use crate::ext::tests::{SlotRenderer, SlotScript};
use crate::ext::{RawRect, Renderer};
use crate::home::Launch;
use crate::link::Line;
use contract::clock::Clock;
use contract::events::EventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// An app with home state at `width` by `height`, attached unless
/// `on_home`.
fn app(width: u16, height: u16, on_home: bool) -> App {
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
    if !on_home {
        app.attach(contract::SessionId(SESSION.to_owned()));
    }
    app.set_size(width, height);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Names the session `name`.
fn named(app: &mut App, name: &str) {
    app.on_line(session_line(
        "session_named",
        serde_json::json!({"name": name, "by": "person"}),
    ));
}

/// A `session_status` line for `session` named `name`.
fn status(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::SessionStatus,
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Draws `app` at `width` by `height`: the buffer and the target count.
fn draw(app: &App, width: u16, height: u16) -> (Buffer, usize) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    (buf, targets.len())
}

#[test]
fn placement_shares_split_geometry_and_vertical_regions() {
    let app = app(160, 40, false);
    let placement = super::super::placement(&app).expect("the attached screen has a placement");
    let regions = placement.regions();
    assert_eq!(
        regions.get(crate::layout::Region::Rail),
        Some(Rect::new(0, 0, 24, 40))
    );
    assert_eq!(
        regions.get(crate::layout::Region::Conversation),
        Some(Rect::new(24, 0, 102, 35))
    );
    assert_eq!(placement.conversation(&app), Rect::new(24, 0, 102, 35));
    assert_eq!(regions.get(crate::layout::Region::WorkingLine), None);
    assert_eq!(regions.get(crate::layout::Region::SteeringQueue), None);
    assert_eq!(
        regions.get(crate::layout::Region::InputBox),
        Some(Rect::new(24, 35, 102, 5))
    );
    assert_eq!(
        regions.get(crate::layout::Region::Panel),
        Some(Rect::new(126, 0, 34, 40))
    );
}

#[test]
fn extension_layout_places_the_same_regions_that_chrome_draws() {
    use std::rc::Rc;

    let mut app = app(160, 40, false);
    let renderer = Rc::new(SlotRenderer::filling(
        &["layout"],
        SlotScript::Replace(Vec::new()),
    ));
    *renderer.layout.borrow_mut() = Some(crate::results_support::four_region_layout(35));
    app.use_renderer(renderer.clone() as Rc<dyn Renderer>);

    let placement = crate::view::placement(&app).expect("placed after install");
    assert_eq!(
        placement.regions().get(crate::layout::Region::Rail),
        Some(Rect::new(0, 0, 20, 40))
    );
    assert_eq!(
        placement.regions().get(crate::layout::Region::Conversation),
        Some(Rect::new(30, 0, 100, 35))
    );
    assert_eq!(
        placement.regions().get(crate::layout::Region::Panel),
        Some(Rect::new(140, 0, 20, 40))
    );
    assert_eq!(
        renderer.layout_inputs.borrow()[0].regions,
        [
            crate::layout::Region::Rail,
            crate::layout::Region::Conversation,
            crate::layout::Region::InputBox,
            crate::layout::Region::Panel,
        ]
    );
    // The extension replaces placement only; the split still owns the
    // built-in grip/narrow decisions and width floor.
    assert_eq!(
        app.chrome().layout().unwrap().rail,
        Some(Rect::new(0, 0, 24, 40))
    );

    let (buf, _) = draw(&app, 160, 40);
    assert_eq!(buf.cell((19, 20)).map(|cell| cell.symbol()), Some("⋮"));
    assert_eq!(buf.cell((140, 20)).map(|cell| cell.symbol()), Some("⋮"));
}

#[test]
fn placement_row_counts_ignore_the_previous_extension_column_width() {
    use std::rc::Rc;

    let mut app = app(160, 40, false);
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": "hi"}],
        }]}),
    ));
    let long = std::iter::repeat_n('x', 300).collect::<String>();
    app.on_line(session_line(
        "steering_queue",
        serde_json::json!({"messages": [{
            "content": [{"type": "text", "text": long}],
            "source": "driver",
            "command_id": "c_1",
        }]}),
    ));
    let area = Rect::new(0, 0, 160, 40);
    let split = app.chrome().layout().expect("attached split");
    assert_ne!(
        crate::view::steering_queue::count(&app, split.column().width),
        crate::view::steering_queue::count(&app, 30),
    );
    let before = super::super::placement_from_split(&app, area, split);
    let builtin = before.regions();
    let conversation = builtin
        .get(crate::layout::Region::Conversation)
        .expect("conversation region");
    let mut extension = builtin;
    extension.set(
        crate::layout::Region::Conversation,
        Some(Rect::new(30, conversation.y, 30, conversation.height)),
    );
    let layout = extension
        .shown()
        .map(|(region, rect)| {
            (
                region,
                RawRect {
                    x: i64::from(rect.x),
                    y: i64::from(rect.y),
                    width: i64::from(rect.width),
                    height: i64::from(rect.height),
                },
            )
        })
        .collect();
    let renderer = Rc::new(SlotRenderer::filling(
        &["layout"],
        SlotScript::Replace(Vec::new()),
    ));
    *renderer.layout.borrow_mut() = Some(layout);
    app.use_renderer(renderer as Rc<dyn Renderer>);

    assert_eq!(app.column_width(), 30);
    let after = super::super::placement_from_split(&app, area, split);
    assert_eq!(after, before);
}

#[test]
fn invalid_extension_layout_falls_back_whole_and_notices_once() {
    use std::rc::Rc;

    let mut app = app(160, 40, false);
    let area = Rect::new(0, 0, 160, 40);
    let builtin = crate::view::placement(&app)
        .expect("the attached screen has a placement")
        .regions();
    let renderer = Rc::new(SlotRenderer::filling(
        &["layout"],
        SlotScript::Replace(Vec::new()),
    ));
    *renderer.layout.borrow_mut() = Some(vec![
        (
            crate::layout::Region::Rail,
            RawRect {
                x: 0,
                y: 0,
                width: 20,
                height: 40,
            },
        ),
        (
            crate::layout::Region::Conversation,
            RawRect {
                x: 30,
                y: 0,
                width: 100,
                height: 35,
            },
        ),
        (
            crate::layout::Region::InputBox,
            RawRect {
                x: 30,
                y: 35,
                width: 100,
                height: 5,
            },
        ),
        (
            crate::layout::Region::Panel,
            RawRect {
                x: 150,
                y: 0,
                width: 20,
                height: 40,
            },
        ),
    ]);
    app.use_renderer(Rc::clone(&renderer) as Rc<dyn Renderer>);

    let placed = crate::view::placement(&app).expect("fallback placement");
    assert_eq!(placed.regions(), builtin);
    assert_eq!(renderer.layout_inputs.borrow().len(), 1); // `docs/tui.md`, "How a TUI extension runs": one call per cache miss or frame advance, none on a hit.

    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(app.drawn(&targets));
    assert_eq!(app.notices().len(), 1); // `docs/tui.md`, "The slots": invalid output raises one notice and caches the failure.

    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(!app.drawn(&targets));
    assert_eq!(app.notices().len(), 1); // `docs/tui.md`, "The slots": invalid output raises one notice and caches the failure.
    assert_eq!(renderer.layout_inputs.borrow().len(), 1); // `docs/tui.md`, "How a TUI extension runs": one call per cache miss or frame advance, none on a hit.
}

#[test]
fn builtin_layout_cache_refreshes_when_input_rows_change_placement() {
    use std::rc::Rc;

    let mut app = app(160, 40, false);
    let renderer = Rc::new(SlotRenderer::filling(
        &["layout"],
        SlotScript::Replace(Vec::new()),
    ));
    renderer.layout_builtin.set(true);
    app.use_renderer(Rc::clone(&renderer) as Rc<dyn Renderer>);
    let before = crate::view::placement(&app)
        .expect("attached screen has a placement")
        .regions();
    assert_eq!(renderer.layout_inputs.borrow().len(), 1); // `docs/tui.md`, "How a TUI extension runs": one call per cache miss or frame advance, none on a hit.

    for _ in 0..3 {
        app.on_edit(crate::keys::Edit::ShiftEnter);
    }
    let after = crate::view::placement(&app)
        .expect("attached screen has a placement")
        .regions();

    assert_ne!(
        after.get(crate::layout::Region::InputBox),
        before.get(crate::layout::Region::InputBox)
    );
    assert_ne!(
        after.get(crate::layout::Region::Conversation),
        before.get(crate::layout::Region::Conversation)
    );
    assert_eq!(renderer.layout_inputs.borrow().len(), 4); // `docs/tui.md`, "How a TUI extension runs": one call per cache miss or frame advance, none on a hit.
}

#[test]
fn floor_line_32x8() {
    for on_home in [true, false] {
        let app = app(32, 8, on_home);
        let (buf, targets) = draw(&app, 32, 8);
        assert_eq!(targets, 0);
        assert_eq!(cursor(&app, Rect::new(0, 0, 32, 8)), None);
        insta::assert_snapshot!("floor_line_32x8", text(&buf));
    }
}

#[test]
fn attached_160x40_with_the_panel() {
    let mut app = app(160, 40, false);
    named(&mut app, "fix the parser");
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hello"}]}]}),
    ));
    let (buf, _) = draw(&app, 160, 40);
    insta::assert_snapshot!("attached_160x40_with_the_panel", text(&buf));
    // The panel's first column holds the grip at mid-height, and the
    // cursor sits in the column's input box.
    for y in 19..=21 {
        assert_eq!(buf.cell((126, y)).map(|cell| cell.symbol()), Some("⋮"));
    }
    assert_eq!(buf.cell((126, 18)).map(|cell| cell.symbol()), Some(" "));
    assert_eq!(
        cursor(&app, Rect::new(0, 0, 160, 40)),
        Some(Position::new(28, 36))
    );
}

#[test]
fn attached_100x30_narrow() {
    let mut app = app(100, 30, false);
    named(&mut app, "fix the parser");
    let (buf, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("attached_100x30_narrow", text(&buf));
}

#[test]
fn floor_centres_with_division() {
    // `(area - text) / 2` centres; `% 2` would leave 0 or 1 instead.
    for (width, height, expected_x) in [(10u16, 5u16, 4u16), (9u16, 4u16, 3u16)] {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        super::floor("hi", area, &mut buf);
        let expected_y = height / 2;
        let symbol =
            |x: u16, y: u16, buf: &Buffer| buf.cell((x, y)).map(|cell| cell.symbol().to_owned());
        assert_eq!(symbol(expected_x, expected_y, &buf), Some("h".to_owned()));
        assert_eq!(
            symbol(expected_x + 1, expected_y, &buf),
            Some("i".to_owned())
        );
        assert_eq!(symbol(0, expected_y, &buf), Some(" ".to_owned()));
    }
}

#[test]
fn the_conversation_has_no_header_and_a_blank_column_each_side() {
    use ratatui::style::Color;
    let mut app = app(160, 40, false);
    named(&mut app, "fix the parser");
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "say hi"}]}]}),
    ));
    app.on_line(session_line(
        "assistant_message_started",
        serde_json::json!({}),
    ));
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({"text": "hello world"}),
    ));
    let (buf, _) = draw(&app, 160, 40);
    let row0: String = (0..126)
        .filter_map(|x| buf.cell((x, 0)).map(|cell| cell.symbol().to_owned()))
        .collect();
    assert!(!row0.contains("fix the parser"), "row 0: {row0:?}");
    let y = (0..40)
        .find(|y| {
            let row: String = (0..126)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row");
    let layout = app.chrome().layout().expect("a layout with the rail");
    let rail = layout.rail().expect("a rail");
    let tint = buf
        .cell((rail.right().saturating_add(1), y))
        .map(|cell| cell.bg)
        .unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset, "the reply's tint");
    assert_eq!(
        buf.cell((rail.right(), y)).map(|cell| cell.bg),
        Some(Color::Reset)
    );
    assert_eq!(
        buf.cell((rail.right().saturating_add(1), y))
            .map(|cell| cell.bg),
        Some(tint)
    );
    assert_eq!(buf.cell((124, y)).map(|cell| cell.bg), Some(tint));
    assert_eq!(buf.cell((125, y)).map(|cell| cell.bg), Some(Color::Reset));
    // A second live session keeps the rail: the card starts one column
    // after the rail's edge.
    app.on_line(status(SESSION, "two"));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "two"));
    let layout = app.chrome().layout().expect("a layout with the rail");
    let rail = layout.rail().expect("a rail");
    let (buf, _) = draw(&app, 160, 40);
    let y = (0..40)
        .find(|y| {
            let row: String = (0..160)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row with the rail");
    let tint = buf
        .cell((rail.right().saturating_add(1), y))
        .map(|cell| cell.bg)
        .unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset);
    assert_eq!(
        buf.cell((rail.right(), y)).map(|cell| cell.bg),
        Some(Color::Reset)
    );
    assert_eq!(
        buf.cell((rail.right().saturating_add(1), y))
            .map(|cell| cell.bg),
        Some(tint)
    );
    // Hiding the rail leaves its grip at column 0: blank at 1, card at 2.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(crate::keys::Key::AltR, now);
    let (buf, _) = draw(&app, 160, 40);
    let y = (0..40)
        .find(|y| {
            let row: String = (0..160)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row with the grip");
    let tint = buf.cell((2, y)).map(|cell| cell.bg).unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset);
    assert_eq!(buf.cell((1, y)).map(|cell| cell.bg), Some(Color::Reset));
    assert_eq!(buf.cell((2, y)).map(|cell| cell.bg), Some(tint));
}

#[test]
fn an_idle_grip_is_dim_and_an_active_one_bold_in_the_accent() {
    use ratatui::style::Modifier;
    let region = Rect::new(10, 5, 20, 10);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 20));
    super::grip(&mut buf, 10, region, false);
    for y in 9..=11 {
        let cell = buf.cell((10, y)).expect("a grip cell");
        assert_eq!(cell.symbol(), "⋮");
        assert_eq!(cell.fg, crate::theme::Role::Muted.color());
        assert!(!cell.modifier.contains(Modifier::BOLD));
    }
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 20));
    super::grip(&mut buf, 10, region, true);
    for y in 9..=11 {
        let cell = buf.cell((10, y)).expect("a grip cell");
        assert_eq!(cell.symbol(), "⋮");
        assert_eq!(cell.fg, crate::theme::Role::Accent.color());
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn grip_needs_both_bounds() {
    // A one-row region: the mid window reaches past it on both sides.
    // `||` would draw outside while `&&` clips.
    let region = Rect::new(10, 5, 20, 1);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    super::grip(&mut buf, 10, region, false);
    let symbol = |y: u16, buf: &Buffer| buf.cell((10, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(5, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(4, &buf), Some(" ".to_owned()));
    assert_eq!(symbol(6, &buf), Some(" ".to_owned()));
}

#[test]
fn grip_excludes_the_bottom_edge() {
    // The window reaches the region's bottom edge, which `<` excludes
    // while `<=` would draw.
    let region = Rect::new(10, 5, 20, 2);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    super::grip(&mut buf, 10, region, false);
    let symbol = |y: u16, buf: &Buffer| buf.cell((10, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(5, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(6, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(7, &buf), Some(" ".to_owned()));
}

#[test]
fn the_rail_and_panel_regions_take_the_panel_tint() {
    use crate::theme::Role;
    use ratatui::style::Color;
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
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(160, 40);
    for session in [SESSION, "s_bbbbbbbbbbbbbbbb"] {
        app.on_line(status(session, "fix the parser"));
    }
    let (buf, _) = draw(&app, 160, 40);
    let layout = app.chrome().layout().expect("a layout");
    let panel = layout.panel().expect("a panel");
    let rail = layout.rail().expect("a rail");
    let panel_tint = Role::Panel.color();
    for y in panel.top()..panel.bottom() {
        assert_eq!(buf[(panel.x, y)].bg, panel_tint, "panel edge row {y}");
    }
    for x in panel.left()..panel.right() {
        assert_eq!(buf[(x, panel.y)].bg, panel_tint, "panel top column {x}");
    }
    for y in panel.top()..panel.bottom() {
        for x in panel.left()..panel.right() {
            assert_ne!(buf[(x, y)].bg, Color::Reset, "panel cell ({x}, {y})");
        }
    }
    let edge = rail.right().saturating_sub(1);
    for y in rail.top()..rail.bottom() {
        assert_eq!(buf[(edge, y)].bg, panel_tint, "rail edge row {y}");
    }
    for x in rail.left()..rail.right() {
        assert_eq!(
            buf[(x, rail.bottom().saturating_sub(1))].bg,
            panel_tint,
            "rail bottom column {x}"
        );
    }
    for y in rail.top()..rail.bottom() {
        for x in rail.left()..rail.right() {
            assert_ne!(buf[(x, y)].bg, Color::Reset, "rail cell ({x}, {y})");
        }
    }
    // A card row of the panel still carries the surface tint.
    let surface = Role::Surface.color();
    let card = (panel.y..panel.bottom())
        .find(|y| buf[(panel.x + 1, *y)].bg == surface)
        .expect("a card row");
    assert!(card > panel.y);
}
