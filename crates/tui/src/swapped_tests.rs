//! Tests for the views' frame: the selection's keys and scrolling, and
//! what the frame draws (`docs/tui.md`, "Swapped views").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{Frame, Ink, List, Spot, about, compact, render, rows_height};
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::theme::Role;

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
fn a_cell_with_a_spot_gets_a_target_over_its_text() {
    let area = Rect::new(0, 0, 20, 3);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(0);
    shown.below.clear();
    shown.rows = vec![vec![
        ("ab".to_owned(), None, Ink::Plain),
        (
            "[x]".to_owned(),
            Some(Spot::Switch { row: 0, at: 0 }),
            Ink::Plain,
        ),
    ]];
    let mut targets = Vec::new();
    render(&shown, area, &mut buf, &mut targets);
    let switch = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Switch { row: 0, at: 0 }))
        .unwrap_or_else(|| panic!("no switch target in {targets:?}"));
    assert_eq!(switch.rect.x, 2);
    assert_eq!(switch.rect.width, 3);
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
fn compact_counts_stay_exact_under_a_thousand() {
    for (tokens, said) in [(0, "0"), (1, "1"), (999, "999")] {
        assert_eq!(compact(tokens), said);
    }
}

#[test]
fn compact_counts_round_half_up_to_whole_thousands() {
    for (tokens, said) in [
        (1000, "1k"),
        (1499, "1k"),
        (1500, "2k"),
        (15_000, "15k"),
        (120_000, "120k"),
        (268_000, "268k"),
        (400_000, "400k"),
        (999_499, "999k"),
    ] {
        assert_eq!(compact(tokens), said, "{tokens}");
    }
}

#[test]
fn compact_counts_past_a_million_read_in_millions() {
    for (tokens, said) in [(999_500, "1M"), (1_000_000, "1M")] {
        assert_eq!(compact(tokens), said, "{tokens}");
    }
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
        command: "/settings".to_owned(),
        rows: (0..rows)
            .map(|at| vec![(format!("row {at}"), None, Ink::Plain)])
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
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let mut shown = frame(5);
    shown.list.select(1, 5, rows_height(&shown, 6));
    render(&shown, area, &mut buf, &mut targets);
    assert_eq!(rows_height(&shown, 6), 3);
    assert_eq!(
        text(&buf, 0),
        format!(" Settings  /settings{}Esc returns", " ".repeat(9))
    );
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

/// The top row's cells at `width`: name bold, command and `Esc
/// returns` dim, all on the `surface` tint.
fn top_row(title: &str, command: &'static str, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 3);
    let mut buf = Buffer::empty(area);
    let frame = Frame {
        title: title.to_owned(),
        command: command.to_owned(),
        rows: Vec::new(),
        list: List::default(),
        below: Vec::new(),
        field: None,
        footer: String::new(),
    };
    render(&frame, area, &mut buf, &mut Vec::new());
    buf
}

#[test]
fn the_top_row_names_the_view_with_its_command_dim() {
    let buf = top_row("Context", "/context", 40);
    assert_eq!(
        text(&buf, 0),
        format!(" Context  /context{}Esc returns", " ".repeat(11))
    );
    // The whole row sits on the `surface` tint.
    for x in 0..40 {
        assert_eq!(buf[(x, 0)].bg, Role::Surface.color(), "no tint at {x}");
    }
    // The name is bold from column 1; the command follows it, dim.
    for (at, cell) in "Context".chars().enumerate() {
        let x = u16::try_from(at + 1).expect("a column");
        assert_eq!(buf[(x, 0)].symbol(), cell.to_string().as_str());
        assert!(
            buf[(x, 0)].modifier.contains(Modifier::BOLD),
            "not bold at {x}"
        );
    }
    for (at, cell) in "  /context".chars().enumerate() {
        let x = u16::try_from(at + 8).expect("a column");
        assert_eq!(buf[(x, 0)].symbol(), cell.to_string().as_str());
        assert!(
            buf[(x, 0)].modifier.contains(Modifier::DIM),
            "not dim at {x}"
        );
    }
    // `Esc returns` ends the row, dim.
    for (at, cell) in "Esc returns".chars().enumerate() {
        let x = u16::try_from(at + 29).expect("a column");
        assert_eq!(buf[(x, 0)].symbol(), cell.to_string().as_str());
        assert!(
            buf[(x, 0)].modifier.contains(Modifier::DIM),
            "Esc not dim at {x}"
        );
    }
    assert!(!text(&buf, 0).contains('✕'), "no cross on the top row");
}

#[test]
fn the_whole_top_row_is_the_close_target() {
    for width in [21, 22, 40] {
        let area = Rect::new(0, 0, width, 3);
        let mut buf = Buffer::empty(area);
        let mut targets = Vec::new();
        render(&frame(1), area, &mut buf, &mut targets);
        let close: Vec<Rect> = targets
            .iter()
            .filter(|target| target.id == TargetId::View(Spot::Close))
            .map(|target| target.rect)
            .collect();
        assert_eq!(
            close,
            [Rect::new(0, 0, width, 1)],
            "the whole row closes at width {width}"
        );
        // The close target is pushed before every row's own target.
        assert_eq!(targets[0].id, TargetId::View(Spot::Close));
    }
}

#[test]
fn a_narrow_top_row_drops_the_command_before_esc() {
    // `" Settings"` is 9 cells, `"  /settings"` 11, `Esc returns`
    // 11: the full row needs 32, the name with Esc 21, the name 9.
    assert_eq!(
        text(&top_row("Settings", "/settings", 40), 0),
        format!(" Settings  /settings{}Esc returns", " ".repeat(9))
    );
    assert_eq!(
        text(&top_row("Settings", "/settings", 32), 0),
        " Settings  /settings Esc returns"
    );
    assert_eq!(
        text(&top_row("Settings", "/settings", 31), 0),
        format!(" Settings{}Esc returns", " ".repeat(11))
    );
    assert_eq!(
        text(&top_row("Settings", "/settings", 22), 0),
        format!(" Settings{}Esc returns", " ".repeat(2))
    );
    assert_eq!(
        text(&top_row("Settings", "/settings", 21), 0),
        " Settings Esc returns"
    );
    assert_eq!(text(&top_row("Settings", "/settings", 20), 0), " Settings");
    assert_eq!(text(&top_row("Settings", "/settings", 9), 0), " Settings");
    assert_eq!(text(&top_row("Settings", "/settings", 8), 0), " Setting");
}

#[test]
fn a_heading_cell_is_bold_and_a_muted_cell_is_muted() {
    let frame = Frame {
        title: "Models".to_owned(),
        command: "/model".to_owned(),
        rows: vec![
            vec![("acme".to_owned(), None, Ink::Heading)],
            vec![
                ("m1  ".to_owned(), Some(Spot::Cell(1, 0)), Ink::Plain),
                (
                    " · review   ".to_owned(),
                    Some(Spot::Cell(1, 1)),
                    Ink::Muted,
                ),
            ],
        ],
        list: List::default(),
        below: Vec::new(),
        field: None,
        footer: "keys".to_owned(),
    };
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&frame, area, &mut buf, &mut targets);
    // The heading draws bold, the plain and muted cells do not.
    assert!(buf[(0, 1)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(0, 2)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(5, 2)].modifier.contains(Modifier::BOLD));
    // The muted cell draws in the muted role's colour; the plain cells
    // keep the default colour.
    assert_eq!(buf[(5, 2)].fg, Role::Muted.color());
    assert_eq!(buf[(0, 1)].fg, ratatui::style::Color::Reset);
    assert_eq!(buf[(0, 2)].fg, ratatui::style::Color::Reset);
    // Each cell with a target of its own pushes it after its row's.
    let ids: Vec<TargetId> = targets.iter().map(|target| target.id).collect();
    assert_eq!(
        ids,
        [
            TargetId::View(Spot::Close),
            TargetId::View(Spot::Row(0)),
            TargetId::View(Spot::Row(1)),
            TargetId::View(Spot::Cell(1, 0)),
            TargetId::View(Spot::Cell(1, 1)),
        ]
    );
}

#[test]
fn a_role_cell_draws_its_role_and_a_muted_cell_is_faint() {
    let frame = Frame {
        title: "Context".to_owned(),
        command: "/context".to_owned(),
        rows: vec![
            vec![("■ ".to_owned(), None, Ink::Role(Role::Error))],
            vec![("tail".to_owned(), None, Ink::Muted)],
        ],
        list: List::default(),
        below: Vec::new(),
        field: None,
        footer: String::new(),
    };
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&frame, area, &mut buf, &mut targets);
    // The role cell draws its role's colour; the muted cell is faint
    // in the muted colour.
    assert_eq!(buf[(0, 1)].fg, Role::Error.color());
    assert_eq!(buf[(0, 2)].fg, Role::Muted.color());
    assert!(buf[(0, 2)].modifier.contains(Modifier::DIM));
    assert!(!buf[(0, 1)].modifier.contains(Modifier::DIM));
}

#[test]
fn the_footer_and_the_lines_below_draw_dim() {
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    render(&frame(1), area, &mut buf, &mut Vec::new());
    for (y, word) in [(2, "below"), (3, "keys")] {
        for x in 0..u16::try_from(word.len()).expect("a width") {
            let cell = &buf[(x, y)];
            assert!(cell.modifier.contains(Modifier::DIM), "{word} at {x}");
            assert_eq!(cell.fg, Role::Muted.color(), "{word} at {x}");
        }
    }
    // A row stays as its ink draws it.
    assert!(!buf[(0, 1)].modifier.contains(Modifier::DIM));
}

#[test]
fn a_gutter_list_marks_its_selected_row_and_draws_no_bar() {
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let mut shown = frame(5);
    shown.list = List::gutter();
    shown.rows[1][0].1 = Some(Spot::Revoke(1));
    shown.list.select(1, 5, rows_height(&shown, 6));
    render(&shown, area, &mut buf, &mut targets);
    assert!(shown.list.is_gutter());
    assert!(!List::default().is_gutter());
    assert_eq!(text(&buf, 1), "  row 0");
    assert_eq!(text(&buf, 2), "› row 1");
    assert_eq!(text(&buf, 3), "  row 2");
    for y in 1..4 {
        for x in 0..40 {
            assert!(
                !buf[(x, y)].modifier.contains(Modifier::REVERSED),
                "bar at {x},{y}"
            );
        }
    }
    // A cell's target starts after the gutter; the row's covers the line.
    let revoke = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Revoke(1)))
        .expect("a revoke target");
    assert_eq!((revoke.rect.x, revoke.rect.y), (2, 2));
    let row = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Row(1)))
        .expect("a row target");
    assert_eq!((row.rect.x, row.rect.width), (0, 40));
}

#[test]
fn a_blurred_gutter_list_marks_no_row() {
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(3);
    shown.list = List::gutter().blur();
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 1), "  row 0");
    assert_eq!(text(&buf, 2), "  row 1");
    assert_eq!(text(&buf, 3), "  row 2");
    assert!(!buf[(0, 1)].modifier.contains(Modifier::REVERSED));
    assert_eq!(shown.list.top(), 0);
}

#[test]
fn a_gutter_in_a_one_column_area_does_not_overrun() {
    let area = Rect::new(0, 0, 1, 4);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(2);
    shown.list = List::gutter();
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 1), "›");
}

/// A list after pressing `keys` over `rows` rows shown `height` at a
/// time, moving only onto `takes`; its selection after each key.
fn stepped(keys: &[Key], rows: usize, height: usize, takes: &[usize]) -> Vec<usize> {
    let mut list = List::default();
    list.select(takes[0], rows, height);
    keys.iter()
        .map(|key| {
            list.key_over(key, rows, height, takes);
            list.selected()
        })
        .collect()
}

#[test]
fn arrows_move_only_between_the_rows_that_take_a_key() {
    let takes = [1, 2, 5];
    assert_eq!(
        stepped(
            &[Key::Down, Key::Down, Key::Down, Key::Up, Key::Up, Key::Up],
            8,
            8,
            &takes
        ),
        [2, 5, 5, 2, 1, 1]
    );
    let mut list = List::default();
    list.select(1, 8, 8);
    assert!(list.key_over(&Key::Down, 8, 8, &takes));
    assert!(!list.key_over(&Key::Enter, 8, 8, &takes));
    list.select(5, 8, 8);
    assert!(!list.key_over(&Key::Down, 8, 8, &takes));
    list.select(1, 8, 8);
    assert!(!list.key_over(&Key::Up, 8, 8, &takes));
}

#[test]
fn a_page_key_moves_as_the_plain_list_does() {
    // Only ↑ and ↓ skip: a page lands where the plain list puts it.
    let takes = [1, 3];
    let mut list = List::default();
    list.select(1, 10, 4);
    assert!(list.key_over(&Key::PageDown, 10, 4, &takes));
    assert_eq!(list.selected(), 4);
    assert!(list.key_over(&Key::PageUp, 10, 4, &takes));
    assert_eq!(list.selected(), 1);
}

#[test]
fn rest_on_keeps_a_taking_row_else_the_next_else_the_last() {
    let takes = [2, 5];
    for (from, to) in [(0, 2), (2, 2), (3, 5), (5, 5), (6, 5)] {
        let mut list = List::default();
        list.select(from, 8, 8);
        list.rest_on(&takes, 8, 8);
        assert_eq!(list.selected(), to, "from {from}");
    }
    // With none that take a key, the selection stays.
    let mut list = List::default();
    list.select(3, 8, 8);
    list.rest_on(&[], 8, 8);
    assert_eq!(list.selected(), 3);
    // The window follows: from row 6 the three-row window starts at 4,
    // and row 5 is still in it.
    let mut list = List::default();
    list.select(6, 8, 3);
    list.rest_on(&takes, 8, 3);
    assert_eq!((list.selected(), list.top()), (5, 4));
    // From row 0 to row 2 the window stays at the top.
    let mut list = List::default();
    list.rest_on(&takes, 8, 3);
    assert_eq!((list.selected(), list.top()), (2, 0));
}

#[test]
fn with_no_taking_rows_a_key_moves_as_the_plain_list_does() {
    let mut list = List::default();
    assert!(list.key_over(&Key::Down, 5, 5, &[]));
    assert_eq!(list.selected(), 1);
}

#[test]
fn an_empty_footer_draws_nothing() {
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(1);
    shown.footer = String::new();
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 1), "row 0");
    assert_eq!(text(&buf, 2), "below");
    assert_eq!(text(&buf, 3), "");
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
    // Too narrow for the command with `Esc returns`: the name draws.
    assert_eq!(text(&buf, 0), " Settings");
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

#[test]
fn marked_ink_draws_the_role_colour_reversed() {
    let style = Ink::Marked(Role::Added).style();
    assert_eq!(style.fg, Some(Role::Added.color()));
    assert!(style.add_modifier.contains(Modifier::REVERSED));
}

/// A drag over a frame's rows for the selection: press, drag and
/// release over `area`, mapping through the frame `select_frame` notes
/// (`docs/tui.md`, "Selection and copy").
fn drag_over(
    app: &mut crate::app::App,
    frame: &Frame,
    width: u16,
    height: u16,
    from: (u16, u16),
    to: (u16, u16),
) -> crate::app::Effect {
    use crate::keys::{Button, Mouse, MouseKind};
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    super::select_frame(app, frame, area, &mut buf);
    let targets = Vec::new();
    let _ = app.on_select(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: from.0,
            row: from.1,
        },
        &targets,
    );
    let _ = app.on_select(
        &Mouse {
            kind: MouseKind::Drag(Button::Left),
            col: to.0,
            row: to.1,
        },
        &targets,
    );
    app.on_select(
        &Mouse {
            kind: MouseKind::Release,
            col: to.0,
            row: to.1,
        },
        &targets,
    )
}

fn made_frame(rows: &[&str], below: &[&str], field: Option<(&str, usize)>) -> Frame {
    Frame {
        title: "Keys".to_owned(),
        command: "/keys".to_owned(),
        rows: rows
            .iter()
            .map(|row| vec![((*row).to_owned(), None, Ink::Plain)])
            .collect(),
        list: List::gutter(),
        below: below.iter().map(|line| (*line).to_owned()).collect(),
        field: field.map(|(text, caret)| (text.to_owned(), caret)),
        footer: "move".to_owned(),
    }
}

fn app() -> crate::app::App {
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    app.set_size(80, 24);
    app
}

/// A drag over a frame's rows copies their text, the gutter left out:
/// the title and footer never copy (`docs/tui.md`, "Selection and
/// copy").
#[test]
fn a_drag_over_a_frame_copies_its_rows() {
    use crate::app::{Effect, SurfaceId};
    let mut app = app();
    let frame = made_frame(&["alpha", "beta", "gamma"], &[], None);
    let area = Rect::new(0, 0, 80, 24);
    let effect = drag_over(&mut app, &frame, 80, 24, (2, 1), (6, 3));
    assert_eq!(effect, Effect::Copy("alpha\nbeta\ngamma".to_owned()));
    // The drag's highlight sits over the rows it covered.
    assert!(!app.selection_cells(SurfaceId::Swapped, area).is_empty());
}

/// A drag over the lines below a frame's rows copies them too: what a
/// write said reads as its own rows (`docs/tui.md`, "Selection and
/// copy").
#[test]
fn a_drag_over_below_lines_copies_them() {
    use crate::app::Effect;
    let mut app = app();
    let frame = made_frame(&["alpha"], &["note one"], None);
    let effect = drag_over(&mut app, &frame, 80, 24, (0, 22), (7, 22));
    assert_eq!(effect, Effect::Copy("note one".to_owned()));
}

/// A frame too short to show its lines below notes none: the rows stop
/// at the footer (`docs/tui.md`, "Selection and copy").
#[test]
fn a_cramped_frame_skips_below_at_the_footer() {
    use crate::app::Effect;
    let mut app = app();
    let frame = made_frame(&["alpha", "beta"], &["note one"], None);
    let effect = drag_over(&mut app, &frame, 80, 2, (0, 1), (7, 1));
    assert_eq!(effect, Effect::None);
}

/// A drag over a frame's field copies its text past the prompt: the
/// field reads as the input box reads past its own (`docs/tui.md`,
/// "Selection and copy").
#[test]
fn a_drag_over_a_frame_field_copies_its_text() {
    use crate::app::Effect;
    let mut app = app();
    let frame = made_frame(&["alpha"], &[], Some(("query text", 10)));
    let effect = drag_over(&mut app, &frame, 80, 24, (2, 22), (6, 22));
    assert_eq!(effect, Effect::Copy("query".to_owned()));
}

/// A frame too short to show its field notes none: the field stops at
/// the footer (`docs/tui.md`, "Selection and copy").
#[test]
fn a_cramped_frame_skips_the_field_at_the_footer() {
    use crate::app::Effect;
    let mut app = app();
    let frame = made_frame(&["alpha"], &[], Some(("query text", 10)));
    let effect = drag_over(&mut app, &frame, 80, 2, (2, 1), (6, 1));
    assert_eq!(effect, Effect::None);
}
