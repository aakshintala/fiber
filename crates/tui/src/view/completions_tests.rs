//! Tests for the `/` and `@` completion panels' overlay drawing:
//! snapshots, cell colours and input (`docs/tui.md`, "Rules", "Look",
//! "Overlays").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::path::PathBuf;

use super::{cut_left, dot_cut, match_in, range_text};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::theme::Role;
use contract::clock::Clock;

/// An app at `width` by `height`, connected to the hub and attached to
/// a session.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// Types `text` into the draft.
fn type_text(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        let _ = app.on_key(Key::Char(ch), now);
    }
}

/// One session envelope.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Answers the `commands` a `reloaded` asks for with skills named
/// `names`, so the `/` list holds them after the built-ins.
fn answer_commands(app: &mut App, names: &[&str]) {
    let asked = app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "reloaded",
        serde_json::json!({"servers": {"kept": [], "restarted": [], "started": [],
            "extensions": []}}),
    ));
    assert_eq!(asked.len(), 1);
    let asked: serde_json::Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    let rows: Vec<serde_json::Value> = names
        .iter()
        .map(|name| serde_json::json!({"name": name, "description": format!("Runs {name}."), "tag": "skill"}))
        .collect();
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_accepted",
        serde_json::json!({"command_id": asked["id"], "result": {"commands": rows}}),
    ));
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

/// The rows with a cell on the selection bar: the bar covers the whole
/// row in `accent`. The fixtures draw nothing else on `accent`.
fn barred(buf: &Buffer) -> Vec<u16> {
    let area = buf.area;
    (area.top()..area.bottom())
        .filter(|y| (area.left()..area.right()).any(|x| buf[(x, *y)].bg == Role::Accent.color()))
        .collect()
}

/// The row's text, trailing spaces kept.
fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

/// An app with a 40-row `/` list: the built-ins and 17 session skills.
fn forty() -> App {
    let mut app = attached(160, 48);
    let skills: Vec<String> = (0..17).map(|n| format!("skill{n:02}")).collect();
    let refs: Vec<&str> = skills.iter().map(String::as_str).collect();
    answer_commands(&mut app, &refs);
    app
}

#[test]
fn slash_eight_of_forty() {
    let mut app = forty();
    type_text(&mut app, "/");
    insta::assert_snapshot!("slash", screen(&app, 160, 48));
    // Eight entries and the range line, with the first match focused.
    let shown = screen(&app, 160, 48);
    assert!(shown.contains("1–8 of 40 · ↓ 32 more"), "{shown}");
    let buf = buffer(&app, 160, 48);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, bars[0]).contains("home"), "{shown}");
}

#[test]
fn a_scrolled_slash_list_counts_both_directions() {
    let mut app = forty();
    type_text(&mut app, "/");
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..10 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let shown = screen(&app, 160, 48);
    assert!(
        shown.contains("↑ 3 above · 4–11 of 40 · ↓ 29 more"),
        "{shown}"
    );
    let buf = buffer(&app, 160, 48);
    assert_eq!(barred(&buf).len(), 1, "{shown}");
}

#[test]
fn slash_filtered() {
    let mut app = attached(160, 48);
    type_text(&mut app, "/re");
    insta::assert_snapshot!("slash-filtered", screen(&app, 160, 48));
    // The whole list fits, so no range line shows.
    let shown = screen(&app, 160, 48);
    assert!(!shown.contains(" of "), "{shown}");
    // The query's letters read bold in an unfocused row's name, past
    // the name the bold ends.
    let buf = buffer(&app, 160, 48);
    let reload = (0..48)
        .find(|y| row_text(&buf, *y).contains("reload"))
        .expect("the reload row");
    let row = row_text(&buf, reload);
    let at = row.find("reload").expect("the name");
    for (n, ch) in ["r", "e"].iter().enumerate() {
        let x = u16::try_from(at + n).unwrap_or(u16::MAX);
        assert_eq!(buf[(x, reload)].symbol(), *ch);
        assert!(
            buf[(x, reload)].modifier.contains(Modifier::BOLD),
            "the matched {ch} at ({x}, {reload})"
        );
    }
    let plain = u16::try_from(at + 2).unwrap_or(u16::MAX);
    assert!(!buf[(plain, reload)].modifier.contains(Modifier::BOLD));
}

#[test]
fn slash_hint() {
    let mut app = attached(160, 48);
    type_text(&mut app, "/h");
    insta::assert_snapshot!("slash-hint", screen(&app, 160, 48));
    let buf = buffer(&app, 160, 48);
    // The handoff row is past the focused one, so its hint keeps its
    // own colour: the argument hint in `attention`, the tag dim.
    let row = (0..48)
        .find(|y| row_text(&buf, *y).contains("handoff"))
        .expect("the handoff row");
    let text = row_text(&buf, row);
    let hint = text.find("[instructions]").expect("the hint");
    for n in 0.."[instructions]".len() {
        let x = u16::try_from(hint + n).unwrap_or(u16::MAX);
        assert_eq!(
            buf[(x, row)].style().fg,
            Some(Role::Attention.color()),
            "cell {x}"
        );
    }
    let tag = text.find("command").expect("the tag");
    assert!(
        buf[(u16::try_from(tag).unwrap_or(u16::MAX), row)]
            .modifier
            .contains(Modifier::DIM)
    );
}

#[test]
fn the_slash_row_names_share_one_column_and_the_bar_is_black_on_accent() {
    let mut app = attached(160, 48);
    type_text(&mut app, "/h");
    let buf = buffer(&app, 160, 48);
    let shown = screen(&app, 160, 48);
    // The names start in one column on every row.
    let mut starts = Vec::new();
    for name in ["home", "handoff", "help", "thinking"] {
        let row = (0..48)
            .map(|y| row_text(&buf, y))
            .find(|row| row.contains(name))
            .unwrap_or_else(|| panic!("the {name} row\n{shown}"));
        // Cell columns, not byte indices: the focused row's gutter
        // holds a three-byte `›` in one cell.
        let at = row.find(name).expect("the name");
        starts.push(row[..at].chars().count());
    }
    assert!(starts.windows(2).all(|pair| pair[0] == pair[1]), "{shown}");
    // The focused row sits on the bar in `accent` with black text, bold
    // throughout, across the overlay's width.
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    let y = bars[0];
    assert!(row_text(&buf, y).contains("home"), "{shown}");
    let across: Vec<u16> = (0..160)
        .filter(|x| buf[(*x, y)].bg == Role::Accent.color())
        .collect();
    assert!(across.len() >= 40, "{shown}");
    assert!(
        across.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{shown}"
    );
    for x in across {
        assert_eq!(
            buf[(x, y)].style().fg,
            Some(crate::look::BAR_TEXT),
            "cell {x}"
        );
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD), "cell {x}");
    }
}

#[test]
fn at() {
    let mut app = attached(160, 48);
    type_text(&mut app, "look at @ma");
    let found = ["src/main.rs", "crates/tui/src/main.rs"].map(str::to_owned);
    app.on_files(app.generation(), Ok(found.to_vec()));
    insta::assert_snapshot!("at", screen(&app, 160, 48));
    let buf = buffer(&app, 160, 48);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1);
    assert!(row_text(&buf, bars[0]).contains("src/main.rs"));
}

#[test]
fn at_empty() {
    let mut app = attached(160, 48);
    type_text(&mut app, "@zzz");
    app.on_files(app.generation(), Ok(Vec::new()));
    insta::assert_snapshot!("at-empty", screen(&app, 160, 48));
    // One dim row, with no bar anywhere on screen.
    let shown = screen(&app, 160, 48);
    assert!(shown.contains("no files match"), "{shown}");
    assert!(barred(&buffer(&app, 160, 48)).is_empty(), "{shown}");
}

#[test]
fn an_empty_slash_list_bars_nothing() {
    let mut app = attached(80, 24);
    type_text(&mut app, "/zzz");
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("no matches"), "{shown}");
    assert!(barred(&buffer(&app, 80, 24)).is_empty(), "{shown}");
}

#[test]
fn a_file_error_is_one_dim_row_with_no_bar() {
    let mut app = attached(80, 24);
    type_text(&mut app, "@ma");
    app.on_files(app.generation(), Err("gone".to_owned()));
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("No files: gone"), "{shown}");
    assert!(barred(&buffer(&app, 80, 24)).is_empty(), "{shown}");
}

#[test]
fn narrow_slash() {
    let mut app = forty();
    app.set_size(100, 40);
    type_text(&mut app, "/");
    insta::assert_snapshot!("narrow-slash", screen(&app, 100, 40));
    assert!(screen(&app, 100, 40).contains("1–8 of 40 · ↓ 32 more"));
}

#[test]
fn narrow_at_cuts_from_the_left_so_the_file_name_stays() {
    let mut app = attached(100, 40);
    type_text(&mut app, "@repo");
    let long = format!("crates/{}/name.rs", "x".repeat(100));
    app.on_files(app.generation(), Ok(vec![long]));
    insta::assert_snapshot!("narrow-at", screen(&app, 100, 40));
    let buf = buffer(&app, 100, 40);
    let row = (0..40)
        .map(|y| row_text(&buf, y))
        .find(|row| row.contains("name.rs"))
        .expect("the cut row");
    assert!(row.contains('…'), "{row}");
    assert!(row.trim_end().ends_with("name.rs"), "{row}");
}

#[test]
fn multibyte_paths_keep_their_match_bold_after_the_cut() {
    let mut app = attached(80, 24);
    type_text(&mut app, "@s");
    app.on_files(
        app.generation(),
        Ok(vec![
            "\u{65e5}\u{672c}\u{8a9e}/notes.rs".to_owned(),
            "src/main.rs".to_owned(),
        ]),
    );
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    // The CJK row draws whole under the bar: its symbols all show.
    assert!(shown.contains('\u{65e5}'), "{shown}");
    assert!(shown.contains("notes.rs"), "{shown}");
    // The second row is past the bar, so its match keeps its own
    // colours: the "s" of "main.rs" bold in `accent`, past the name
    // the bold ends. The row is ASCII, so bytes are columns.
    let row = (0..24)
        .find(|y| row_text(&buf, *y).contains("main.rs"))
        .expect("the path row");
    let text = row_text(&buf, row);
    let at = text.find("main.rs").expect("the name") + 6;
    let x = u16::try_from(at).unwrap_or(u16::MAX);
    assert_eq!(buf[(x, row)].symbol(), "s");
    assert!(buf[(x, row)].modifier.contains(Modifier::BOLD), "cell {x}");
    assert_eq!(
        buf[(x, row)].style().fg,
        Some(Role::Accent.color()),
        "cell {x}"
    );
    let plain = x.saturating_add(1);
    assert!(!buf[(plain, row)].modifier.contains(Modifier::BOLD));
}

#[test]
fn path_spans_bold_only_the_shown_part_of_a_match() {
    use super::path_spans;
    use std::ops::Range;
    let accent = ratatui::style::Style::new().fg(Role::Accent.color());
    let bold = accent.add_modifier(Modifier::BOLD);
    // The whole path shows: the match reads bold between accent ends.
    let spans = path_spans(
        "caf\u{e9}/na\u{ef}ve.rs",
        0,
        Some(Range { start: 6, end: 10 }),
        14,
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.clone())
            .collect::<Vec<_>>(),
        ["caf\u{e9}/", "na\u{ef}", "ve.rs"]
    );
    assert_eq!(spans[0].style, accent);
    assert_eq!(spans[1].style, bold);
    assert_eq!(spans[2].style, accent);
    // Cut inside the match: only the shown part reads bold.
    let spans = path_spans("\u{ef}ve.rs", 7, Some(Range { start: 6, end: 10 }), 14);
    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.clone())
            .collect::<Vec<_>>(),
        ["", "\u{ef}v", "e.rs"]
    );
    assert_eq!(spans[1].style, bold);
    // No overlap: one accent span.
    let spans = path_spans("caf\u{e9}/", 0, Some(Range { start: 6, end: 10 }), 5);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style, accent);
    // No match: one accent span.
    let spans = path_spans("src/main.rs", 0, None, 12);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style, accent);
}

#[test]
fn the_panel_draws_over_the_conversation() {
    let mut app = attached(80, 24);
    for n in 1..=6 {
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "turn_started",
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {n}")}]}]}),
        ));
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "turn_completed",
            serde_json::json!({"outcome": "completed"}),
        ));
    }
    type_text(&mut app, "/");
    // The panel's cells win over the conversation's: the barred row is
    // a `/` entry, not a transcript row.
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, bars[0]).contains("home"), "{shown}");
}

#[test]
fn range_text_counts_bindings_on_both_sides() {
    // The line shows only while the list does not fit.
    assert_eq!(range_text(0, 8, 8), None);
    assert_eq!(range_text(0, 0, 0), None);
    // Neither end hidden: no counts, only where the rows sit.
    assert_eq!(
        range_text(0, 8, 40),
        Some("1–8 of 40 · ↓ 32 more".to_owned())
    );
    // The end shown: no "more" behind it.
    assert_eq!(range_text(0, 8, 9), Some("1–8 of 9 · ↓ 1 more".to_owned()));
    assert_eq!(range_text(1, 8, 9), Some("↑ 1 above · 2–9 of 9".to_owned()));
    // Scrolled: what hides above leads.
    assert_eq!(
        range_text(3, 8, 40),
        Some("↑ 3 above · 4–11 of 40 · ↓ 29 more".to_owned())
    );
    // Fewer drawn than matched, however many fit: the counts follow
    // the entries actually drawn.
    assert_eq!(range_text(0, 7, 8), Some("1–7 of 8 · ↓ 1 more".to_owned()));
    assert_eq!(
        range_text(3, 3, 23),
        Some("↑ 3 above · 4–6 of 23 · ↓ 17 more".to_owned())
    );
}

#[test]
fn dot_cut_keeps_fitting_text_and_marks_a_cut() {
    assert_eq!(dot_cut("abcdef", 6), "abcdef");
    assert_eq!(dot_cut("abcdef", 7), "abcdef");
    assert_eq!(dot_cut("abcdefg", 6), "abcde…");
    assert_eq!(dot_cut("abcdefg", 1), "…");
    assert_eq!(dot_cut("abcdefg", 0), "");
    assert_eq!(dot_cut("caf\u{e9}x", 5), "caf\u{e9}x");
    assert_eq!(dot_cut("caf\u{e9}xy", 4), "caf…");
}

#[test]
fn cut_left_keeps_the_file_name() {
    // A fitting path keeps every cell and its start.
    assert_eq!(cut_left("a/b.rs", 24), ("a/b.rs".to_owned(), 0));
    assert_eq!(cut_left("ab/cd", 5), ("ab/cd".to_owned(), 0));
    // Past the width, the cut starts at the first `/` whose remainder
    // fits, with the ellipsis drawn in front of it.
    assert_eq!(
        cut_left("crates/doors/tests/serve_attach.rs", 24),
        ("/tests/serve_attach.rs".to_owned(), 12)
    );
    // With no fitting remainder, the last cells stay, two cells wide
    // or one.
    assert_eq!(cut_left("ab/cdef", 5), ("cdef".to_owned(), 3));
    assert_eq!(cut_left("abcdef", 5), ("cdef".to_owned(), 2));
    assert_eq!(
        cut_left("\u{65e5}\u{672c}\u{8a9e}/notes.rs", 8),
        ("otes.rs".to_owned(), 11)
    );
}

#[test]
fn match_in_prefers_the_file_name_then_the_path() {
    use std::ops::Range;
    // The name holds the query: the occurrence in the name.
    assert_eq!(
        match_in("src/main.rs", "main"),
        Some(Range { start: 4, end: 8 })
    );
    assert_eq!(
        match_in("src/main.rs", "MAIN"),
        Some(Range { start: 4, end: 8 })
    );
    // The name holds none of it: the first in the path.
    assert_eq!(match_in("src/main.rs", "sr"), Some(0..2));
    assert_eq!(match_in("src/main.rs", "zzz"), None);
    assert_eq!(match_in("src/main.rs", ""), None);
}

/// Eight windowed `/` rows named `n0` to `n7`.
fn windowed() -> Vec<crate::slash::Row> {
    (0..8)
        .map(|n| crate::slash::Row {
            name: format!("n{n}"),
            description: format!("Does n{n}."),
            hint: None,
            tag: "skill".to_owned(),
        })
        .collect()
}

/// A `/` panel of `total` matches with the first eight windowed, the
/// selection on `selected`.
fn panel(total: usize, selected: usize) -> crate::completion_rows::Completions {
    crate::completion_rows::Completions {
        selected: Some(selected),
        rows: crate::completion_rows::Rows::Slash(windowed()),
        start: 0,
        total,
        query: String::new(),
    }
}

/// The body rows as text with whether each sits on the bar.
fn texts(rows: &[super::Row]) -> Vec<(String, bool)> {
    rows.iter()
        .map(|row| {
            let mut text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
            for span in &row.right {
                text.push_str(span.content.as_ref());
            }
            (text, row.barred)
        })
        .collect()
}

/// The fit window keeps the selection visible with exactly one bar,
/// and the range counts the entries actually drawn.
#[test]
fn fit_windows_around_the_selection_with_exactly_one_bar() {
    let bars = |drawn: &[(String, bool)]| drawn.iter().filter(|(_, barred)| *barred).count();
    // Nine body rows hold eight entries and the range.
    let drawn = texts(&super::body(&panel(40, 5), 60, 9));
    assert_eq!(drawn.len(), 9);
    assert_eq!(bars(&drawn), 1, "{drawn:?}");
    assert!(drawn[5].0.contains("n5") && drawn[5].1, "{drawn:?}");
    assert!(drawn[8].0.contains("1–8 of 40 · ↓ 32 more"), "{drawn:?}");
    // Eight hold seven entries: the range keeps its row.
    let drawn = texts(&super::body(&panel(40, 5), 60, 8));
    assert_eq!(drawn.len(), 8);
    assert_eq!(bars(&drawn), 1, "{drawn:?}");
    assert!(drawn[5].0.contains("n5") && drawn[5].1, "{drawn:?}");
    assert!(drawn[7].0.contains("1–7 of 40 · ↓ 33 more"), "{drawn:?}");
    // Seven hold six, still around the selection.
    let drawn = texts(&super::body(&panel(40, 5), 60, 7));
    assert_eq!(drawn.len(), 7);
    assert_eq!(bars(&drawn), 1, "{drawn:?}");
    assert!(drawn[5].0.contains("n5") && drawn[5].1, "{drawn:?}");
    assert!(drawn[6].0.contains("1–6 of 40 · ↓ 34 more"), "{drawn:?}");
    // Room for one: the selection alone, counted where it sits.
    let drawn = texts(&super::body(&panel(40, 5), 60, 1));
    assert_eq!(drawn.len(), 2);
    assert!(drawn[0].0.contains("n5") && drawn[0].1, "{drawn:?}");
    assert!(
        drawn[1].0.contains("↑ 5 above · 6–6 of 40 · ↓ 34 more"),
        "{drawn:?}"
    );
    // No room: still the selection alone; the frame clips the range.
    let floored = texts(&super::body(&panel(40, 5), 60, 0));
    assert_eq!(floored, drawn);
    // All matches fit: every entry, no range.
    let drawn = texts(&super::body(&panel(8, 0), 60, 8));
    assert_eq!(drawn.len(), 8);
    assert!(drawn[0].1, "{drawn:?}");
    assert!(
        drawn.iter().all(|(text, _)| !text.contains(" of ")),
        "{drawn:?}"
    );
    // One match hides: the range shows over seven entries.
    let drawn = texts(&super::body(&panel(9, 0), 60, 8));
    assert_eq!(drawn.len(), 8);
    assert!(drawn[7].0.contains("1–7 of 9 · ↓ 2 more"), "{drawn:?}");
}

/// A short conversation with a long `/` list: the eleventh match stays
/// barred, counted of the whole list.
#[test]
fn short_conversation_slash_keeps_its_selection_barred() {
    let mut app = forty();
    app.set_size(80, 12);
    type_text(&mut app, "/");
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..10 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let buf = buffer(&app, 80, 12);
    let shown = screen(&app, 80, 12);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, bars[0]).contains("rules"), "{shown}");
    assert!(shown.contains("of 40"), "{shown}");
}

/// A short conversation with an `@` list: the third path stays barred,
/// counted of its list.
#[test]
fn short_at_keeps_its_selection_barred() {
    let mut app = attached(80, 12);
    type_text(&mut app, "@s");
    app.on_files(
        app.generation(),
        Ok(vec![
            "sa.rs".to_owned(),
            "sb.rs".to_owned(),
            "sc.rs".to_owned(),
            "sd.rs".to_owned(),
            "se.rs".to_owned(),
            "sf.rs".to_owned(),
            "sg.rs".to_owned(),
            "sh.rs".to_owned(),
            "si.rs".to_owned(),
            "sj.rs".to_owned(),
        ]),
    );
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..2 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let buf = buffer(&app, 80, 12);
    let shown = screen(&app, 80, 12);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{shown}");
    assert!(row_text(&buf, bars[0]).contains("sc.rs"), "{shown}");
    assert!(shown.contains("of 10"), "{shown}");
}
