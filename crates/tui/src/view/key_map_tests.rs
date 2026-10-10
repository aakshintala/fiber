//! The key map overlay's drawing: snapshots, cell colours and input.

use super::draw;
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::theme::Role;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::path::PathBuf;

/// An attached app at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// Opens the key map.
fn open(app: &mut App) {
    app.on_key(Key::F1, fakes::clock::FakeClock::new().now());
    assert_eq!(app.keymap_top(), Some(0));
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Renders `app` on a `width` by `height` screen, returning its buffer.
fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
}

/// The rows of `buf` as text.
fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn key_map_160x48() {
    let mut app = attached(160, 48);
    open(&mut app);
    insta::assert_snapshot!("key_map_160x48", screen(&app, 160, 48));
}

#[test]
fn key_map_80x24() {
    let mut app = attached(80, 24);
    open(&mut app);
    insta::assert_snapshot!("key_map_80x24", screen(&app, 80, 24));
}

#[test]
fn key_map_narrow_scrolled() {
    let mut app = attached(100, 40);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..80 {
        app.on_key(Key::PageDown, now);
    }
    assert!(app.keymap_top().unwrap_or(0) > 0);
    insta::assert_snapshot!("key_map_100x40_scrolled", screen(&app, 100, 40));
}

#[test]
fn the_title_is_bold_accent_and_the_tabs_mark_the_choice() {
    let mut app = attached(80, 24);
    open(&mut app);
    let buf = buffer(&app, 80, 24);
    let shown = rows(&buf);
    let title = shown
        .iter()
        .position(|row| row.contains("Key map"))
        .expect("the title row");
    let title_y = u16::try_from(title).unwrap_or(u16::MAX);
    let cell = &buf[(2, title_y)];
    assert_eq!(cell.symbol(), "K");
    assert_eq!(cell.fg, Role::Accent.color());
    assert!(cell.modifier.contains(Modifier::BOLD));
    // The title's ✕ ends the overlay's right end, dim.
    let cross = &buf[(80 - 3, title_y)];
    assert_eq!(cross.symbol(), "✕");
    assert!(cross.modifier.contains(Modifier::DIM));
    // The tabs row: the chosen tab dark on white, the rest dim.
    let tabs = shown
        .iter()
        .position(|row| row.contains("All") && row.contains("Sessions"))
        .expect("the tabs row");
    let tabs_y = u16::try_from(tabs).unwrap_or(u16::MAX);
    let chosen = (0..80)
        .find(|x| buf[(*x, tabs_y)].symbol() == "A")
        .expect("the All tab");
    assert_eq!(buf[(chosen, tabs_y)].fg, Color::Rgb(0, 0, 0));
    assert_eq!(buf[(chosen, tabs_y)].bg, Color::Rgb(255, 255, 255));
    assert!(buf[(chosen, tabs_y)].modifier.contains(Modifier::BOLD));
    let other = (0..80)
        .find(|x| buf[(*x, tabs_y)].symbol() == "S")
        .expect("another tab");
    assert!(buf[(other, tabs_y)].modifier.contains(Modifier::DIM));
    // The first binding reads bold on the full-width bar.
    let first = shown
        .iter()
        .position(|row| row.contains("Send a prompt"))
        .expect("the focused row");
    let first_y = u16::try_from(first).unwrap_or(u16::MAX);
    for x in 0..80 {
        assert_eq!(buf[(x, first_y)].bg, Role::Accent.color(), "col {x}");
    }
    assert_eq!(buf[(2, first_y)].symbol(), "›");
    assert_eq!(buf[(2, first_y)].fg, Color::Rgb(0, 0, 0));
}

#[test]
fn typing_narrows_the_rows_to_the_query() {
    let mut app = attached(80, 24);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for ch in "session".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let map = app.keymap().expect("open");
    assert_eq!(map.query(), "session");
    let ids: Vec<&str> = map
        .visible(app.keys())
        .iter()
        .map(|binding| binding.id)
        .collect();
    assert_eq!(
        ids,
        [
            "new_session",
            "rail_row_n",
            "delete_session",
            "recall_prompt",
            "next_request",
            "session_only"
        ]
    );
    // The draft keeps nothing typed into the map.
    assert_eq!(app.draft(), "");
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("session"), "{shown}");
    assert!(!shown.contains("Paste an image"), "{shown}");
}

#[test]
fn left_and_right_move_between_tabs() {
    let mut app = attached(80, 24);
    open(&mut app);
    app.on_edit(crate::keys::Edit::Right);
    assert_eq!(app.keymap().expect("open").tab(), 1);
    // The Sessions tab shows its rows: one reads on screen, while
    // another area's rows hide.
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("Close what is on top"), "{shown}");
    assert!(!shown.contains("Search those prompts"), "{shown}");
    app.on_edit(crate::keys::Edit::Left);
    assert_eq!(app.keymap().expect("open").tab(), 0);
    // At the ends the tab holds.
    app.on_edit(crate::keys::Edit::Left);
    assert_eq!(app.keymap().expect("open").tab(), 0);
    for _ in 0..10 {
        app.on_edit(crate::keys::Edit::Right);
    }
    assert_eq!(app.keymap().expect("open").tab(), 5);
}

#[test]
fn up_and_down_move_the_focus_and_esc_closes() {
    let mut app = attached(80, 24);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Down, now);
    assert_eq!(app.keymap().expect("open").focus(), 1);
    app.on_key(Key::Up, now);
    assert_eq!(app.keymap().expect("open").focus(), 0);
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.keymap_top(), None);
}

#[test]
fn exactly_one_row_is_barred() {
    let mut app = attached(80, 24);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for ch in "zzz".chars() {
        app.on_key(Key::Char(ch), now);
    }
    // Nothing matching: no bar anywhere.
    let buf = buffer(&app, 80, 24);
    assert!(
        (0..24).all(|y| (0..80).all(|x| buf[(x, y)].bg != Role::Accent.color())),
        "no bar with no result"
    );
    assert!(screen(&app, 80, 24).contains("Key map"));
}

#[test]
fn a_binding_taller_than_the_body_shows_its_top_cut() {
    // At 40x12 one body row is left while every binding wraps past
    // it: the focused binding shows its top rows only, cut at the
    // body's last row, with no ↓ gutter marker.
    let mut app = attached(40, 12);
    open(&mut app);
    let buf = buffer(&app, 40, 12);
    let shown = rows(&buf);
    let barred: Vec<&String> = shown.iter().filter(|row| row.starts_with("  › ")).collect();
    assert_eq!(barred.len(), 1, "{shown:?}");
    assert!(barred[0].contains("Send a"), "{shown:?}");
    assert!(
        shown.iter().all(|row| !row.starts_with("  ↓ ")),
        "no gutter marker on a cut binding: {shown:?}"
    );
}

#[test]
fn shrinking_keeps_the_focused_binding_whole() {
    let mut app = attached(100, 40);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..10 {
        app.on_key(Key::Down, now);
    }
    let focus = app.keymap().expect("open").focus();
    assert_eq!(focus, 10);
    // Narrower and shorter: the columns wrap each binding taller and
    // the body holds fewer, so the old scroll would leave the focus
    // below the window.
    app.set_size(80, 30);
    let buf = buffer(&app, 80, 30);
    let shown = rows(&buf);
    // The focused binding's height in the drawn layout.
    let keys = app.keys().clone();
    let map = app.keymap().expect("open").clone();
    let visible = map.visible(&keys);
    let inner = app.column_width().saturating_sub(4);
    let cols = crate::keymap::columns(&visible, &keys, inner);
    let grown = crate::keymap::heights(&visible, &keys, &cols);
    let body = crate::keymap::chrome(app.conversation_height()).body;
    let height = grown[focus];
    // Preconditions: the old scroll hides the focus, while the new
    // body fits it whole, so the test discriminates the reconcile.
    assert!(crate::keymap::fits_from(0, &grown, body) <= focus);
    assert!(height <= body.saturating_sub(1));
    // Every row of the focused binding draws barred and contiguous,
    // with › in its first row's gutter: it is whole, not cut or gone.
    let barred: Vec<u16> = (0..30)
        .filter(|y| (0..80).any(|x| buf[(x, *y)].bg == crate::theme::Role::Accent.color()))
        .collect();
    assert_eq!(barred.len(), height, "{shown:?}");
    assert!(
        barred.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{shown:?}"
    );
    assert!(
        shown[usize::from(barred[0])].starts_with("  › "),
        "{shown:?}"
    );
}

#[test]
fn draw_takes_no_room_and_keeps_the_screen() {
    // The draw entry point draws nothing without an open map, and keeps
    // the buffer it is given.
    let app = attached(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    draw(&app, area, &mut buf, &mut targets);
    assert!(targets.is_empty());
    assert_eq!(buf, Buffer::empty(area));
}

#[test]
fn action_rows_split_the_description_from_its_condition() {
    use super::action_rows;
    // One row: the condition keeps the space that follows the description.
    assert_eq!(
        action_rows("Open", "idle", 80),
        [("Open".to_owned(), " (idle)".to_owned())]
    );
    // Wrapped: each row starts where the last one ended, past the space
    // the wrap dropped, and the condition lands in the tail once the
    // description is used up.
    assert_eq!(
        action_rows("aaa bbb ccc", "ddd", 8),
        [
            ("aaa bbb".to_owned(), String::new()),
            ("ccc".to_owned(), String::new()),
            (String::new(), "(ddd)".to_owned()),
        ]
    );
}

#[test]
fn every_row_of_a_binding_keeps_its_columns_full_width() {
    use super::binding_rows;
    let binding = crate::bindings::BINDINGS
        .iter()
        .find(|row| row.id == "send")
        .expect("the row");
    let cols = crate::keymap::Columns {
        area: 6,
        action: 12,
        keys: 8,
    };
    let rows = binding_rows(binding, "Enter", &cols, false, false);
    // The action wraps past the keys: the keys column pads the rows its
    // text does not reach, so every row is as wide as the three columns.
    assert!(rows.len() > 1, "the action should wrap");
    for row in &rows {
        let wide: usize = row
            .spans
            .iter()
            .map(|span| crate::format::width(&span.content))
            .sum();
        assert_eq!(wide, 2 + 6 + 2 + 12 + 2 + 8);
    }
}

#[test]
fn a_map_showing_every_binding_marks_no_more_and_no_gutter() {
    let mut app = attached(200, 120);
    open(&mut app);
    let shown = rows(&buffer(&app, 200, 120));
    assert!(shown.iter().all(|row| !row.contains("more")), "{shown:?}");
    assert!(
        shown.iter().all(|row| !row.starts_with("  ↓ ")),
        "{shown:?}"
    );
}

#[test]
fn a_scrolled_map_counts_the_bindings_below() {
    let mut app = attached(80, 24);
    open(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..12 {
        app.on_key(Key::Down, now);
    }
    assert!(app.keymap_top().unwrap_or(0) > 0);
    let shown = rows(&buffer(&app, 80, 24));
    assert!(
        shown
            .iter()
            .any(|row| row.contains("↑") && row.contains("↓ ") && row.contains("more")),
        "{shown:?}"
    );
}
