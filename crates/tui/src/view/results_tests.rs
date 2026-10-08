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
fn a_long_preceding_line_never_hides_the_match() {
    // A 400-character line before the match at width 80: the match
    // text appears in the row.
    let entry = Snippet {
        before: "x".repeat(400),
        line: "needle here".to_owned(),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 80);
    assert_eq!(crate::format::width(&text), 80);
    assert!(text.contains("needle"), "the match stays visible: {text:?}");
    assert_eq!(&text[hit.start as usize..hit.end as usize], "needle");
}

#[test]
fn a_match_at_the_rows_end_clamps_the_window_to_the_row() {
    let entry = Snippet {
        before: "x".repeat(400),
        line: "needle".to_owned(),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 10);
    assert_eq!(crate::format::width(&text), 10);
    assert_eq!(&text[hit.start as usize..hit.end as usize], "needle");
    assert!(
        text.ends_with("needle"),
        "the window ends at the row: {text:?}"
    );
}

#[test]
fn a_match_at_the_rows_start_opens_the_window_at_the_row() {
    let entry = Snippet {
        before: String::new(),
        line: format!("needle {}", "x".repeat(400)),
        at: 0..6,
        after: String::new(),
    };
    let (text, hit) = super::entry_row(&entry, 10);
    assert!(
        text.starts_with("needle"),
        "the window opens at the row: {text:?}"
    );
    assert_eq!(&text[hit.start as usize..hit.end as usize], "needle");
}

#[test]
fn a_row_exactly_the_views_width_shows_whole() {
    let entry = Snippet {
        before: "ab".to_owned(),
        line: "needle".to_owned(),
        at: 0..6,
        after: "cd".to_owned(),
    };
    // `ab needle cd` is 12 cells: exactly the width shows it whole.
    let (text, hit) = super::entry_row(&entry, 12);
    assert_eq!(text, "ab needle cd");
    assert_eq!(hit, 3..9);
    // One cell narrower windows around the match.
    let (cut, hit) = super::entry_row(&entry, 11);
    assert_eq!(crate::format::width(&cut), 11);
    assert_eq!(&cut[hit.start as usize..hit.end as usize], "needle");
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
