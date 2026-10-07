//! Tests for clicks on the conversation and the copy target.

use super::super::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;

/// One session envelope.
fn session_line(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
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
    crate::view::render(app, area, &mut buf);
    crate::view::text(&buf)
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|at| u16::try_from(at).ok())
}

const TWO_BLOCKS: &str = "```rust\nlet a = 1;\n```\n\ntext\n\n```sh\necho b\n```";

#[test]
fn a_click_on_copy_copies_that_blocks_code_and_shows_copied() {
    let mut app = with_reply(30, 12, TWO_BLOCKS);
    let first = row_of(&app, 30, 12, "rust").expect("first header");
    let second = row_of(&app, 30, 12, "sh  ").expect("second header");
    assert_eq!(
        app.on_click(26, first),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert!(app.copied());
    assert_eq!(app.on_click(29, second), Effect::Copy("echo b".to_owned()));
    // The cells left of `copy` and the rows around it copy nothing, and a
    // click clears "Copied".
    for (col, row) in [(25, first), (0, first), (26, first + 1), (26, first - 1)] {
        assert_eq!(app.on_click(col, row), Effect::None, "{col},{row}");
        assert!(!app.copied());
    }
}

#[test]
fn any_key_clears_copied() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = with_reply(30, 12, TWO_BLOCKS);
    let first = row_of(&app, 30, 12, "rust").expect("first header");
    app.on_click(27, first);
    assert!(app.copied());
    app.on_key(Key::Char('x'), now);
    assert!(!app.copied());
    app.on_click(27, first);
    app.on_key(Key::CtrlC, now);
    assert!(!app.copied());
}

#[test]
fn a_click_maps_through_the_rows_while_scrolled_up() {
    let now = fakes::clock::FakeClock::new().now();
    let block = "```rust\nlet a = 1;\n```\n\n";
    let lines: String = (1..=12).map(|n| format!("line {n}\n\n")).collect();
    let mut app = with_reply(30, 8, &format!("{block}{lines}"));
    assert_eq!(row_of(&app, 30, 8, "rust"), None);
    // Scroll up until the header shows; the click follows the drawn row.
    while row_of(&app, 30, 8, "rust").is_none() {
        app.on_key(Key::PageUp, now);
    }
    let header = row_of(&app, 30, 8, "rust").expect("header");
    assert_eq!(
        app.on_click(27, header),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert_eq!(app.on_click(27, header + 1), Effect::None);
}

#[test]
fn the_overlay_row_hides_a_copy_target_under_it() {
    let now = fakes::clock::FakeClock::new().now();
    let filler: String = (1..=12).map(|n| format!("line {n}\n\n")).collect();
    let code = "1\n2\n3\n4\n5\n6";
    let mut app = with_reply(30, 8, &format!("{filler}```rust\n{code}\n```"));
    // Seven conversation rows: one PageUp moves the top up six, so the
    // header, six rows from the end, lands on the last conversation row.
    assert_eq!(app.conversation_height(), 7);
    app.on_key(Key::PageUp, now);
    assert_eq!(row_of(&app, 30, 8, "rust"), Some(6));
    assert_eq!(app.on_click(27, 6), Effect::Copy(code.to_owned()));
    app.on_line(turn_started("more"));
    assert!(app.has_new());
    assert_eq!(row_of(&app, 30, 8, "rust"), None);
    assert_eq!(app.on_click(27, 6), Effect::None);
}

#[test]
fn a_click_below_the_conversation_or_on_a_plain_line_copies_nothing() {
    let mut app = with_reply(30, 12, TWO_BLOCKS);
    let prompt = row_of(&app, 30, 12, " hi").expect("prompt");
    assert_eq!(app.on_click(27, prompt), Effect::None);
    assert_eq!(app.on_click(27, 11), Effect::None);
    assert_eq!(app.on_click(27, 200), Effect::None);
}

#[test]
fn a_resize_moves_the_copy_target_with_the_new_width() {
    let mut app = with_reply(30, 12, TWO_BLOCKS);
    app.set_size(20, 12);
    let first = row_of(&app, 20, 12, "rust").expect("first header");
    assert_eq!(app.on_click(26, first), Effect::None);
    assert_eq!(
        app.on_click(16, first),
        Effect::Copy("let a = 1;".to_owned())
    );
}

#[test]
fn a_click_maps_past_a_prompt_that_wraps() {
    let mut app = attached(30, 12);
    app.on_line(turn_started(&"w".repeat(70)));
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({ "text": "```rust\nlet a = 1;\n```" }),
        Some("a_1"),
    ));
    let header = row_of(&app, 30, 12, "rust").expect("header");
    assert_eq!(header, 9);
    assert_eq!(
        app.on_click(27, header),
        Effect::Copy("let a = 1;".to_owned())
    );
    assert_eq!(app.on_click(27, header - 1), Effect::None);
    assert_eq!(app.on_click(27, header + 1), Effect::None);
}

#[test]
fn a_click_on_the_blank_rows_above_a_short_conversation_copies_nothing() {
    let mut app = with_reply(30, 12, "```rust\nlet a = 1;\n```");
    let header = row_of(&app, 30, 12, "rust").expect("header");
    assert!(row_of(&app, 30, 12, " hi").is_some_and(|prompt| prompt > 0));
    assert_eq!(app.on_click(27, 0), Effect::None);
    assert_eq!(
        app.on_click(27, header),
        Effect::Copy("let a = 1;".to_owned())
    );
}
