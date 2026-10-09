//! The scroll bar's geometry: its thumb rows, its column split and the
//! width the conversation's rows wrap at.

use super::{split, text_width, thumb};
use ratatui::layout::Rect;

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
