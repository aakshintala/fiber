//! Tests for the views' frame: the selection's keys and scrolling, and
//! what the frame draws (`docs/tui.md`, "Swapped views").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{Frame, List, Spot, about, render, rows_height};
use crate::keys::Key;
use crate::mouse::TargetId;

/// A list after pressing `keys` over `rows` rows shown `height` at a time.
fn pressed(keys: &[Key], rows: usize, height: usize) -> List {
    let mut list = List::default();
    for key in keys {
        list.key(key, rows, height);
    }
    list
}

#[test]
fn selection_clamps_at_both_ends() {
    let list = pressed(&[Key::Up], 5, 10);
    assert_eq!(list.selected(), 0);
    let list = pressed(&vec![Key::Down; 9], 5, 10);
    assert_eq!(list.selected(), 4);
    let list = pressed(&[Key::PageDown, Key::PageDown], 5, 3);
    assert_eq!(list.selected(), 4);
    let list = pressed(&[Key::Down, Key::PageUp], 5, 3);
    assert_eq!(list.selected(), 0);
}

#[test]
fn page_moves_by_the_height_less_one() {
    let mut list = List::default();
    assert!(list.key(&Key::PageDown, 100, 10));
    assert_eq!(list.selected(), 9);
    assert!(list.key(&Key::PageUp, 100, 10));
    assert_eq!(list.selected(), 0);
    // At the top nothing moves, and the key says so.
    assert!(!list.key(&Key::PageUp, 100, 10));
    assert!(!list.key(&Key::Up, 100, 10));
    // A one-row view still moves by one.
    assert!(list.key(&Key::PageDown, 100, 1));
    assert_eq!(list.selected(), 1);
    // Any other key leaves it.
    assert!(!list.key(&Key::Enter, 100, 10));
    assert_eq!(list.selected(), 1);
}

#[test]
fn scroll_keeps_the_selection_in_view() {
    for height in [1, 3, 10] {
        let mut list = List::default();
        for _ in 0..20 {
            list.key(&Key::Down, 20, height);
            assert!(list.top() <= list.selected(), "height {height}");
            assert!(list.selected() < list.top() + height, "height {height}");
        }
        assert_eq!(list.selected(), 19, "height {height}");
        assert_eq!(list.top(), 20 - height, "height {height}");
        for _ in 0..20 {
            list.key(&Key::Up, 20, height);
            assert!(list.top() <= list.selected(), "height {height}");
            assert!(list.selected() < list.top() + height, "height {height}");
        }
        assert_eq!((list.selected(), list.top()), (0, 0), "height {height}");
    }
}

#[test]
fn an_empty_list_ignores_every_key() {
    for key in [Key::Up, Key::Down, Key::PageUp, Key::PageDown] {
        let mut list = List::default();
        assert!(!list.key(&key, 0, 10));
        assert_eq!((list.selected(), list.top()), (0, 0));
    }
}

#[test]
fn zero_height_keeps_the_selection() {
    let mut list = List::default();
    list.key(&Key::Down, 5, 3);
    assert!(!list.key(&Key::Down, 5, 0));
    assert_eq!((list.selected(), list.top()), (1, 0));
}

#[test]
fn a_shrunk_list_clamps_on_select() {
    let mut list = List::default();
    list.select(9, 10, 3);
    list.select(list.selected(), 4, 3);
    assert_eq!(list.selected(), 3);
    assert!(list.top() <= 3);
}

#[test]
fn about_groups_thousands() {
    for (tokens, said) in [
        (0, "0"),
        (999, "999"),
        (1000, "1,000"),
        (180_000, "180,000"),
        (1_234_567, "1,234,567"),
    ] {
        assert_eq!(about(tokens), said);
    }
}

/// A frame of `rows` one-cell rows, one line below and a footer.
fn frame(rows: usize) -> Frame {
    Frame {
        title: "Settings".to_owned(),
        rows: (0..rows)
            .map(|at| vec![(format!("row {at}"), None)])
            .collect(),
        list: List::default(),
        below: vec!["below".to_owned()],
        field: None,
        footer: "keys".to_owned(),
    }
}

/// The text of row `y` of `buf`, trailing spaces trimmed.
fn text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn the_frame_draws_the_header_rows_below_and_footer() {
    let area = Rect::new(0, 0, 20, 6);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let mut shown = frame(5);
    shown.list.select(1, 5, rows_height(&shown, 6));
    render(&shown, area, &mut buf, &mut targets);
    assert_eq!(rows_height(&shown, 6), 3);
    assert_eq!(text(&buf, 0), "Settings           ✕");
    assert_eq!(text(&buf, 1), "row 0");
    assert_eq!(text(&buf, 2), "row 1");
    assert_eq!(text(&buf, 3), "row 2");
    assert_eq!(text(&buf, 4), "below");
    assert_eq!(text(&buf, 5), "keys");
    assert!(buf[(0, 2)].modifier.contains(Modifier::REVERSED));
    assert!(!buf[(0, 1)].modifier.contains(Modifier::REVERSED));
    let ids: Vec<TargetId> = targets.iter().map(|target| target.id).collect();
    assert_eq!(
        ids,
        [
            TargetId::View(Spot::Close),
            TargetId::View(Spot::Row(0)),
            TargetId::View(Spot::Row(1)),
            TargetId::View(Spot::Row(2)),
        ]
    );
}

#[test]
fn the_field_scrolls_to_show_its_caret() {
    let area = Rect::new(0, 0, 10, 4);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(0);
    shown.below.clear();
    shown.field = Some(("abcdefghijkl".to_owned(), 12));
    render(&shown, area, &mut buf, &mut Vec::new());
    // Seven columns of text fit beside `> ` and the caret's cell.
    assert_eq!(text(&buf, 2), "> fghijkl");
    assert!(buf[(9, 2)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn the_field_takes_a_line_from_the_rows() {
    let mut shown = frame(0);
    shown.below.clear();
    assert_eq!(rows_height(&shown, 6), 4);
    shown.field = Some(("x".to_owned(), 0));
    assert_eq!(rows_height(&shown, 6), 3);
}

#[test]
fn a_field_never_draws_over_the_footer() {
    let area = Rect::new(0, 0, 20, 3);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(0);
    shown.below = vec!["one".to_owned(), "two".to_owned()];
    shown.field = Some(("abcdef".to_owned(), 6));
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 1), "one");
    assert_eq!(text(&buf, 2), "keys");
}

#[test]
fn a_one_row_view_draws_no_footer_over_its_header() {
    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    render(&frame(0), area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 0), "Settings           ✕");
}

#[test]
fn a_caret_past_the_field_is_not_drawn() {
    // Two columns hold `> ` and no text, so the caret has no cell here.
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    let mut shown = frame(0);
    shown.below.clear();
    shown.field = Some(("abc".to_owned(), 0));
    render(&shown, Rect::new(0, 0, 2, 4), &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 2), ">");
    assert!(!buf[(2, 2)].modifier.contains(Modifier::REVERSED));
}
