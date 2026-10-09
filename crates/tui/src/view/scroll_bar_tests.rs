//! The scroll bar's geometry: its thumb rows, its column split and the
//! width the conversation's rows wrap at.

use super::{THUMB, TRACK, split, text_width, thumb};
use crate::app::App;
use crate::link::Line;
use crate::markdown::Role;
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

/// Fourteen done turns: a history taller than the screen.
fn tall() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(WIDTH, HEIGHT);
    app.attach(contract::SessionId(SESSION.to_owned()));
    for n in 1..=14 {
        app.on_line(session_line(
            "turn_started",
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {n}")}]}]}),
            None,
        ));
        app.on_line(session_line(
            "turn_completed",
            json!({"outcome": "completed"}),
            None,
        ));
    }
    app
}

/// `app` drawn at `width` by `height`.
fn rendered(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
}

/// The thumb's rows within the bar's column, top-relative.
fn thumb_rows(app: &App, buf: &Buffer) -> Vec<u16> {
    let conversation = app.conversation_area();
    (conversation.top()..conversation.bottom())
        .filter(|y| buf[(WIDTH - 1, *y)].symbol() == THUMB)
        .map(|y| y - conversation.y)
        .collect()
}

/// The drawn thumb is the rows `thumb` names for the shown view, over a
/// conversation as tall as the view counts.
fn check_bar(app: &App, buf: &Buffer) {
    let (top, total) = app.scroll();
    let rows = app.conversation_height();
    let conversation = app.conversation_area();
    assert_eq!(rows, usize::from(conversation.height));
    let track = u16::try_from(rows).unwrap_or(u16::MAX);
    let expected: Vec<u16> = thumb(top, total, track)
        .expect("a thumb while the history is taller")
        .collect();
    assert_eq!(thumb_rows(app, buf), expected);
}

#[test]
fn scroll_bar_at_the_top() {
    let mut app = tall();
    app.jump(0);
    let buf = rendered(&app, WIDTH, HEIGHT);
    // The thumb's first cell is the conversation's first row.
    assert_eq!(thumb_rows(&app, &buf).first(), Some(&0));
    check_bar(&app, &buf);
    insta::assert_snapshot!("scroll_bar_at_the_top", crate::view::text(&buf));
}

#[test]
fn scroll_bar_in_the_middle() {
    let mut app = tall();
    app.jump(app.scroll().1 / 2);
    let buf = rendered(&app, WIDTH, HEIGHT);
    // The thumb touches neither end of the track.
    let found = thumb_rows(&app, &buf);
    let track = app.conversation_area().height;
    assert!(found.first().is_some_and(|first| *first > 0));
    assert!(found.last().is_some_and(|last| *last < track - 1));
    check_bar(&app, &buf);
    insta::assert_snapshot!("scroll_bar_in_the_middle", crate::view::text(&buf));
}

#[test]
fn scroll_bar_at_the_end() {
    let app = tall();
    let buf = rendered(&app, WIDTH, HEIGHT);
    // Following: the thumb's last cell is the conversation's last row.
    let track = app.conversation_area().height;
    assert_eq!(thumb_rows(&app, &buf).last(), Some(&(track - 1)));
    check_bar(&app, &buf);
    insta::assert_snapshot!("scroll_bar_at_the_end", crate::view::text(&buf));
}

#[test]
fn no_scroll_bar_when_everything_fits() {
    let app = replied("hi");
    let buf = rendered(&app, WIDTH, HEIGHT);
    // Neither glyph in the bar's column.
    let conversation = app.conversation_area();
    for y in conversation.top()..conversation.bottom() {
        assert_ne!(buf[(WIDTH - 1, y)].symbol(), THUMB, "row {y}");
        assert_ne!(buf[(WIDTH - 1, y)].symbol(), TRACK, "row {y}");
    }
    insta::assert_snapshot!(
        "no_scroll_bar_when_everything_fits",
        crate::view::text(&buf)
    );
}

#[test]
fn the_bar_is_muted_and_its_glyphs_differ() {
    // The bar is a rule with a grip: both glyphs share the muted role,
    // and differ by glyph, so no colour keeps them apart.
    assert_ne!(THUMB, TRACK);
    let app = tall();
    let buf = rendered(&app, WIDTH, HEIGHT);
    let conversation = app.conversation_area();
    let (top, total) = app.scroll();
    let covered = thumb(top, total, conversation.height).expect("a thumb while taller");
    let empty = Buffer::empty(Rect::new(0, 0, WIDTH, HEIGHT));
    for y in conversation.top()..conversation.bottom() {
        let cell = &buf[(WIDTH - 1, y)];
        assert_eq!(cell.fg, Role::Muted.color(), "row {y}");
        // The bar sets the foreground and the symbol only: its cells
        // keep the background the buffer already has.
        assert_eq!(cell.bg, empty[(WIDTH - 1, y)].bg, "row {y}");
        let want = if covered.contains(&(y - conversation.y)) {
            THUMB
        } else {
            TRACK
        };
        assert_eq!(cell.symbol(), want, "row {y}");
    }
}

#[test]
fn no_bar_at_one_column() {
    // A conversation never wraps at 0 columns: at width 1 the rows keep
    // the whole column and no bar draws.
    let mut app = tall();
    app.set_size(1, HEIGHT);
    let buf = rendered(&app, 1, HEIGHT);
    for y in 0..HEIGHT {
        assert_ne!(buf[(0, y)].symbol(), THUMB, "row {y}");
        assert_ne!(buf[(0, y)].symbol(), TRACK, "row {y}");
    }
    // A direct draw into the zero-width bar rect touches nothing.
    let (_, bar) = split(Rect::new(0, 0, 1, HEIGHT));
    let mut narrow = Buffer::empty(Rect::new(0, 0, 3, HEIGHT));
    super::draw(&app, bar, &mut narrow);
    for y in 0..HEIGHT {
        assert_eq!(narrow[(1, y)].symbol(), " ", "row {y}");
    }
}

#[test]
fn a_notice_draws_over_the_bar() {
    let mut app = tall();
    app.connect_failed("Could not reach the hub: refused".to_owned());
    let buf = rendered(&app, WIDTH, HEIGHT);
    // The notice floats over the conversation's top-right corner: its
    // first row ends in its dismiss mark, not a bar glyph.
    let cell = &buf[(WIDTH - 1, app.conversation_area().y)];
    assert_eq!(cell.symbol(), "✕");
    assert_ne!(cell.symbol(), THUMB);
    assert_ne!(cell.symbol(), TRACK);
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

#[test]
fn a_bar_below_the_top_row_draws_its_thumb_on_its_own_rows() {
    // The thumb rows are relative to the bar's own top, wherever it sits.
    let app = tall();
    let bar = Rect::new(2, 3, 1, 5);
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 10));
    super::draw(&app, bar, &mut buf);
    let total = app.scroll().1;
    let top = app.top().unwrap_or(total);
    let covered = thumb(top, total, bar.height).expect("a thumb while taller");
    for y in 0..10u16 {
        let want = match y.checked_sub(bar.y) {
            Some(at) if at < bar.height => {
                if covered.contains(&at) {
                    THUMB
                } else {
                    TRACK
                }
            }
            _ => " ",
        };
        assert_eq!(buf[(bar.x, y)].symbol(), want, "row {y}");
    }
}
