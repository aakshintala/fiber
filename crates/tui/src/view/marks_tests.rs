//! The selection's highlight over the conversation, and "Copied" above the
//! notices: snapshots of the text, each with a mask of the highlighted
//! cells under it.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::SELECTION;
use crate::app::{App, Effect};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// A session line of `kind`.
fn session_line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app connected and attached at `width` by `height`, showing a turn
/// with the reply `text`.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, height);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app.on_line(session_line(
        "text_completed",
        json!({ "text": text }),
        Some("a_m"),
    ));
    app
}

/// The screen at the app's size, and the targets drawn.
fn draw(app: &App, width: u16, height: u16) -> (Buffer, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = super::super::render(app, area, &mut buf, None);
    (buf, targets)
}

/// The screen's text, then a rule, then a mask: `#` where the selection's
/// highlight is, `.` elsewhere.
fn shown(app: &App, width: u16, height: u16) -> String {
    let (buf, _) = draw(app, width, height);
    let mut mask = String::new();
    for y in 0..height {
        for x in 0..width {
            let lit = buf
                .cell((x, y))
                .is_some_and(|cell| cell.bg == SELECTION.bg.unwrap_or_default());
            mask.push(if lit { '#' } else { '.' });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", super::super::text(&buf))
}

/// A press at `from`, a drag to `to` and its release.
fn select(app: &mut App, width: u16, height: u16, from: (u16, u16), to: (u16, u16)) -> Effect {
    let report = |app: &mut App, kind: MouseKind, (col, row): (u16, u16)| {
        let (_, targets) = draw(app, width, height);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    report(app, MouseKind::Press(Button::Left), from);
    report(app, MouseKind::Drag(Button::Left), to);
    report(app, MouseKind::Release, to)
}

/// A reply long enough to wrap over several rows at 80 columns.
const LONG: &str = "Selection copies text unwrapped. This paragraph is long enough to \
wrap across more than three rows at eighty columns, so a drag from the middle of \
its first row to the middle of its third highlights the rest of the first, the \
whole second and the start of the third.";

#[test]
fn selection_over_three_rows_80x24() {
    let mut app = replied(80, 24, LONG);
    let text = super::super::text(&draw(&app, 80, 24).0);
    let first = text
        .lines()
        .position(|row| row.starts_with("Selection"))
        .expect("the reply's first row");
    let first = u16::try_from(first).expect("a row");
    assert!(matches!(
        select(&mut app, 80, 24, (10, first), (20, first + 2)),
        Effect::Copy(_)
    ));
    insta::assert_snapshot!("selection_over_three_rows_80x24", shown(&app, 80, 24));
}

#[test]
fn selection_does_not_paint_the_new_below_row() {
    let paragraphs: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(40, 12, &paragraphs.join("\n\n"));
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::PageUp, now);
    app.on_line(session_line(
        "text_completed",
        json!({"text": "more"}),
        Some("a_n"),
    ));
    assert!(app.has_new());
    // From the top row to past the bottom: clamped above the overlay.
    select(&mut app, 40, 12, (0, 0), (39, 30));
    insta::assert_snapshot!(
        "selection_does_not_paint_the_new_below_row",
        shown(&app, 40, 12)
    );
}

#[test]
fn copied_sits_above_the_notices() {
    let mut app = replied(60, 10, "copy this line");
    app.on_line(session_line(
        "notice",
        json!({"code": "extension_failed", "message": "a notice"}),
        None,
    ));
    let text = super::super::text(&draw(&app, 60, 10).0);
    let row = text
        .lines()
        .position(|row| row.contains("copy this"))
        .expect("the reply's row");
    let row = u16::try_from(row).expect("a row");
    assert!(matches!(
        select(&mut app, 60, 10, (0, row), (3, row)),
        Effect::Copy(_)
    ));
    assert!(app.copied());
    insta::assert_snapshot!("copied_sits_above_the_notices", shown(&app, 60, 10));
}

/// Under `NO_COLOR` the selection and the matches keep distinct marks:
/// the role colours paint to the default, the modifiers stay.
#[test]
fn no_color_keeps_selection_and_matches_distinct() {
    use super::{CURRENT, MATCH};
    use crate::look::{Look, ThemeSetting};
    use ratatui::style::{Color, Modifier};

    let mut app = replied(40, 12, "alpha beta needle one needle two gamma");
    let text = super::super::text(&draw(&app, 40, 12).0);
    let row = u16::try_from(
        text.lines()
            .position(|row| row.contains("alpha"))
            .expect("the reply's row"),
    )
    .expect("a row");
    let col = u16::try_from(
        text.lines()
            .find_map(|row| row.find("alpha"))
            .expect("alpha"),
    )
    .expect("a column");
    assert!(matches!(
        select(&mut app, 40, 12, (col, row), (col + 4, row)),
        Effect::Copy(_)
    ));
    search(&mut app, "needle");
    let (mut buf, _) = draw(&app, 40, 12);
    // The cells each mark carries, by its role marker before the paint.
    let at = |style: ratatui::style::Style| {
        let marked = style.bg.unwrap_or_default();
        (0..40).flat_map(|x| (0..12).map(move |y| (x, y))).find(|at| {
            buf.cell(*at)
                .is_some_and(|cell| cell.bg == marked)
        })
    };
    let (selected, matched, current) = (
        at(SELECTION).expect("a selected cell"),
        at(MATCH).expect("a matched cell"),
        at(CURRENT).expect("a current cell"),
    );
    let vars: Vec<(String, String)> = vec![("NO_COLOR".to_owned(), "1".to_owned())];
    let (look, notice) = Look::new(ThemeSetting::Dark, &|name: &str| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    assert_eq!(notice, None);
    look.paint(&mut buf);
    let cell = |at| buf.cell(at).expect("in the area").clone();
    for at in [selected, matched, current] {
        assert_eq!((cell(at).fg, cell(at).bg), (Color::Reset, Color::Reset));
    }
    assert_eq!(cell(selected).modifier, Modifier::REVERSED);
    assert_eq!(cell(matched).modifier, Modifier::UNDERLINED);
    assert_eq!(
        cell(current).modifier,
        Modifier::REVERSED | Modifier::BOLD
    );
    assert!(
        cell(selected).modifier != cell(matched).modifier
            && cell(matched).modifier != cell(current).modifier
            && cell(selected).modifier != cell(current).modifier,
        "the marks stay apart with no colour"
    );
}

/// The screen with the pointer over the link, then a rule, then a mask:
/// `#` where the hover tint is, `.` elsewhere.
fn hover_shown(app: &App, width: u16, height: u16, at: (u16, u16)) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    super::super::render(app, area, &mut buf, Some(at));
    let mut mask = String::new();
    let tint = super::super::HOVER_TINT.bg.unwrap_or_default();
    for y in 0..height {
        for x in 0..width {
            let lit = buf.cell((x, y)).is_some_and(|cell| cell.bg == tint);
            mask.push(if lit { '#' } else { '.' });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", super::super::text(&buf))
}

#[test]
fn hover_tints_a_link() {
    let mut app = replied(60, 10, "[docs](https://example.com/a) tail");
    app.set_opener(true);
    let (buf, targets) = draw(&app, 60, 10);
    let link = targets
        .iter()
        .find(|target| matches!(target.id, crate::mouse::TargetId::Link { .. }))
        .expect("a link target");
    let at = (link.rect.x, link.rect.y);
    // The link's cells draw underlined.
    let underlined = buf
        .cell(at)
        .is_some_and(|cell| cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
    assert!(underlined, "the link underlines");
    insta::assert_snapshot!("hover_tints_a_link", hover_shown(&app, 60, 10, at));
}

/// The screen's text, then a mask: `m` where a match is marked, `c` where
/// the current match is brighter, `.` elsewhere.
fn marked(app: &App, width: u16, height: u16) -> String {
    use super::{CURRENT, MATCH};
    let (buf, _) = draw(app, width, height);
    let current = CURRENT.bg.unwrap_or_default();
    let matched = MATCH.bg.unwrap_or_default();
    let mut mask = String::new();
    for y in 0..height {
        for x in 0..width {
            let bg = buf.cell((x, y)).map(|cell| cell.bg).unwrap_or_default();
            mask.push(if bg == current {
                'c'
            } else if bg == matched {
                'm'
            } else {
                '.'
            });
        }
        mask.push('\n');
    }
    format!("{}\n---\n{mask}", super::super::text(&buf))
}

/// Opens the bar, types `query` and starts the scan: resident pages only.
fn search(app: &mut App, query: &str) {
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlF, now), Effect::None);
    let mut generation = 0u64;
    for ch in query.chars() {
        generation += 1;
        assert!(
            matches!(app.on_key(Key::Char(ch), now), Effect::FindPause { .. }),
            "typing {ch:?}"
        );
    }
    assert!(app.find_due(generation).is_empty());
}

#[test]
fn marks_on_a_match_wrapped_over_two_rows() {
    let mut app = replied(40, 12, &format!("{} brown fox jumps", "x".repeat(33)));
    search(&mut app, "brown fox");
    insta::assert_snapshot!(
        "marks_on_a_match_wrapped_over_two_rows",
        marked(&app, 40, 12)
    );
}

#[test]
fn the_current_match_is_brighter() {
    let mut app = replied(40, 12, "needle one\n\nneedle two");
    search(&mut app, "needle");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    insta::assert_snapshot!("the_current_match_is_brighter", marked(&app, 40, 12));
}

#[test]
fn the_bar_cursor_sits_after_the_query_on_the_bar_row() {
    let mut app = replied(40, 12, "needle");
    search(&mut app, "ab");
    let (buf, _) = draw(&app, 40, 12);
    let row_text = |y: u16| -> String {
        (0..40u16)
            .filter_map(|x| buf.cell((x, y)).map(|cell| cell.symbol().to_owned()))
            .collect()
    };
    // The bar's text as drawn: the cursor sits just past its query.
    let (y, col) = (0..12u16)
        .find_map(|y| {
            row_text(y)
                .find("find: ab")
                .map(|byte| (y, row_text(y)[..byte].chars().count()))
        })
        .expect("the bar is drawn");
    let x = u16::try_from(col + "find: ab".len()).expect("a column");
    assert_eq!(
        crate::view::cursor(&app, Rect::new(0, 0, 40, 12)),
        Some(ratatui::layout::Position::new(x, y))
    );
}
