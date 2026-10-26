//! Tests for the Ctrl+R panel on the overlay frame: the query, the
//! count and the matches around the selection (`docs/tui.md`, "The
//! input box", "Overlays").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::draw;
use crate::app::{App, Effect};
use crate::completion_rows::Completions;
use crate::keys::Key;
use crate::link::Line;
use crate::theme::Role;
use contract::clock::Clock;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// An app in `/w`, attached to `S_A`, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_project("-Users-me-work-fiber-.git".to_owned());
    app.set_size(width, height);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// A `turn_started` for `S_A` with one driver message.
fn turn_started(text: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": text}],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

/// Types `text` key by key.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, Key::Char(ch));
    }
}

/// An app with `prompts` taken (newest last) and the search panel open
/// on `query`.
fn searching(width: u16, height: u16, prompts: &[&str], query: &str) -> App {
    let mut app = attached(width, height);
    for prompt in prompts {
        app.on_line(turn_started(prompt));
    }
    press(&mut app, Key::CtrlR);
    assert!(app.completions().is_some(), "the panel opens");
    type_text(&mut app, query);
    app
}

/// The panel's completions.
fn panel(app: &App) -> Completions {
    app.completions().expect("the panel shows")
}

/// Draws only the panel on an empty buffer: the slab rect and targets.
fn drawn(
    completions: &Completions,
    width: u16,
    height: u16,
) -> (Buffer, Vec<crate::mouse::Target>, Rect) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let slab = draw(completions, area, &mut buf, &mut targets);
    (buf, targets, slab)
}

/// The rows with a cell on the selection bar.
fn barred(buf: &Buffer) -> Vec<u16> {
    let area = buf.area;
    (area.top()..area.bottom())
        .filter(|y| (area.left()..area.right()).any(|x| buf[(x, *y)].bg == Role::Accent.color()))
        .collect()
}

/// The row's text, trailing spaces kept.
fn row_text(buf: &Buffer, width: u16, y: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

/// Renders `app` at `width` by `height` as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn three_matches_with_the_second_selected() {
    let mut app = searching(
        80,
        24,
        &[
            "how do I backfill embeddings for old sessions?",
            "back up the state file before the migration",
            "roll back the handoff note",
        ],
        "back",
    );
    press(&mut app, Key::Down);
    insta::assert_snapshot!(
        "three_matches_with_the_second_selected",
        screen(&app, 80, 24)
    );
}

#[test]
fn no_match() {
    let app = searching(80, 24, &["fix the build", "run the tests"], "zzz");
    insta::assert_snapshot!("no_match", screen(&app, 80, 24));
}

#[test]
fn one_match_names_its_single_count() {
    let app = searching(80, 24, &["fix the build", "run the tests"], "tests");
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 80, 24);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("1 match · newest first"), "{shown}");
    assert!(!shown.contains("matches ·"), "{shown}");
    assert_eq!(barred(&buf).len(), 1, "{shown}");
}

#[test]
fn the_panel_titles_counts_and_bars_exactly_its_selection() {
    let app = searching(
        80,
        24,
        &[
            "how do I backfill embeddings for old sessions?",
            "back up the state file before the migration",
            "roll back the handoff note",
        ],
        "back",
    );
    let completions = panel(&app);
    let (buf, _, slab) = drawn(&completions, 80, 24);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("Prompt history"), "{shown}");
    assert!(shown.contains("3 matches · newest first"), "{shown}");
    assert!(shown.contains("Enter recalls · Esc closes"), "{shown}");
    // Exactly one barred row, and it opens with the gutter.
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    let row = row_text(&buf, 80, bars[0]);
    let text = row.trim_start();
    assert!(text.starts_with("› "), "{row:?}");
    // Recall order is newest first: the selection opens on "roll back".
    assert!(text.contains("roll back the handoff note"), "{row:?}");
    // The query hit reads bold; the slab writes no cell outside itself.
    let hit = row.find("back").expect("the hit");
    let x = u16::try_from(row[..hit].chars().count()).expect("a column");
    assert!(
        buf[(x, bars[0])].modifier.contains(Modifier::BOLD),
        "{row:?}"
    );
    for y in 0..24 {
        for x in 0..80 {
            if buf[(x, y)].symbol() != " " {
                assert!(
                    x >= slab.x && x < slab.right() && y >= slab.y && y < slab.bottom(),
                    "cell ({x}, {y}) outside {slab:?}"
                );
            }
        }
    }
    // No per-match origin tag.
    assert!(!shown.contains("this session"), "{shown}");
    assert!(!shown.contains("· project"), "{shown}");
}

#[test]
fn a_long_match_wraps_hanging_under_the_gutter() {
    let app = searching(
        40,
        24,
        &[
            "back up the state file before the migration",
            "fix the build",
        ],
        "back",
    );
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 40, 24);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("back up the state"), "{shown}");
    assert!(shown.contains("migration"), "{shown}");
    // The wrapped line hangs under the gutter, past its two columns.
    let hanging = shown
        .lines()
        .find(|line| line.contains("migration"))
        .expect("the wrapped line");
    let indented = hanging.trim_start();
    assert!(hanging.len() > indented.len() + 1, "{hanging:?}");
    assert!(!indented.starts_with('›'), "{hanging:?}");
}

#[test]
fn a_short_screen_shows_the_selected_entry() {
    let mut app = searching(80, 12, &["fix one", "fix two", "fix three"], "fix");
    press(&mut app, Key::Down);
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 80, 12);
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix two"), "{shown}");
}

#[test]
fn short_areas_give_way_query_then_count_then_entries() {
    let app = searching(80, 24, &["fix one", "fix two"], "fix");
    let completions = panel(&app);
    // Nine rows: the chrome takes eight, so the query row alone shows.
    let (buf, _, _) = drawn(&completions, 80, 9);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("› fix"), "{shown}");
    assert!(!shown.contains("matches ·"), "{shown}");
    assert!(!shown.contains("fix two"), "{shown}");
    assert!(!shown.contains("fix one"), "{shown}");
    // Ten rows: the count row joins it.
    let (buf, _, _) = drawn(&completions, 80, 10);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("› fix"), "{shown}");
    assert!(shown.contains("2 matches · newest first"), "{shown}");
    assert!(!shown.contains("fix two"), "{shown}");
    assert!(!shown.contains("fix one"), "{shown}");
    // Eleven rows: the first entry joins them. Recall order is newest
    // first, so it is "fix two".
    let (buf, _, _) = drawn(&completions, 80, 11);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("fix two"), "{shown}");
}

#[test]
fn the_count_names_every_match_past_the_eight_shown() {
    for count in [7usize, 8, 9] {
        let prompts: Vec<String> = (1..=count).map(|n| format!("fix {n}")).collect();
        let refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
        let app = searching(80, 24, &refs, "fix");
        let completions = panel(&app);
        assert_eq!(completions.total, count);
        let (buf, _, _) = drawn(&completions, 80, 24);
        let shown = crate::view::text(&buf);
        assert!(
            shown.contains(&format!("{count} matches · newest first")),
            "{shown}"
        );
        let entries = shown.lines().filter(|line| line.contains("fix ")).count();
        assert_eq!(entries, count.min(8), "{shown}");
    }
}

#[test]
fn the_window_keeps_a_late_selection_in_view() {
    let prompts: Vec<String> = (1..=8).map(|n| format!("fix {n}")).collect();
    let refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
    let mut app = searching(80, 11, &refs, "fix");
    for _ in 0..7 {
        press(&mut app, Key::Down);
    }
    let completions = panel(&app);
    assert_eq!(completions.selected, Some(8));
    let (buf, _, _) = drawn(&completions, 80, 11);
    let shown = crate::view::text(&buf);
    // Eleven rows leave one entry row: the selected last match. Recall
    // order is newest first, so index 7 is the oldest, "fix 1".
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix 1"), "{shown}");
}

#[test]
fn a_tall_selected_entry_keeps_its_first_rows() {
    let tall = format!("fix {}", "word ".repeat(60));
    let mut app = attached(80, 11);
    app.on_line(turn_started(&tall));
    app.on_line(turn_started("fix small"));
    press(&mut app, Key::CtrlR);
    type_text(&mut app, "fix");
    press(&mut app, Key::Down);
    let completions = panel(&app);
    assert_eq!(completions.total, 2);
    let (buf, _, _) = drawn(&completions, 80, 11);
    let shown = crate::view::text(&buf);
    // Eleven rows leave one entry row: the window scrolls to the tall
    // selected entry, whose first wrapped row shows while the short
    // entry drops out.
    assert!(shown.contains("word word"), "{shown}");
    assert!(!shown.contains("fix small"), "{shown}");
}

#[test]
fn narrow_areas_keep_the_forty_column_floor() {
    let app = searching(80, 24, &["fix the build"], "fix");
    let completions = panel(&app);
    for width in [41u16, 40] {
        let (buf, _, _) = drawn(&completions, width, 24);
        let edge = (0..24)
            .map(|y| row_text(&buf, width, y))
            .find(|row| row.contains('▄'))
            .expect("no top edge");
        assert_eq!(edge.chars().filter(|ch| *ch == '▄').count(), 40);
    }
    let (buf, _, _) = drawn(&completions, 39, 24);
    let edge = (0..24)
        .map(|y| row_text(&buf, 39, y))
        .find(|row| row.contains('▄'))
        .expect("no top edge");
    assert_eq!(edge.chars().filter(|ch| *ch == '▄').count(), 39);
}

#[test]
fn wrapped_matches_before_a_late_selection_keep_the_selection() {
    // Each match wraps to two rows at 40 columns, so the window must
    // count rows, not matches: the matches above the late selection
    // must not push its barred rows out of the frame.
    let prompts: Vec<String> = (1..=8)
        .map(|n| format!("fix {n} {}", "word ".repeat(10)))
        .collect();
    let refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
    let mut app = searching(40, 20, &refs, "fix");
    for _ in 0..7 {
        press(&mut app, Key::Down);
    }
    let completions = panel(&app);
    assert_eq!(completions.selected, Some(8));
    let (buf, _, _) = drawn(&completions, 40, 20);
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert!(!bars.is_empty(), "the selection shows: {shown}");
    assert!(row_text(&buf, 40, bars[0]).contains("fix 1"), "{shown}");
}

#[test]
fn a_tall_selected_entry_keeps_a_narrow_slab() {
    // Eleven rows leave one entry row. The selected entry wraps to two
    // rows, and the frame measures every body row before it clips, so
    // the hidden second row must not widen the slab: the window keeps
    // only the first row (see #1792).
    let tall = format!("fix {}", "x".repeat(65));
    let app = searching(80, 11, &[&tall], "fix");
    let completions = panel(&app);
    let (buf, _, slab) = drawn(&completions, 80, 11);
    assert_eq!(slab.width, 40, "{}", crate::view::text(&buf));
    // The hidden second row's run of x's is not among the panel's rows.
    assert!(
        !crate::view::text(&buf).contains("xx"),
        "{}",
        crate::view::text(&buf)
    );
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{}", crate::view::text(&buf));
    assert!(
        row_text(&buf, 80, bars[0]).contains("fix"),
        "{}",
        crate::view::text(&buf)
    );
    // The whole screen agrees: a 40-column slab. (The conversation
    // behind the panel still shows the prompt's own x's.)
    let shown = screen(&app, 80, 11);
    let edge = shown
        .lines()
        .find(|line| line.contains('▄'))
        .expect("no top edge");
    assert_eq!(edge.chars().filter(|ch| *ch == '▄').count(), 40, "{shown}");
}

#[test]
fn an_entry_exactly_filling_the_room_shows_alone() {
    // Eleven rows leave one entry row and the selected entry takes
    // exactly it, so no truncation drops a row and no neighbour fits.
    let app = searching(80, 11, &["fix one", "fix two"], "fix");
    let completions = panel(&app);
    let (buf, _, slab) = drawn(&completions, 80, 11);
    assert_eq!(slab.width, 40, "{}", crate::view::text(&buf));
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix two"), "{shown}");
    assert!(!shown.contains("fix one"), "{shown}");
}

#[test]
fn an_entry_one_row_short_of_the_room_keeps_its_neighbour() {
    // Thirteen rows leave three entry rows. The selected tall entry
    // takes two, one short of the room, so the next match fills the
    // last row exactly.
    let tall = format!("fix {}", "x".repeat(65));
    let app = searching(80, 13, &["fix small", &tall], "fix");
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 80, 13);
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 2, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix"), "{shown}");
    assert!(shown.contains("fix small"), "{shown}");
}

#[test]
fn down_past_the_end_holds_the_selection() {
    let mut app = searching(80, 24, &["fix one", "fix two"], "fix");
    for _ in 0..5 {
        press(&mut app, Key::Down);
    }
    assert_eq!(panel(&app).selected, Some(2));
    press(&mut app, Key::Up);
    assert_eq!(panel(&app).selected, Some(1));
}

#[test]
fn the_window_fills_its_rows_down_from_the_first_match() {
    // Twelve rows leave two entry rows. The first match takes both
    // rows exactly, so the second match shows beneath it.
    let app = searching(80, 12, &["fix one", "fix two", "fix three"], "fix");
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 80, 12);
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix three"), "{shown}");
    assert!(shown.contains("fix two"), "{shown}");
}

#[test]
fn the_window_fills_its_rows_up_to_a_late_selection() {
    // The late selection and the match above it fill the two entry
    // rows exactly, so the match above shows.
    let prompts: Vec<String> = (1..=8).map(|n| format!("fix {n}")).collect();
    let refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
    let mut app = searching(80, 12, &refs, "fix");
    for _ in 0..7 {
        press(&mut app, Key::Down);
    }
    let completions = panel(&app);
    let (buf, _, _) = drawn(&completions, 80, 12);
    let shown = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, 80, bars[0]).contains("fix 1"), "{shown}");
    assert!(shown.contains("fix 2"), "{shown}");
}
