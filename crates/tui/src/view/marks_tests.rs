//! The selection's highlight over the conversation, and "Copied" above the
//! notices: snapshots of the text, each with a mask of the highlighted
//! cells under it.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::SELECTION;
use crate::app::{App, Effect, FindBar};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::theme::Role;

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
        .position(|row| row.trim_start().starts_with("Selection"))
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
fn copied_above_the_notice_rows() {
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
    // With the bar closed "Copied" sits on the top row, in `info` on
    // the `surface` tint.
    let (buf, _) = draw(&app, 60, 10);
    let screen = super::super::text(&buf);
    let at = screen
        .lines()
        .position(|line| line.contains("Copied"))
        .expect("Copied shows");
    assert_eq!(at, 0);
    let col = u16::try_from(
        screen
            .lines()
            .nth(at)
            .map(|line| {
                line[..line.find("Copied").expect("its column")]
                    .chars()
                    .count()
            })
            .expect("its column")
            .saturating_sub(1),
    )
    .expect("a column");
    for dx in 0..8 {
        let cell = &buf[(col + dx, 0)];
        assert_eq!(cell.fg, Role::Info.color(), "Copied {dx}");
        assert_eq!(cell.bg, Role::Surface.color(), "Copied {dx}");
    }
    insta::assert_snapshot!("copied_above_the_notice_rows", shown(&app, 60, 10));
}

/// Under `NO_COLOR` the selection and the matches keep distinct marks:
/// the role colours paint to the default, the modifiers stay. The
/// process-wide box switch is set for this test alone: under nextest
/// each test runs in its own process, and the guard restores colour
/// afterwards.
#[test]
fn no_color_keeps_selection_and_matches_distinct() {
    use super::{CURRENT_NO_COLOR, MATCH_NO_COLOR, SELECTION_NO_COLOR};
    use crate::look::{Look, ThemeSetting};
    use ratatui::style::{Color, Modifier};

    struct Unboxed;
    impl Drop for Unboxed {
        fn drop(&mut self) {
            crate::surface::init(&|_: &str| None);
        }
    }
    let _guard = Unboxed;
    crate::surface::init(&|name: &str| {
        if name == "NO_COLOR" {
            Some("1".to_owned())
        } else {
            None
        }
    });
    assert!(crate::surface::boxed(), "the box switch is set");
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
        (0..40)
            .flat_map(|x| (0..12).map(move |y| (x, y)))
            .find(|at| buf.cell(*at).is_some_and(|cell| cell.bg == marked))
    };
    let (selected, matched, current) = (
        at(SELECTION_NO_COLOR).expect("a selected cell"),
        at(MATCH_NO_COLOR).expect("a matched cell"),
        at(CURRENT_NO_COLOR).expect("a current cell"),
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
    assert_eq!(cell(current).modifier, Modifier::REVERSED | Modifier::BOLD);
    assert!(
        cell(selected).modifier != cell(matched).modifier
            && cell(matched).modifier != cell(current).modifier
            && cell(selected).modifier != cell(current).modifier,
        "the marks stay apart with no colour"
    );
}

/// The screen with the pointer over the link, then a rule, then a mask:
/// `#` where the lifted link tint is, `.` elsewhere.
fn hover_shown(app: &App, width: u16, height: u16, at: (u16, u16)) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    super::super::render(app, area, &mut buf, Some(at));
    let mut mask = String::new();
    let tint = crate::theme::hover_bg(Role::Turn.color());
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

#[test]
fn a_bare_url_draws_in_accent_and_hover_keeps_it() {
    use crate::theme::Role;
    // A bare URL carries no markdown style of its own: the overlay
    // gives it the link look (`docs/tui.md`, "Links").
    let mut app = replied(60, 10, "see https://example.com/a tail");
    app.set_opener(true);
    let (buf, targets) = draw(&app, 60, 10);
    let link = targets
        .iter()
        .find(|target| matches!(target.id, crate::mouse::TargetId::Link { .. }))
        .expect("a link target");
    let at = (link.rect.x, link.rect.y);
    let cell = buf[at].clone();
    assert_eq!(cell.fg, Role::Accent.color(), "the link's colour");
    assert!(
        cell.modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "the link underlines"
    );
    // Under the pointer the link takes the hover tint and keeps the
    // accent foreground and its underline.
    let area = Rect::new(0, 0, 60, 10);
    let mut hovered = Buffer::empty(area);
    super::super::render(&app, area, &mut hovered, Some(at));
    let cell = hovered[at].clone();
    assert_eq!(
        cell.bg,
        crate::theme::hover_bg(Role::Turn.color()),
        "the hovered link's tint"
    );
    assert_eq!(cell.fg, Role::Accent.color(), "the hovered link's colour");
    assert!(
        cell.modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "the hovered link underlines"
    );
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
fn the_bar_cursor_sits_on_the_block_after_the_query() {
    let mut app = replied(40, 12, "needle");
    search(&mut app, "ab");
    let (buf, _) = draw(&app, 40, 12);
    let row_text = |y: u16| -> String {
        (0..40u16)
            .filter_map(|x| buf.cell((x, y)).map(|cell| cell.symbol().to_owned()))
            .collect()
    };
    // The bar's content row: the block sits just past its query.
    let (y, col) = (0..12u16)
        .find_map(|y| {
            row_text(y)
                .find("⌕ ab")
                .map(|byte| (y, row_text(y)[..byte].chars().count()))
        })
        .expect("the bar is drawn");
    let x = u16::try_from(col + "⌕ ab".chars().count()).expect("a column");
    assert_eq!(buf.cell((x, y)).map(|cell| cell.symbol()), Some("█"));
    assert_eq!(
        crate::view::cursor(&app, Rect::new(0, 0, 40, 12)),
        Some(ratatui::layout::Position::new(x, y))
    );
}

#[test]
fn the_cursor_hides_while_the_results_are_open() {
    let mut app = replied(40, 12, "needle one\n\nneedle two");
    search(&mut app, "needle");
    let now = fakes::clock::FakeClock::new().now();
    // A second Ctrl+F opens the results; the bar is hidden, so its
    // cursor hides too.
    assert_eq!(app.on_key(Key::CtrlF, now), Effect::None);
    assert!(app.results_open(), "the results are open");
    assert_eq!(crate::view::cursor(&app, Rect::new(0, 0, 40, 12)), None);
    // Esc closes the view; the cursor sits on the bar's block again.
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert!(!app.results_open());
    let at = crate::view::cursor(&app, Rect::new(0, 0, 40, 12)).expect("the bar cursor");
    let (buf, _) = draw(&app, 40, 12);
    assert_eq!(buf[(at.x, at.y)].symbol(), "█");
}

/// A bar with `query` and `count` for the direct draw tests.
fn find_bar(query: &str, count: &str) -> FindBar {
    FindBar {
        query: query.to_owned(),
        count: count.to_owned(),
    }
}

/// `draw_bar` on `area`: the buffer and the targets drawn.
fn bar_into(area: Rect, bar: &FindBar) -> (Buffer, Vec<crate::mouse::Target>) {
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_bar(bar, area, &mut buf, &mut targets);
    (buf, targets)
}

/// The bar's content row in `buf`: the row holding the search glyph.
fn bar_row(buf: &Buffer) -> u16 {
    let area = buf.area;
    (area.top()..area.bottom())
        .find(|y| {
            (area.left()..area.right())
                .any(|x| buf.cell((x, *y)).is_some_and(|cell| cell.symbol() == "⌕"))
        })
        .expect("the bar's content row")
}

/// The `surface` run holding the bar on `y`: the contiguous run around
/// the search glyph's column, so other `surface` cells on the row (a
/// rail card, say) stay out of it.
fn surface_run(buf: &Buffer, y: u16) -> (u16, u16) {
    let area = buf.area;
    let tint = Role::Surface.color();
    let at = (area.left()..area.right())
        .find(|x| buf.cell((*x, y)).is_some_and(|cell| cell.symbol() == "⌕"))
        .expect("the search glyph");
    let mut first = at;
    while first > area.left() && buf.cell((first - 1, y)).is_some_and(|cell| cell.bg == tint) {
        first -= 1;
    }
    let mut last = at;
    while last + 1 < area.right() && buf.cell((last + 1, y)).is_some_and(|cell| cell.bg == tint) {
        last += 1;
    }
    (first, last)
}

#[test]
fn the_bar_rect_pins_the_widths_and_the_floors() {
    use super::bar_rect;
    // An empty area draws nothing.
    assert_eq!(bar_rect(Rect::new(0, 0, 0, 0)), None);
    assert_eq!(bar_rect(Rect::new(0, 0, 40, 0)), None);
    // Under 6 columns the bar draws nothing; at 6 it is 5 wide.
    assert_eq!(bar_rect(Rect::new(0, 0, 5, 10)), None);
    assert_eq!(
        bar_rect(Rect::new(0, 0, 6, 10)),
        Some(Rect::new(0, 1, 5, 3))
    );
    // The bar caps at 40: 39, 40 and 40 wide at areas 40, 41 and 42.
    assert_eq!(
        bar_rect(Rect::new(0, 0, 40, 10)).map(|rect| rect.width),
        Some(39)
    );
    assert_eq!(
        bar_rect(Rect::new(0, 0, 41, 10)).map(|rect| rect.width),
        Some(40)
    );
    assert_eq!(
        bar_rect(Rect::new(0, 0, 42, 10)).map(|rect| rect.width),
        Some(40)
    );
    // The right edge sits one column left of the area's right edge.
    assert_eq!(
        bar_rect(Rect::new(10, 0, 41, 10)),
        Some(Rect::new(10, 1, 40, 3))
    );
    // Five rows take the slab's three content rows; four take the bare
    // content row, on the area's first row.
    assert_eq!(
        bar_rect(Rect::new(0, 0, 80, 5)),
        Some(Rect::new(39, 1, 40, 3))
    );
    assert_eq!(
        bar_rect(Rect::new(0, 0, 80, 4)),
        Some(Rect::new(39, 0, 40, 1))
    );
    assert_eq!(
        bar_rect(Rect::new(0, 0, 80, 1)),
        Some(Rect::new(39, 0, 40, 1))
    );
}

#[test]
fn the_bar_rect_pins_the_no_color_floor() {
    use super::bar_rect;
    struct Unboxed;
    impl Drop for Unboxed {
        fn drop(&mut self) {
            crate::surface::init(&|_: &str| None);
        }
    }
    let _guard = Unboxed;
    crate::surface::init(&|name: &str| {
        if name == "NO_COLOR" {
            Some("1".to_owned())
        } else {
            None
        }
    });
    // The box's two sides take two columns, so the floor is one higher.
    assert_eq!(bar_rect(Rect::new(0, 0, 6, 10)), None);
    assert_eq!(
        bar_rect(Rect::new(0, 0, 7, 10)),
        Some(Rect::new(0, 1, 6, 3))
    );
}

#[test]
fn the_bar_is_a_forty_column_slab_at_width_100() {
    let mut app = replied(100, 24, "needle in the reply here");
    search(&mut app, "ab");
    let (buf, _) = draw(&app, 100, 24);
    let y = bar_row(&buf);
    let (first, last) = surface_run(&buf, y);
    assert_eq!(last + 1 - first, 40, "the bar is 40 columns wide");
    // The slab ends one column left of the scroll bar's column: the
    // cell past its right edge keeps the default background.
    let past = buf.cell((last + 1, y)).expect("the cell past the slab");
    assert_eq!(past.symbol(), " ");
    assert_eq!(past.bg, ratatui::style::Color::Reset);
}

#[test]
fn the_bar_draws_its_prefix_query_block_and_count() {
    let bar = find_bar("ab", "1 of 1");
    let area = Rect::new(0, 0, 100, 10);
    let (buf, targets) = bar_into(area, &bar);
    let y = bar_row(&buf);
    assert_eq!(y, 2, "the text row sits between the blank rows");
    let (first, last) = surface_run(&buf, y);
    assert_eq!((first, last), (59, 98));
    let cell = |x: u16| buf.cell((x, y)).expect("in the bar").clone();
    // " ⌕ " in `attention`.
    for x in first..first + 3 {
        assert_eq!(cell(x).fg, Role::Attention.color(), "prefix {x}");
    }
    assert_eq!(cell(first + 1).symbol(), "⌕");
    // The query bold.
    for x in first + 3..first + 5 {
        assert!(
            cell(x).modifier.contains(ratatui::style::Modifier::BOLD),
            "query {x}"
        );
    }
    // The block in the text colour, with no dim.
    assert_eq!(cell(first + 5).symbol(), "█");
    assert_eq!(cell(first + 5).fg, Role::Text.color());
    assert!(
        !cell(first + 5)
            .modifier
            .contains(ratatui::style::Modifier::DIM)
    );
    // The count dim and right-aligned with one trailing blank.
    let text: String = (first..=last)
        .map(|x| cell(x).symbol().to_owned())
        .collect();
    assert!(text.ends_with("1 of 1 "), "{text:?}");
    let at = text[..text.find("1 of 1 ").expect("the count")]
        .chars()
        .count();
    for dx in 0..6 {
        let x = first + u16::try_from(at + dx).expect("a column");
        assert!(
            cell(x).modifier.contains(ratatui::style::Modifier::DIM),
            "count {x}"
        );
    }
    // Five rows: the edges above and below, one blank row inside each.
    for (row, want) in [(0, "▄"), (4, "▀")] {
        for x in first..=last {
            let edge = buf.cell((x, row)).expect("on the edge");
            assert_eq!(edge.symbol(), want, "({x}, {row})");
            assert_eq!(edge.fg, Role::Surface.color(), "({x}, {row})");
        }
    }
    for row in [y - 1, y + 1] {
        for x in first..=last {
            let blank = buf.cell((x, row)).expect("in the slab");
            assert_eq!(blank.symbol(), " ", "({x}, {row})");
            assert_eq!(blank.bg, Role::Surface.color(), "({x}, {row})");
        }
    }
    // The count's cells are the click target; nothing else is.
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, crate::mouse::TargetId::FindCount);
    assert_eq!(
        targets[0].rect,
        Rect::new(last - 6, y, 6, 1),
        "the count's cells"
    );
}

#[test]
fn a_count_cut_to_nothing_is_no_target_and_one_cell_is() {
    // At 41 columns the bar is 40 wide and the query keeps 34 cells:
    // 34 leaves no room for the count, 33 leaves one cell.
    let area = Rect::new(0, 0, 41, 10);
    let (buf, targets) = bar_into(area, &find_bar(&"z".repeat(34), "no matches"));
    assert!(bar_row(&buf) < 10, "the bar itself still draws");
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::FindCount),
        "a count cut to nothing is no target"
    );
    let (buf, targets) = bar_into(area, &find_bar(&"z".repeat(33), "no matches"));
    let y = bar_row(&buf);
    let (first, last) = surface_run(&buf, y);
    assert_eq!((first, last), (0, 39));
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].rect,
        Rect::new(last - 1, y, 1, 1),
        "the one kept cell"
    );
}

#[test]
fn an_overlong_query_cuts_at_the_left_and_keeps_the_block() {
    let area = Rect::new(0, 0, 40, 10);
    let bar = find_bar(&format!("{}tail", "y".repeat(50)), "1 of 1");
    let (buf, _) = bar_into(area, &bar);
    let y = bar_row(&buf);
    let text: String = (0..40).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert!(text.starts_with(" ⌕ yyy"), "{text:?}");
    assert!(
        text.contains("tail█"),
        "the kept end and the block: {text:?}"
    );
    // The cursor sits on the drawn block.
    let cursor = super::bar_cursor(&bar, area).expect("the cursor");
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
}

#[test]
fn a_multibyte_control_cuts_without_splitting_it() {
    // U+0085 is a two-byte control: the grapheme walk filters it, so a
    // cut past it on either side keeps whole graphemes and never
    // splits its bytes.
    let area = Rect::new(0, 0, 41, 10);
    let bar = find_bar(&format!("a\u{85}{}", "z".repeat(33)), "1 of 1");
    let (buf, _) = bar_into(area, &bar);
    let y = bar_row(&buf);
    let text: String = (0..41).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert!(text.starts_with(" ⌕ azzz"), "{text:?}");
    assert!(
        text.contains("zzz█"),
        "the kept end and the block: {text:?}"
    );
    let cursor = super::bar_cursor(&bar, area).expect("the cursor");
    assert_eq!((cursor.x, cursor.y), (37, y));
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
}

#[test]
fn a_wide_query_cuts_on_a_grapheme_boundary() {
    // Sixteen `鎖` (2 cells each) then `tail` is 36 cells: the 34-cell
    // query room drops the first `鎖` whole and keeps `tail`.
    let area = Rect::new(0, 0, 41, 10);
    let bar = find_bar(&format!("{}tail", "鎖".repeat(16)), "1 of 1");
    let (buf, _) = bar_into(area, &bar);
    let y = bar_row(&buf);
    let text: String = (0..41).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert!(
        text.contains("tail█"),
        "the kept end and the block: {text:?}"
    );
    let cursor = super::bar_cursor(&bar, area).expect("the cursor");
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
    assert_eq!((cursor.x, cursor.y), (37, y));
}

#[test]
fn a_fused_pair_measures_as_drawn_not_as_a_string() {
    // `لا` measures 1 cell as a whole string but draws 2: the cursor
    // counts each drawn grapheme, so it sits on the block past both.
    let area = Rect::new(0, 0, 41, 10);
    let bar = find_bar("لا", "1 of 1");
    let (buf, _) = bar_into(area, &bar);
    let y = bar_row(&buf);
    let cursor = super::bar_cursor(&bar, area).expect("the cursor");
    assert_eq!((cursor.x, cursor.y), (5, y));
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
}

#[test]
fn an_empty_query_reads_type_to_search() {
    let mut app = replied(80, 24, "the quick brown fox");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlF, now), Effect::None);
    let (buf, _) = draw(&app, 80, 24);
    let y = bar_row(&buf);
    let (first, last) = surface_run(&buf, y);
    let text: String = (first..=last)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect();
    assert!(text.starts_with(" ⌕ █"), "{text:?}");
    assert!(text.ends_with("type to search "), "{text:?}");
    let at = text[..text.find("type to search").expect("the hint")]
        .chars()
        .count();
    for dx in 0.."type to search".len() {
        let cell = &buf[(first + u16::try_from(at + dx).expect("a column"), y)];
        assert!(
            cell.modifier.contains(ratatui::style::Modifier::DIM),
            "the hint is dim"
        );
    }
}

#[test]
fn the_cursor_sits_on_the_drawn_block() {
    // Every width, height and query: the cursor is the block the draw
    // painted, so cursor and draw cannot disagree.
    let long = "q".repeat(50);
    for width in [6, 7, 39, 40, 41, 42, 80, 100] {
        for height in [1, 2, 4, 5, 6, 10] {
            for query in ["", "ab", long.as_str()] {
                let area = Rect::new(0, 0, width, height);
                let bar = find_bar(query, "1 of 1");
                let (buf, _) = bar_into(area, &bar);
                let cursor = super::bar_cursor(&bar, area).expect("the cursor");
                let at = (cursor.x, cursor.y);
                assert_eq!(buf[at].symbol(), "█", "{width}x{height} {query:?}");
                assert_eq!(buf[at].fg, Role::Text.color(), "{width}x{height} {query:?}");
                assert!(
                    !buf[at].modifier.contains(ratatui::style::Modifier::DIM),
                    "{width}x{height} {query:?}"
                );
            }
        }
    }
}

#[test]
fn a_click_on_the_slab_falls_through() {
    // The slab's blank rows and edges hold no target: only the count's
    // cells do, so a click elsewhere reaches what is under the slab.
    let area = Rect::new(0, 0, 80, 10);
    let (_, targets) = bar_into(area, &find_bar("ab", "1 of 1"));
    assert_eq!(targets.len(), 1);
    let content = targets[0].rect.y;
    for y in [0, 1, 2, 3, 4] {
        if y == content {
            continue;
        }
        for x in 39..79 {
            assert!(
                crate::mouse::hit(&targets, x, y) != Some(crate::mouse::TargetId::FindCount),
                "a click at ({x}, {y}) falls through"
            );
        }
    }
}

#[test]
fn in_colour_the_marks_carry_no_modifier() {
    use super::{CURRENT, MATCH};
    assert!(
        !SELECTION
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    assert_eq!(SELECTION.bg, Some(Role::Selection.color()));
    assert_eq!(
        (MATCH.bg, MATCH.fg),
        (
            Some(Role::Match.color()),
            Some(ratatui::style::Color::Rgb(255, 255, 255))
        )
    );
    assert_eq!(
        (CURRENT.bg, CURRENT.fg),
        (
            Some(Role::MatchCurrent.color()),
            Some(crate::look::BAR_TEXT)
        )
    );
    for style in [SELECTION, MATCH, CURRENT] {
        assert_eq!(
            style.add_modifier,
            ratatui::style::Modifier::empty(),
            "{style:?}"
        );
    }
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
    let (buf, _) = draw(&app, 40, 12);
    let selected = (col, row);
    assert_eq!(buf[selected].bg, Role::Selection.color());
    assert!(
        !buf[selected]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    let current = (0..40)
        .flat_map(|x| (0..12).map(move |y| (x, y)))
        .find(|at| buf[*at].bg == Role::MatchCurrent.color())
        .expect("a current cell");
    assert_eq!(buf[current].fg, ratatui::style::Color::Rgb(0, 0, 0));
    let matched = (0..40)
        .flat_map(|x| (0..12).map(move |y| (x, y)))
        .find(|at| buf[*at].bg == Role::Match.color())
        .expect("a matched cell");
    assert_eq!(buf[matched].fg, ratatui::style::Color::Rgb(255, 255, 255));
}

#[test]
fn the_bar_hides_while_the_results_are_open() {
    use crate::mouse::TargetId;
    let mut app = replied(40, 12, "needle one\n\nneedle two");
    search(&mut app, "needle");
    let now = fakes::clock::FakeClock::new().now();
    // A second Ctrl+F opens the results; the bar draws nothing over
    // them, and its count is no target.
    assert_eq!(app.on_key(Key::CtrlF, now), Effect::None);
    assert!(app.results_open(), "the results are open");
    let (buf, targets) = draw(&app, 40, 12);
    let text = super::super::text(&buf);
    assert!(
        text.lines().next().is_some_and(|row| {
            row.starts_with(" Search  ⌕ needle") && row.ends_with("Esc returns")
        }),
        "the results top row draws without the bar over it: {text:?}"
    );
    assert!(
        targets
            .iter()
            .all(|target| target.id != TargetId::FindCount),
        "no count target over the results"
    );
    // Esc closes the view; the bar draws again with its query and
    // count kept.
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert!(!app.results_open());
    let (buf, targets) = draw(&app, 40, 12);
    let text = super::super::text(&buf);
    assert!(text.contains("⌕ needle"), "the bar draws again: {text:?}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::FindCount),
        "the count target draws again"
    );
}

#[test]
fn the_bar_boxes_with_no_colour() {
    struct Unboxed;
    impl Drop for Unboxed {
        fn drop(&mut self) {
            crate::surface::init(&|_: &str| None);
        }
    }
    let _guard = Unboxed;
    crate::surface::init(&|name: &str| {
        if name == "NO_COLOR" {
            Some("1".to_owned())
        } else {
            None
        }
    });
    let area = Rect::new(0, 0, 41, 10);
    let bar = find_bar("ab", "1 of 1");
    let (buf, targets) = bar_into(area, &bar);
    let y = bar_row(&buf);
    assert_eq!(y, 2, "the content row sits inside the box");
    // The thin box: corners and fills dim, sides on the content rows.
    let top: String = (0..41).map(|x| buf[(x, 0)].symbol().to_owned()).collect();
    assert_eq!(top, format!("┌{}┐ ", "─".repeat(38)));
    let bottom: String = (0..41).map(|x| buf[(x, 4)].symbol().to_owned()).collect();
    assert_eq!(bottom, format!("└{}┘ ", "─".repeat(38)));
    for row in [1, 2, 3] {
        assert_eq!(buf[(0, row)].symbol(), "│", "side {row}");
        assert_eq!(buf[(39, row)].symbol(), "│", "side {row}");
    }
    assert!(buf[(0, 0)].modifier.contains(ratatui::style::Modifier::DIM));
    // The text sits inside the sides.
    let text: String = (1..39).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert!(text.starts_with(" ⌕ ab█"), "{text:?}");
    assert!(text.ends_with("1 of 1 "), "{text:?}");
    assert_eq!(targets.len(), 1);
    // The cursor sits on the drawn block, inside the sides too.
    let cursor = super::bar_cursor(&bar, area).expect("the cursor");
    assert_eq!((cursor.x, cursor.y), (6, y));
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
}
