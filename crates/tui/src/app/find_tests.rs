//! Tests for conversation search (`docs/tui.md`, "Search"): the bar, its
//! query and its keys, against the app and the view it draws.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::FIND_PAUSE;
use crate::app::{App, Effect};
use crate::keys::{Button, Edit, Key, Mouse, MouseKind};
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// A line of the session.
fn line(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
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

/// A turn started by `prompt`; it runs until done.
fn prompt(app: &mut App, text: &str) {
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
        None,
    ));
}

/// A reply `text` from message `action`.
fn reply(app: &mut App, action: &str, text: &str) {
    app.on_line(line(
        "text_completed",
        json!({ "text": text }),
        Some(action),
    ));
}

/// The running turn completes.
fn done(app: &mut App) {
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
}

/// An app showing one reply `text`, the turn done.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = attached(width, height);
    prompt(&mut app, " ");
    reply(&mut app, "a_m", text);
    done(&mut app);
    app
}

/// A `permission_requested` envelope for `action` with request `id`, asked
/// by a global rule.
fn request(action: &str, id: &str) -> Line {
    line(
        "permission_requested",
        json!({"request_id": id, "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "npm"}}),
        Some(action),
    )
}

/// An offer of one MCP server, opening the repository offer's view.
fn offer() -> Line {
    line(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [
            {"kind": "mcp_server", "name": "a", "hash": "h",
             "required": false, "summary": "MCP server: a"},
        ]}),
        None,
    )
}

/// Types `text` into the app, one character per key.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        let effect = app.on_key(Key::Char(ch), now());
        assert_eq!(effect, Effect::None, "typing {ch:?}");
    }
}

/// Opens the bar and types `query` into it.
fn search(app: &mut App, query: &str) {
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_some());
    for ch in query.chars() {
        let effect = app.on_key(Key::Char(ch), now());
        assert!(
            matches!(effect, Effect::FindPause { .. }),
            "typing {ch:?}: {effect:?}"
        );
    }
}

/// The bar's query.
fn query(app: &App) -> String {
    app.find_bar().map(|bar| bar.query).unwrap_or_default()
}

/// The app drawn at 80x24 as text.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// The screen cell where `needle` first shows.
fn find(app: &App, needle: &str) -> (u16, u16) {
    let (width, height) = (app.screen.width(), app.screen.height());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        if let Some(byte) = row.find(needle) {
            let col = row.get(..byte).map_or(0, |before| before.chars().count());
            return (u16::try_from(col).expect("a column"), y);
        }
    }
    panic!("{needle:?} is not on screen");
}

/// One mouse report against the targets the app draws now.
fn report(app: &mut App, kind: MouseKind, (col, row): (u16, u16)) -> Effect {
    let (width, height) = (app.screen.width(), app.screen.height());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.on_select(&Mouse { kind, col, row }, &targets)
}

#[test]
fn ctrl_f_opens_the_bar_and_keeps_the_draft() {
    let mut app = attached(40, 10);
    type_text(&mut app, "hello");
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_some());
    assert_eq!(query(&app), "");
    assert_eq!(app.draft(), "hello");
}

#[test]
fn typing_goes_to_the_query_and_paste_joins_lines() {
    let mut app = attached(40, 10);
    search(&mut app, "ab");
    assert_eq!(query(&app), "ab");
    assert_eq!(app.draft(), "");
    assert!(matches!(
        app.on_edit(Edit::Paste("c\nd".to_owned())),
        Effect::FindPause { .. }
    ));
    assert_eq!(query(&app), "abc d");
}

#[test]
fn a_paste_into_the_bar_schedules_the_pause() {
    let mut app = attached(40, 10);
    search(&mut app, "a");
    assert_eq!(
        app.on_edit(Edit::Paste("b".to_owned())),
        Effect::FindPause {
            generation: 2,
            after: FIND_PAUSE,
        }
    );
    assert_eq!(query(&app), "ab");
}

#[test]
fn a_paste_with_an_approval_open_goes_to_its_feedback() {
    let mut app = attached(40, 10);
    search(&mut app, "a");
    app.on_line(request("a_1", "r_1"));
    assert_eq!(app.open_first(), Effect::None);
    assert!(app.panel().is_some());
    assert_eq!(app.on_edit(Edit::Paste("yes".to_owned())), Effect::None);
    assert_eq!(query(&app), "a");
    assert_eq!(app.draft(), "");
}

#[test]
fn a_paste_with_the_ctrl_r_panel_open_goes_to_the_panel() {
    let mut app = attached(40, 10);
    search(&mut app, "a");
    app.open_search();
    assert!(app.search_panel().is_some());
    assert_eq!(app.on_edit(Edit::Paste("xy".to_owned())), Effect::None);
    assert_eq!(query(&app), "a");
    let panel = app.search_panel().expect("the panel stays open");
    assert!(panel.lines.first().is_some_and(|line| line.ends_with("xy")));
}

#[test]
fn backspace_edits_the_query() {
    let mut app = attached(40, 10);
    search(&mut app, "hi");
    assert_eq!(
        app.on_key(Key::Backspace, now()),
        Effect::FindPause {
            generation: 3,
            after: FIND_PAUSE,
        }
    );
    assert_eq!(query(&app), "h");
    // Backspace on an empty query edits nothing and schedules nothing.
    let mut app = attached(40, 10);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(query(&app), "");
}

#[test]
fn esc_closes_the_bar_and_clears_the_marks() {
    let mut app = attached(40, 10);
    search(&mut app, "hi");
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.find_bar().is_none());
    assert!(app.find_marks(Rect::new(0, 0, 40, 10)).is_empty());
}

#[test]
fn esc_closes_the_bar_before_the_selection() {
    let mut app = replied(40, 10, "hello there");
    search(&mut app, "hell");
    let at = find(&app, "hello");
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Left), at),
        Effect::None
    );
    assert_eq!(
        report(&mut app, MouseKind::Drag(Button::Left), (at.0 + 2, at.1)),
        Effect::None
    );
    assert!(app.select.span().is_some());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.find_bar().is_none());
    assert!(app.select.span().is_some());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.select.span().is_none());
}

#[test]
fn page_keys_still_scroll_with_the_bar_open() {
    let text: Vec<String> = (0..20).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(40, 10, &text.join("\n\n"));
    assert_eq!(app.top(), None);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert!(app.top().is_some());
    assert!(app.find_bar().is_some());
}

#[test]
fn ctrl_f_on_home_does_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(40, 10);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_none());
}

#[test]
fn ctrl_f_over_the_offer_does_nothing() {
    let mut app = attached(40, 10);
    assert!(app.on_line(offer()).is_empty());
    assert!(app.offer_open());
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_none());
}

#[test]
fn going_home_closes_the_bar() {
    let mut app = attached(40, 10);
    search(&mut app, "hi");
    app.go_home();
    assert!(app.find_bar().is_none());
}

#[test]
fn find_bar_80x24() {
    let mut app = replied(80, 24, "the quick brown fox jumps over the lazy dog");
    search(&mut app, "fox");
    insta::assert_snapshot!("find_bar_80x24", screen(&app));
}

#[test]
fn find_bar_with_copied_and_notices_below() {
    let mut app = replied(80, 24, "the quick brown fox jumps over the lazy dog");
    search(&mut app, "fox");
    app.copied = true;
    app.notices.push("A notice.".to_owned());
    insta::assert_snapshot!("find_bar_with_copied_and_notices_below", screen(&app));
}
