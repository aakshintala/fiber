//! Tests for the notice rows: wide glyphs at the cut keep the ✕ in
//! the last column (`docs/tui.md`, "Notices").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::notice_row;
use crate::app::App;
use crate::format;
use crate::mouse::TargetId;

/// An app with one notice under `code`.
fn app_with_notice(code: &str, message: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(30, 24);
    app.push_notice(code, message.to_owned());
    app
}

/// The row `draw` draws for the app's notice at width 30, and the
/// targets it leaves: the row is 30 cells with the ✕ in the last
/// column, and the dismiss target sits over that same cell.
fn drawn(code: &str, message: &str) -> (Buffer, Vec<crate::mouse::Target>) {
    let app = app_with_notice(code, message);
    let area = Rect::new(0, 0, 30, 24);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets);
    (buf, targets)
}

/// The row's text from `buf`, trailing spaces kept.
fn row_text(buf: &Buffer, y: u16) -> String {
    (0..30u16)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

#[test]
fn a_wide_cut_keeps_the_cross_in_the_last_column() {
    // Ten double-width glyphs: the message is cut, and the cut leaves
    // a one-cell gap the pad fills, so the visible ✕ lands where its
    // dismiss target sits.
    let (buf, targets) = drawn("io_failed", &"界".repeat(10));
    let row = row_text(&buf, 23);
    // The row ends in the cross; the cell assertion below is what
    // bites, since joined cell text counts a wide glyph's filler.
    assert!(row.trim_end().ends_with('✕'), "{row:?}");
    assert_eq!(buf[(29, 23)].symbol(), "✕");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::DismissNotice(0)
                && target.rect.x == 29
                && target.rect.width == 1),
        "{targets:?}"
    );
}

#[test]
fn a_wide_message_fills_or_cuts_cleanly_at_the_boundary() {
    // Six glyphs fit whole: no cut, no "…".
    let row = notice_row("io_failed", &"界".repeat(6), 30);
    let text = row_to_string(&row);
    assert_eq!(format::width(&text), 30, "{text:?}");
    assert!(!text.contains('…'), "{text:?}");
    assert!(text.ends_with('✕'), "{text:?}");
    // Seven glyphs cut: one "…" and the pad fills the gap the wide
    // glyph leaves, so the row still fills its width.
    let row = notice_row("io_failed", &"界".repeat(7), 30);
    let text = row_to_string(&row);
    assert_eq!(format::width(&text), 30, "{text:?}");
    assert_eq!(text.chars().filter(|c| *c == '…').count(), 1, "{text:?}");
    assert!(text.ends_with('✕'), "{text:?}");
}

/// A row's text, spans joined.
fn row_to_string(row: &ratatui::text::Line<'static>) -> String {
    row.spans.iter().map(|span| span.content.as_ref()).collect()
}

#[test]
fn a_wide_code_cut_pads_to_the_budget() {
    // The code alone overflows: it is cut with one "…", and the pad
    // fills the gap, in the short and the long case.
    for glyphs in [12, 20] {
        let code: String = "界".repeat(glyphs);
        let row = notice_row(&code, "", 30);
        let text = row_to_string(&row);
        assert_eq!(format::width(&text), 30, "{glyphs}: {text:?}");
        assert_eq!(text.chars().filter(|c| *c == '…').count(), 1, "{text:?}");
        assert!(text.ends_with('✕'), "{glyphs}: {text:?}");
    }
}

#[test]
fn a_long_code_cuts_cleanly() {
    let row = notice_row(&"x".repeat(40), "", 30);
    let text = row_to_string(&row);
    assert_eq!(format::width(&text), 30, "{text:?}");
    assert!(text.ends_with('✕'), "{text:?}");
}

#[test]
fn a_row_with_no_room_for_its_code_keeps_only_the_cross() {
    // The room is the width less the closing blank and the ✕. At one
    // and two columns none is left, so the code is dropped; at three
    // one column is left, and the code is cut to that "…" alone.
    let text = |width| row_to_string(&notice_row("code", "msg", width));
    assert_eq!(text(1), "✕");
    assert_eq!(text(2), " ✕");
    assert_eq!(text(3), "… ✕");
}
