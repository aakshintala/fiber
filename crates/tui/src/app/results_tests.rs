//! Tests for the search results as a swapped view (`docs/tui.md`,
//! "Search"): opening from the bar and the count, moving, jumping and
//! closing, against the app and the view it draws.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::find::FIND_PAUSE;
use crate::app::Snippet;
use crate::app::{App, Effect};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::results_support::{SESSION, attached, now, reply};

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

/// A turn started by `prompt`; it runs until done.
fn prompt(app: &mut App, text: &str) {
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
        None,
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

/// An app showing one turn of `replies`, done.
fn turned(width: u16, height: u16, replies: &[&str]) -> App {
    let mut app = attached(width, height);
    for text in replies {
        reply(&mut app, "a_m", text);
    }
    done(&mut app);
    app
}

/// Types `query` into the search bar and starts its scan: resident pages
/// scan at once, so with nothing dropped nothing goes out.
fn scan(app: &mut App, query: &str) {
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    let mut generation = 0u64;
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
    assert!(app.find_due(generation).is_empty());
}

/// Opens the results with a second Ctrl+F.
fn open(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.results_open(), "a second Ctrl+F opens the results");
}

/// The bar's query.
fn query(app: &App) -> String {
    app.find_bar().map(|bar| bar.query).unwrap_or_default()
}

/// The bar's count.
fn count(app: &App) -> String {
    app.find_bar().map(|bar| bar.count).unwrap_or_default()
}

/// The selected entry and the top row drawn.
fn cursor(app: &App) -> (usize, usize) {
    let view = app.find_results().expect("the results are open");
    (view.selected, view.top)
}

/// The app drawn at its size, with its targets.
fn draw(app: &App, width: u16, height: u16) -> (Buffer, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// One mouse report against the targets the app draws now.
fn report(app: &mut App, kind: MouseKind, (col, row): (u16, u16)) -> Effect {
    let (_, targets) = draw(app, 40, 10);
    app.on_select(&Mouse { kind, col, row }, &targets)
}

/// Types into the draft while the bar is closed.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

#[test]
fn ctrl_f_twice_opens_the_results() {
    let mut app = turned(40, 10, &["a needle here"]);
    scan(&mut app, "needle");
    assert_eq!(count(&app), "1 of 1");
    assert!(!app.results_open());
    open(&mut app);
    let view = app.find_results().expect("open");
    assert_eq!(view.header, "1 matches for “needle”");
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].line, "a needle here");
    assert_eq!((view.selected, view.top), (0, 0));
    assert!(app.find_bar().is_some(), "the bar stays open");
}

#[test]
fn a_click_on_the_count_opens_them() {
    let mut app = turned(40, 10, &["a needle here"]);
    scan(&mut app, "needle");
    let (_, targets) = draw(&app, 40, 10);
    let id = targets
        .iter()
        .find(|target| target.id == TargetId::FindCount)
        .map(|target| target.id)
        .expect("the count is a target");
    assert_eq!(app.on_click(id), Effect::None);
    assert!(app.results_open());
}

#[test]
fn each_entry_is_its_line_with_one_line_either_side() {
    let mut app = turned(40, 10, &["before", "needle", "after"]);
    scan(&mut app, "needle");
    open(&mut app);
    let view = app.find_results().expect("open");
    assert_eq!(view.header, "1 matches for “needle”");
    assert_eq!(
        view.entries,
        vec![Snippet {
            before: "before".to_owned(),
            line: "needle".to_owned(),
            at: 0..6,
            after: "after".to_owned(),
        }]
    );
}

#[test]
fn up_down_and_page_keys_move_the_selection() {
    let replies: Vec<String> = (0..12).map(|n| format!("match {n}")).collect();
    let refs: Vec<&str> = replies.iter().map(String::as_str).collect();
    let mut app = turned(40, 10, &refs);
    scan(&mut app, "match");
    open(&mut app);
    // Up holds at the first entry however far it would go.
    for _ in 0..20 {
        assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    }
    assert_eq!(cursor(&app), (0, 0));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(cursor(&app), (1, 0));
    assert_eq!(app.on_key(Key::Char('j'), now()), Effect::None);
    assert_eq!(cursor(&app), (2, 0));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(cursor(&app), (1, 0));
    assert_eq!(app.on_key(Key::Char('k'), now()), Effect::None);
    assert_eq!(cursor(&app), (0, 0));
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(cursor(&app), (0, 0));
    // A screen is the view's entry rows: the conversation's nine less
    // the header.
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(cursor(&app), (8, 1));
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(cursor(&app), (11, 4));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(cursor(&app), (11, 4));
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(cursor(&app), (11, 4));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(cursor(&app), (3, 3));
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(cursor(&app), (0, 0));
    assert!(app.results_open(), "moving never closes the view");
}

#[test]
fn enter_jumps_to_the_match_and_closes_the_view() {
    let mut app = turned(40, 10, &["needle one", "needle two", "needle three"]);
    scan(&mut app, "needle");
    open(&mut app);
    for _ in 0..20 {
        assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(!app.results_open(), "the jump closes the view");
    assert!(app.find_bar().is_some(), "the bar stays open");
    assert_eq!(query(&app), "needle");
    assert_eq!(count(&app), "2 of 3");
}

#[test]
fn a_click_on_an_entry_jumps() {
    let mut app = turned(40, 10, &["needle one", "needle two", "needle three"]);
    scan(&mut app, "needle");
    open(&mut app);
    let (_, targets) = draw(&app, 40, 10);
    let id = targets
        .iter()
        .find(|target| target.id == TargetId::FindResult(2))
        .map(|target| target.id)
        .expect("every entry is a target");
    assert_eq!(app.on_click(id), Effect::None);
    assert!(!app.results_open());
    assert!(app.find_bar().is_some(), "the bar stays open");
    assert_eq!(count(&app), "3 of 3");
}

#[test]
fn esc_ctrl_f_and_the_cross_close_with_nothing_changed() {
    let mut app = turned(40, 10, &["needle one", "needle two", "needle three"]);
    scan(&mut app, "needle");
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let current = count(&app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.results_open());
    assert_eq!(count(&app), current);
    open(&mut app);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(!app.results_open());
    assert_eq!(count(&app), current);
    open(&mut app);
    let (_, targets) = draw(&app, 40, 10);
    let id = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .map(|target| target.id)
        .expect("the view draws its ✕");
    app.on_click(id);
    assert!(!app.results_open());
    assert_eq!(count(&app), current);
    assert_eq!(query(&app), "needle");
}

#[test]
fn esc_closes_the_notice_overlay_before_the_results() {
    let mut app = turned(40, 10, &["needle one", "needle two"]);
    scan(&mut app, "needle");
    open(&mut app);
    app.notices.push("first".to_owned());
    app.notices.push("second".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    // Esc closes the overlay first, the results staying open.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.notice_overlay().is_none());
    assert!(app.results_open());
    // The ✕ does the same: the overlay goes, the results stay.
    app.open_more_notices();
    let (_, targets) = draw(&app, 40, 10);
    let id = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .map(|target| target.id)
        .expect("the overlay draws its ✕");
    app.on_click(id);
    assert!(app.notice_overlay().is_none());
    assert!(app.results_open());
    // Then Esc closes the results, the bar staying open.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.results_open());
    assert!(app.find_bar().is_some());
}

#[test]
fn the_input_box_keeps_its_draft() {
    let mut app = turned(40, 10, &["a needle here"]);
    type_text(&mut app, "hello");
    scan(&mut app, "needle");
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.draft(), "hello");
}

#[test]
fn typing_with_the_results_open_edits_the_query_and_closes_them() {
    let mut app = turned(40, 10, &["a needle here"]);
    scan(&mut app, "needle");
    open(&mut app);
    assert!(matches!(
        app.on_key(Key::Char('x'), now()),
        Effect::FindPause { .. }
    ));
    assert!(!app.results_open(), "a new query closes the view");
    assert_eq!(query(&app), "needlex");
}

#[test]
fn enter_with_no_matches_jumps_nothing() {
    let mut app = turned(40, 10, &["hello there"]);
    scan(&mut app, "zzz");
    assert_eq!(count(&app), "no matches");
    open(&mut app);
    let view = app.find_results().expect("open with no matches");
    assert_eq!(view.header, "0 matches for “zzz”");
    assert!(view.entries.is_empty());
    for key in [Key::Down, Key::Up, Key::PageDown, Key::PageUp] {
        assert_eq!(app.on_key(key, now()), Effect::None);
    }
    assert_eq!(cursor(&app), (0, 0));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.results_open(), "nothing to jump to leaves the view");
}

#[test]
fn a_count_cut_off_by_the_bar_is_no_target() {
    let mut app = turned(40, 10, &["needle here"]);
    scan(&mut app, &"z".repeat(35));
    assert_eq!(count(&app), "no matches");
    let (buf, targets) = draw(&app, 40, 10);
    assert!(
        targets
            .iter()
            .all(|target| target.id != TargetId::FindCount),
        "a count the bar cuts off is no target"
    );
    assert!(
        crate::view::text(&buf).contains("find: "),
        "the bar itself still draws"
    );
}

#[test]
fn no_selection_starts_over_the_view() {
    let mut app = turned(40, 10, &["needle one", "needle two"]);
    scan(&mut app, "needle");
    open(&mut app);
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Left), (5, 1)),
        Effect::None
    );
    assert_eq!(
        report(&mut app, MouseKind::Drag(Button::Left), (10, 2)),
        Effect::None
    );
    assert_eq!(report(&mut app, MouseKind::Release, (10, 2)), Effect::None);
    assert_eq!(app.select.span(), None);
    assert!(!app.copied());
}

#[test]
fn the_selected_entry_stays_on_its_occurrence_when_matches_appear_before_it() {
    let mut app = attached(40, 10);
    // The pads stream first, so completing them later replaces their
    // lines instead of appending.
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "pad one"}),
        Some("a_p1"),
    ));
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "pad two"}),
        Some("a_p2"),
    ));
    reply(&mut app, "a_m1", "echo needle");
    reply(&mut app, "a_m2", "echo needle");
    scan(&mut app, "needle");
    assert_eq!(count(&app), "1 of 2");
    open(&mut app);
    assert_eq!(cursor(&app), (0, 0));
    // Two more matches appear before the identical lines: the first
    // occurrence is now the third, and the selection stays on it.
    reply(&mut app, "a_p1", "zero needle");
    reply(&mut app, "a_p2", "one needle");
    assert_eq!(count(&app), "3 of 4");
    let view = app.find_results().expect("open");
    assert_eq!(view.entries.len(), 4);
    assert_eq!((view.selected, view.top), (2, 0));
    assert_eq!(view.entries[2].line, "echo needle");
}

#[test]
fn an_equal_match_either_side_of_the_old_ordinal_selects_the_first() {
    let mut app = attached(40, 10);
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "alpha needle"}),
        Some("a_p1"),
    ));
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "echo needle"}),
        Some("a_m"),
    ));
    app.on_line(line(
        "assistant_message_delta",
        json!({"text": "beta needle"}),
        Some("a_p2"),
    ));
    scan(&mut app, "needle");
    assert_eq!(count(&app), "1 of 3");
    open(&mut app);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(cursor(&app), (1, 0));
    // The middle line stops matching while the outer lines become the
    // same occurrence: both are one ordinal away, so the first wins.
    reply(&mut app, "a_m", "zebra needle");
    reply(&mut app, "a_p1", "echo needle");
    reply(&mut app, "a_p2", "echo needle");
    assert_eq!(count(&app), "2 of 3");
    let view = app.find_results().expect("open");
    assert_eq!((view.selected, view.top), (0, 0));
    assert_eq!(view.entries[0].line, "echo needle");
}

#[test]
fn the_selected_entry_survives_new_output() {
    let mut app = turned(40, 10, &["needle one", "needle two"]);
    scan(&mut app, "needle");
    open(&mut app);
    for _ in 0..20 {
        assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(cursor(&app), (1, 0));
    // Live output rescans the pages: the equal match stays selected.
    prompt(&mut app, " ");
    reply(&mut app, "a_n", "needle three");
    done(&mut app);
    assert!(app.results_open());
    let view = app.find_results().expect("open");
    assert_eq!(view.entries.len(), 3);
    assert_eq!((view.selected, view.top), (1, 0));
    assert_eq!(view.entries[1].line, "needle two");
}

/// One durable session envelope with `seq`, recorded in `log` for the
/// fake hub's answers.
fn numbered(
    log: &mut Vec<contract::Envelope>,
    seq: &mut u64,
    kind: &str,
    payload: Value,
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
fn logged_turn(log: &mut Vec<contract::Envelope>, seq: &mut u64, app: &mut App, replies: &[&str]) {
    app.on_line(numbered(
        log,
        seq,
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    for text in replies {
        app.on_line(numbered(
            log,
            seq,
            "text_completed",
            json!({ "text": text }),
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
        payload: json!({"command_id": id, "result": {"lines": held}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The `history` command `line` asks for: its id and seq range.
fn command(line: &str) -> (String, u64, u64) {
    let value: Value = serde_json::from_str(line).expect("a command line");
    assert_eq!(value["command"], "history");
    (
        value["id"].as_str().unwrap_or_default().to_owned(),
        value["args"]["from_seq"].as_u64().unwrap_or(0),
        value["args"]["to_seq"].as_u64().unwrap_or(0),
    )
}

#[test]
fn entries_grow_while_the_scan_runs() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    logged_turn(&mut log, &mut seq, &mut app, &["a needle here"]);
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|at| format!("filler {turn} {at}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        logged_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().part(0).is_none(), "the first page dropped");
    // Typing starts the scan: the resident pages hold no match, and the
    // dropped first page is fetched without waiting.
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
    assert_eq!(outgoing.len(), 1, "one fetch on the wire");
    // The view opens on the matches kept so far: none yet.
    open(&mut app);
    let view = app.find_results().expect("open");
    assert!(view.entries.is_empty());
    // The fetch's answer folds into the scan, and the entries grow with
    // the selection staying put.
    let (id, from, to) = command(&outgoing[0]);
    app.on_line(hub_answer(&id, &log, from, to));
    assert!(app.results_open());
    let view = app.find_results().expect("open");
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].line, "a needle here");
    assert_eq!((view.selected, view.top), (0, 0));
}
