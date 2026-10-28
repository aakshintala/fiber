//! Tests for the notice rows: wide glyphs at the cut keep the ✕ in
//! the last column (`docs/tui.md`, "Notices").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::notice_row;
use crate::app::App;
use crate::format;
use crate::mouse::TargetId;
use contract::clock::Clock;

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
    let bound = super::frame_bound(&app);
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);
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

// The notice overlay on the shared frame (`docs/tui.md`, "Overlays").

/// The prototype `notice` case's whole text, for the single form.
const SINGLE_TEXT: &str = "Two actions share the key Ctrl+F: `search` and `search_results` act in a context they share, so both entries reverted to their defaults. Rebind one of them with /keys. An entry equal to the defaults counts as unset.";

/// The four prototype `notices`, oldest first, so the overlay lists
/// them newest first.
fn four() -> [(&'static str, &'static str); 4] {
    [
        ("key_clash", SINGLE_TEXT),
        (
            "stream_closed",
            "this session's log can't be read past here; reopen the session to try again",
        ),
        (
            "config_invalid",
            "a configuration file is invalid: one extension key is set under both its full and short name",
        ),
        (
            "extension_failed",
            "an extension failed to start or missed its deadline, so loading skipped it",
        ),
    ]
}

/// An app at `width` by `height` with `notices` pushed oldest first and
/// the overlay open: the single form, or every notice for `all`.
fn overlay_app(width: u16, height: u16, notices: &[(&str, &str)], all: bool) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    for (code, text) in notices {
        app.push_notice(code, (*text).to_owned());
    }
    if all {
        app.open_more_notices();
    } else {
        app.open_notice(0);
    }
    app
}

/// Renders `app` at `width` by `height`: the text and the buffer.
fn overlay_screen(app: &App, width: u16, height: u16) -> (String, Buffer) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), buf)
}

/// Draws only the overlay on an empty buffer: the slab rows and targets.
fn overlay_only(app: &App, width: u16, height: u16) -> (Buffer, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let bound = super::frame_bound(app);
    super::overlay(app, area, &mut buf, &mut targets, &bound);
    (buf, targets)
}

/// Every row of `buf` as text, trailing spaces kept.
fn buf_rows(buf: &Buffer, width: u16, height: u16) -> Vec<String> {
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn notice_overlay() {
    let app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let (shown, _) = overlay_screen(&app, 100, 30);
    insta::assert_snapshot!("notice_overlay", shown);
}

#[test]
fn notice_overlay_list() {
    let owned = four();
    let app = overlay_app(100, 30, &owned, true);
    let (shown, _) = overlay_screen(&app, 100, 30);
    insta::assert_snapshot!("notice_overlay_list", shown);
}

#[test]
fn the_single_overlay_titles_and_foots_itself() {
    let app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let (shown, _) = overlay_screen(&app, 100, 30);
    assert!(shown.contains("Notice · key_clash"), "{shown}");
    assert!(shown.contains("Esc closes"), "{shown}");
    // The whole text reads, wrapped whole-word.
    for word in SINGLE_TEXT.split(' ') {
        assert!(shown.contains(word), "missing {word}");
    }
}

#[test]
fn the_list_overlay_titles_itself_notices() {
    let owned = four();
    let app = overlay_app(100, 30, &owned, true);
    let (shown, _) = overlay_screen(&app, 100, 30);
    assert!(shown.contains("Notices"), "{shown}");
    assert!(shown.contains("Esc closes"), "{shown}");
}

#[test]
fn text_rows_keep_the_two_blank_edge_columns() {
    let owned = four();
    for all in [false, true] {
        let notices: &[(&str, &str)] = if all { &owned } else { &[owned[0]] };
        let app = overlay_app(100, 30, notices, all);
        let (buf, _) = overlay_only(&app, 100, 30);
        for y in 0..30 {
            let row: String = (0..100).map(|x| buf[(x, y)].symbol().to_owned()).collect();
            if row.trim().is_empty() {
                continue;
            }
            if row.trim().chars().all(|ch| ch == '▄' || ch == '▀') {
                continue;
            }
            assert!(row.starts_with("  "), "{row:?}");
            assert!(row.ends_with("  "), "{row:?}");
        }
    }
}

#[test]
fn list_paragraphs_start_dim_and_read_plain_after() {
    use ratatui::style::Modifier;
    let owned = four();
    let app = overlay_app(100, 30, &owned, true);
    let (buf, _) = overlay_only(&app, 100, 30);
    let rows = buf_rows(&buf, 100, 30);
    // Newest first: each paragraph starts below the last.
    let mut at = 0;
    for (code, _) in owned.iter().rev() {
        let (y, line) = rows
            .iter()
            .enumerate()
            .find(|(i, row)| *i >= at && row.contains(code))
            .unwrap_or_else(|| panic!("{code} not drawn"));
        // ASCII throughout, so byte indices are cell columns.
        let col = line.find(code).unwrap();
        let row = u16::try_from(y).expect("a row");
        for (k, ch) in code.chars().enumerate() {
            let cell = &buf[(u16::try_from(col + k).expect("a column"), row)];
            assert_eq!(cell.symbol(), &ch.to_string(), "{code} moved");
            assert!(cell.modifier.contains(Modifier::DIM), "{code} not dim");
        }
        let after = u16::try_from(col + code.len() + " · ".chars().count()).expect("a column");
        assert!(
            !buf[(after, row)].modifier.contains(Modifier::DIM),
            "{code} text dim"
        );
        at = y + 1;
    }
}

#[test]
fn list_paragraphs_keep_one_blank_row_between() {
    let owned = four();
    let app = overlay_app(100, 30, &owned, true);
    let (buf, _) = overlay_only(&app, 100, 30);
    let rows = buf_rows(&buf, 100, 30);
    let blank = |y: usize| rows[y].trim().is_empty();
    let mut starts = Vec::new();
    for (code, _) in owned.iter().rev() {
        let (y, _) = rows
            .iter()
            .enumerate()
            .find(|(i, row)| !starts.contains(i) && row.contains(code))
            .unwrap_or_else(|| panic!("{code} not drawn"));
        starts.push(y);
    }
    assert_eq!(starts.len(), 4);
    for pair in starts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        assert!(!(a + 1..b - 1).any(&blank), "gap inside a paragraph");
        assert!(blank(b - 1), "no blank row between paragraphs");
    }
    // The last paragraph runs into the frame's own blank row only.
    let last = *starts.last().unwrap();
    let foot = (last + 1..30).find(|y| blank(*y)).expect("no foot gap");
    assert!(
        !(last + 1..foot).any(blank),
        "gap inside the last paragraph"
    );
    assert!(
        rows[foot + 1].contains("Esc closes"),
        "{:?}",
        rows[foot + 1]
    );
}

#[test]
fn short_texts_shrink_and_narrow_areas_keep_the_floor() {
    // Content narrower than the floor centres a 40-column slab.
    for width in [41u16, 40] {
        let app = overlay_app(width, 24, &[("x", "Saved.")], false);
        let (buf, _) = overlay_only(&app, width, 24);
        let edge = buf_rows(&buf, width, 24)
            .into_iter()
            .find(|row| row.contains('▄'))
            .expect("no top edge");
        assert_eq!(edge.chars().filter(|ch| *ch == '▄').count(), 40);
    }
    // Below 40 columns the overlay is the area wide.
    let app = overlay_app(39, 24, &[("x", "Saved.")], false);
    let (buf, _) = overlay_only(&app, 39, 24);
    let edge = buf_rows(&buf, 39, 24)
        .into_iter()
        .find(|row| row.contains('▄'))
        .expect("no top edge");
    assert_eq!(edge.chars().filter(|ch| *ch == '▄').count(), 39);
}

#[test]
fn every_listed_notice_shows_past_the_row_cap() {
    for count in [2usize, 3, 4] {
        let owned = four();
        let app = overlay_app(100, 30, &owned[..count], true);
        let (shown, _) = overlay_screen(&app, 100, 30);
        for (code, _) in &owned[..count] {
            assert!(shown.contains(code), "{count} notices: {code} missing");
        }
    }
}

#[test]
fn short_areas_keep_the_title_and_footer_until_the_chrome_gives() {
    // The chrome takes 8 rows, so one body row shows at 9.
    let app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let first = format::wrap(SINGLE_TEXT, 67).remove(0);
    let second = format::wrap(SINGLE_TEXT, 67).remove(1);
    let (buf, _) = overlay_only(&app, 100, 9);
    let shown = buf_rows(&buf, 100, 9).join("\n");
    assert!(shown.contains("Notice · key_clash"), "{shown}");
    assert!(shown.contains("Esc closes"), "{shown}");
    assert!(shown.contains(&first), "{shown}");
    assert!(!shown.contains(&second), "{shown}");
    // At 8 rows the body gives way and the title and footer stay.
    let (buf, _) = overlay_only(&app, 100, 8);
    let shown = buf_rows(&buf, 100, 8).join("\n");
    assert!(shown.contains("Notice · key_clash"), "{shown}");
    assert!(shown.contains("Esc closes"), "{shown}");
    assert!(!shown.contains("Two actions"), "{shown}");
    // At 7 rows the footer gives way before the title.
    let (buf, _) = overlay_only(&app, 100, 7);
    let shown = buf_rows(&buf, 100, 7).join("\n");
    assert!(shown.contains("Notice · key_clash"), "{shown}");
    assert!(!shown.contains("Esc closes"), "{shown}");
}

#[test]
fn no_overlay_target_over_a_zero_row_area() {
    let app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let (_, targets) = overlay_only(&app, 100, 0);
    assert!(targets.is_empty(), "{targets:?}");
}

#[test]
fn the_cross_closes_and_esc_closes() {
    use crate::app::Effect;
    use crate::keys::Key;
    let mut app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let cross = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the overlay draws its cross");
    let row: String = (0..100)
        .map(|x| buf[(x, cross.rect.y)].symbol().to_owned())
        .collect();
    assert!(row.contains("Notice · key_clash"), "{row}");
    assert_eq!(cross.rect.width, 1);
    assert_eq!(
        buf.cell((cross.rect.x, cross.rect.y))
            .map(|cell| cell.symbol()),
        Some("✕")
    );
    assert_eq!(app.on_click(TargetId::CloseOverlay), Effect::None);
    assert_eq!(app.notice_overlay(), None);
    // Esc closes it too, with the same click target drawn first.
    let mut app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.notice_overlay(), None);
}

#[test]
fn the_overlay_hides_the_conversation_targets() {
    use crate::keys::Key;
    use crate::results_support::{attached, now, reply};
    let mut app = attached(100, 30);
    reply(&mut app, "a_m", "needle one");
    assert_eq!(app.on_key(Key::CtrlF, now()), crate::app::Effect::None);
    for (n, ch) in "needle".chars().enumerate() {
        assert!(matches!(
            app.on_key(Key::Char(ch), now()),
            crate::app::Effect::FindPause { generation, .. } if generation == n as u64 + 1
        ));
    }
    assert!(app.find_due(6).is_empty());
    assert_eq!(app.on_key(Key::CtrlF, now()), crate::app::Effect::None);
    assert!(app.results_open(), "a second Ctrl+F opens the results");
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .any(|target| matches!(target.id, TargetId::FindResult(_))),
        "needs a FindResult target: {targets:?}"
    );
    app.push_notice("key_clash", SINGLE_TEXT.to_owned());
    app.open_notice(0);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .all(|target| !matches!(target.id, TargetId::FindResult(_))),
        "a FindResult target under the overlay: {targets:?}"
    );
}

#[test]
fn the_single_overlay_draws_its_thin_box_without_colour() {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let app = overlay_app(100, 30, &[("key_clash", SINGLE_TEXT)], false);
    let (shown, _) = overlay_screen(&app, 100, 30);
    assert!(shown.contains('┌'), "{shown}");
    assert!(shown.contains('└'), "{shown}");
    assert!(shown.contains('│'), "{shown}");
    assert!(shown.contains("Notice · key_clash"), "{shown}");
}

#[test]
fn wrap_runs_breaks_at_the_edge_and_keeps_styles() {
    use ratatui::style::{Modifier, Style};
    let dim = Style::new().add_modifier(Modifier::DIM);
    let plain = Style::new();
    // "ab · cd" is 7 columns: at room 7 it stays one row ...
    let rows = super::wrap_runs(&[("ab · ".to_owned(), dim), ("cd".to_owned(), plain)], 7);
    assert_eq!(rows.len(), 1);
    // ... and at room 6 the text drops to the next row.
    let rows = super::wrap_runs(&[("ab · ".to_owned(), dim), ("cd".to_owned(), plain)], 6);
    assert_eq!(rows.len(), 2);
    // The dim prefix stays dim and the text stays plain across rows.
    assert!(rows[0].iter().all(|span| span.style == dim));
    assert_eq!(rows[1].len(), 1);
    assert_eq!(rows[1][0].content, "cd");
    assert_eq!(rows[1][0].style, plain);
}

#[test]
fn wrap_runs_breaks_a_word_wider_than_the_row() {
    use ratatui::style::Style;
    let rows = super::wrap_runs(&[("abcdefgh".to_owned(), Style::new())], 5);
    assert_eq!(rows.len(), 2);
    let joined: String = rows
        .iter()
        .flat_map(|row| row.iter().map(|span| span.content.as_ref()))
        .collect();
    assert_eq!(joined, "abcdefgh");
}

#[test]
fn wrap_runs_starts_a_row_at_a_newline() {
    use ratatui::style::Style;
    let rows = super::wrap_runs(&[("a\nb".to_owned(), Style::new())], 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0].content, "a");
    assert_eq!(rows[1][0].content, "b");
}

#[test]
fn wrap_runs_joins_split_runs_with_no_space() {
    use ratatui::style::Style;
    let plain = Style::new();
    // A marked hit split across runs stays one word: runs join with
    // nothing, and a space reads only where the source holds one.
    let rows = super::wrap_runs(
        &[
            ("how do I ".to_owned(), plain),
            ("back".to_owned(), plain),
            ("fill it".to_owned(), plain),
        ],
        67,
    );
    assert_eq!(rows.len(), 1);
    let text: String = rows[0].iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "how do I backfill it");
}

/// Each row of `rows` as its text.
fn row_texts(rows: &[Vec<ratatui::text::Span<'static>>]) -> Vec<String> {
    rows.iter()
        .map(|row| row.iter().map(|span| span.content.as_ref()).collect())
        .collect()
}

#[test]
fn wrap_runs_fills_a_row_with_a_word_exactly_as_wide() {
    use ratatui::style::Style;
    // After the newline "abcd" is exactly the room, so it fills its row
    // and "ef" must wrap: a row short by one column would take "ef".
    let rows = super::wrap_runs(&[("x\n abcd ef".to_owned(), Style::new())], 4);
    assert_eq!(row_texts(&rows), ["x", "abcd", "ef"]);
}

#[test]
fn wrap_runs_drops_a_paragraph_space_left_by_an_empty_run() {
    use ratatui::style::Style;
    let plain = Style::new();
    // The first run is one space: the paragraph has not started, so the
    // next run's first word takes no space before it.
    let rows = super::wrap_runs(&[(" ".to_owned(), plain), ("abc".to_owned(), plain)], 10);
    assert_eq!(row_texts(&rows), ["abc"]);
}

#[test]
fn wrap_runs_drops_a_space_before_a_word_after_a_newline() {
    use ratatui::style::Style;
    let plain = Style::new();
    // The run before ends with a space, but the newline starts a row, so
    // the word after it takes no space.
    let rows = super::wrap_runs(&[("x ".to_owned(), plain), ("\nabc".to_owned(), plain)], 10);
    assert_eq!(row_texts(&rows), ["x", "abc"]);
}

#[test]
fn wrapped_rows_join_as_the_source_reads() {
    use crate::rows::Join;
    use ratatui::style::Style;
    let rows = super::wrap_joined_runs(&[("ab cd ef".to_owned(), Style::new())], 5);
    let plain: Vec<(String, Join)> = rows
        .into_iter()
        .map(|(spans, join)| {
            (
                spans.iter().map(|span| span.content.clone()).collect(),
                join,
            )
        })
        .collect();
    assert_eq!(
        plain,
        vec![
            ("ab cd".to_owned(), Join::Break),
            ("ef".to_owned(), Join::WrapSpace),
        ]
    );
    // A word broken mid-word joins with nothing.
    let rows = super::wrap_joined_runs(&[("abcdefgh".to_owned(), Style::new())], 5);
    let plain: Vec<(String, Join)> = rows
        .into_iter()
        .map(|(spans, join)| {
            (
                spans.iter().map(|span| span.content.clone()).collect(),
                join,
            )
        })
        .collect();
    assert_eq!(
        plain,
        vec![
            ("abcde".to_owned(), Join::Break),
            ("fgh".to_owned(), Join::Wrap),
        ]
    );
    // A newline breaks the line even with no blank row drawn.
    let rows = super::wrap_joined_runs(&[("a\nb".to_owned(), Style::new())], 10);
    let plain: Vec<(String, Join)> = rows
        .into_iter()
        .map(|(spans, join)| {
            (
                spans.iter().map(|span| span.content.clone()).collect(),
                join,
            )
        })
        .collect();
    assert_eq!(
        plain,
        vec![("a".to_owned(), Join::Break), ("b".to_owned(), Join::Break)]
    );
}

/// Draws the app's single notice through `script`: the buffer, the
/// targets, the fake, and how many rows `bottom` moved.
fn seamed(
    code: &str,
    message: &str,
    script: crate::ext::tests::SlotScript,
    flat: bool,
) -> (
    Buffer,
    Vec<crate::mouse::Target>,
    std::rc::Rc<crate::ext::tests::SlotRenderer>,
    u16,
) {
    use std::rc::Rc;
    let app = app_with_notice(code, message);
    let fake = Rc::new(crate::ext::tests::SlotRenderer::filling(
        &["notice"],
        script,
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, flat);
    let area = Rect::new(0, 0, 30, 24);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);
    (buf, targets, fake, 24 - bottom)
}

#[test]
fn a_notice_replacement_draws_instead_of_the_row() {
    use crate::ext::tests::{SlotScript, raw};
    let (buf, targets, fake, moved) = seamed(
        "io_failed",
        "a read failed",
        SlotScript::Replace(vec![vec![raw("custom")]]),
        false,
    );
    // (`docs/tui.md`, "The slots": a notice replacement paints its fitting lines and reserves those rows.)
    assert_eq!(moved, 1);
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(fake.calls.borrow()[0].1, "notice");
    assert_eq!(fake.calls.borrow()[0].2, 30);
    let row = row_text(&buf, 23);
    assert!(row.starts_with("custom"), "{row:?}");
    assert!(!row.contains('⚠'), "{row:?}");
    assert!(
        targets.iter().any(|target| target.id == TargetId::Notice(0)
            && target.rect.x == 0
            && target.rect.width == 30),
        "{targets:?}"
    );
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
fn session_half_lookup_returns_only_the_recorded_map() {
    use std::collections::BTreeMap;

    let session = contract::SessionId("session-a".to_owned());
    let mut pages = crate::window::Pages::new(80);
    // (`docs/tui.md`, "How a TUI extension runs": range-bound halves stay inert until `extensions_loaded`.)
    assert_eq!(pages.halves_for(&session), None);
    let expected = BTreeMap::from([
        ("ledger".to_owned(), "2.1.0".to_owned()),
        ("shell".to_owned(), "1.0.0".to_owned()),
    ]);
    pages.record_halves(&session, expected.clone());
    assert_eq!(pages.halves_for(&session), Some(expected));
}

#[test]
fn a_notice_replacement_fills_the_last_available_rows() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    use std::rc::Rc;

    let app = app_with_notice("io_failed", "a read failed");
    let fake = Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Replace(vec![vec![raw("first")], vec![raw("last")]]),
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let area = Rect::new(0, 22, 30, 2);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);

    // (`docs/tui.md`, "What a renderer returns" and "The slots": a notice replacement paints fitting lines in order, reserves those rows, and spans its targets over the painted lines.)
    assert!(row_text(&buf, 22).starts_with("first"));
    assert!(row_text(&buf, 23).starts_with("last"));
    assert!(targets.iter().any(|target| {
        target.id == TargetId::Notice(0) && target.rect.y == 22 && target.rect.height == 1
    }));
    assert!(targets.iter().any(|target| {
        target.id == TargetId::Notice(0) && target.rect.y == 23 && target.rect.height == 1
    }));
    assert!(targets.iter().any(|target| {
        target.id == TargetId::DismissNotice(0) && target.rect.y == 22 && target.rect.height == 2
    }));
}

#[test]
fn a_three_line_notice_replacement_clips_to_available_rows_and_dismisses() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    use std::rc::Rc;

    let mut app = app_with_notice("io_failed", "a read failed");
    let fake = Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Replace(vec![
            vec![raw("first")],
            vec![raw("second")],
            vec![raw("third")],
        ]),
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let area = Rect::new(0, 22, 30, 2);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);

    // (`docs/tui.md`, "The slots": a notice replacement keeps its first fitting lines, reserves those rows, and spans its targets over them.)
    assert!(row_text(&buf, 22).starts_with("first"));
    assert!(row_text(&buf, 23).starts_with("second"));
    assert_eq!(24 - bottom, 2);
    assert!(targets.iter().any(|target| {
        target.id == TargetId::DismissNotice(0) && target.rect.y == 22 && target.rect.height == 2
    }));

    let clicked = crate::mouse::hit(&targets, 29, 23).expect("second row has a dismiss target");
    assert_eq!(clicked, TargetId::DismissNotice(0));
    // (`docs/tui.md`, "Notices": ✕ dismisses an event notice.)
    app.on_click(clicked);
    assert!(app.notices().is_empty());
}

#[test]
fn a_notice_decorator_draws_the_built_in_row_under_its_badge() {
    use crate::ext::tests::{SlotScript, raw};
    let (buf, targets, fake, _) = seamed(
        "io_failed",
        "a read failed",
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
        false,
    );
    // The badge decorates the built-in row; the notice target covers both painted rows.
    assert_eq!(fake.builtins.borrow().as_slice(), &[25]);
    assert!(row_text(&buf, 22).starts_with("BADGE"));
    let row = row_text(&buf, 23);
    assert!(row.contains('⚠'), "{row:?}");
    assert!(
        targets.iter().any(|target| target.id == TargetId::Notice(0)
            && target.rect.y == 22
            && target.rect.height == 1),
        "{targets:?}"
    );
    assert!(
        targets.iter().any(|target| target.id == TargetId::Notice(0)
            && target.rect.y == 23
            && target.rect.height == 1),
        "{targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::DismissNotice(0)
                && target.rect.x == 29
                && target.rect.y == 22
                && target.rect.height == 2),
        "{targets:?}"
    );
}

#[test]
fn a_notice_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    use std::rc::Rc;
    let app = app_with_notice("io_failed", "a read failed");
    let fake = Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        let area = Rect::new(0, 0, 30, 24);
        let mut buf = Buffer::empty(area);
        let mut bottom = area.bottom();
        let mut targets = Vec::new();
        super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);
        let row = row_text(&buf, 23);
        assert!(row.contains('⚠'), "{row:?}");
    }
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "the failure calls nothing again"
    );
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "notice");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_flat_notice_replacement_drops_every_style() {
    use crate::ext::tests::SlotScript;
    use ratatui::style::{Color, Modifier};
    let script = || {
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "custom".into(),
            bold: true,
            fg: Some("accent".into()),
            ..crate::ext::RawSpan::default()
        }]])
    };
    let (styled, _, _, _) = seamed("io_failed", "a read failed", script(), false);
    let cell = &styled[(0, 23)];
    assert!(cell.modifier.contains(Modifier::BOLD), "bold lost");
    assert_ne!(cell.fg, Color::Reset, "accent lost");
    // Flat drops both: the same replacement draws plain.
    let (buf, _, _, moved) = seamed("io_failed", "a read failed", script(), true);
    // (`docs/tui.md`, "The slots": a notice replacement paints its fitting lines and reserves those rows.)
    assert_eq!(moved, 1);
    assert!(row_text(&buf, 23).starts_with("custom"));
    for x in 0..30 {
        let cell = &buf[(x, 23)];
        assert!(!cell.modifier.contains(Modifier::BOLD), "cell {x}");
        assert_eq!(cell.fg, Color::Reset, "cell {x}");
    }
}

#[test]
fn a_width_1_row_still_draws_its_cross() {
    // One column holds no text: the dismiss cross still draws, and its
    // target sits over that same cell.
    let mut app = app_with_notice("io_failed", "a read failed");
    app.set_size(1, 24);
    let area = Rect::new(0, 0, 1, 24);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    let bound = super::frame_bound(&app);
    super::draw(&app, area, &mut buf, &mut bottom, &mut targets, &bound);
    assert_eq!(buf[(0, 23)].symbol(), "✕");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::DismissNotice(0)
                && target.rect.x == 0
                && target.rect.width == 1),
        "{targets:?}"
    );
}

#[test]
fn an_extension_failed_notice_ignores_the_renderer() {
    use crate::ext::tests::{SlotScript, raw};
    let (buf, _, _, _) = seamed(
        "extension_failed",
        "switched off",
        SlotScript::Replace(vec![vec![raw("custom")]]),
        false,
    );
    let row = row_text(&buf, 23);
    assert!(row.contains('⚠'), "{row:?}");
    assert!(!row.contains("custom"), "{row:?}");
}

/// An app with one notice and an animated `notice` renderer installed,
/// drawing `rows` lines a frame.
fn animated_app(rows: usize) -> (App, std::rc::Rc<crate::ext::tests::AnimatedRenderer>) {
    let mut app = app_with_notice("io_failed", "x");
    let fake = std::rc::Rc::new(crate::ext::tests::AnimatedRenderer::filling(
        "notice",
        std::time::Duration::from_millis(500),
        rows,
    ));
    app.install_renderer(fake.clone());
    (app, fake)
}

/// Draws the app's notice rows at `width` by `height` in frame time
/// `at`, returning the painted replacement rows' animation keys.
fn draw_keys(
    app: &App,
    width: u16,
    height: u16,
    at: std::time::Instant,
) -> (Buffer, Vec<crate::ext::anim::AnimKey>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    app.pages().ext.cache.begin_anim_frame(Some(at), false);
    let bound = super::frame_bound(app);
    let keys = super::draw(app, area, &mut buf, &mut bottom, &mut targets, &bound);
    (buf, keys)
}

#[test]
fn draw_returns_exactly_the_painted_replacement_rows_keys() {
    let (app, fake) = animated_app(1);
    let t0 = fakes::clock::FakeClock::new().origin();
    let (buf, keys) = draw_keys(&app, 30, 24, t0);
    // One replacement row painted: one key, carrying the notice instance.
    assert_eq!(fake.frames.borrow().clone(), vec![0]);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys.first().expect("one key").notice_id(), Some(0));
    assert!(row_text(&buf, 23).contains("anim0-0"), "{buf:?}");
    // The key is the live animation: marking it schedules the notice.
    app.pages().ext.cache.mark_painted(&keys);
    assert_eq!(
        app.pages().ext.cache.anim_dues(),
        vec![(t0, std::time::Duration::from_millis(500))]
    );
}

#[test]
fn draw_drops_rows_past_the_top_without_their_keys() {
    use crate::ext::tests::AnimatedRenderer;
    let mut app = app_with_notice("io_failed", "x");
    let fake = std::rc::Rc::new(AnimatedRenderer::growing(
        "notice",
        std::time::Duration::from_millis(500),
        2,
    ));
    app.install_renderer(fake.clone());
    let t0 = fakes::clock::FakeClock::new().origin();
    // Frame 0 draws two lines, and the tall draw paints both keys.
    let (full, full_keys) = draw_keys(&app, 30, 2, t0);
    assert_eq!(full_keys.len(), 2);
    let body = crate::view::text(&full);
    assert!(
        body.contains("anim0-0") && body.contains("anim0-1"),
        "{body}"
    );
    // The clipped repaint hits the stored two-line frame but lends only
    // the painted row's key, with no new renderer call.
    let (buf, keys) = draw_keys(&app, 30, 1, t0);
    assert_eq!(fake.frames.borrow().clone(), vec![0]);
    assert_eq!(keys.len(), 1);
    assert!(row_text(&buf, 0).contains("anim0-0"), "{buf:?}");
}

#[test]
fn draw_ignores_builtin_rows() {
    // No renderer installed: the built-in row draws, carrying no key.
    let app = app_with_notice("io_failed", "x");
    let t0 = fakes::clock::FakeClock::new().origin();
    let (_, keys) = draw_keys(&app, 30, 24, t0);
    assert!(keys.is_empty());
}

#[test]
fn an_empty_replacement_hides_the_notice_without_a_key() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let mut app = app_with_notice("io_failed", "x");
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["notice"],
        SlotScript::Replace(vec![]),
    ));
    app.install_renderer(fake.clone());
    let t0 = fakes::clock::FakeClock::new().origin();
    let (buf, keys) = draw_keys(&app, 30, 24, t0);
    // No row paints and nothing marks: the hidden notice never advances
    // or schedules.
    assert!(keys.is_empty());
    assert!(!crate::view::text(&buf).contains("io_failed"));
}
