//! The scroll bar's geometry: its thumb rows, its column split and the
//! width the conversation's rows wrap at.

use super::{split, text_width, thumb};
use crate::app::App;
use crate::link::Line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const WIDTH: u16 = 60;
const HEIGHT: u16 = 12;

/// One session envelope.
fn session_line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app attached at 60 by 12, showing one done turn with `text`.
fn replied(text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
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
    app.on_line(session_line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    app
}

#[test]
fn thumb_marks_where_the_view_sits_among_every_row() {
    let cases: Vec<(usize, usize, u16, Option<std::ops::Range<u16>>)> = vec![
        (0, 5, 0, None),
        (0, 9, 10, None),
        (0, 10, 10, None),
        (0, 11, 10, Some(0..9)),
        (1, 11, 10, Some(1..10)),
        (0, 20, 10, Some(0..5)),
        (1, 20, 10, Some(0..5)),
        (5, 20, 10, Some(2..7)),
        (9, 20, 10, Some(4..9)),
        (10, 20, 10, Some(5..10)),
        (11, 20, 10, Some(5..10)),
        (usize::MAX, 20, 10, Some(5..10)),
        (0, 1000, 10, Some(0..1)),
        (989, 1000, 10, Some(8..9)),
        (990, 1000, 10, Some(9..10)),
        (0, 2, 1, Some(0..1)),
        (1, 2, 1, Some(0..1)),
        (usize::MAX / 2, usize::MAX, u16::MAX, Some(32767..32768)),
        (usize::MAX, usize::MAX, u16::MAX, Some(65534..65535)),
    ];
    for (top, total, track, expected) in cases {
        assert_eq!(
            thumb(top, total, track),
            expected,
            "top {top} total {total} track {track}"
        );
    }
}

#[test]
fn split_keeps_the_last_column_for_the_bar() {
    let area = Rect::new(0, 0, 0, 4);
    assert_eq!(
        split(area),
        (area, Rect::new(area.right(), area.y, 0, area.height))
    );
    let area = Rect::new(3, 1, 1, 12);
    assert_eq!(
        split(area),
        (area, Rect::new(area.right(), area.y, 0, area.height))
    );
    assert_eq!(
        split(Rect::new(0, 0, 2, 7)),
        (Rect::new(0, 0, 1, 7), Rect::new(1, 0, 1, 7))
    );
    assert_eq!(
        split(Rect::new(10, 2, 30, 5)),
        (Rect::new(10, 2, 29, 5), Rect::new(39, 2, 1, 5))
    );
    let area = Rect::new(5, 5, 10, 0);
    assert_eq!(split(area), (Rect::new(5, 5, 9, 0), Rect::new(14, 5, 1, 0)));
}

#[test]
fn a_full_row_leaves_the_bar_column_blank() {
    // A 150-wide reply wraps at the rows' width, so its full rows end
    // at the last text column and the bar's column stays blank.
    let long: String = std::iter::repeat_n('w', 150).collect();
    let app = replied(&long);
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let conversation = app.conversation_area();
    for y in conversation.top()..conversation.bottom() {
        assert_eq!(buf[(59, y)].symbol(), " ", "row {y} keeps its last column");
    }
    let widest = (conversation.top()..conversation.bottom())
        .map(|y| (0..WIDTH).filter(|x| buf[(*x, y)].symbol() == "w").count())
        .max()
        .unwrap_or(0);
    assert_eq!(widest, 59);
}

#[test]
fn text_width_is_the_rows_width() {
    for (column, expected) in [(0, 0), (1, 1), (2, 1), (80, 79)] {
        assert_eq!(text_width(column), expected, "column {column}");
        assert_eq!(
            text_width(column),
            split(Rect::new(0, 0, column, 1)).0.width,
            "split agrees at {column}"
        );
    }
}
