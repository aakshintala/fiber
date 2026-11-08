//! Tests for clicks on the conversation and the copy target.

use super::super::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use contract::events::EventKind;
use std::path::PathBuf;

/// One session envelope.
fn session_line(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `turn_started` line with one message.
fn turn_started(text: &str) -> Line {
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": text}]}]});
    session_line("turn_started", input, None)
}

/// An app at `width` by `height`, attached, with nothing on screen.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// An app at `width` by `height`, attached, with `text` streamed as one
/// reply after a prompt.
fn with_reply(width: u16, height: u16, text: &str) -> App {
    let mut app = attached(width, height);
    app.on_line(turn_started("hi"));
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({ "text": text }),
        Some("a_1"),
    ));
    app
}

/// The screen row whose text contains `needle`, drawn at the app's size.
fn row_of(app: &App, width: u16, height: u16, needle: &str) -> Option<u16> {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|at| u16::try_from(at).ok())
}

/// A left click at a 0-based cell of the app drawn at its size, as the
/// loop handles one: the press clears "Copied", then the target drawn
/// there, if any, is clicked.
fn click(app: &mut App, col: u16, row: u16) -> Effect {
    app.clear_copied();
    let area = ratatui::layout::Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    crate::mouse::hit(&targets, col, row).map_or(Effect::None, |target| app.on_click(target))
}

const TWO_BLOCKS: &str = "```rust\nlet a = 1;\n```\n\ntext\n\n```sh\necho b\n```";

#[test]
fn a_click_on_copy_copies_that_blocks_code_and_shows_copied() {
    let mut app = with_reply(40, 20, TWO_BLOCKS);
    let first = row_of(&app, 40, 20, "rust").expect("first header");
    let second = row_of(&app, 40, 20, "sh  ").expect("second header");
    assert_eq!(
        click(&mut app, 36, first),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert!(app.copied());
    assert_eq!(
        click(&mut app, 37, second),
        Effect::Copy("echo b".to_owned())
    );
    // The cells left of `copy` and the rows around it copy nothing, and a
    // click clears "Copied".
    for (col, row) in [(24, first), (2, first), (36, first + 1), (36, first - 1)] {
        click(&mut app, 37, second);
        assert_eq!(click(&mut app, col, row), Effect::None, "{col},{row}");
        assert!(!app.copied());
    }
    click(&mut app, 37, second);
    app.on_click(crate::mouse::TargetId::NewBelow);
    assert!(!app.copied());
}

#[test]
fn any_key_clears_copied() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = with_reply(40, 20, TWO_BLOCKS);
    let first = row_of(&app, 40, 20, "rust").expect("first header");
    click(&mut app, 37, first);
    assert!(app.copied());
    app.on_key(Key::Char('x'), now);
    assert!(!app.copied());
    click(&mut app, 37, first);
    app.on_key(Key::CtrlC, now);
    assert!(!app.copied());
}

#[test]
fn a_click_maps_through_the_rows_while_scrolled_up() {
    let now = fakes::clock::FakeClock::new().now();
    let block = "```rust\nlet a = 1;\n```\n\n";
    let lines: String = (1..=12).map(|n| format!("line {n}\n\n")).collect();
    let mut app = with_reply(40, 10, &format!("{block}{lines}"));
    assert_eq!(row_of(&app, 40, 10, "rust"), None);
    // Scroll up until the header shows; the click follows the drawn row.
    while row_of(&app, 40, 10, "rust").is_none() {
        app.on_key(Key::PageUp, now);
    }
    let header = row_of(&app, 40, 10, "rust").expect("header");
    assert_eq!(
        click(&mut app, 37, header),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert_eq!(click(&mut app, 37, header + 1), Effect::None);
}

#[test]
fn the_overlay_row_hides_a_copy_target_under_it() {
    let now = fakes::clock::FakeClock::new().now();
    let filler: String = (1..=12).map(|n| format!("line {n}\n\n")).collect();
    let code = "1\n2\n3\n4";
    let mut app = with_reply(40, 10, &format!("{filler}```rust\n{code}\n```"));
    // Five conversation rows: one PageUp moves the top up four. Nudge
    // back down so the code header sits on the bottom conversation
    // row, where the overlay draws.
    assert_eq!(app.conversation_height(), 5);
    app.on_key(Key::PageUp, now);
    let top = app.top().expect("scrolled up");
    app.jump(top.saturating_sub(2));
    assert_eq!(row_of(&app, 40, 10, "rust"), Some(4));
    assert_eq!(click(&mut app, 37, 4), Effect::Copy(code.to_owned()));
    app.on_line(turn_started("more"));
    assert!(app.has_new());
    // The overlay covers the header's row, so the same cells copy
    // nothing now.
    assert_eq!(row_of(&app, 40, 10, "New messages"), Some(4));
    assert_eq!(click(&mut app, 37, 4), Effect::None);
}

#[test]
fn a_click_below_the_conversation_or_on_a_plain_line_copies_nothing() {
    let mut app = with_reply(40, 12, TWO_BLOCKS);
    // The plain line between the two code blocks has no target, and
    // neither does the input box.
    let plain = row_of(&app, 40, 12, "text").expect("plain line");
    assert_eq!(click(&mut app, 37, plain), Effect::None);
    assert_eq!(click(&mut app, 37, 10), Effect::None);
    assert_eq!(click(&mut app, 37, 200), Effect::None);
}

#[test]
fn a_resize_moves_the_copy_target_with_the_new_width() {
    let mut app = with_reply(60, 12, TWO_BLOCKS);
    app.set_size(40, 20);
    // At 40 columns the label sits at the new right edge; the cell it
    // held at 60 is off the screen.
    let first = row_of(&app, 40, 20, "copy").expect("first header");
    assert_eq!(click(&mut app, 56, first), Effect::None);
    assert_eq!(
        click(&mut app, 37, first),
        Effect::Copy("let a = 1;".to_owned())
    );
}

#[test]
fn a_click_maps_past_a_prompt_that_wraps() {
    let mut app = attached(40, 14);
    app.on_line(turn_started(&"w".repeat(70)));
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({ "text": "```rust\nlet a = 1;\n```" }),
        Some("a_1"),
    ));
    let header = row_of(&app, 40, 14, "rust").expect("header");
    assert_eq!(header, 5);
    assert_eq!(
        click(&mut app, 37, header),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert_eq!(click(&mut app, 37, header - 1), Effect::None);
    assert_eq!(click(&mut app, 37, header + 1), Effect::None);
}

#[test]
fn a_click_on_the_blank_rows_above_a_short_conversation_copies_nothing() {
    let mut app = with_reply(40, 16, "```rust\nlet a = 1;\n```");
    let header = row_of(&app, 40, 16, "rust").expect("header");
    assert!(row_of(&app, 40, 16, " hi").is_some_and(|prompt| prompt > 0));
    assert_eq!(click(&mut app, 37, 0), Effect::None);
    assert_eq!(
        click(&mut app, 37, header),
        Effect::Copy("let a = 1;".to_owned())
    );
}

#[test]
fn a_click_copies_from_the_reply_it_lands_on() {
    let mut app = with_reply(40, 14, "```rust\nlet a = 1;\n```");
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({ "text": "```sh\necho b\n```" }),
        Some("a_2"),
    ));
    let second = row_of(&app, 40, 14, "sh  ").expect("second reply's header");
    assert_eq!(
        click(&mut app, 37, second),
        Effect::Copy("echo b".to_owned())
    );
}

#[test]
fn a_copy_target_in_a_quote_is_on_its_drawn_copy_cells() {
    let mut app = with_reply(40, 12, "> ```rust\n> let a = 1;\n> ```");
    let header = row_of(&app, 40, 12, "copy").expect("header");
    assert_eq!(
        click(&mut app, 36, header),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert_eq!(click(&mut app, 24, header), Effect::None);
    assert_eq!(click(&mut app, 2, header), Effect::None);
}

#[test]
fn the_copy_cells_are_the_drawn_label_inside_the_padding() {
    // At W = 59 the card pads by one, so the copy cells are the drawn
    // label shifted past it (`docs/tui.md`, "Turns", "Selection and copy").
    let app = with_reply(60, 20, "```rust\nlet a = 1;\n```");
    let copy = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| match target {
            super::super::Target::Copy { .. } => Some(target),
            super::super::Target::Group(_)
            | super::super::Target::Call(_)
            | super::super::Target::CallView(_)
            | super::super::Target::Thought(_)
            | super::super::Target::Login
            | super::super::Target::Note(_)
            | super::super::Target::Image(_)
            | super::super::Target::Orphans(_) => None,
        })
        .expect("a copy target");
    let cols = app.copy_cells(copy).expect("copy cells");
    let area = ratatui::layout::Rect::new(0, 0, 60, 20);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let row = text
        .lines()
        .find(|line| line.contains("click to copy"))
        .expect("the drawn label");
    let start = row.find("click to copy").expect("the label's start");
    let start = u16::try_from(crate::format::width(&row[..start])).expect("a column");
    // The cells are relative to the conversation's left edge, past the
    // grip and the gutter.
    let origin = app.conversation_area().x;
    assert_eq!(
        cols.start + origin..cols.end + origin,
        start..start.saturating_add(13)
    );
    assert_eq!(cols, 43..56);
}
