//! Tests for drag to select and copy on release (`docs/tui.md`,
//! "Selection and copy"), against the app and the view it draws.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::COPY_FAILED;
use crate::app::{App, Effect, Target};
use crate::home::Launch;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{self, TargetId};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// A line of the session, numbered `seq` when it has one.
fn line(kind: &str, payload: Value, action: Option<&str>, seq: Option<u64>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: seq.unwrap_or(0).saturating_mul(1_000),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: seq.map(contract::Seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The hub's `hub_hello`.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// An app connected to the hub and attached to [`SESSION`] at `width` by
/// `height`, with no home: the conversation is the whole screen.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// A turn started by `prompt`; it runs until [`done`].
fn prompt(app: &mut App, text: &str) {
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
        None,
        None,
    ));
}

/// A reply `text` from message `action`.
fn reply(app: &mut App, action: &str, text: &str) {
    app.on_line(line(
        "text_completed",
        json!({ "text": text }),
        Some(action),
        None,
    ));
}

/// The running turn completes.
fn done(app: &mut App) {
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
        None,
    ));
}

/// A session `notice` line saying `message`.
fn notice(app: &mut App, message: &str) {
    app.on_line(line(
        "notice",
        json!({"code": "extension_failed", "message": message}),
        None,
        None,
    ));
}

/// The app drawn at its size: the screen and the targets, as the loop's
/// last frame holds them.
fn draw(app: &App) -> (Buffer, Vec<mouse::Target>) {
    let (width, height) = (app.screen.width(), app.screen.height());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// The screen's rows as text.
fn rows(app: &App) -> Vec<String> {
    let (buf, _) = draw(app);
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buf.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect()
        })
        .collect()
}

/// The screen cell where `needle` first shows, by column count.
fn find(app: &App, needle: &str) -> (u16, u16) {
    for (y, row) in rows(app).iter().enumerate() {
        if let Some(byte) = row.find(needle) {
            let col = row.get(..byte).map_or(0, |before| before.chars().count());
            return (
                u16::try_from(col).expect("a column"),
                u16::try_from(y).expect("a row"),
            );
        }
    }
    panic!("{needle:?} is not on screen: {:#?}", rows(app));
}

/// One mouse report against the targets the app draws now.
fn report(app: &mut App, kind: MouseKind, (col, row): (u16, u16)) -> Effect {
    let (_, targets) = draw(app);
    app.on_select(&Mouse { kind, col, row }, &targets)
}

fn press(app: &mut App, at: (u16, u16)) -> Effect {
    report(app, MouseKind::Press(Button::Left), at)
}

fn drag(app: &mut App, at: (u16, u16)) -> Effect {
    report(app, MouseKind::Drag(Button::Left), at)
}

fn release(app: &mut App, at: (u16, u16)) -> Effect {
    report(app, MouseKind::Release, at)
}

/// A press at `from`, a drag to `to` and the release there: what it
/// copies.
fn select(app: &mut App, from: (u16, u16), to: (u16, u16)) -> Effect {
    assert_eq!(press(app, from), Effect::None);
    assert_eq!(drag(app, to), Effect::None);
    release(app, to)
}

/// `(col + by, row)`.
fn right(at: (u16, u16), by: u16) -> (u16, u16) {
    (at.0 + by, at.1)
}

/// An app showing one reply `text` at `width`, the turn done.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = attached(width, height);
    prompt(&mut app, " ");
    reply(&mut app, "a_m", text);
    done(&mut app);
    app
}

#[test]
fn a_press_then_release_selects_nothing() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    assert_eq!(press(&mut app, at), Effect::None);
    assert_eq!(release(&mut app, at), Effect::None);
    assert_eq!(app.select.span(), None);
    assert!(!app.copied());
    assert!(app.selection_cells(app.conversation_area()).is_empty());
    // A drag reported on the press's own cell is still a click.
    press(&mut app, at);
    assert_eq!(drag(&mut app, at), Effect::None);
    assert_eq!(app.select.span(), None);
    assert_eq!(release(&mut app, at), Effect::None);
    assert!(!app.copied());
}

#[test]
fn a_drag_without_a_press_selects_nothing() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    assert_eq!(drag(&mut app, at), Effect::None);
    assert_eq!(drag(&mut app, right(at, 4)), Effect::None);
    assert_eq!(release(&mut app, right(at, 4)), Effect::None);
    assert_eq!(app.select.span(), None);
    // After a release, a drag reported without a press moves nothing.
    select(&mut app, at, right(at, 1));
    let span = app.select.span();
    assert!(span.is_some());
    assert_eq!(drag(&mut app, right(at, 4)), Effect::None);
    assert_eq!(app.select.span(), span);
}

#[test]
fn a_middle_or_right_release_after_a_selection_copies_nothing() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    assert_eq!(
        select(&mut app, at, right(at, 4)),
        Effect::Copy("hello".to_owned())
    );
    let span = app.select.span();
    assert!(span.is_some());
    // A middle click's release after the finished selection copies
    // nothing, and the highlight stays.
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Middle), at),
        Effect::None
    );
    assert_eq!(report(&mut app, MouseKind::Release, at), Effect::None);
    assert_eq!(app.select.span(), span);
    // So does a right click's.
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Right), at),
        Effect::None
    );
    assert_eq!(report(&mut app, MouseKind::Release, at), Effect::None);
    assert_eq!(app.select.span(), span);
}

#[test]
fn dragging_selects_from_the_press_to_the_pointer() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    press(&mut app, at);
    drag(&mut app, right(at, 4));
    let area = app.conversation_area();
    assert_eq!(
        app.selection_cells(area),
        [Rect::new(at.0, at.1, 5, 1)],
        "the press's cell to the pointer's"
    );
    assert_eq!(
        release(&mut app, right(at, 4)),
        Effect::Copy("hello".to_owned())
    );
    assert!(app.copied());
    // The highlight stays after the release.
    assert!(app.select.span().is_some());
}

#[test]
fn a_drag_that_returns_to_its_cell_selects_one_cell() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    press(&mut app, at);
    drag(&mut app, right(at, 1));
    drag(&mut app, at);
    assert_eq!(release(&mut app, at), Effect::Copy("h".to_owned()));
}

#[test]
fn release_copies_the_unwrapped_text_and_shows_copied() {
    let text = "one two three four five six seven";
    let mut app = replied(14, 10, text);
    let first = find(&app, "one");
    let last = find(&app, "seven");
    assert!(last.1 >= first.1 + 2, "three rows: {:#?}", rows(&app));
    let effect = select(&mut app, first, right(last, 4));
    assert_eq!(effect, Effect::Copy(text.to_owned()));
    assert!(app.copied());
}

#[test]
fn a_selection_over_paragraphs_keeps_the_blank_line_between() {
    // The blank line after "beta" is outside the selection.
    let mut app = replied(40, 10, "alpha\n\nbeta\n\ngamma");
    let alpha = find(&app, "alpha");
    let beta = find(&app, "beta");
    assert_eq!(
        select(&mut app, alpha, right(beta, 3)),
        Effect::Copy("alpha\n\nbeta".to_owned())
    );
}

#[test]
fn the_highlight_stays_inside_the_area_and_off_new_below() {
    let mut app = attached(40, 10);
    prompt(&mut app, " ");
    let text: Vec<String> = (0..20).map(|n| format!("paragraph {n}")).collect();
    reply(&mut app, "a_m", &text.join("\n\n"));
    let area = app.conversation_area();
    select(&mut app, (0, area.y + 1), (39, area.bottom() - 1));
    // Scrolled up three rows, the selection runs past the area's bottom,
    // and new output shows "↓ New messages below" on its last row.
    let (top, _) = app.scroll();
    app.jump(top - 3);
    reply(&mut app, "a_n", "more");
    assert!(app.has_new());
    let cells = app.selection_cells(area);
    assert!(!cells.is_empty());
    for rect in cells {
        assert!(rect.y < area.bottom() - 1, "{rect:?} past {area:?}");
    }
}

#[test]
fn a_selection_over_a_list_copies_markers_without_hang_indent() {
    let mut app = replied(14, 10, "- alpha beta gamma\n- delta");
    let first = find(&app, "• alpha");
    let last = find(&app, "delta");
    assert_eq!(
        select(&mut app, first, right(last, 4)),
        Effect::Copy("• alpha beta gamma\n• delta".to_owned())
    );
}

#[test]
fn a_code_selection_copies_without_the_gutter() {
    let mut app = replied(30, 10, "```\nlet a = 1;\nlet b = 2;\n```");
    let first = find(&app, "1 │");
    let last = find(&app, "2 │");
    // Whole rows, the header's `copy` excluded: from the gutter of the
    // first line to the right edge of the second.
    let effect = select(&mut app, (0, first.1), (29, last.1));
    assert_eq!(effect, Effect::Copy("let a = 1;\nlet b = 2;".to_owned()));
}

#[test]
fn a_bubble_copies_without_its_padding() {
    let mut app = attached(40, 10);
    prompt(&mut app, "hello world");
    let at = find(&app, "hello world");
    // The whole row, the bubble's pads and the blank left of it included.
    assert_eq!(
        select(&mut app, (0, at.1), (39, at.1)),
        Effect::Copy("hello world".to_owned())
    );
}

#[test]
fn a_wrapped_tool_line_keeps_the_space_ratatui_dropped() {
    let mut app = attached(20, 10);
    prompt(&mut app, " ");
    app.on_line(line(
        "steering_applied",
        json!({"content": [{"type": "text", "text": "aaaaaa bbbbbbbbbb"}], "source": "driver"}),
        None,
        None,
    ));
    let first = find(&app, "steer");
    let last = find(&app, "bbbb");
    assert_eq!(last.1, first.1 + 1, "wrapped once: {:#?}", rows(&app));
    assert_eq!(
        select(&mut app, (0, first.1), (19, last.1)),
        Effect::Copy("steer · aaaaaa bbbbbbbbbb".to_owned())
    );
}

#[test]
fn a_partial_first_and_last_row_copy_only_their_cells() {
    let mut app = replied(12, 10, "alpha beta gamma delta");
    let beta = find(&app, "beta");
    let gamma = find(&app, "gamma");
    assert_eq!(gamma.1, beta.1 + 1, "{:#?}", rows(&app));
    assert_eq!(
        select(&mut app, beta, right(gamma, 2)),
        Effect::Copy("beta gam".to_owned())
    );
}

#[test]
fn a_backwards_drag_copies_the_same_text() {
    let mut app = replied(12, 10, "alpha beta gamma delta");
    let beta = find(&app, "beta");
    let gamma = find(&app, "gamma");
    assert_eq!(
        select(&mut app, right(gamma, 2), beta),
        Effect::Copy("beta gam".to_owned())
    );
}

/// The screen columns of "a界b" shown on its row: `a` at `base`, `界`
/// in `base + 1` and `base + 2`, `b` at `base + 3`.
fn wide(app: &App) -> (u16, u16) {
    find(app, "a界")
}

#[test]
fn a_selection_starting_inside_a_wide_character_copies_it_whole() {
    let mut app = replied(40, 10, "a界b");
    let (base, row) = wide(&app);
    // From the second cell of 界 to b.
    let from = (base + 2, row);
    let to = (base + 3, row);
    assert_eq!(press(&mut app, from), Effect::None);
    assert_eq!(drag(&mut app, to), Effect::None);
    assert_eq!(
        app.selection_cells(app.conversation_area()),
        [Rect::new(base + 2, row, 2, 1)],
        "the second cell of 界 and b"
    );
    assert_eq!(release(&mut app, to), Effect::Copy("界b".to_owned()));
}

#[test]
fn a_selection_ending_inside_a_wide_character_copies_it_whole() {
    let mut app = replied(40, 10, "a界b");
    let (base, row) = wide(&app);
    // Pressed on b, dragged back to the second cell of 界.
    assert_eq!(
        select(&mut app, (base + 3, row), (base + 2, row)),
        Effect::Copy("界b".to_owned())
    );
}

#[test]
fn a_selection_ending_on_a_wide_characters_first_cell_copies_it_whole() {
    let mut app = replied(40, 10, "a界b");
    let (base, row) = wide(&app);
    // From a to the first cell of 界: the highlight meets only half of
    // 界, the copy is the whole of it.
    assert_eq!(
        select(&mut app, (base, row), (base + 1, row)),
        Effect::Copy("a界".to_owned())
    );
}

#[test]
fn a_selection_inside_a_wide_character_copies_it_whole() {
    let mut app = replied(40, 10, "a界b");
    let (base, row) = wide(&app);
    // Pressed on the second cell of 界, dragged to b and back.
    assert_eq!(press(&mut app, (base + 2, row)), Effect::None);
    assert_eq!(drag(&mut app, (base + 3, row)), Effect::None);
    assert_eq!(drag(&mut app, (base + 2, row)), Effect::None);
    assert_eq!(
        release(&mut app, (base + 2, row)),
        Effect::Copy("界".to_owned())
    );
}

/// What a press, drag and release from `at` select: the span after.
fn tried(app: &mut App, at: (u16, u16)) -> Option<(super::Point, super::Point)> {
    press(app, at);
    drag(app, (at.0.saturating_sub(1), at.1.saturating_sub(1)));
    drag(app, right(at, 1));
    release(app, right(at, 1));
    app.select.span()
}

/// The launch description: `/w`, outside git, default shares.
fn launch() -> Launch {
    Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        attention: crate::Attention::default(),
    }
}

/// A feed `session_status` for `session`, idle.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: json!({
            "name": "s", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// An app with home state, attached, at `width` by `height`, two sessions
/// live: the layout applies, with the rail where it fits.
fn laid_out(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    // With home the hello also asks for the session list.
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app.on_line(status(SESSION));
    app.on_line(status("s_bbbbbbbbbbbbbbbb"));
    app
}

#[test]
fn presses_outside_the_area_select_nothing() {
    // The input box, and the blank rows above a short conversation.
    let mut app = replied(40, 10, "hello there");
    let area = app.conversation_area();
    assert_eq!(tried(&mut app, (2, area.bottom())), None, "the input box");
    let hello = find(&app, "hello");
    assert!(
        hello.1 > area.y + 1,
        "a short conversation: {:#?}",
        rows(&app)
    );
    assert_eq!(tried(&mut app, (2, area.y)), None, "a blank row above it");

    // The approval panel.
    let mut app = replied(40, 12, "hello there");
    app.on_line(line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
        Some("a_1"),
        None,
    ));
    assert!(app.panel().is_some());
    let area = app.conversation_area();
    assert_eq!(tried(&mut app, (2, area.bottom())), None, "the panel");

    // The rail, the panel and the column's header at 160x48.
    let mut app = laid_out(160, 48);
    prompt(&mut app, " ");
    reply(&mut app, "a_m", "hello there");
    done(&mut app);
    let layout = app.chrome().layout().expect("the layout applies");
    let rail = layout.rail.expect("a rail at 160 columns");
    let panel = layout.panel.expect("a panel at 160 columns");
    assert_eq!(tried(&mut app, (rail.x, 30)), None, "the rail");
    assert_eq!(tried(&mut app, (panel.x + 1, 30)), None, "the panel");
    assert_eq!(
        tried(&mut app, (layout.column.x + 1, layout.column.y)),
        None,
        "the column's header row"
    );
    // The text itself still selects.
    let at = find(&app, "hello");
    assert!(tried(&mut app, at).is_some());
}

#[test]
fn a_press_on_a_notice_or_new_below_selects_nothing() {
    let mut app = attached(40, 10);
    prompt(&mut app, " ");
    let text: Vec<String> = (0..20).map(|n| format!("paragraph {n}")).collect();
    reply(&mut app, "a_m", &text.join("\n\n"));
    notice(&mut app, "careful now");
    let (_, targets) = draw(&app);
    let notice = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Notice(_)))
        .expect("the notice is drawn");
    assert_eq!(
        tried(&mut app, (notice.rect.x + 1, notice.rect.y)),
        None,
        "a notice"
    );
    // Scrolled up, new output shows "↓ New messages below".
    app.on_key(Key::PageUp, now());
    reply(&mut app, "a_n", "more");
    assert!(app.has_new());
    let (_, targets) = draw(&app);
    let below = targets
        .iter()
        .find(|target| target.id == TargetId::NewBelow)
        .expect("new below is drawn");
    assert_eq!(
        tried(&mut app, (below.rect.x + 1, below.rect.y)),
        None,
        "new below"
    );
    // Its row selects nothing even beside the label.
    assert_eq!(tried(&mut app, (0, below.rect.y)), None, "new below's row");
}

#[test]
fn no_selection_on_home_or_under_a_cover() {
    // No session: rows folded straight into the pages still select none.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(40, 10);
    for line in [
        line(
            "turn_started",
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": " "}]}]}),
            None,
            None,
        ),
        line(
            "text_completed",
            json!({"text": "hello there"}),
            Some("a_m"),
            None,
        ),
    ] {
        if let Line::Session(envelope) = line {
            app.screen.pages_mut().apply(&envelope);
        }
    }
    app.settle();
    assert!(app.session().is_none());
    let at = find(&app, "hello");
    assert_eq!(tried(&mut app, at), None, "no session");

    // Home: no session attached.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.set_size(80, 24);
    assert!(app.home_screen().is_some());
    assert_eq!(tried(&mut app, (10, 10)), None, "home");

    // The key map.
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    app.on_key(Key::F1, now());
    assert!(app.keymap_top().is_some());
    assert_eq!(tried(&mut app, at), None, "the key map");

    // The notice overlay.
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    notice(&mut app, "careful");
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(tried(&mut app, at), None, "the notice overlay");

    // The repository offer's swapped view.
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    app.on_line(line(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [{"kind": "mcp_server", "name": "a",
            "hash": "h", "required": false, "summary": "MCP server: a"}]}),
        None,
        None,
    ));
    assert!(app.offer_open());
    assert_eq!(tried(&mut app, at), None, "the repository offer");
}

#[test]
fn dragging_past_the_edge_clamps() {
    let mut app = replied(20, 10, "alpha beta gamma delta epsilon");
    let area = app.conversation_area();
    let alpha = find(&app, "alpha");
    press(&mut app, alpha);
    // Past the right edge and below the area: its last cell.
    drag(&mut app, (200, 200));
    let (from, to) = app.select.span().expect("a selection");
    assert_eq!(from.col, alpha.0);
    assert_eq!(to.col, area.width - 1);
    let last = app.scroll().1 - 1;
    assert_eq!(to.row, last);
    // Past the left edge and above the area: its first shown cell.
    drag(&mut app, (0, 0));
    let (from, _) = app.select.span().expect("a selection");
    assert_eq!(from.col, 0);
    assert_eq!(from.row, app.scroll().0);
}

#[test]
fn esc_clears_the_selection_before_it_interrupts() {
    let mut app = attached(40, 10);
    prompt(&mut app, "go");
    reply(&mut app, "a_m", "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    assert!(app.select.span().is_some());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.select.span(), None);
    assert!(matches!(app.on_key(Key::Esc, now()), Effect::Send(_)));
}

#[test]
fn esc_with_only_a_press_interrupts() {
    let mut app = attached(40, 10);
    prompt(&mut app, "go");
    reply(&mut app, "a_m", "hello there");
    let at = find(&app, "hello");
    press(&mut app, at);
    assert!(matches!(app.on_key(Key::Esc, now()), Effect::Send(_)));
}

#[test]
fn esc_with_the_key_map_open_closes_it_first() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    app.on_key(Key::F1, now());
    assert!(app.keymap_top().is_some());
    app.on_key(Key::Esc, now());
    assert!(app.keymap_top().is_none());
    assert!(app.select.span().is_some());
    app.on_key(Key::Esc, now());
    assert_eq!(app.select.span(), None);
}

#[test]
fn esc_with_an_approval_open_puts_it_aside_before_the_selection() {
    let mut app = replied(40, 12, "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    app.on_line(line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
        Some("a_1"),
        None,
    ));
    assert!(app.panel().is_some());
    app.on_key(Key::Esc, now());
    assert!(app.panel().is_none());
    assert!(app.select.span().is_some());
    app.on_key(Key::Esc, now());
    assert_eq!(app.select.span(), None);
}

#[test]
fn esc_with_the_notice_overlay_open_closes_it_before_the_selection() {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    notice(&mut app, "careful");
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    app.on_key(Key::Esc, now());
    assert!(app.notice_overlay().is_none());
    assert!(app.select.span().is_some());
    app.on_key(Key::Esc, now());
    assert_eq!(app.select.span(), None);
}

/// An app with "hello" selected.
fn selected() -> App {
    let mut app = replied(40, 10, "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    assert!(app.select.span().is_some());
    app
}

#[test]
fn a_new_width_clears_the_selection() {
    let mut app = selected();
    app.set_size(41, 10);
    assert_eq!(app.select.span(), None);
}

#[test]
fn a_height_change_keeps_it() {
    let mut app = selected();
    app.set_size(40, 12);
    assert!(app.select.span().is_some());
}

#[test]
fn ctrl_o_and_opening_a_line_clear_it() {
    let mut app = selected();
    app.on_key(Key::CtrlO, now());
    assert_eq!(app.select.span(), None);

    let mut app = attached(40, 12);
    prompt(&mut app, "go");
    app.on_line(line("reasoning_started", json!({}), Some("a_r"), None));
    app.on_line(line(
        "reasoning_completed",
        json!({"text": "weigh it"}),
        Some("a_r"),
        None,
    ));
    reply(&mut app, "a_m", "hello there");
    done(&mut app);
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    let thought = app
        .pages()
        .rows()
        .into_iter()
        .find_map(|(_, target)| target.filter(|target| matches!(target, Target::Thought(_))))
        .expect("a thought's line");
    app.open(thought);
    assert_eq!(app.select.span(), None);
}

#[test]
fn going_home_clears_it() {
    let mut app = selected();
    app.go_home();
    assert_eq!(app.select.span(), None);
}

#[test]
fn live_output_below_keeps_it() {
    let mut app = attached(40, 12);
    prompt(&mut app, "go");
    reply(&mut app, "a_m", "hello there");
    let at = find(&app, "hello");
    select(&mut app, at, right(at, 4));
    reply(&mut app, "a_n", "more below");
    assert!(app.select.span().is_some());
}

/// A session whose first turn runs 40 steps, so it spans pages, then a
/// short second turn.
fn spanning() -> Vec<Line> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, payload: Value, action: Option<String>| {
        lines.push(line(kind, payload, action.as_deref(), Some(seq)));
        seq += 1;
    };
    push(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    );
    for step in 0..40 {
        let message = format!("m_{step}");
        push("step_started", json!({}), None);
        push(
            "assistant_message_started",
            json!({}),
            Some(message.clone()),
        );
        push(
            "text_completed",
            json!({"text": format!("reply {step}")}),
            Some(message.clone()),
        );
        push(
            "assistant_message_completed",
            json!({"outcome": "completed"}),
            Some(message),
        );
    }
    push("turn_completed", json!({"outcome": "completed"}), None);
    push(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "next"}]}]}),
        None,
    );
    push(
        "text_completed",
        json!({"text": "last reply"}),
        Some("m_last".to_owned()),
    );
    push("turn_completed", json!({"outcome": "completed"}), None);
    lines
}

/// Loads every page `app` needs from `lines`, as the loop's `page_in` does.
fn load_needed(app: &mut App, lines: &[Line]) {
    while let Some(range) = app.needs().first().cloned() {
        let chunk: Vec<contract::Envelope> = lines
            .iter()
            .filter_map(|line| match line {
                Line::Session(envelope) => Some(envelope.clone()),
                Line::Hub(_) => None,
            })
            .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
            .collect();
        assert!(!chunk.is_empty(), "no lines for {range:?}");
        app.load(chunk);
    }
}

/// `spanning` open at 40x8 with every needed page loaded, page 0 dropped.
fn spanning_app(lines: &[Line]) -> App {
    let mut app = attached(40, 8);
    for line in lines {
        app.on_line(line.clone());
    }
    load_needed(&mut app, lines);
    assert!(app.pages().index().pages().len() > 2, "too few pages");
    assert!(app.pages().part(0).is_none(), "page 0 stays resident");
    app
}

/// A selection from the conversation's first row to its third, made at
/// the top and released at the end, so its pages dropped: what the
/// release returned, and the text the top rows showed.
fn select_dropped(app: &mut App, lines: &[Line]) -> (Effect, Vec<String>) {
    app.jump(0);
    load_needed(app, lines);
    let shown: Vec<String> = rows(app)
        .into_iter()
        .take(3)
        .map(|row| row.trim().to_owned())
        .collect();
    press(app, (0, 0));
    drag(app, (39, 2));
    app.on_key(Key::End, now());
    assert!(app.pages().part(0).is_none(), "page 0 stays resident");
    (release(app, (39, 2)), shown)
}

#[test]
fn a_selection_over_a_dropped_page_copies_after_it_loads() {
    let lines = spanning();
    let mut app = spanning_app(&lines);
    let prior = app.pages().pinned();
    // Scrolled during the drag with PageUp: the press at the bottom, the
    // pointer at the top.
    let bottom = app.conversation_area().bottom() - 1;
    press(&mut app, (0, bottom));
    for _ in 0..8 {
        app.on_key(Key::PageUp, now());
        load_needed(&mut app, &lines);
    }
    let top_row = rows(&app).first().cloned().unwrap_or_default();
    drag(&mut app, (0, 0));
    app.on_key(Key::End, now());
    let (first, _) = app.select.span().expect("a selection");
    let (page, _) = app
        .pages()
        .index()
        .locate(first.row)
        .expect("the top row's page");
    assert!(app.pages().part(page).is_none(), "its page stays resident");
    assert_eq!(release(&mut app, (0, bottom)), Effect::None);
    let pinned = app.pages().pinned();
    assert!(pinned > prior);
    assert_eq!(app.take_copy(), None, "the page is not loaded yet");
    assert_eq!(app.pages().pinned(), pinned, "a waiting copy pins once");
    load_needed(&mut app, &lines);
    let text = app.take_copy().expect("the copy runs once the page loads");
    assert!(
        text.starts_with(top_row.trim()),
        "{text:?} from {top_row:?}"
    );
    assert!(text.lines().count() > 8, "{text:?}");
    assert!(app.copied());
    assert_eq!(app.pages().pinned(), prior, "its pages are unpinned");
    assert_eq!(app.take_copy(), None, "a copy runs once");
}

#[test]
fn a_failed_page_abandons_the_copy_with_a_notice() {
    let lines = spanning();
    let mut app = spanning_app(&lines);
    let prior = app.pages().pinned();
    let (effect, _) = select_dropped(&mut app, &lines);
    assert_eq!(effect, Effect::None);
    for range in app.needs() {
        app.load_failed(&range, "boom");
    }
    assert_eq!(app.take_copy(), None);
    assert_eq!(app.notice(), Some(COPY_FAILED));
    assert_eq!(app.pages().pinned(), prior);
    assert!(!app.copied());
}

#[test]
fn a_lost_link_abandons_the_copy() {
    let lines = spanning();
    let mut app = spanning_app(&lines);
    let prior = app.pages().pinned();
    let (effect, _) = select_dropped(&mut app, &lines);
    assert_eq!(effect, Effect::None);
    app.disconnected();
    assert_eq!(app.notice(), Some(COPY_FAILED));
    assert_eq!(app.take_copy(), None);
    assert_eq!(app.pages().pinned(), prior);

    // A copy released with the link already down is abandoned in the step.
    let mut app = spanning_app(&lines);
    app.disconnected();
    let (effect, _) = select_dropped(&mut app, &lines);
    assert_eq!(effect, Effect::None);
    assert_ne!(app.notice(), Some(COPY_FAILED));
    assert_eq!(app.take_copy(), None);
    assert_eq!(app.notice(), Some(COPY_FAILED));
    assert_eq!(app.pages().pinned(), prior);
}

#[test]
fn overlapping_pending_copies_keep_each_others_pins() {
    let lines = spanning();
    let mut app = spanning_app(&lines);
    let prior = app.pages().pinned();
    let (effect, shown) = select_dropped(&mut app, &lines);
    assert_eq!(effect, Effect::None);
    // A whole-turn copy of turn 0 waits on page 0 too.
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    load_needed(&mut app, &lines);
    let text = app.take_copy().expect("the selection copies");
    assert!(text.contains(&shown[0]), "{text:?} from {shown:?}");
    // The turn copy still pins its pages: page 0 stays.
    assert!(app.pages().pinned() > prior);
    app.on_key(Key::End, now());
    assert!(
        app.pages().part(0).is_some(),
        "the turn copy's page dropped"
    );
    app.focus = Some(TargetId::Turn(0));
    assert!(matches!(app.on_key(Key::Char('y'), now()), Effect::Copy(_)));
    assert_eq!(app.pages().pinned(), prior);
}

/// Whether `text` shows on the screen, and whether a notice target is
/// drawn.
fn shows(app: &App, text: &str) -> (bool, bool) {
    let (_, targets) = draw(app);
    let drawn = rows(app).iter().any(|row| row.contains(text));
    let target = targets
        .iter()
        .any(|target| matches!(target.id, TargetId::Notice(_)));
    (drawn, target)
}

#[test]
fn a_notice_during_a_drag_waits_for_the_release() {
    let mut app = replied(60, 10, "hello there");
    let at = find(&app, "hello");
    press(&mut app, at);
    drag(&mut app, right(at, 3));
    notice(&mut app, "arrived late");
    assert_eq!(shows(&app, "arrived late"), (false, false));
    release(&mut app, right(at, 3));
    assert_eq!(shows(&app, "arrived late"), (true, true));
    let (_, targets) = draw(&app);
    let top = targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Notice(_)))
        .map(|target| target.rect.y)
        .min();
    assert_eq!(
        top,
        Some(find(&app, "arrived late").1),
        "the newest is on top"
    );
}

#[test]
fn a_notice_from_before_the_press_stays_shown() {
    let mut app = replied(60, 10, "hello there");
    notice(&mut app, "was here");
    let at = find(&app, "hello");
    press(&mut app, at);
    drag(&mut app, right(at, 3));
    assert_eq!(shows(&app, "was here"), (true, true));
}

#[test]
fn a_notice_during_a_click_shows_at_once() {
    let mut app = replied(60, 10, "hello there");
    let at = find(&app, "hello");
    press(&mut app, at);
    notice(&mut app, "mid click");
    assert_eq!(shows(&app, "mid click"), (false, false));
    release(&mut app, at);
    assert_eq!(shows(&app, "mid click"), (true, true));
}

/// Where the view draws the conversation's first row: the top row's first
/// cell of a conversation scrolled to its top.
fn drawn_origin(app: &mut App) -> (u16, u16) {
    app.jump(0);
    find(app, "first")
}

/// An app whose reply fills more than its screen, its first row "first".
fn filled(mut app: App) -> App {
    prompt(&mut app, " ");
    let text: Vec<String> = std::iter::once("first".to_owned())
        .chain((0..60).map(|n| format!("p{n}")))
        .collect();
    reply(&mut app, "a_m", &text.join("\n\n"));
    done(&mut app);
    app
}

#[test]
fn the_conversation_area_is_the_drawn_rect() {
    // 80x24 with no layout, 160x48 with the rail, and with the approval
    // panel open.
    let mut panel = filled(attached(80, 24));
    panel.on_line(line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
        Some("a_1"),
        None,
    ));
    assert!(panel.panel().is_some());
    for (name, mut app) in [
        ("80x24", filled(attached(80, 24))),
        ("160x48 with the rail", filled(laid_out(160, 48))),
        ("the approval panel", panel),
    ] {
        let area = app.conversation_area();
        assert_eq!(drawn_origin(&mut app), (area.x, area.y), "{name}");
        assert_eq!(
            usize::from(area.height),
            app.conversation_height(),
            "{name}"
        );
        // Following, the conversation's last row is the area's last.
        app.on_key(Key::End, now());
        let screen = rows(&app);
        let last = screen
            .get(usize::from(area.bottom() - 1))
            .cloned()
            .unwrap_or_default();
        assert!(last.contains("▣ completed"), "{name}: {last:?}");
        let below = screen
            .get(usize::from(area.bottom()))
            .cloned()
            .unwrap_or_default();
        assert!(!below.contains("▣"), "{name}: {below:?}");
    }
}

#[test]
fn a_selection_starting_after_a_zero_width_character_skips_it() {
    // A zero-width grapheme draws into no cell: the copy starts at the
    // selected `c`, not at the character before it.
    let mut app = replied(40, 10, "ab\u{200b}cd");
    let at = find(&app, "ab");
    let c = right(at, 2);
    assert_eq!(
        select(&mut app, c, right(c, 1)),
        Effect::Copy("cd".to_owned())
    );
}

#[test]
fn a_selection_starting_after_a_wrapped_space_copies_no_space_before_it() {
    // The space the wrap drops has no cell and lies before the selection.
    let mut app = replied(12, 10, "alpha beta gamma delta");
    let gamma = find(&app, "gamma");
    let delta = find(&app, "delta");
    assert_eq!(delta.1, gamma.1, "{:#?}", rows(&app));
    assert_eq!(
        select(&mut app, gamma, right(delta, 4)),
        Effect::Copy("gamma delta".to_owned())
    );
}

#[test]
fn a_selection_at_the_area_edge_draws_no_empty_rect() {
    // No pointer reaches the column past the area's right edge: a span
    // starting there covers no cell, so it draws no zero-width rect.
    let mut app = replied(40, 10, "hello there");
    let area = app.conversation_area();
    let (top, _) = app.scroll();
    let edge = super::Point {
        row: top,
        col: area.width,
    };
    app.select.anchor = Some(edge);
    app.select.head = Some(edge);
    assert_eq!(app.selection_cells(area), Vec::<Rect>::new());
}
