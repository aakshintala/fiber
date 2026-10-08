//! Drawing the search results (`docs/tui.md`, "Search"): the header and
//! one row per entry, each a jump target.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::Snippet;
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::results_support::{attached, now, reply};

/// Types `query` and starts its scan, then opens the results.
fn searched(app: &mut App, query: &str) {
    let at = now();
    assert_eq!(app.on_key(Key::CtrlF, at), Effect::None);
    for ch in query.chars() {
        assert!(
            matches!(app.on_key(Key::Char(ch), at), Effect::FindPause { .. }),
            "typing {ch:?} starts the pause"
        );
    }
    // The last keystroke's generation is the query's length: the bar
    // opened fresh, so no earlier query bumped it.
    let generation = u64::try_from(query.chars().count()).unwrap_or(u64::MAX);
    assert!(app.find_due(generation).is_empty());
    assert_eq!(app.on_key(Key::CtrlF, at), Effect::None);
    assert!(app.results_open());
}

/// The app drawn at `width` by `height`: its text, then a rule, then a
/// mask: `S` where the selected entry is, `H` where a match is marked,
/// `.` elsewhere.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    let mut mask = String::new();
    for y in 0..height {
        for x in 0..width {
            let cell = buf.cell((x, y));
            let selected = cell.is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED));
            let hit = cell.is_some_and(|cell| cell.bg == super::HIT.bg.unwrap_or_default());
            mask.push(if selected {
                'S'
            } else if hit {
                'H'
            } else {
                '.'
            });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", crate::view::text(&buf))
}

#[test]
fn results_80x24() {
    let mut app = attached(80, 24);
    reply(&mut app, "a_m", "the first needle here");
    reply(
        &mut app,
        "a_m",
        &format!("a long line with a needle in it {}", "x".repeat(200)),
    );
    reply(&mut app, "a_m", "a last needle");
    searched(&mut app, "needle");
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    insta::assert_snapshot!("results_80x24", screen(&app, 80, 24));
}

#[test]
fn results_with_the_selected_entry_scrolled() {
    let mut app = attached(80, 24);
    for n in 0..40 {
        reply(&mut app, "a_m", &format!("match number {n}"));
    }
    searched(&mut app, "match");
    for _ in 0..50 {
        assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    insta::assert_snapshot!(
        "results_with_the_selected_entry_scrolled",
        screen(&app, 80, 24)
    );
}

#[test]
fn a_long_row_windows_around_its_match() {
    let entry = Snippet {
        before: "before".to_owned(),
        line: format!("a needle {}", "x".repeat(400)),
        at: 2..8,
        after: "after".to_owned(),
    };
    // The row is wider than the view: it shows 80 cells around the
    // match instead of the context hiding it.
    let (text, hit) = super::entry_row(&entry, 80);
    assert_eq!(crate::format::width(&text), 80);
    assert_eq!(&text[hit.start as usize..hit.end as usize], "needle");
    // A narrower width centres the window on the match.
    let (narrow, hit) = super::entry_row(&entry, 10);
    assert_eq!(crate::format::width(&narrow), 10);
    assert_eq!(&narrow[hit.start as usize..hit.end as usize], "needle");
}

#[test]
fn a_match_exactly_the_views_width_fills_it() {
    // The hit alone is exactly `target`: the window is the hit, which
    // tells `>` from `>=` in the clip guard.
    let entry = Snippet {
        before: String::new(),
        line: format!("needle{}", "x".repeat(400)),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 6);
    assert_eq!(text, "needle");
    assert_eq!(hit, 0..6);
}

#[test]
fn a_wide_char_with_room_to_spare_is_kept() {
    // `\u{3042}` is two cells: with 8 cells for it and `needle` the
    // window keeps it, which tells `<=` from `<` in the growth guard.
    let entry = Snippet {
        before: String::new(),
        line: "\u{3042}needle".to_owned(),
        at: 1..7,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 8);
    assert_eq!(text, "\u{3042}needle");
    assert!(text.ends_with("needle"), "the match is whole: {text:?}");
    assert_eq!(hit, 2..8);
}

#[test]
fn a_match_wider_than_the_view_clips_at_its_edge() {
    let entry = Snippet {
        before: String::new(),
        line: "needle".to_owned(),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 4);
    assert_eq!(text, "need");
    assert_eq!(hit, 0..4);
}

#[test]
fn a_row_for_no_cells_is_empty() {
    let entry = Snippet {
        before: "before".to_owned(),
        line: "needle".to_owned(),
        at: 0..6,
        after: "after".to_owned(),
    };
    assert_eq!(super::entry_row(&entry, 0), (String::new(), 0..0));
}

#[test]
fn a_wide_char_straddling_the_window_edge_is_dropped() {
    // `\u{3042}` is two cells: ending the window mid-char drops it,
    // so the row never runs past the view.
    let entry = Snippet {
        before: String::new(),
        line: "\u{3042}needle".to_owned(),
        at: 1..7,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 7);
    assert!(crate::format::width(&text) <= 7);
    assert_eq!(&text[hit.start as usize..hit.end as usize], "needle");
}

#[test]
fn an_entry_at_a_page_edge_shows_only_what_the_scan_kept() {
    let entry = Snippet {
        before: String::new(),
        line: "needle".to_owned(),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 80);
    assert_eq!(text, "needle");
    assert_eq!(hit, 0..6);
}

#[test]
fn results_on_an_empty_area_draw_nothing() {
    let mut app = attached(80, 24);
    reply(&mut app, "a_m", "a needle here");
    searched(&mut app, "needle");
    let view = app.find_results().expect("open");
    let area = Rect::new(0, 0, 0, 0);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::render(&view, area, &mut buf, &mut targets);
    assert!(targets.is_empty());
}

/// The widths every drawn-row test runs at: either side of the
/// narrowest view a match shows whole in, and of the common terminal.
const WIDTHS: [u16; 6] = [7, 8, 13, 40, 80, 81];

/// Context pieces whose string width differs from the cells
/// `Buffer::set_stringn` draws them in, or which it skips: lam-alef, a
/// ZWJ sequence, a wide char, a combining mark, a mix, a flag and a
/// control character.
const PIECES: [&str; 7] = [
    "\u{644}\u{627}",
    "\u{1f469}\u{200d}\u{1f4bb}",
    "\u{3042}",
    "e\u{301}",
    "ab\u{3042}\u{644}\u{627}\u{1f469}\u{200d}\u{1f4bb}",
    "\u{1f1ef}\u{1f1f5}",
    "a\u{7}b",
];

/// One entry's row as the results view draws it, read back cell by cell.
#[derive(Debug)]
struct Row {
    /// Every cell's symbol, left to right.
    text: String,
    /// The symbols of the cells marked as the match.
    marked: String,
    /// The marked cells, which are always one run.
    hit: std::ops::Range<u16>,
    /// The cells up to the end of the last grapheme drawn.
    filled: u16,
}

/// `entry` drawn as the only result in a view `width` wide, through
/// [`super::render`] and `Buffer::set_stringn`, as the terminal shows it.
fn drawn(entry: &Snippet, width: u16) -> Row {
    use ratatui::buffer::CellWidth;
    let view = crate::app::results::ResultsView {
        header: String::new(),
        entries: vec![entry.clone()],
        selected: usize::MAX,
        top: 0,
    };
    let area = Rect::new(0, 0, width, 2);
    let mut buf = Buffer::empty(area);
    super::render(&view, area, &mut buf, &mut Vec::new());
    let (mut text, mut marked, mut marks, mut filled) = (String::new(), String::new(), vec![], 0);
    for x in 0..width {
        let cell = buf.cell((x, 1)).expect("in the area");
        text.push_str(cell.symbol());
        if cell.bg == super::HIT.bg.unwrap_or_default() {
            marked.push_str(cell.symbol());
            marks.push(x);
        }
        if cell.symbol() != " " {
            filled = x + cell.symbol().cell_width();
        }
    }
    let hit = marks.first().copied().unwrap_or(0)..marks.last().map_or(0, |&last| last + 1);
    assert_eq!(
        marks,
        hit.clone().collect::<Vec<_>>(),
        "the match is one run of cells: {text:?}"
    );
    Row {
        text,
        marked,
        hit,
        filled,
    }
}

/// A match on a line of 100 `piece`s before it and, when `after`, 100
/// after it.
fn among(piece: &str, after: bool) -> Snippet {
    let context = piece.repeat(100);
    let start = context.chars().count();
    let tail = if after { context.as_str() } else { "" };
    Snippet {
        before: String::new(),
        line: format!("{context}needle{tail}"),
        at: start..start + 6,
        after: String::new(),
    }
}

#[test]
fn every_kind_of_context_keeps_the_match_drawn_and_marked() {
    for piece in PIECES {
        for after in [false, true] {
            for width in WIDTHS {
                let row = drawn(&among(piece, after), width);
                let case = format!("{piece:?} after={after} width={width}: {row:?}");
                assert!(row.text.contains("needle"), "{case}");
                // Exactly the match's cells are marked: none of the
                // context's either side.
                assert_eq!(row.marked, "needle", "{case}");
                // The window uses the view: at most one cell is left
                // where a two-cell grapheme would not fit.
                assert!(row.filled + 1 >= width, "{case}");
                if !after {
                    assert_eq!(row.hit.end, row.filled, "the match ends the row: {case}");
                }
            }
        }
    }
}

#[test]
fn a_match_opening_the_row_is_drawn_at_its_start() {
    for width in WIDTHS {
        let entry = Snippet {
            before: String::new(),
            line: format!("needle{}", PIECES[4].repeat(100)),
            at: 0..6,
            after: String::new(),
        };
        let row = drawn(&entry, width);
        assert_eq!(row.hit, 0..6, "{row:?}");
        assert!(row.text.starts_with("needle"), "{row:?}");
        assert!(row.filled + 1 >= width, "{row:?}");
    }
}

#[test]
fn a_match_ending_the_row_is_drawn_at_its_end() {
    for width in WIDTHS {
        let entry = Snippet {
            before: PIECES[4].repeat(100),
            line: "needle".to_owned(),
            at: 0..6,
            after: String::new(),
        };
        let row = drawn(&entry, width);
        assert_eq!(row.marked, "needle", "{row:?}");
        assert_eq!(row.hit.end, row.filled, "{row:?}");
        assert!(row.filled + 1 >= width, "{row:?}");
    }
}

#[test]
fn a_match_wider_than_the_view_is_drawn_clipped() {
    let letters = "abcdefghij".repeat(10);
    for width in WIDTHS {
        let entry = Snippet {
            before: PIECES[4].repeat(100),
            line: letters.clone(),
            at: 0..100,
            after: PIECES[4].repeat(100),
        };
        let row = drawn(&entry, width);
        assert_eq!(row.hit, 0..width, "{row:?}");
        assert_eq!(row.marked, letters[..usize::from(width)], "{row:?}");
    }
    // A wide match clips at its last whole grapheme.
    let entry = Snippet {
        before: String::new(),
        line: "\u{3042}".repeat(50),
        at: 0..50,
        after: String::new(),
    };
    assert_eq!(drawn(&entry, 7).hit, 0..6);
    assert_eq!(drawn(&entry, 8).hit, 0..8);
}

#[test]
fn a_row_exactly_the_views_width_is_drawn_whole() {
    let entry = Snippet {
        before: "ab".to_owned(),
        line: "needle".to_owned(),
        at: 0..6,
        after: "cd".to_owned(),
    };
    // `ab needle cd` is 12 cells, the last added on the right: exactly
    // the width shows it whole.
    let row = drawn(&entry, 12);
    assert_eq!(row.text, "ab needle cd");
    assert_eq!(row.hit, 3..9);
    // One cell narrower drops that last one.
    let row = drawn(&entry, 11);
    assert_eq!(row.text, "ab needle c");
    assert_eq!(row.hit, 3..9);
}

#[test]
fn an_empty_match_anchors_the_window_at_its_bytes() {
    // The window grows from the grapheme starting at the empty match,
    // `7`, one cell each side in turn: never from the one before it.
    let entry = Snippet {
        before: String::new(),
        line: "0123456789".repeat(40),
        at: 7..7,
        after: String::new(),
    };
    let row = drawn(&entry, 4);
    assert_eq!(row.text, "5678");
    assert_eq!(row.marked, "");
}
