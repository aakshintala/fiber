//! Tests for the session screen's chrome on the app: the column's width,
//! rewrapping, the header and the panel's hide.

use super::super::{App, Target};
use super::header;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The launch description: `/w`, outside git, default shares.
fn launch() -> Launch {
    Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
    }
}

/// An app with home state, attached, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// An app without home state, attached, at `width` by `height`.
fn plain(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of `session`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
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

/// A `turn_started` with one message.
fn turn_started(text: &str) -> Line {
    session_line(
        SESSION,
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    )
}

/// A feed `session_status` for `session` named `name`.
fn status(session: &str, name: &str) -> Line {
    session_line(
        session,
        "session_status",
        serde_json::json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// Draws `app` at its size: the screen's text and the targets.
fn draw(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

/// The wrapped rows of the conversation.
fn total(app: &App) -> usize {
    app.scroll().1
}

/// Types `text` into the draft.
fn type_draft(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now);
    }
}

#[test]
fn without_home_the_column_is_the_screen() {
    let app = plain(160, 40);
    assert_eq!(app.chrome().layout(), None);
    assert_eq!(app.chrome().floor_line(), None);
    assert_eq!(app.column_width(), 160);
    // No header row: the input box's one row is all that sits below.
    assert_eq!(app.conversation_height(), 39);
    // Below the floor, without home, today's screen still draws.
    let small = plain(30, 8);
    assert_eq!(small.chrome().floor_line(), None);
}

#[test]
fn on_home_there_is_no_layout() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.set_size(160, 40);
    assert!(app.on_home());
    assert_eq!(app.chrome().layout(), None);
    assert_eq!(app.column_width(), 160);
    app.set_size(39, 40);
    assert_eq!(
        app.chrome().floor_line(),
        Some("Fiber needs 40×10 · now 39×40")
    );
}

#[test]
fn attached_at_160_the_column_is_the_screen_less_the_panel() {
    let app = attached(160, 40);
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!(layout.column, Rect::new(0, 0, 126, 40));
    assert_eq!(layout.panel, Some(Rect::new(126, 0, 34, 40)));
    assert_eq!(app.column_width(), 126);
    assert_eq!(app.chrome().regions().panel, layout.panel);
    assert_eq!(app.regions.panel, layout.panel);
    // The header's row and the input box's.
    assert_eq!(app.conversation_height(), 38);
}

#[test]
fn the_pages_rewrap_at_the_column_width() {
    let long: String = std::iter::repeat_n('w', 400).collect();
    let mut app = attached(160, 40);
    app.on_line(turn_started(&long));
    let mut narrow = plain(126, 40);
    narrow.on_line(turn_started(&long));
    let mut wide = plain(160, 40);
    wide.on_line(turn_started(&long));
    assert_ne!(total(&narrow), total(&wide));
    assert_eq!(total(&app), total(&narrow));
}

#[test]
fn alt_p_hides_and_shows_the_panel_and_rewraps() {
    let now = fakes::clock::FakeClock::new().now();
    let long: String = std::iter::repeat_n('w', 400).collect();
    let mut app = attached(160, 40);
    app.on_line(turn_started(&long));
    let mut wide = plain(160, 40);
    wide.on_line(turn_started(&long));
    let shown = total(&app);
    app.on_key(Key::AltP, now);
    assert_eq!(app.chrome().layout().and_then(|layout| layout.panel), None);
    assert_eq!(app.chrome().regions().panel, None);
    assert_eq!(app.regions.panel, None);
    assert_eq!(app.column_width(), 160);
    assert_eq!(total(&app), total(&wide));
    app.on_key(Key::AltP, now);
    assert_eq!(app.column_width(), 126);
    assert_eq!(total(&app), shown);
}

#[test]
fn slash_panel_does_the_same() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(160, 40);
    type_draft(&mut app, "/panel");
    app.on_key(Key::Enter, now);
    assert_eq!(app.column_width(), 160);
    assert!(app.input().is_empty());
    type_draft(&mut app, "/panel");
    app.on_key(Key::Enter, now);
    assert_eq!(app.column_width(), 126);
}

#[test]
fn the_header_is_the_row_name_then_the_named_fold_then_empty() {
    let mut app = attached(160, 40);
    assert_eq!(app.header(), "");
    app.on_line(session_line(
        SESSION,
        "session_named",
        serde_json::json!({"name": "named", "by": "person"}),
    ));
    assert_eq!(app.header(), "named");
    app.on_line(status(SESSION, "fix the\tparser"));
    assert_eq!(app.header(), "fix the parser");
    // Another session's row names nothing here.
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "other"));
    assert_eq!(app.header(), "fix the parser");
    app.on_line(status(SESSION, ""));
    assert_eq!(app.header(), "named");
}

#[test]
fn header_drops_controls_to_spaces_and_prefers_a_nonempty_row() {
    assert_eq!(header(Some("a\u{1b}b\nc"), None), "a b c");
    assert_eq!(header(Some(""), Some("n")), "n");
    assert_eq!(header(Some("r"), Some("n")), "r");
    assert_eq!(header(None, None), "");
}

#[test]
fn conversation_height_equals_the_drawn_rows() {
    let long: String = std::iter::repeat_n('w', 20_000).collect();
    for (width, height) in [(160, 40), (100, 30), (114, 12), (300, 20), (60, 10)] {
        let mut app = attached(width, height);
        app.on_line(turn_started(&long));
        let (screen, _) = draw(&app, width, height);
        let drawn = screen.lines().filter(|row| row.contains("www")).count();
        assert_eq!(drawn, app.conversation_height(), "{width}x{height}");
        // The header holds row 0 and the input box the last row.
        assert!(!screen.lines().next().unwrap_or_default().contains('w'));
    }
}

#[test]
fn a_copy_target_sits_inside_the_column_with_the_panel_shown() {
    let mut app = attached(160, 40);
    app.on_line(turn_started("hi"));
    app.on_line(session_line(
        SESSION,
        "assistant_message_delta",
        serde_json::json!({"text": "```rust\nlet a = 1;\n```"}),
    ));
    let (_, targets) = draw(&app, 160, 40);
    let copy: Vec<Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Line(Target::Copy { .. })))
        .map(|target| target.rect)
        .collect();
    assert!(!copy.is_empty());
    for rect in copy {
        assert!(rect.right() <= 126, "{rect:?}");
        assert!(rect.right() >= 120, "{rect:?}: at the column's right edge");
    }
}

#[test]
fn up_and_down_move_through_a_draft_wrapped_at_the_column_width() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(160, 40);
    let draft: String = std::iter::repeat_n('d', 140).collect();
    type_draft(&mut app, &draft);
    assert_eq!(app.input().cursor(126).0, 1);
    app.on_key(Key::Up, now);
    assert_eq!(app.input().cursor(126).0, 0);
    app.on_key(Key::Down, now);
    assert_eq!(app.input().cursor(126).0, 1);
    assert_eq!(app.input_height(), 2);
}

#[test]
fn notices_are_cut_at_the_column() {
    let mut app = attached(160, 40);
    let long: String = std::iter::repeat_n("word ", 60).collect();
    app.on_line(session_line(
        SESSION,
        "notice",
        serde_json::json!({"code": "io_failed", "message": long}),
    ));
    let (_, targets) = draw(&app, 160, 40);
    let notice = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Notice(_)))
        .map(|target| target.rect)
        .expect("a notice");
    // At the column's right edge, 40% of its width at most.
    assert_eq!(notice.right(), 126);
    assert!(notice.width <= 51, "{notice:?}");
}

#[test]
fn the_rail_is_wanted_from_two_live_sessions() {
    let mut app = attached(160, 40);
    app.on_line(status(SESSION, "one"));
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!((layout.rail, layout.grip), (None, None));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "two"));
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!(layout.rail, Some(Rect::new(0, 0, 24, 40)));
    assert_eq!(app.regions.rail, layout.rail);
    assert_eq!(app.column_width(), 102);
    // Narrower, the rail hides for width and leaves its grip.
    app.set_size(130, 40);
    let layout = app.chrome().layout().expect("a layout");
    assert_eq!(layout.rail, None);
    assert_eq!(layout.grip, Some(Rect::new(0, 0, 1, 40)));
}

#[test]
fn below_the_floor_an_attached_app_has_no_layout() {
    let app = attached(39, 40);
    assert_eq!(app.chrome().layout(), None);
    assert_eq!(app.column_width(), 39);
    assert_eq!(
        app.chrome().floor_line(),
        Some("Fiber needs 40×10 · now 39×40")
    );
}

#[test]
fn title_is_fiber_on_home() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.set_size(160, 40);
    assert_eq!(app.title(), "fiber");
}

#[test]
fn title_has_the_glyph_and_name_when_attached() {
    let mut app = attached(160, 40);
    app.on_line(status(SESSION, "fix\u{7}the parser"));
    assert_eq!(app.title(), "✓ fix\u{7}the parser · fiber");
    // Another session's row is not the title's.
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "other"));
    assert_eq!(app.title(), "✓ fix\u{7}the parser · fiber");
}

#[test]
fn title_without_a_row_has_no_glyph() {
    let mut app = attached(160, 40);
    assert_eq!(app.title(), "fiber");
    app.on_line(session_line(
        SESSION,
        "session_named",
        serde_json::json!({"name": "named", "by": "person"}),
    ));
    assert_eq!(app.title(), "named · fiber");
}

#[test]
fn title_parts_are_left_out_when_empty() {
    use super::title;
    assert_eq!(title(false, Some("✓"), "n"), "fiber");
    assert_eq!(title(true, None, ""), "fiber");
    assert_eq!(title(true, Some("!"), ""), "! · fiber");
    assert_eq!(title(true, Some("!"), "n"), "! n · fiber");
}
