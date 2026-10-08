//! The selection's highlight over the conversation, and "Copied" above the
//! notices: snapshots of the text, each with a mask of the highlighted
//! cells under it.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::SELECTION;
use crate::app::{App, Effect};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// A session line of `kind`.
fn session_line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app connected and attached at `width` by `height`, showing a turn
/// with the reply `text`.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, height);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app.on_line(session_line(
        "text_completed",
        json!({ "text": text }),
        Some("a_m"),
    ));
    app
}

/// The screen at the app's size, and the targets drawn.
fn draw(app: &App, width: u16, height: u16) -> (Buffer, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = super::super::render(app, area, &mut buf, None);
    (buf, targets)
}

/// The screen's text, then a rule, then a mask: `#` where the selection's
/// highlight is, `.` elsewhere.
fn shown(app: &App, width: u16, height: u16) -> String {
    let (buf, _) = draw(app, width, height);
    let mut mask = String::new();
    for y in 0..height {
        for x in 0..width {
            let lit = buf
                .cell((x, y))
                .is_some_and(|cell| cell.bg == SELECTION.bg.unwrap_or_default());
            mask.push(if lit { '#' } else { '.' });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", super::super::text(&buf))
}

/// A press at `from`, a drag to `to` and its release.
fn select(app: &mut App, width: u16, height: u16, from: (u16, u16), to: (u16, u16)) -> Effect {
    let report = |app: &mut App, kind: MouseKind, (col, row): (u16, u16)| {
        let (_, targets) = draw(app, width, height);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    report(app, MouseKind::Press(Button::Left), from);
    report(app, MouseKind::Drag(Button::Left), to);
    report(app, MouseKind::Release, to)
}

/// A reply long enough to wrap over several rows at 80 columns.
const LONG: &str = "Selection copies text unwrapped. This paragraph is long enough to \
wrap across more than three rows at eighty columns, so a drag from the middle of \
its first row to the middle of its third highlights the rest of the first, the \
whole second and the start of the third.";

#[test]
fn selection_over_three_rows_80x24() {
    let mut app = replied(80, 24, LONG);
    let text = super::super::text(&draw(&app, 80, 24).0);
    let first = text
        .lines()
        .position(|row| row.starts_with("Selection"))
        .expect("the reply's first row");
    let first = u16::try_from(first).expect("a row");
    assert!(matches!(
        select(&mut app, 80, 24, (10, first), (20, first + 2)),
        Effect::Copy(_)
    ));
    insta::assert_snapshot!("selection_over_three_rows_80x24", shown(&app, 80, 24));
}

#[test]
fn selection_does_not_paint_the_new_below_row() {
    let paragraphs: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(40, 12, &paragraphs.join("\n\n"));
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::PageUp, now);
    app.on_line(session_line(
        "text_completed",
        json!({"text": "more"}),
        Some("a_n"),
    ));
    assert!(app.has_new());
    // From the top row to past the bottom: clamped above the overlay.
    select(&mut app, 40, 12, (0, 0), (39, 30));
    insta::assert_snapshot!(
        "selection_does_not_paint_the_new_below_row",
        shown(&app, 40, 12)
    );
}

#[test]
fn copied_sits_above_the_notices() {
    let mut app = replied(60, 10, "copy this line");
    app.on_line(session_line(
        "notice",
        json!({"code": "extension_failed", "message": "a notice"}),
        None,
    ));
    let text = super::super::text(&draw(&app, 60, 10).0);
    let row = text
        .lines()
        .position(|row| row.contains("copy this"))
        .expect("the reply's row");
    let row = u16::try_from(row).expect("a row");
    assert!(matches!(
        select(&mut app, 60, 10, (0, row), (3, row)),
        Effect::Copy(_)
    ));
    assert!(app.copied());
    insta::assert_snapshot!("copied_sits_above_the_notices", shown(&app, 60, 10));
}

/// The screen with the pointer over the link, then a rule, then a mask:
/// `#` where the hover tint is, `.` elsewhere.
fn hover_shown(app: &App, width: u16, height: u16, at: (u16, u16)) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    super::super::render(app, area, &mut buf, Some(at));
    let mut mask = String::new();
    let tint = super::super::HOVER_TINT.bg.unwrap_or_default();
    for y in 0..height {
        for x in 0..width {
            let lit = buf.cell((x, y)).is_some_and(|cell| cell.bg == tint);
            mask.push(if lit { '#' } else { '.' });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", super::super::text(&buf))
}

#[test]
fn hover_tints_a_link() {
    let mut app = replied(60, 10, "[docs](https://example.com/a) tail");
    app.set_opener(true);
    let (buf, targets) = draw(&app, 60, 10);
    let link = targets
        .iter()
        .find(|target| matches!(target.id, crate::mouse::TargetId::Link { .. }))
        .expect("a link target");
    let at = (link.rect.x, link.rect.y);
    // The link's cells draw underlined.
    let underlined = buf
        .cell(at)
        .is_some_and(|cell| cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
    assert!(underlined, "the link underlines");
    insta::assert_snapshot!("hover_tints_a_link", hover_shown(&app, 60, 10, at));
}
