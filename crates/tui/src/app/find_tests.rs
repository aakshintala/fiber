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

pub(super) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
pub(super) fn now() -> Instant {
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
pub(super) fn attached(width: u16, height: u16) -> App {
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

/// The bar's count.
pub(super) fn count(app: &App) -> String {
    app.find_bar().map(|bar| bar.count).unwrap_or_default()
}

/// One durable session envelope with `seq`, recorded in `log` for the
/// fake hub's answers.
fn numbered(
    log: &mut Vec<contract::Envelope>,
    seq: &mut u64,
    kind: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    *seq += 1;
    let envelope = contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(contract::Seq(*seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    };
    log.push(envelope.clone());
    Line::Session(envelope)
}

/// A turn of `replies` text replies, seqs from `seq`, recorded in `log`.
pub(super) fn text_turn(
    log: &mut Vec<contract::Envelope>,
    seq: &mut u64,
    app: &mut App,
    replies: &[&str],
) {
    app.on_line(numbered(
        log,
        seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    for reply in replies {
        app.on_line(numbered(
            log,
            seq,
            "text_completed",
            json!({"text": reply}),
            Some("a_m"),
        ));
    }
    app.on_line(numbered(
        log,
        seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
}

/// The fake hub's `history` answer to command `id` for `from` to `to`:
/// the logged lines it holds.
fn hub_answer(id: &str, log: &[contract::Envelope], from: u64, to: u64) -> Line {
    let held: Vec<contract::Envelope> = log
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .cloned()
        .collect();
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "result": {"lines": held}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The `history` command `line` asks for: its id and seq range.
fn command(line: &str) -> (String, u64, u64) {
    let value: serde_json::Value = serde_json::from_str(line).expect("a command line");
    assert_eq!(value["command"], "history");
    (
        value["id"].as_str().unwrap_or_default().to_owned(),
        value["args"]["from_seq"].as_u64().unwrap_or(0),
        value["args"]["to_seq"].as_u64().unwrap_or(0),
    )
}

/// Answers every pending search fetch from `log`, starting with `first`,
/// until none remains: at most the pages plus one round. Returns every
/// seq range asked for, in order.
fn answer_all(app: &mut App, log: &[contract::Envelope], first: Vec<String>) -> Vec<(u64, u64)> {
    let mut outgoing = first;
    outgoing.extend(app.find_outgoing());
    let mut ranges = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..app.pages().page_count().saturating_add(2) {
        let Some(line) = outgoing.into_iter().next() else {
            return ranges;
        };
        let (id, from, to) = command(&line);
        assert!(!ids.contains(&id), "the same fetch went out twice");
        ids.push(id.clone());
        ranges.push((from, to));
        outgoing = app.on_line(hub_answer(&id, log, from, to));
    }
    panic!("the scan never settled");
}

/// Opens the bar, types `query` and starts the scan, answering every
/// fetch from `log`. Returns every seq range asked for, in order.
pub(super) fn search_all(
    app: &mut App,
    log: &[contract::Envelope],
    query: &str,
) -> Vec<(u64, u64)> {
    if app.find_bar().is_some() {
        assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = app.find.generation();
    for ch in query.chars() {
        generation += 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: FIND_PAUSE,
            }
        );
    }
    let first = app.find_due(generation);
    answer_all(app, log, first)
}

/// The current match's line text, if any.
fn current_text(app: &App) -> Option<String> {
    let current = app.find.current().cloned()?;
    let (rows, texts) = app.pages().page_text_open(current.anchor.page)?;
    crate::logical::logical(&rows, &texts)
        .into_iter()
        .find(|line| {
            line.text.chars().count() == current.anchor.line_len
                && super::scan::line_hash(&line.text) == current.anchor.line_hash
        })
        .map(|line| line.text)
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
    assert_eq!(app.next_request(), Effect::None);
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
fn a_pause_from_before_a_close_does_nothing_after_the_bar_reopens() {
    let mut app = replied(40, 10, "hello there");
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let Effect::FindPause {
        generation: old, ..
    } = app.on_key(Key::Char('h'), now())
    else {
        panic!("typing schedules a pause");
    };
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let Effect::FindPause {
        generation: new, ..
    } = app.on_key(Key::Char('h'), now())
    else {
        panic!("typing schedules a pause");
    };
    assert_ne!(old, new, "a reopened bar never reuses a generation");
    assert!(app.find_due(old).is_empty());
    assert!(!app.find.due(), "the old pause started no scan");
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

#[test]
fn resident_pages_are_scanned_with_no_command() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["a needle here", "plain"]);
    assert_eq!(app.pages().page_count(), 1);
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty(), "a resident scan sends nothing");
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(current_text(&app).as_deref(), Some("a needle here"));
}

#[test]
fn a_dropped_page_is_fetched_with_history_and_scanned_on_its_answer() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["a needle here"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().page_count() > 1);
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    let ranges = search_all(&mut app, &log, "needle");
    assert!(!ranges.is_empty(), "the dropped page is fetched");
    assert_eq!(count(&app), "1 of 1");
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(current.anchor.page, 0);
    // The answer folds into the scan, never into the pages.
    assert!(app.pages().part(0).is_none());
    // The reveal scrolled to the dropped page, so it loads next frame.
    let start = app.pages().index().start(0);
    assert_eq!(app.top(), Some(start));
    let first = log.first().and_then(|line| line.seq).expect("a first seq");
    assert!(app.needs().iter().any(|range| range.contains(&first)));
}

#[test]
fn a_page_over_256_lines_is_fetched_in_chunks() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    for line in 0..300 {
        let text = if line == 290 {
            "a needle here".to_owned()
        } else {
            format!("filler {line}")
        };
        app.on_line(numbered(
            &mut log,
            &mut seq,
            "text_completed",
            json!({"text": text}),
            Some("a_m"),
        ));
    }
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    // One page of 302 lines, dropped below the window.
    assert_eq!(app.pages().page_count(), 1);
    for _ in 0..20 {
        text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    }
    assert!(app.pages().part(0).is_none());
    let first = app
        .pages()
        .index()
        .pages()
        .first()
        .map(|page| (page.first_seq.0, page.last_seq.0));
    let Some((first_seq, last_seq)) = first else {
        panic!("no first page");
    };
    assert!(
        last_seq - first_seq >= 256,
        "fewer than 257 lines: {first_seq}..={last_seq}"
    );
    let ranges = search_all(&mut app, &log, "needle");
    assert_eq!(
        ranges,
        [(first_seq, first_seq + 255), (first_seq + 256, last_seq),]
    );
    assert_eq!(count(&app), "1 of 1");
}

#[test]
fn the_scan_starts_at_the_page_on_screen_and_wraps() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["page zero"]);
    for _ in 0..8 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &[
                "filler", "filler", "filler", "filler", "filler", "filler", "filler", "filler",
            ],
        );
    }
    text_turn(&mut log, &mut seq, &mut app, &["page one"]);
    for _ in 0..8 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &[
                "filler", "filler", "filler", "filler", "filler", "filler", "filler", "filler",
            ],
        );
    }
    text_turn(&mut log, &mut seq, &mut app, &["page two"]);
    assert!(app.pages().page_count() > 2, "fewer than three pages");
    // The view follows at the bottom: the scan starts on the last page.
    let last = app.pages().page_count() - 1;
    let ranges = search_all(&mut app, &log, "page");
    // Only dropped pages are fetched, in scan order past the screen page.
    let starts: Vec<u64> = ranges.into_iter().map(|(from, _)| from).collect();
    let page_starts: Vec<u64> = app
        .pages()
        .index()
        .pages()
        .iter()
        .map(|page| page.first_seq.0)
        .collect();
    assert_eq!(starts.first(), page_starts.first(), "page 0 fetches first");
    assert_eq!(count(&app), "3 of 3");
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(
        current.anchor.page, last,
        "the screen page's match is current"
    );
}

#[test]
fn the_first_match_at_or_after_the_top_row_is_current() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies = vec!["needle above"];
    replies.extend((0..8).map(|_| "filler"));
    replies.push("needle below");
    let refs: Vec<&str> = replies.clone();
    text_turn(&mut log, &mut seq, &mut app, &refs);
    assert_eq!(app.pages().page_count(), 1);
    // The top row between the two matches: only the one below can become
    // current, and nothing scrolls back.
    app.jump(3);
    assert_eq!(app.top(), Some(3));
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "2 of 2");
    assert_eq!(current_text(&app).as_deref(), Some("needle below"));
    assert_eq!(app.top(), Some(3));
}

#[test]
fn with_no_match_below_the_top_the_first_after_wrapping_is_current() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["needle zero"]);
    for _ in 0..8 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &[
                "filler", "filler", "filler", "filler", "filler", "filler", "filler", "filler",
            ],
        );
    }
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &[
            "needle one",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
            "tail",
        ],
    );
    assert!(app.pages().page_count() > 1);
    // Past the last page's match, which sits above the top row: nothing
    // at or after the top anywhere after it, so page 0's match wins.
    let start = app.pages().index().start(1);
    let (rows, _) = app.pages().page_text(1).expect("the last page");
    let at = rows
        .iter()
        .position(|(line, _)| line.to_string().contains("needle one"))
        .expect("the match line");
    app.jump(start + at + 1);
    assert_eq!(app.top(), Some(start + at + 1));
    search_all(&mut app, &log, "needle");
    assert_eq!(count(&app), "1 of 2");
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(current.anchor.page, 0);
}

#[test]
fn a_match_in_a_closed_section_counts_from_its_section_line() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_started",
        json!({}),
        Some("a_r1"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_completed",
        json!({"text": "weigh it\nneedle one"}),
        Some("a_r1"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_started",
        json!({}),
        Some("a_r2"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_completed",
        json!({"text": "weigh it\nneedle two"}),
        Some("a_r2"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "reply"}),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 2");
    let current = app.find.current().cloned().expect("a current match");
    assert!(!current.anchor.scopes.is_empty(), "no section named");
    // Becoming current expands its section: the first thought shows.
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    assert!(
        rows.iter()
            .any(|(line, _)| line.to_string().contains("needle one")),
        "the section did not expand"
    );
    // The other thought stays closed: its match counts from its own
    // line, hidden until it becomes current.
    let other = app.find.flat().get(1).copied().expect("two matches");
    let (row, hidden) = app.current_place(other).expect("a row");
    assert!(hidden, "a closed section's match shows as visible");
    let (line, _) = rows.get(row).expect("the section line");
    assert!(
        line.to_string().contains("Thought"),
        "not the section's own line: {line}"
    );
}

/// A two-page app with `needle` on its dropped first page: the fetch for
/// page 0 on the wire, answered from `log`. Returns the fetch line.
pub(super) fn dropped_fetch() -> (App, Vec<contract::Envelope>, String) {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["a needle here"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().page_count() > 1);
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
    for ch in "needle".chars() {
        generation += 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: FIND_PAUSE,
            }
        );
    }
    let mut first = app.find_due(generation);
    first.extend(app.find_outgoing());
    assert_eq!(first.len(), 1, "one fetch on the wire");
    (app, log, first.into_iter().next().unwrap_or_default())
}

#[test]
fn a_new_query_drops_the_stale_answer() {
    let (mut app, log, fetch) = dropped_fetch();
    let (id, _, _) = command(&fetch);
    // A new query while it is in flight sends nothing new.
    assert_eq!(
        app.on_key(Key::Char('s'), now()),
        Effect::FindPause {
            generation: 7,
            after: FIND_PAUSE,
        }
    );
    assert!(app.find_due(7).is_empty());
    assert!(app.find.fetching(), "the stale fetch stays recorded");
    // Its answer folds nothing: the page stays dropped.
    let (_, from, to) = command(&fetch);
    let outgoing = app.on_line(hub_answer(&id, &log, from, to));
    assert!(app.pages().part(0).is_none());
    // The pump runs for the current generation instead.
    assert_eq!(outgoing.len(), 1);
    let (next, _, _) = command(&outgoing[0]);
    assert_ne!(next, id);
}

#[test]
fn keys_typed_during_a_fetch_are_handled_before_its_answer() {
    let (mut app, log, fetch) = dropped_fetch();
    let (id, _, _) = command(&fetch);
    assert_eq!(
        app.on_key(Key::Char('x'), now()),
        Effect::FindPause {
            generation: 7,
            after: FIND_PAUSE,
        }
    );
    assert_eq!(query(&app), "needlex");
    assert_eq!(app.draft(), "");
    let (_, from, to) = command(&fetch);
    assert!(app.on_line(hub_answer(&id, &log, from, to)).is_empty());
    assert_eq!(query(&app), "needlex");
    // Its pause then starts the new query's scan: the next request goes
    // out for it.
    let outgoing = app.find_due(7);
    assert_eq!(outgoing.len(), 1, "the new query fetches next");
    let (next, _, _) = command(&outgoing[0]);
    assert_ne!(next, id);
}

#[test]
fn a_rejected_answer_marks_the_search_incomplete_with_a_notice() {
    let (mut app, _, fetch) = dropped_fetch();
    let (id, _, _) = command(&fetch);
    let rejected = Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "message": "past the latest line"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    let outgoing = app.on_line(rejected);
    assert!(outgoing.is_empty(), "a rejected page is not fetched again");
    assert!(count(&app).ends_with(" · incomplete"));
    assert_eq!(
        app.notice(),
        Some("Could not search all of history: past the latest line")
    );
}

#[test]
fn an_unreadable_answer_marks_it_incomplete() {
    let (mut app, _, fetch) = dropped_fetch();
    let (id, _, _) = command(&fetch);
    let unreadable = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "result": {}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(app.on_line(unreadable).is_empty());
    assert!(count(&app).ends_with(" · incomplete"));
    assert_eq!(
        app.notice(),
        Some("Could not search all of history: the answer could not be read")
    );
}

#[test]
fn a_lost_link_marks_it_incomplete() {
    let (mut app, _, _) = dropped_fetch();
    app.disconnected();
    assert!(!app.find.fetching());
    assert!(count(&app).ends_with(" · incomplete"));
    assert_eq!(
        app.notice(),
        Some("Could not search all of history: connection lost")
    );
}

#[test]
fn live_output_rescans_the_open_page() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "nothing here"}),
        Some("a_m"),
    ));
    let ranges = search_all(&mut app, &log, "cat");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "no matches");
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "a cat sat"}),
        Some("a_n"),
    ));
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(current_text(&app).as_deref(), Some("a cat sat"));
}

#[test]
fn an_equal_length_reply_change_rescans_the_open_page() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "cat"}),
        Some("a_m"),
    ));
    let ranges = search_all(&mut app, &log, "cat");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    // The completion replaces the streamed text with equal length: the
    // card changes, so the page rescans and the match is gone.
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "dog"}),
        Some("a_m"),
    ));
    assert_eq!(app.find.total(), 0);
    assert!(app.find.current().is_none());
    assert_eq!(count(&app), "no matches");
}

#[test]
fn typing_starts_no_scan_until_the_pause() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["abc here"]);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    for (at, ch) in "abc".chars().enumerate() {
        let generation = u64::try_from(at).unwrap_or(0) + 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: FIND_PAUSE,
            }
        );
    }
    assert!(app.find_due(1).is_empty());
    assert!(app.find_due(2).is_empty());
    assert_eq!(count(&app), "");
    assert_eq!(app.find.total(), 0);
    assert!(!app.find.due());
    assert!(!app.find_due(3).is_empty() || app.find.total() > 0);
    assert_eq!(count(&app), "1 of 1");
}

#[test]
fn an_earlier_generation_due_does_nothing() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["abc here"]);
    let ranges = search_all(&mut app, &log, "abc");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    // A flipped guard would scan again on the stale pause.
    assert!(app.find_due(1).is_empty());
    assert!(app.find_due(2).is_empty());
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(app.find.total(), 1);
}

#[test]
fn a_query_change_during_a_fetch_sends_nothing_new() {
    let (mut app, log, fetch) = dropped_fetch();
    let (id, _, _) = command(&fetch);
    assert_eq!(
        app.on_key(Key::Char('s'), now()),
        Effect::FindPause {
            generation: 7,
            after: FIND_PAUSE,
        }
    );
    // The new generation scans its resident pages but sends nothing: one
    // request stays on the wire.
    let outgoing = app.find_due(7);
    assert!(outgoing.is_empty());
    assert!(app.find_outgoing().is_empty());
    let (_, from, to) = command(&fetch);
    let outgoing = app.on_line(hub_answer(&id, &log, from, to));
    assert!(
        app.pages().part(0).is_none(),
        "the stale answer folds nothing"
    );
    assert_eq!(outgoing.len(), 1, "then the next request goes out");
}

#[test]
fn a_page_cut_scans_the_new_page() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    for _ in 0..20 {
        text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    }
    let ranges = search_all(&mut app, &log, "cat");
    assert_eq!(count(&app), "no matches");
    // A new turn cuts a page: the page never scanned scans too.
    let before = app.pages().page_count();
    text_turn(&mut log, &mut seq, &mut app, &["a cat sat"]);
    assert!(app.pages().page_count() >= before);
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(current_text(&app).as_deref(), Some("a cat sat"));
    let _ = ranges;
}

#[test]
fn the_cap_stops_the_scan_at_10000() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    for _ in 0..2 {
        app.on_line(numbered(
            &mut log,
            &mut seq,
            "text_completed",
            json!({"text": "x ".repeat(6000)}),
            Some("a_m"),
        ));
    }
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert_eq!(
        app.on_key(Key::Char('x'), now()),
        Effect::FindPause {
            generation: 1,
            after: FIND_PAUSE,
        }
    );
    assert!(app.find_due(1).is_empty(), "no page drops: no fetch");
    assert!(!app.find.fetching());
    assert_eq!(app.find.total(), 10_000);
    assert!(app.find.capped());
    assert_eq!(count(&app), "1 of 10000+");
}

#[test]
fn an_empty_query_clears_everything() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["xy here"]);
    let ranges = search_all(&mut app, &log, "xy");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(
        app.on_key(Key::Backspace, now()),
        Effect::FindPause {
            generation: 3,
            after: FIND_PAUSE
        }
    );
    assert_eq!(
        app.on_key(Key::Backspace, now()),
        Effect::FindPause {
            generation: 4,
            after: FIND_PAUSE
        }
    );
    assert_eq!(query(&app), "");
    // An empty query starts no scan, even at the current generation.
    assert!(app.find_due(4).is_empty());
    assert!(!app.find.due());
    assert_eq!(app.find.total(), 0);
    assert!(app.find.current().is_none());
    assert_eq!(count(&app), "");
}

#[test]
fn a_match_across_a_soft_wrap_matches_the_unwrapped_text() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let reply = format!("{} brown fox jumps", "x".repeat(33));
    text_turn(&mut log, &mut seq, &mut app, &[reply.as_str()]);
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    assert!(rows.len() > 3, "the reply wraps: {}", rows.len());
    let ranges = search_all(&mut app, &log, "brown fox");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    // The anchor names the whole unwrapped line, not a screen row.
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(current.anchor.line_len, reply.chars().count());
}

#[test]
fn a_new_width_keeps_the_matches() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["a needle here"]);
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    let before = app.find.current().cloned().expect("a current match");
    app.set_size(50, 10);
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(
        app.find.current().cloned().map(|kept| kept.anchor),
        Some(before.anchor)
    );
}

#[test]
fn a_dropped_pages_usage_change_refetches_it() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let usage = |tokens: u64| {
        json!({"generation_id": "g0", "model": "fake/m",
            "tokens": {"input": tokens, "cache_read": 0, "cache_write": {}, "output": 40},
            "input_bytes": 0, "cost": null})
    };
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "old words"}),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "usage_recorded",
        usage(100),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    search_all(&mut app, &log, "zzz");
    assert_eq!(count(&app), "no matches");
    // A late usage line for its turn moves its ▣ line: the dropped page
    // scans again, fetched from the hub.
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "usage_recorded",
        usage(200),
        Some("a_m"),
    ));
    let outgoing = app.find_outgoing();
    assert_eq!(outgoing.len(), 1, "the dropped page refetches");
    let range = app
        .pages()
        .index()
        .pages()
        .first()
        .map(|page| (page.first_seq.0, page.last_seq.0));
    let (_, from, to) = command(&outgoing[0]);
    assert_eq!(Some((from, to)), range);
}

#[test]
fn rescanning_keeps_the_current_match_when_an_earlier_one_appears() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies = vec!["needle one"];
    replies.extend((0..10).map(|_| "filler"));
    replies.push("needle two");
    let refs: Vec<&str> = replies.clone();
    text_turn(&mut log, &mut seq, &mut app, &refs);
    app.jump(3);
    assert_eq!(app.top(), Some(3));
    search_all(&mut app, &log, "needle");
    assert_eq!(current_text(&app).as_deref(), Some("needle two"));
    let current = app.find.current().cloned().expect("a current match");
    // Scrolling up makes the earlier match eligible, and live output
    // rescans: the current match stays.
    app.jump(0);
    text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    assert_eq!(
        app.find.current().cloned().map(|kept| kept.anchor),
        Some(current.anchor)
    );
    assert_eq!(current_text(&app).as_deref(), Some("needle two"));
}

#[test]
fn the_current_falls_to_the_next_match_when_its_line_is_gone() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "alpha needle"}),
        Some("a_m1"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "beta needle"}),
        Some("a_m2"),
    ));
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    assert_eq!(current_text(&app).as_deref(), Some("alpha needle"));
    // Its line rewritten without the query: the completion replaces the
    // streamed text, the next match becomes current, and nothing is
    // revealed twice.
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "alpha gone"}),
        Some("a_m1"),
    ));
    assert_eq!(count(&app), "1 of 1");
    assert_eq!(current_text(&app).as_deref(), Some("beta needle"));
}

#[test]
fn matches_stay_in_page_then_render_order_after_rescans() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["zero a", "zero b"]);
    for _ in 0..8 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &[
                "filler", "filler", "filler", "filler", "filler", "filler", "filler", "filler",
            ],
        );
    }
    text_turn(&mut log, &mut seq, &mut app, &["one a"]);
    search_all(&mut app, &log, "zero");
    // A rescan replaces whole pages: the order stays page order, then
    // render order.
    text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    let flat: Vec<(usize, usize)> = app
        .find
        .flat()
        .into_iter()
        .map(|kept| (kept.anchor.page, kept.nth))
        .collect();
    assert_eq!(flat, [(0, 0), (0, 1)]);
    search_all(&mut app, &log, "a");
    let flat: Vec<(usize, usize)> = app
        .find
        .flat()
        .into_iter()
        .map(|kept| (kept.anchor.page, kept.nth))
        .collect();
    assert!(!flat.is_empty());
    let mut ordered = flat.clone();
    ordered.sort();
    assert_eq!(flat, ordered, "page order, then render order");
}

/// An app with three `needle` replies on one page.
pub(super) fn three_matches() -> (App, Vec<contract::Envelope>) {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["needle one", "needle two", "needle three"],
    );
    (app, log)
}

#[test]
fn enter_and_down_move_to_the_next_match_and_wrap() {
    let (mut app, log) = three_matches();
    search_all(&mut app, &log, "needle");
    assert_eq!(count(&app), "1 of 3");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(count(&app), "2 of 3");
    assert_eq!(current_text(&app).as_deref(), Some("needle two"));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(count(&app), "3 of 3");
    assert_eq!(current_text(&app).as_deref(), Some("needle three"));
    // Wraps past the end.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(count(&app), "1 of 3");
    assert_eq!(current_text(&app).as_deref(), Some("needle one"));
}

#[test]
fn up_and_shift_enter_move_back_and_wrap() {
    let (mut app, log) = three_matches();
    search_all(&mut app, &log, "needle");
    assert_eq!(count(&app), "1 of 3");
    // Wraps back from the first.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(count(&app), "3 of 3");
    assert_eq!(current_text(&app).as_deref(), Some("needle three"));
    assert_eq!(app.on_edit(Edit::ShiftEnter), Effect::None);
    assert_eq!(count(&app), "2 of 3");
    assert_eq!(current_text(&app).as_deref(), Some("needle two"));
}

#[test]
fn the_count_reads_3_of_41() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let needles: Vec<String> = (0..41).map(|n| format!("needle {n}")).collect();
    let refs: Vec<&str> = needles.iter().map(String::as_str).collect();
    for chunk in refs.chunks(10) {
        text_turn(&mut log, &mut seq, &mut app, chunk);
    }
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    let index: usize = count(&app)
        .split(' ')
        .next()
        .and_then(|first| first.parse().ok())
        .expect("an index");
    assert!(count(&app).ends_with(" of 41"));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(count(&app), format!("{} of 41", index + 2));
}

#[test]
fn the_count_shows_scanning_and_incomplete() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["nothing here"]);
    for turn in 0..34 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().page_count() > 2);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
    for ch in "needle".chars() {
        generation += 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: FIND_PAUSE,
            }
        );
    }
    let mut outgoing = app.find_due(generation);
    outgoing.extend(app.find_outgoing());
    assert_eq!(outgoing.len(), 1);
    assert_eq!(count(&app), "…");
    // Its answer rejected: incomplete, while the next page still scans.
    let (id, _, _) = command(&outgoing[0]);
    let rejected = Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id, "message": "gone"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    let outgoing = app.on_line(rejected);
    assert_eq!(outgoing.len(), 1, "the next page still scans");
    assert_eq!(count(&app), "… · incomplete");
}

#[test]
fn a_match_in_a_closed_ledger_expands_it_when_current() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_started",
        json!({}),
        Some("a_r"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_completed",
        json!({"text": "weigh it\nneedle here"}),
        Some("a_r"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "reply"}),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    assert!(
        rows.iter()
            .any(|(line, _)| line.to_string().contains("needle here")),
        "the ledger did not expand"
    );
}

#[test]
fn a_match_in_a_closed_call_detail_opens_its_group_and_call() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "tool_call_requested",
        json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
        Some("a_t"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "tool_call_completed",
        json!({"status": "completed", "content": [{"type": "text", "text": "needle inside detail"}]}),
        Some("a_t"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "reply"}),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    // The match sits inside the group and the call: both open, outermost
    // first, and stay open.
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(current.anchor.scopes.len(), 2);
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    let texts: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        texts.iter().any(|line| line.contains("read")),
        "the group did not open"
    );
    assert!(
        texts
            .iter()
            .any(|line| line.contains("needle inside detail")),
        "the call did not open: {texts:?}"
    );
}

#[test]
fn an_expansion_clears_the_selection() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_started",
        json!({}),
        Some("a_r"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "reasoning_completed",
        json!({"text": "weigh it\nneedle here"}),
        Some("a_r"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "reply words"}),
        Some("a_m"),
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    let at = find(&app, "reply");
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Left), at),
        Effect::None
    );
    assert_eq!(
        report(&mut app, MouseKind::Drag(Button::Left), (at.0 + 2, at.1)),
        Effect::None
    );
    assert!(app.select.span().is_some());
    search_all(&mut app, &log, "needle");
    assert!(
        app.select.span().is_none(),
        "the expansion clears the selection"
    );
}

#[test]
fn revealing_scrolls_only_when_the_match_is_not_shown() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies = vec!["filler", "filler", "filler", "filler", "needle five"];
    replies.extend((0..22).map(|_| "filler"));
    replies.push("needle far");
    let refs: Vec<&str> = replies.clone();
    text_turn(&mut log, &mut seq, &mut app, &refs);
    app.jump(2);
    assert_eq!(app.top(), Some(2));
    search_all(&mut app, &log, "needle");
    // The first match shows: no scroll.
    assert_eq!(current_text(&app).as_deref(), Some("needle five"));
    assert_eq!(app.top(), Some(2));
    // The next does not: the view scrolls to it.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(current_text(&app).as_deref(), Some("needle far"));
    let (rows, _) = app.pages().page_text(0).expect("the resident page");
    let at = rows
        .iter()
        .position(|(line, _)| line.to_string().contains("needle far"))
        .expect("the far match");
    let (top, _) = app.scroll();
    let height = app.conversation_height();
    assert_ne!(top, 2);
    assert!((top..top.saturating_add(height)).contains(&at));
}

#[test]
fn revealing_a_match_on_a_dropped_page_loads_it_in_the_same_step() {
    let (mut app, log, fetch) = dropped_fetch();
    let (id, from, to) = command(&fetch);
    app.on_line(hub_answer(&id, &log, from, to));
    // The reveal already scrolled while answering: the dropped page is
    // what the frame needs next.
    let start = app.pages().index().start(0);
    assert_eq!(app.top(), Some(start));
    let first = log.first().and_then(|line| line.seq).expect("a first seq");
    assert!(app.needs().iter().any(|range| range.contains(&first)));
}

#[test]
fn an_approval_takes_typing_and_esc_over_the_bar() {
    let mut app = attached(40, 10);
    search(&mut app, "ab");
    // A request opens the approval panel over the bar.
    app.on_line(request("a_1", "r_1"));
    assert!(app.panel().is_some());
    assert_eq!(app.on_key(Key::Char('x'), now()), Effect::None);
    assert_eq!(query(&app), "ab", "typing reaches the approval first");
    // Esc puts the approval aside; the bar stays open with its query.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.panel().is_none());
    assert_eq!(query(&app), "ab");
}

#[test]
fn ctrl_f_opens_the_bar_while_an_approval_waits() {
    let mut app = attached(40, 10);
    app.on_line(request("a_1", "r_1"));
    assert!(app.panel().is_some());
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_bar().is_some());
}

#[test]
fn the_ctrl_r_panel_takes_typing_over_the_bar() {
    let mut app = attached(40, 10);
    search(&mut app, "");
    app.open_search();
    assert!(app.search_panel().is_some());
    assert_eq!(app.on_key(Key::Char('x'), now()), Effect::None);
    assert_eq!(query(&app), "");
    let panel = app.search_panel().expect("the panel keeps typing");
    assert!(panel.lines.first().is_some_and(|line| line.ends_with('x')));
}

#[test]
fn esc_closes_the_notice_overlay_then_the_bar_then_the_selection() {
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
    app.notices.push("first".to_owned());
    app.notices.push("second".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    // The overlay first, the bar and the selection staying.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.find_bar().is_some());
    assert!(app.select.span().is_some());
    // Then the bar, the selection staying.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.find_bar().is_none());
    assert!(app.select.span().is_some());
    // Then the selection.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.select.span().is_none());
}

#[test]
fn a_page_cut_after_the_query_starts_scans_the_new_page() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    for _ in 0..5 {
        text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    }
    assert_eq!(app.pages().page_count(), 1);
    let ranges = search_all(&mut app, &log, "cat");
    assert!(ranges.is_empty(), "one resident page sends nothing");
    assert_eq!(count(&app), "no matches");
    // Turns cut a second page after the query started: with no new
    // query its matches are found and counted.
    let mut added = 0;
    while app.pages().page_count() == 1 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &["a cat sat", "filler", "filler", "filler"],
        );
        added += 1;
        assert!(added < 40, "no new page was cut");
    }
    assert!(app.pages().part(0).is_some());
    assert!(app.pages().part(1).is_some());
    for at in 0..app.pages().page_count() {
        assert!(app.find.scanned(at).is_some(), "page {at} never scanned");
    }
    assert_eq!(app.find.total(), added);
    assert!(count(&app).ends_with(&format!("of {added}")));
}

#[test]
fn identical_lines_walk_one_occurrence_at_a_time() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["echo needle", "echo needle"],
    );
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    // Two identical lines anchor alike; only the occurrence index
    // tells them apart.
    assert_eq!(app.find.flat().len(), 2);
    assert_eq!(count(&app), "1 of 2");
    let current = app.find.current().cloned().expect("a current match");
    let (first_row, _) = app.current_place(&current).expect("a row");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(count(&app), "2 of 2");
    let current = app.find.current().cloned().expect("a current match");
    assert_eq!(current.nth, 1);
    let (second_row, _) = app.current_place(&current).expect("a row");
    assert_ne!(first_row, second_row);
    // Wraps past the end.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(count(&app), "1 of 2");
    assert_eq!(app.find.current().cloned().map(|kept| kept.nth), Some(0));
    // Only one occurrence is current on screen at a time.
    let marks = app.find_marks(Rect::new(0, 0, 40, 10));
    assert!(!marks.is_empty());
    let mut rows: Vec<u16> = marks
        .iter()
        .filter(|(_, current)| *current)
        .map(|(rect, _)| rect.y)
        .collect();
    rows.sort();
    rows.dedup();
    assert_eq!(rows.len(), 1);
}

/// An app with one `needle` line above fifteen filler lines, searched.
/// Returns the app with its log and the match's conversation row.
fn needle_above_fillers() -> (App, Vec<contract::Envelope>, usize) {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies = vec!["needle top"];
    replies.extend((0..15).map(|_| "filler"));
    text_turn(&mut log, &mut seq, &mut app, &replies);
    search_all(&mut app, &log, "needle");
    assert_eq!(count(&app), "1 of 1");
    let current = app.find.current().cloned().expect("a current match");
    let (row, _) = app.current_place(&current).expect("a row");
    (app, log, row)
}

#[test]
fn a_match_on_the_top_row_marks_it() {
    let (mut app, _, row) = needle_above_fillers();
    app.jump(row);
    assert_eq!(app.top(), Some(row));
    assert!(!app.find_marks(Rect::new(0, 0, 40, 10)).is_empty());
}

#[test]
fn a_match_one_row_above_marks_nothing() {
    let (mut app, _, row) = needle_above_fillers();
    app.jump(row.saturating_add(1));
    assert_eq!(app.top(), Some(row.saturating_add(1)));
    assert!(app.find_marks(Rect::new(0, 0, 40, 10)).is_empty());
}

fn resident_match_and_dropped_fetch() -> (App, Vec<contract::Envelope>, String) {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    text_turn(&mut log, &mut seq, &mut app, &["a needle here"]);
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
    for ch in "needle".chars() {
        generation += 1;
        assert_eq!(
            app.on_key(Key::Char(ch), now()),
            Effect::FindPause {
                generation,
                after: FIND_PAUSE,
            }
        );
    }
    let mut first = app.find_due(generation);
    first.extend(app.find_outgoing());
    assert_eq!(first.len(), 1, "one fetch on the wire");
    (app, log, first.into_iter().next().unwrap_or_default())
}

#[test]
fn the_count_says_scanning_beside_the_matches_it_has_kept() {
    let (mut app, log, fetch) = resident_match_and_dropped_fetch();
    assert_eq!(count(&app), "1 of 1…");
    let ranges = answer_all(&mut app, &log, vec![fetch]);
    assert_eq!(ranges.len(), 1);
    assert_eq!(count(&app), "1 of 1");
}

#[test]
fn a_mark_on_the_right_edge_is_not_drawn() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["xxxxx needle"]);
    search_all(&mut app, &log, "needle");
    // The match starts at column 6: an area six wide has no room for it.
    assert!(app.find_marks(Rect::new(0, 0, 6, 10)).is_empty());
    assert_eq!(app.find_marks(Rect::new(0, 0, 7, 10)).len(), 1);
}

#[test]
fn a_mark_one_row_below_the_viewport_is_not_drawn() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies: Vec<&str> = (0..30).map(|_| "filler").collect();
    replies.push("needle top");
    replies.extend((0..15).map(|_| "filler"));
    text_turn(&mut log, &mut seq, &mut app, &replies);
    search_all(&mut app, &log, "needle");
    let current = app.find.current().cloned().expect("a current match");
    let (row, _) = app.current_place(&current).expect("a row");
    // The match on the viewport's last row shows; one row lower it does not.
    // "needle" is six chars, one mark each.
    app.jump(row.saturating_sub(9));
    assert_eq!(app.find_marks(Rect::new(0, 0, 40, 10)).len(), 6);
    app.jump(row.saturating_sub(10));
    assert!(app.find_marks(Rect::new(0, 0, 40, 10)).is_empty());
}

#[test]
fn a_match_on_the_last_row_marks_it_when_that_row_is_on_top() {
    // A one-row screen: the session's last row is the only row it can top.
    let mut app = attached(40, 1);
    let mut log = Vec::new();
    let mut seq = 0u64;
    // A running turn: its last row is the reply, not a completed marker.
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "text_completed",
        json!({"text": "needle last"}),
        Some("a_m"),
    ));
    search_all(&mut app, &log, "needle");
    let current = app.find.current().cloned().expect("a current match");
    let (row, _) = app.current_place(&current).expect("a row");
    app.jump(row);
    assert_eq!(app.top(), Some(row));
    assert_eq!(app.find_marks(Rect::new(0, 0, 40, 1)).len(), 6);
}

#[test]
fn a_match_across_a_soft_wrap_marks_each_drawn_cell_once() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let reply = format!("{} brown fox jumps", "x".repeat(33));
    text_turn(&mut log, &mut seq, &mut app, &[reply.as_str()]);
    search_all(&mut app, &log, "brown fox");
    let marks = app.find_marks(Rect::new(0, 0, 40, 10));
    let mut cells: Vec<(u16, u16)> = marks.iter().map(|(rect, _)| (rect.x, rect.y)).collect();
    let drawn = cells.len();
    cells.sort();
    cells.dedup();
    assert_eq!(cells.len(), drawn, "a cell marked twice");
    assert_eq!(drawn, 8, "brown and fox, the wrap's space is no cell");
}

#[test]
fn a_zero_width_char_in_a_match_marks_only_the_cells_it_draws() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["ab\u{200b}cd"]);
    let ranges = search_all(&mut app, &log, "b\u{200b}c");
    assert!(ranges.is_empty());
    assert_eq!(count(&app), "1 of 1");
    // The zero-width char draws no cell: the match marks `b` and `c` once.
    let marks = app.find_marks(Rect::new(0, 0, 40, 10));
    assert_eq!(marks.len(), 2, "{marks:?}");
}

#[test]
fn find_matches_counts_the_kept_matches() {
    let (mut app, log) = three_matches();
    assert!(search_all(&mut app, &log, "needle").is_empty());
    assert_eq!(app.find_matches(), 3);
}

#[test]
fn a_page_of_257_lines_keeps_its_first_chunk_when_its_last_line_is_fetched_alone() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    // A turn of 257 lines: its start, 255 replies and its end.
    let replies: Vec<String> = (0..255)
        .map(|line| {
            if line == 0 {
                "a needle here".to_owned()
            } else {
                format!("filler {line}")
            }
        })
        .collect();
    let refs: Vec<&str> = replies.iter().map(String::as_str).collect();
    text_turn(&mut log, &mut seq, &mut app, &refs);
    for _ in 0..20 {
        text_turn(&mut log, &mut seq, &mut app, &["filler"]);
    }
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    let (first_seq, last_seq) = app
        .pages()
        .index()
        .pages()
        .first()
        .map(|page| (page.first_seq.0, page.last_seq.0))
        .expect("a first page");
    assert_eq!(last_seq - first_seq, 256, "the page holds 257 lines");
    let ranges = search_all(&mut app, &log, "needle");
    assert_eq!(
        ranges,
        [(first_seq, first_seq + 255), (first_seq + 256, last_seq)]
    );
    // The match sits in the first chunk: the last line's fetch keeps it.
    assert_eq!(count(&app), "1 of 1");
}
