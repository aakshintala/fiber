//! Drawing the search results (`docs/tui.md`, "Search"): the header and
//! one row per entry, each a jump target.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::app::Snippet;
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::results_support::{attached, now, reply};

#[test]
fn small_results_areas_draw_only_complete_entries() {
    let mut app = attached(80, 24);
    reply(&mut app, "a1", "needle one");
    searched(&mut app, "needle");
    let view = app.find_results().unwrap();
    for height in 0..=8 {
        let area = Rect::new(2, 3, 80, height);
        let mut buf = Buffer::empty(area);
        let mut targets = Vec::new();
        super::render(&view, area, &mut buf, &mut targets);
        assert_eq!(
            targets
                .iter()
                .filter(|target| target.id == crate::mouse::TargetId::CloseOverlay)
                .count(),
            usize::from(height > 0)
        );
        assert_eq!(
            targets
                .iter()
                .filter(|target| matches!(target.id, crate::mouse::TargetId::FindResult(_)))
                .count(),
            if height >= 7 { 2 } else { 0 }
        );
        assert_eq!(
            crate::view::text(&buf).contains("Enter jumps to the match"),
            height > 1
        );
        assert!(
            targets
                .iter()
                .all(|target| area.contains(target.rect.as_position()))
        );
    }
}

#[test]
fn the_label_needs_a_cell_between_it_and_the_path() {
    for path in ["src/a.rs".to_owned(), "\u{644}\u{627}".repeat(4)] {
        for (width, label) in [(14, false), (15, true), (16, true)] {
            let entry = Snippet {
                path: path.clone(),
                turn: Some(0),
                ..Snippet::default()
            };
            let area = Rect::new(0, 0, width, 1);
            let mut buf = Buffer::empty(area);
            super::path_row(&entry, area, &mut buf);
            let text = crate::view::text(&buf);
            assert!(text.starts_with(&path));
            assert_eq!(
                text.trim_end().ends_with("turn 1"),
                label,
                "{path:?} at {width}"
            );
            for x in 0..8 {
                assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));
            }
            if label {
                for x in width - 6..width {
                    assert!(buf[(x, 0)].modifier.contains(Modifier::DIM));
                    assert!(!buf[(x, 0)].modifier.contains(Modifier::BOLD));
                }
                assert_eq!(buf[(8, 0)].symbol(), " ");
            }
        }
        let entry = Snippet {
            path: path.clone(),
            ..Snippet::default()
        };
        let area = Rect::new(0, 0, 16, 1);
        let mut buf = Buffer::empty(area);
        super::path_row(&entry, area, &mut buf);
        assert_eq!(crate::view::text(&buf).trim_end(), path);
    }
}

#[test]
fn the_label_needs_one_cell_before_it() {
    for (width, turn, label) in [
        (6, Some(0), false),
        (7, Some(0), true),
        (8, Some(0), true),
        (8, None, false),
    ] {
        let entry = Snippet {
            turn,
            ..Snippet::default()
        };
        let area = Rect::new(2, 3, width, 1);
        let mut buf = Buffer::empty(area);
        super::path_row(&entry, area, &mut buf);
        assert_eq!(
            crate::view::text(&buf).trim_end().ends_with("turn 1"),
            label
        );
    }
}

#[test]
fn two_turns_show_each_matchs_own_turn_label() {
    let mut app = attached(80, 24);
    reply(&mut app, "a_one", "needle one");
    for (kind, payload) in [
        (
            "turn_completed",
            serde_json::json!({"outcome": "completed"}),
        ),
        (
            "turn_started",
            serde_json::json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "next"}]}]}),
        ),
    ] {
        app.on_line(crate::link::Line::Session(contract::Envelope {
            kind: kind.to_owned(),
            session_id: contract::SessionId(crate::results_support::SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: Some(contract::TurnId("t_two".to_owned())),
            action_id: None,
            seq: None,
            payload: payload.as_object().unwrap().clone(),
        }));
    }
    reply(&mut app, "a_two", "needle two");
    searched(&mut app, "needle");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    for (y, label) in [(3, "turn 1"), (6, "turn 2")] {
        let row: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        assert!(row.ends_with(label), "{row:?}");
        for x in 74..80 {
            assert!(buf[(x, y)].modifier.contains(Modifier::DIM));
        }
    }
}

#[test]
fn the_view_matches_the_doc() {
    let mut app = attached(80, 24);
    for (action, text) in [
        ("a1", "needle one"),
        ("a2", "needle two"),
        ("a3", "needle three"),
    ] {
        reply(&mut app, action, text);
    }
    searched(&mut app, "needle");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let rows: Vec<_> = text.lines().collect();
    assert!(rows[0].starts_with(" Search  ⌕ needle"));
    assert!(rows[0].ends_with("Esc returns"));
    assert_eq!(rows[1], "1 of 3 · ⌕ needle");
    assert_eq!(rows[2], "");
    assert!(buf[(0, 1)].modifier.contains(Modifier::DIM));
    let close: Vec<_> = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::CloseOverlay)
        .collect();
    assert_eq!(close.len(), 1);
    assert_eq!(close[0].rect, Rect::new(0, 0, 80, 1));
    for (at, y) in [(0, 3), (1, 6), (2, 9)] {
        let rows: Vec<_> = targets
            .iter()
            .filter(|target| target.id == crate::mouse::TargetId::FindResult(at))
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rect, Rect::new(0, y, 80, 1));
        assert_eq!(rows[1].rect, Rect::new(0, y + 1, 80, 1));
        assert!(targets.iter().all(|target| {
            !target
                .rect
                .contains(ratatui::layout::Position::new(0, y + 2))
        }));
    }
    assert!(buf[(0, 3)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(0, 4)].modifier.contains(Modifier::REVERSED));
    assert_eq!(rows[18], "Enter jumps to the match");
    assert!(buf[(0, 18)].modifier.contains(Modifier::DIM));
}

#[test]
fn the_current_match_is_black_on_match_current() {
    let mut app = attached(80, 24);
    reply(&mut app, "a1", "needle one");
    reply(&mut app, "a2", "needle two");
    searched(&mut app, "needle");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let current = (0..80)
        .find(|&x| buf[(x, 4)].bg == crate::theme::Role::MatchCurrent.color())
        .unwrap();
    assert_eq!(buf[(current, 4)].fg, crate::look::BAR_TEXT);
    // The second snippet includes its preceding line: find its marked cells.
    let other = (0..80)
        .find(|&x| buf[(x, 7)].bg == crate::theme::Role::Match.color())
        .unwrap();
    assert_eq!(buf[(other, 7)].fg, crate::look::TAB_BAR);
}

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
            let hit = cell.is_some_and(|cell| matches!(cell.bg, bg if bg == crate::theme::Role::Match.color() || bg == crate::theme::Role::MatchCurrent.color()));
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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

/// Under `NO_COLOR` a result's match keeps its underline and the
/// selected entry its reversal: the role colours paint to the default.
#[test]
fn no_color_keeps_results_marks_distinct() {
    use crate::look::{Look, ThemeSetting};
    use crate::theme::Role;
    use ratatui::style::{Color, Modifier};

    struct Unboxed;
    impl Drop for Unboxed {
        fn drop(&mut self) {
            crate::surface::init(&|_: &str| None);
        }
    }
    let _guard = Unboxed;
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut app = attached(80, 24);
    reply(&mut app, "a_m", "the first needle here");
    reply(&mut app, "a_m", "a second needle");
    searched(&mut app, "needle");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let marked = Role::Match.color();
    let (mut hit, mut selected) = (None, None);
    for y in 0..24 {
        for x in 0..80 {
            let cell = buf.cell((x, y)).expect("in the area");
            if cell.bg == marked && !cell.modifier.contains(Modifier::REVERSED) {
                hit.get_or_insert((x, y));
            }
            if cell.modifier.contains(Modifier::REVERSED)
                && cell.bg != marked
                && cell.bg != Role::MatchCurrent.color()
            {
                selected.get_or_insert((x, y));
            }
        }
    }
    let (hit, selected) = (
        hit.expect("a matched cell"),
        selected.expect("a selected cell"),
    );
    let current = (0..24)
        .flat_map(|y| (0..80).map(move |x| (x, y)))
        .find(|&at| buf[at].bg == Role::MatchCurrent.color())
        .expect("a current match");
    assert!(buf[current].modifier.contains(Modifier::BOLD));
    let vars: Vec<(String, String)> = vec![("NO_COLOR".to_owned(), "1".to_owned())];
    let (look, notice) = Look::new(ThemeSetting::Dark, &|name: &str| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    assert_eq!(notice, None);
    look.paint(&mut buf);
    let cell = |at| buf.cell(at).expect("in the area").clone();
    for at in [hit, selected] {
        assert_eq!((cell(at).fg, cell(at).bg), (Color::Reset, Color::Reset));
    }
    assert_eq!(cell(hit).modifier, Modifier::UNDERLINED);
    assert_eq!(cell(selected).modifier, Modifier::REVERSED);
    assert_eq!(
        (cell(current).fg, cell(current).bg),
        (Color::Reset, Color::Reset)
    );
    assert_eq!(
        cell(current).modifier,
        Modifier::REVERSED.union(Modifier::BOLD)
    );
    assert_ne!(
        cell(hit).modifier,
        cell(selected).modifier,
        "the match and the selection stay apart with no colour"
    );
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
        query: String::new(),
        current: None,
        entries: vec![entry.clone()],
        selected: usize::MAX,
        top: 0,
    };
    let area = Rect::new(0, 0, width, 7);
    let mut buf = Buffer::empty(area);
    super::render(&view, area, &mut buf, &mut Vec::new());
    let (mut text, mut marked, mut marks, mut filled) = (String::new(), String::new(), vec![], 0);
    for x in 0..width {
        let cell = buf.cell((x, 4)).expect("in the area");
        text.push_str(cell.symbol());
        if cell.bg == crate::theme::Role::Match.color() {
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
        path: String::new(),
        turn: None,
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
            path: String::new(),
            turn: None,
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
            path: String::new(),
            turn: None,
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
            path: String::new(),
            turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
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
        path: String::new(),
        turn: None,
        before: String::new(),
        line: "0123456789".repeat(40),
        at: 7..7,
        after: String::new(),
    };
    let row = drawn(&entry, 4);
    assert_eq!(row.text, "5678");
    assert_eq!(row.marked, "");
}

/// A drag over a result entry copies its text: the path row and the
/// match row read in order (`docs/tui.md`, "Selection and copy").
#[test]
fn a_drag_over_a_result_entry_copies_its_match() {
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = attached(80, 24);
    reply(&mut app, "a1", "needle one");
    searched(&mut app, "needle");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let screen = crate::view::text(&buf);
    let (y, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("needle one"))
        .map(|(y, line)| (u16::try_from(y).unwrap_or(u16::MAX), line.to_owned()))
        .expect("the match row draws");
    let x = u16::try_from(line.find("needle").unwrap_or_default()).unwrap_or(u16::MAX);
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    let _ = report(MouseKind::Press(Button::Left), x, y);
    let _ = report(MouseKind::Drag(Button::Left), x + 5, y);
    assert_eq!(
        report(MouseKind::Release, x + 5, y),
        Effect::Copy("needle".to_owned())
    );
}
