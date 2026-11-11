//! Tests for the views' frame: the selection's keys and scrolling, and
//! what the frame draws (`docs/tui.md`, "Swapped views").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::{
    Chrome, Frame, Ink, Inline, List, Spot, about, compact, render, rows_height,
    wheel_targets_window, wheel_window,
};
use crate::keys::Key;
use crate::mouse::TargetId;
use crate::rows::Join;
use crate::theme::Role;

/// A list after pressing `keys` over `rows` rows shown `height` at a time.
fn pressed(keys: &[Key], rows: usize, height: usize) -> List {
    let mut list = List::default();
    for key in keys {
        list.key(key, rows, height);
    }
    list
}

/// Wheel steps shift three displayed rows; focus lands on the nearest
/// shown target (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_targets_window_moves_three_rows_and_clamps_to_targets() {
    assert_eq!(
        wheel_targets_window(false, 2, 0, 20, 6, &[0, 2, 5, 7, 9]),
        (5, 3)
    );
    // A stale stored top steps from what is visible: keyboard focus on
    // row 15 renders at top 10, so one step down reaches top 13 with the
    // focus still on screen (`docs/tui.md`, "Swapped views").
    assert_eq!(
        wheel_targets_window(false, 15, 3, 20, 6, &[0, 2, 5, 7, 9]),
        (15, 13)
    );
    assert_eq!(wheel_targets_window(false, 1, 0, 20, 6, &[]), (1, 3));
    assert_eq!(wheel_targets_window(false, 1, 0, 20, 0, &[1, 2]), (1, 0));
}

#[test]
fn selection_wraps_past_either_end() {
    // ↑ on the first row goes to the last, and the key says so.
    let mut list = List::default();
    assert!(list.key(&Key::Up, 5, 10));
    assert_eq!(list.selected(), 4);
    // ↓ on the last row goes to the first, and the key says so.
    assert!(list.key(&Key::Down, 5, 10));
    assert_eq!(list.selected(), 0);
    // Just inside either end still steps to the neighbour.
    let list = pressed(&vec![Key::Down; 4], 5, 10);
    assert_eq!(list.selected(), 4);
    let list = pressed(&[Key::Down, Key::Up], 5, 10);
    assert_eq!(list.selected(), 0);
    // A full turn down five rows lands back on the first.
    let list = pressed(&vec![Key::Down; 5], 5, 10);
    assert_eq!(list.selected(), 0);
    // A two-position list wraps like a pair of tabs.
    let list = pressed(&[Key::Up], 2, 10);
    assert_eq!(list.selected(), 1);
    let list = pressed(&vec![Key::Down; 2], 2, 10);
    assert_eq!(list.selected(), 0);
    // Pages still clamp at both ends.
    let list = pressed(&[Key::PageDown, Key::PageDown], 5, 3);
    assert_eq!(list.selected(), 4);
    let list = pressed(&[Key::Down, Key::PageUp], 5, 3);
    assert_eq!(list.selected(), 0);
    // A zero-height window moves nothing.
    let mut list = List::default();
    assert!(!list.key(&Key::Up, 5, 0));
    assert!(!list.key(&Key::Down, 5, 0));
    assert_eq!((list.selected(), list.top()), (0, 0));
}

#[test]
fn page_moves_by_the_height_less_one() {
    let mut list = List::default();
    assert!(list.key(&Key::PageDown, 100, 10));
    assert_eq!(list.selected(), 9);
    assert!(list.key(&Key::PageUp, 100, 10));
    assert_eq!(list.selected(), 0);
    // At the top a page still stops, and the key says so.
    assert!(!list.key(&Key::PageUp, 100, 10));
    // At the top ↑ wraps to the last row, and the key says so.
    assert!(list.key(&Key::Up, 100, 10));
    assert_eq!(list.selected(), 99);
    // A one-row view still moves by one.
    let mut list = List::default();
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
        for _ in 0..19 {
            list.key(&Key::Down, 20, height);
            assert!(list.top() <= list.selected(), "height {height}");
            assert!(list.selected() < list.top() + height, "height {height}");
        }
        assert_eq!(list.selected(), 19, "height {height}");
        assert_eq!(list.top(), 20 - height, "height {height}");
        // One more ↓ wraps to the first row, still in view.
        assert!(list.key(&Key::Down, 20, height), "height {height}");
        assert_eq!((list.selected(), list.top()), (0, 0), "height {height}");
        for _ in 0..20 {
            list.key(&Key::Up, 20, height);
            assert!(list.top() <= list.selected(), "height {height}");
            assert!(list.selected() < list.top() + height, "height {height}");
        }
        assert_eq!((list.selected(), list.top()), (0, 0), "height {height}");
    }
}

/// ↑ and ↓ wrap past either end of the entries that take a key, in
/// both directions; pages never reach this path.
#[test]
fn key_over_wraps_past_either_end_of_takes() {
    let takes = vec![0, 3, 6];
    let mut list = List::default();
    list.select(0, 9, 4);
    // ↑ on the first entry goes to the last, and the key says so.
    assert!(list.key_over(&Key::Up, 9, 4, &takes));
    assert_eq!(list.selected(), 6);
    // ↓ on the last entry goes to the first, and the key says so.
    assert!(list.key_over(&Key::Down, 9, 4, &takes));
    assert_eq!(list.selected(), 0);
    // Between entries it still steps to the neighbour.
    assert!(list.key_over(&Key::Down, 9, 4, &takes));
    assert_eq!(list.selected(), 3);
    assert!(list.key_over(&Key::Up, 9, 4, &takes));
    assert_eq!(list.selected(), 0);
    // A two-entry list wraps like a pair of tabs.
    let pair = vec![0, 1];
    let mut list = List::default();
    assert!(list.key_over(&Key::Up, 2, 10, &pair));
    assert_eq!(list.selected(), 1);
    assert!(list.key_over(&Key::Down, 2, 10, &pair));
    assert_eq!(list.selected(), 0);
    // With no entries it moves as `key` does, which wraps too.
    let mut list = List::default();
    assert!(list.key_over(&Key::Up, 5, 10, &[]));
    assert_eq!(list.selected(), 4);
    // Any other key leaves it.
    let mut list = List::default();
    list.select(3, 9, 4);
    assert!(!list.key_over(&Key::Enter, 9, 4, &takes));
    assert_eq!(list.selected(), 3);
}

/// The line window sets the selection and the top independently: a
/// selected line may sit above the first shown one, which `select`
/// never produces since it keeps the top at or below the selection
/// (`docs/tui.md`, "Swapped views").
#[test]
fn line_window_sets_selection_and_top_independently() {
    let mut list = List::gutter();
    list.set_line_window(2, 3);
    assert_eq!((list.selected(), list.top()), (2, 3));
    assert!(list.is_gutter());
    // `select` on the same selected line keeps the top below it.
    let mut selected = List::gutter();
    selected.select(2, 10, 4);
    assert_eq!(selected.selected(), 2);
    assert!(selected.top() <= selected.selected());
    assert!(selected.is_gutter());
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
        joins: Vec::new(),
        title: "Settings".to_owned(),
        command: "/settings".to_owned(),
        rows: (0..rows)
            .map(|at| vec![(format!("row {at}"), None, Ink::Plain)])
            .collect(),
        list: List::default(),
        gutter_mark: Style::default(),
        below: vec!["below".to_owned()],
        field: None,
        footer: "keys".to_owned(),
        chrome: None,
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
        joins: Vec::new(),
        title: title.to_owned(),
        command: command.to_owned(),
        rows: Vec::new(),
        list: List::default(),
        gutter_mark: Style::default(),
        below: Vec::new(),
        field: None,
        footer: String::new(),
        chrome: None,
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
        joins: Vec::new(),
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
        gutter_mark: Style::default(),
        below: Vec::new(),
        field: None,
        footer: "keys".to_owned(),
        chrome: None,
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
        joins: Vec::new(),
        title: "Context".to_owned(),
        command: "/context".to_owned(),
        rows: vec![
            vec![("■ ".to_owned(), None, Ink::Role(Role::Error))],
            vec![("tail".to_owned(), None, Ink::Muted)],
        ],
        list: List::default(),
        gutter_mark: Style::default(),
        below: Vec::new(),
        field: None,
        footer: String::new(),
        chrome: None,
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

/// The gutter mark draws with the frame's `gutter_mark` style; a
/// frame that leaves the default draws it plain.
#[test]
fn the_gutter_mark_draws_with_the_frames_style() {
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    let mut shown = frame(5);
    shown.list = List::gutter();
    shown.list.select(1, 5, rows_height(&shown, 6));
    shown.gutter_mark = Style::new()
        .fg(Role::Accent.color())
        .add_modifier(Modifier::BOLD);
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 2), "› row 1");
    assert_eq!(buf[(0, 2)].fg, Role::Accent.color());
    assert!(buf[(0, 2)].modifier.contains(Modifier::BOLD));
    // The default frame keeps the mark plain.
    let mut buf = Buffer::empty(area);
    let mut shown = frame(5);
    shown.list = List::gutter();
    shown.list.select(1, 5, rows_height(&shown, 6));
    render(&shown, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 2), "› row 1");
    assert_eq!(buf[(0, 2)].fg, Color::Reset);
    assert!(!buf[(0, 2)].modifier.contains(Modifier::BOLD));
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
        [2, 5, 1, 5, 2, 1]
    );
    let mut list = List::default();
    list.select(1, 8, 8);
    assert!(list.key_over(&Key::Down, 8, 8, &takes));
    assert!(!list.key_over(&Key::Enter, 8, 8, &takes));
    // Past the last take ↓ wraps to the first.
    list.select(5, 8, 8);
    assert!(list.key_over(&Key::Down, 8, 8, &takes));
    assert_eq!(list.selected(), 1);
    // Past the first take ↑ wraps to the last.
    list.select(1, 8, 8);
    assert!(list.key_over(&Key::Up, 8, 8, &takes));
    assert_eq!(list.selected(), 5);
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
        joins: Vec::new(),
        title: "Keys".to_owned(),
        command: "/keys".to_owned(),
        rows: rows
            .iter()
            .map(|row| vec![((*row).to_owned(), None, Ink::Plain)])
            .collect(),
        list: List::gutter(),
        gutter_mark: Style::default(),
        below: below.iter().map(|line| (*line).to_owned()).collect(),
        field: field.map(|(text, caret)| (text.to_owned(), caret)),
        footer: "move".to_owned(),
        chrome: None,
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

/// One wheel step moves the window three rows, clamped at either end
/// (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_moves_the_window_three_rows() {
    let (selected, top) = wheel_window(false, 0, 0, 20, 6, 3);
    assert_eq!((selected, top), (3, 3));
    let (selected, top) = wheel_window(false, 3, 3, 20, 6, 3);
    assert_eq!((selected, top), (6, 6));
    let (selected, top) = wheel_window(true, 6, 6, 20, 6, 3);
    assert_eq!((selected, top), (6, 3));
    // Clamped at either end: the last window starts at rows less height.
    let (selected, top) = wheel_window(false, 14, 14, 20, 6, 3);
    assert_eq!((selected, top), (14, 14));
    let (selected, top) = wheel_window(false, 10, 10, 20, 6, 3);
    assert_eq!((selected, top), (13, 13));
    let (selected, top) = wheel_window(true, 3, 2, 20, 6, 3);
    assert_eq!((selected, top), (3, 0));
}

/// A selection still on screen stays where it was; one that leaves the
/// window moves to the nearest shown row (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_keeps_a_visible_selection_and_clamps_a_leaving_one() {
    // In the middle, at the first row, and at the last row: all stay.
    assert_eq!(wheel_window(false, 7, 3, 20, 6, 3), (7, 6));
    assert_eq!(wheel_window(false, 6, 3, 20, 6, 3), (6, 6));
    assert_eq!(wheel_window(false, 10, 3, 20, 6, 3), (10, 6));
    assert_eq!(wheel_window(false, 11, 3, 20, 6, 3), (11, 6));
    // Just above the window clamps down to the new top; just below it
    // clamps up to the new bottom row.
    assert_eq!(wheel_window(false, 5, 3, 20, 6, 3), (6, 6));
    assert_eq!(wheel_window(false, 12, 3, 20, 6, 3), (11, 6));
    assert_eq!(wheel_window(true, 2, 6, 20, 6, 3), (3, 3));
    assert_eq!(wheel_window(true, 8, 6, 20, 6, 3), (8, 3));
}

/// An empty or short list, or a zero-height window, does not move, and a
/// blurred selection stays blurred (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_leaves_empty_short_and_zero_height_lists() {
    assert_eq!(wheel_window(false, 0, 0, 0, 6, 3), (0, 0));
    assert_eq!(wheel_window(false, 0, 4, 0, 6, 3), (0, 4));
    assert_eq!(wheel_window(true, 2, 4, 5, 6, 3), (2, 0));
    assert_eq!(wheel_window(false, 2, 0, 5, 6, 3), (2, 0));
    assert_eq!(wheel_window(false, 2, 0, 5, 0, 3), (2, 0));
    // A blurred list keeps no selection, whatever the window does.
    assert_eq!(
        wheel_window(false, usize::MAX, 0, 20, 6, 3),
        (usize::MAX, 3)
    );
    assert_eq!(wheel_window(true, usize::MAX, 6, 20, 6, 3), (usize::MAX, 3));
}

/// `List::wheel` moves the window the shared helper computes: a stale
/// top clamps into range before stepping (`docs/tui.md`, "Swapped views").
#[test]
fn list_wheel_shifts_the_window_and_clamps_the_selection() {
    let mut list = List::default();
    list.select(10, 20, 6);
    list.wheel(false, 20, 6, 3);
    assert_eq!(list.top(), 8);
    assert_eq!(list.selected(), 10);
    list.wheel(false, 20, 6, 3);
    assert_eq!(list.top(), 11);
    assert_eq!(list.selected(), 11);
    list.wheel(true, 20, 6, 3);
    assert_eq!(list.top(), 8);
    assert_eq!(list.selected(), 11);
    // A stale top past the end clamps before the step runs.
    let mut stale = List::default();
    stale.select(19, 20, 6);
    stale.wheel(false, 20, 6, 3);
    assert_eq!(stale.top(), 14);
    assert_eq!(stale.selected(), 19);
}

/// A past-end selection stays past the last row while the window still
/// steps three rows (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_window_keeps_a_past_end_selection() {
    assert_eq!(wheel_window(false, 20, 0, 20, 6, 3), (20, 3));
    assert_eq!(wheel_window(true, 20, 6, 20, 6, 3), (20, 3));
}

/// The rebinding screen notes how each display line joins the one
/// before: the first line of a wrapped action breaks, its
/// continuations wrap, and every other row breaks
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn rebind_continuations_note_wrap_joins_while_first_lines_break() {
    use crate::keyset::Keyset;
    let keys = Keyset::default();
    let screen = crate::rebind::KeysScreen::default();
    let frame = screen.frame(&keys, 80, 24);
    let grown = crate::rebind::heights(&keys, 80);
    // The delete_session action wraps its keys after the comma and its
    // description across three display lines.
    let at: usize = (0..7).map(|action| grown[action]).sum();
    assert_eq!(
        &frame.joins[at..at + 3],
        &[Join::Break, Join::WrapSpace, Join::WrapSpace]
    );
    // A single-line action notes one break.
    let single: usize = (0..3).map(|action| grown[action]).sum();
    assert_eq!(&frame.joins[single..single + 1], &[Join::Break]);
}

/// A drag over a wrapped rebinding action copies it whole: the
/// continuation joins onto the first line with no newline
/// (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_a_wrapped_rebind_action_copies_it_whole() {
    use crate::app::Effect;
    use crate::keyset::Keyset;
    let keys = Keyset::default();
    let screen = crate::rebind::KeysScreen::default();
    let frame = screen.frame(&keys, 80, 24);
    let grown = crate::rebind::heights(&keys, 80);
    let start: usize = (0..7).map(|action| grown[action]).sum();
    let top = u16::try_from(1 + start).expect("a row");
    let mut app = app();
    let effect = drag_over(
        &mut app,
        &frame,
        80,
        24,
        (2, top),
        (79, top.saturating_add(1)),
    );
    // The two display lines join with the dropped space and no newline.
    let first: String = frame.rows[start]
        .iter()
        .map(|(text, _, _)| text.as_str())
        .collect();
    let second: String = frame.rows[start + 1]
        .iter()
        .map(|(text, _, _)| text.as_str())
        .collect();
    assert_eq!(
        effect,
        Effect::Copy(format!("{} {}", first.trim_end(), second.trim_end()))
    );
}

/// A wrapped focused action reads barred on every display line, with
/// the gutter mark on the first only; the next action reads plain
/// (`docs/tui.md`, "Bindings").
#[test]
fn the_focused_wrapped_action_reads_barred_on_every_line() {
    use crate::keyset::Keyset;
    let keys = Keyset::default();
    let screen = crate::rebind::KeysScreen::default();
    let frame = screen.frame(&keys, 80, 24);
    let grown = crate::rebind::heights(&keys, 80);
    assert!(grown[0] > 1, "send wraps at 80 wide");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&frame, area, &mut buf, &mut Vec::new());
    for line in 0..grown[0] {
        let y = u16::try_from(1 + line).expect("a row");
        for x in 0..80 {
            assert!(
                buf[(x, y)].modifier.contains(Modifier::REVERSED),
                "bar at {x},{y}"
            );
        }
    }
    assert!(text(&buf, 1).starts_with("› "));
    assert!(text(&buf, 2).starts_with("  "));
    let rest = u16::try_from(1 + grown[0]).expect("a row");
    for x in 0..80 {
        assert!(
            !buf[(x, rest)].modifier.contains(Modifier::REVERSED),
            "no bar at {x},{rest}"
        );
    }
}

/// The lines below the rows and the field note breaks of their own:
/// after a wrapped row they start new lines (`docs/tui.md`,
/// "Selection and copy").
#[test]
fn below_and_field_note_breaks_after_a_wrap() {
    use crate::app::Effect;
    let mut frame = made_frame(&["a", "b"], &["note one"], None);
    frame.joins = vec![Join::Break, Join::Wrap];
    let mut noted = app();
    let effect = drag_over(&mut noted, &frame, 80, 24, (2, 1), (79, 22));
    assert_eq!(effect, Effect::Copy("ab\nnote one".to_owned()));
    let mut fielded = made_frame(&["a", "b"], &[], Some(("query text", 10)));
    fielded.joins = vec![Join::Break, Join::Wrap];
    let mut other = app();
    let effect = drag_over(&mut other, &fielded, 80, 24, (2, 22), (6, 22));
    assert_eq!(effect, Effect::Copy("query".to_owned()));
}

/// Selecting with an unknown height never adds: a scrolled list keeps
/// its top, and the window's edge scrolls exactly onto the selection.
#[test]
fn select_with_an_unknown_height_never_adds() {
    // A scrolled list: down a short list with a small height first.
    let mut list = List::default();
    list.select(30, 40, 6);
    assert_eq!((list.selected(), list.top()), (30, 25));
    // The rewind view's unknown height: the selection moves and the top
    // stays, instead of overflowing `top + height`.
    list.select(30, 40, usize::MAX);
    assert_eq!((list.selected(), list.top()), (30, 25));
    // Just below the edge the top stays.
    let mut list = List::default();
    list.select(5, 10, 3);
    assert_eq!((list.selected(), list.top()), (5, 3));
    list.select(5, 10, 3);
    assert_eq!((list.selected(), list.top()), (5, 3));
    // Exactly on the edge the window scrolls onto the selection.
    list.select(6, 10, 3);
    assert_eq!((list.selected(), list.top()), (6, 4));
    // Past the edge it scrolls the same way.
    list.select(8, 10, 3);
    assert_eq!((list.selected(), list.top()), (8, 6));
}

/// A selection exactly on the new top stays, and a target exactly on the
/// exclusive bottom stays outside: the window still moves three rows
/// (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_targets_window_holds_exact_edges() {
    // Selected target 0 sits exactly on the new top 0 after a wheel-up
    // that cannot move: it stays.
    assert_eq!(
        wheel_targets_window(true, 0, 0, 20, 6, &[0, 2, 5, 7, 9]),
        (0, 0)
    );
    // Selected row 3 sits exactly on the new top 3 after one step down
    // from a stale top: it stays, while a `<=` outside check would jump
    // it to the next target 5.
    assert_eq!(
        wheel_targets_window(false, 3, 0, 20, 6, &[0, 2, 5, 7, 9]),
        (3, 3)
    );
    // Target 7 sits exactly on the exclusive bottom of the new window
    // [1, 7): it stays outside, so a focus above clamps to 5, not 7.
    assert_eq!(
        wheel_targets_window(true, 9, 0, 20, 6, &[0, 2, 5, 7, 9]),
        (5, 1)
    );
}

/// A re-flow that leaves the selection or the first row shown past the
/// last row brings them onto it; a list in range stays put.
#[test]
fn clamp_moves_a_list_only_when_it_is_past_the_last_row() {
    // Selection 9, top 5: the last row is 9 at ten rows.
    let list = pressed(&vec![Key::Down; 9], 20, 5);
    assert_eq!((list.selected(), list.top()), (9, 5));
    let mut at_edge = list;
    at_edge.clamp(10);
    assert_eq!(at_edge, list, "the last row is in range");
    let mut past = list;
    past.clamp(9);
    assert_eq!((past.selected(), past.top()), (8, 5));
    let mut both = list;
    both.clamp(5);
    assert_eq!((both.selected(), both.top()), (4, 4));
    let mut none = list;
    none.clamp(0);
    assert_eq!((none.selected(), none.top()), (0, 0));
}

// What `Chrome` adds: fixed rows under the title, a field drawn in a row
// and a footer with styled keys.

/// `frame(rows)` with `top` fixed rows, each the text `top N` in bold.
fn chromed(rows: usize, top: usize) -> Frame {
    let mut shown = frame(rows);
    shown.list = List::gutter();
    shown.chrome = Some(Chrome {
        top: (0..top)
            .map(|at| {
                vec![(
                    format!("top {at}"),
                    Style::new().add_modifier(Modifier::BOLD),
                )]
            })
            .collect(),
        draft: None,
        footer: Vec::new(),
    });
    shown
}

#[test]
fn fixed_rows_take_height_from_the_body_one_for_one() {
    // 6 rows high: the title, the footer and one line below leave 3.
    for (top, body) in [(0, 3), (1, 2), (2, 1), (3, 0), (9, 0)] {
        assert_eq!(rows_height(&chromed(8, top), 6), body, "{top} fixed rows");
    }
}

#[test]
fn fixed_rows_draw_under_the_title_before_the_body_in_their_style() {
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&chromed(5, 2), area, &mut buf, &mut targets);
    assert_eq!(text(&buf, 1), "top 0");
    assert_eq!(text(&buf, 2), "top 1");
    assert_eq!(text(&buf, 3), "› row 0");
    assert!(buf[(0, 1)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(2, 3)].modifier.contains(Modifier::BOLD));
    // A body row's target sits on the row it is drawn on.
    let row_0 = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Row(0)))
        .map(|target| target.rect.y);
    assert_eq!(row_0, Some(3));
}

#[test]
fn fixed_rows_never_reach_the_footer_row() {
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    render(&chromed(5, 9), area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 1), "top 0");
    // The line below is drawn over a fixed row that has no room; the
    // footer is whole.
    assert_eq!(text(&buf, 2), "below");
    assert_eq!(text(&buf, 3), "keys");
}

/// `chromed(rows, 0)` with a field in `row` at `column`.
fn drafting(rows: usize, row: usize, column: usize, text: &str, caret: usize) -> Frame {
    let mut shown = chromed(rows, 0);
    if let Some(chrome) = &mut shown.chrome {
        chrome.draft = Some(Inline {
            row,
            column,
            text: text.to_owned(),
            caret,
        });
    }
    shown
}

#[test]
fn an_inline_field_draws_in_its_row_at_its_column_with_the_caret_reversed() {
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    // The gutter is two columns; the field starts six further in, just
    // past the row's own text.
    render(
        &drafting(5, 1, 6, "abc", 3),
        area,
        &mut buf,
        &mut Vec::new(),
    );
    assert_eq!(text(&buf, 2), "  row 1 abc");
    assert!(buf[(11, 2)].modifier.contains(Modifier::REVERSED));
    assert!(!buf[(10, 2)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn an_inline_field_in_a_row_scrolled_away_or_past_the_shown_rows_is_not_drawn() {
    let area = Rect::new(0, 0, 40, 6);
    // Three body rows fit. The field's row 2 is the last shown, row 3 the
    // first hidden.
    for (row, drawn) in [(2, true), (3, false)] {
        let mut buf = Buffer::empty(area);
        render(
            &drafting(8, row, 6, "zz", 0),
            area,
            &mut buf,
            &mut Vec::new(),
        );
        let any = (0..6).any(|y| text(&buf, y).contains("zz"));
        assert_eq!(any, drawn, "row {row}");
    }
    // Scrolled so the list starts at row 3, a field in row 2 is above it
    // and one in row 3 is the first shown.
    for (row, drawn) in [(2, false), (3, true)] {
        let mut scrolled = drafting(8, row, 6, "zz", 0);
        scrolled.list.select(5, 8, 3);
        assert_eq!(scrolled.list.top(), 3);
        let mut buf = Buffer::empty(area);
        render(&scrolled, area, &mut buf, &mut Vec::new());
        let any = (0..6).any(|y| text(&buf, y).contains("zz"));
        assert_eq!(any, drawn, "scrolled, row {row}");
    }
}

#[test]
fn a_long_inline_field_scrolls_to_keep_the_caret_in_view() {
    let area = Rect::new(0, 0, 12, 6);
    let mut buf = Buffer::empty(area);
    // Column 2 + 6 leaves 4 cells: the caret at the end of 20 characters.
    render(
        &drafting(2, 0, 6, "abcdefghijklmnopqrst", 20),
        area,
        &mut buf,
        &mut Vec::new(),
    );
    assert_eq!(text(&buf, 1), "› row 0 rst");
}

#[test]
fn a_styled_footer_replaces_the_plain_one_and_an_empty_one_leaves_it() {
    let area = Rect::new(0, 0, 40, 6);
    let key = Style::new().fg(Color::Red);
    let mut styled = chromed(2, 0);
    if let Some(chrome) = &mut styled.chrome {
        chrome.footer = vec![
            ("Enter".to_owned(), key),
            (" save".to_owned(), Style::new()),
        ];
    }
    let mut buf = Buffer::empty(area);
    render(&styled, area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 5), "Enter save");
    assert_eq!(buf[(0, 5)].fg, Color::Red);
    assert_ne!(buf[(6, 5)].fg, Color::Red);
    let mut buf = Buffer::empty(area);
    render(&chromed(2, 0), area, &mut buf, &mut Vec::new());
    assert_eq!(text(&buf, 5), "keys");
}

/// What a drag from the top-left of the body to the bottom-right of an
/// `area`-sized frame copies.
fn copied_from(frame: &Frame, width: u16, height: u16) -> String {
    let mut app = app();
    let effect = drag_over(
        &mut app,
        frame,
        width,
        height,
        (0, 1),
        (width - 1, height - 2),
    );
    assert!(
        matches!(effect, crate::app::Effect::Copy(_)),
        "no copy: {effect:?}"
    );
    if let crate::app::Effect::Copy(text) = effect {
        text
    } else {
        String::new()
    }
}

#[test]
fn a_drag_copies_an_inline_field_only_from_a_shown_row() {
    // Six rows high: three body rows fit. The field's row 2 is the last
    // shown, row 3 the first hidden.
    let shown = copied_from(&drafting(8, 2, 6, "zz", 0), 40, 6);
    assert!(shown.contains("zz"), "{shown:?}");
    let hidden = copied_from(&drafting(8, 3, 6, "zz", 0), 40, 6);
    assert!(!hidden.contains("zz"), "{hidden:?}");
}

#[test]
fn a_drag_copies_the_inline_field_as_drawn_scrolled_to_its_caret() {
    // Twelve columns: the field starts at column 8, leaving four cells,
    // and the caret at the end of twenty characters shows the last three.
    let copied = copied_from(&drafting(2, 0, 6, "abcdefghijklmnopqrst", 20), 12, 6);
    assert!(copied.contains("rst"), "{copied:?}");
    assert!(!copied.contains("qrst"), "{copied:?}");
    assert!(!copied.contains("abc"), "{copied:?}");
}
