//! Tests for the search's state (`docs/tui.md`, "Search"): the match
//! store and scan order through the interface the driver calls.

use ratatui::layout::Rect;
use serde_json::json;

use super::super::tests::{SESSION, attached, dropped_fetch, now, search_all, text_turn};
use crate::app::{App, Effect};
use crate::keys::Key;

#[test]
fn a_match_on_the_top_row_at_the_start_is_current() {
    // The scan starts with the top row on the first needle's line: the
    // top-row needle is current, not the one below it. This pins the
    // `*row >= top` boundary the scan picks the current match with.
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let mut replies = vec!["needle top", "needle below"];
    replies.extend((0..15).map(|_| "filler"));
    text_turn(&mut log, &mut seq, &mut app, &replies);
    search_all(&mut app, &log, "needle");
    let row = app
        .current_place(app.find.current().expect("a current match"))
        .expect("a row")
        .0;
    app.jump(row);
    assert_eq!(app.top(), Some(row));
    search_all(&mut app, &log, "needle");
    let flat = app.find.flat();
    assert_eq!(flat.len(), 2);
    assert!(
        app.find
            .current()
            .is_some_and(|current| flat.first().is_some_and(|first| current == *first)),
        "the top-row needle is not current"
    );
}

/// An envelope of `kind` from `session` whose payload names command `id`.
fn command_reply(kind: &str, session: &str, id: &str) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"command_id": id})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }
}

#[test]
fn answers_takes_only_the_fetchs_reply_from_the_attached_session() {
    let mut app = attached(40, 10);
    let session = contract::SessionId(SESSION.to_owned());
    // No fetch on the wire: nothing answers.
    assert!(
        !app.find
            .answers(&command_reply("command_accepted", SESSION, "c_1"), &session)
    );
    app.find.fetch = Some(super::Fetch {
        id: "c_1".to_owned(),
        page: 0,
        generation: 0,
        held: Vec::new(),
    });
    assert!(
        app.find
            .answers(&command_reply("command_accepted", SESSION, "c_1"), &session)
    );
    assert!(
        app.find
            .answers(&command_reply("command_rejected", SESSION, "c_1"), &session)
    );
    assert!(
        !app.find
            .answers(&command_reply("text_completed", SESSION, "c_1"), &session)
    );
    assert!(!app.find.answers(
        &command_reply("command_accepted", "s_other", "c_1"),
        &session
    ));
    assert!(
        !app.find
            .answers(&command_reply("command_accepted", SESSION, "c_2"), &session)
    );
}

/// An anchor on `page` for the record and reconcile tests: its line hash
/// tells equal-looking matches apart.
fn anchor_at(page: usize, hash: u64) -> super::Anchor {
    super::Anchor {
        page,
        scopes: Vec::new(),
        line_hash: hash,
        line_len: 1,
        at: 0..1,
    }
}

/// A match on `page` with line hash `hash`, the `nth` of its kind there.
fn match_at(page: usize, hash: u64, nth: usize) -> super::Match {
    super::Match {
        anchor: anchor_at(page, hash),
        nth,
        snippet: super::Snippet::default(),
    }
}

#[test]
fn record_caps_only_past_the_limit() {
    let mut app = attached(40, 10);
    // Three matches of room left: three found fill it without capping.
    app.find.total = super::MAX_MATCHES - 3;
    let found: Vec<_> = (0..3)
        .map(|_| (anchor_at(0, 1), 0usize, false, super::Snippet::default()))
        .collect();
    let kept = app.find.record(0, 1, found);
    assert_eq!(kept.len(), 3);
    assert!(
        !app.find.capped,
        "exactly the room left is not past the cap"
    );
    assert_eq!(app.find.total, super::MAX_MATCHES);
    let more: Vec<_> = (0..4)
        .map(|_| (anchor_at(1, 1), 0usize, false, super::Snippet::default()))
        .collect();
    assert!(app.find.record(1, 1, more).is_empty());
    assert!(app.find.capped);
}

#[test]
fn a_page_wants_scanning_until_it_is_scanned_at_its_revision() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["needle"]);
    assert_eq!(app.pages().page_count(), 1);
    assert!(app.find.wants(app.pages()), "a page never scanned wants it");
    let ranges = search_all(&mut app, &log, "needle");
    assert!(ranges.is_empty());
    assert!(!app.find.wants(app.pages()), "a scanned page wants nothing");
}

#[test]
fn scanning_holds_while_a_page_waits_or_a_fetch_is_on_the_wire() {
    // Typed, not yet due: nothing fetched, a page unscanned.
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(&mut log, &mut seq, &mut app, &["needle"]);
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    for ch in "needle".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert!(app.find.fetch.is_none());
    assert!(app.find.scanning(app.pages()));
    // A fetch on the wire keeps it scanning even at the cap.
    let (mut app, _, _) = dropped_fetch();
    app.find.capped = true;
    assert!(app.find.scanning(app.pages()));
}

#[test]
fn marks_follow_the_bar_the_pause_and_the_kept_matches() {
    // Each guard alone clears the marks: a pause still to pass, no kept
    // match, and a closed bar.
    let searched = || {
        let mut app = attached(40, 10);
        let mut log = Vec::new();
        let mut seq = 0u64;
        text_turn(&mut log, &mut seq, &mut app, &["needle one"]);
        search_all(&mut app, &log, "needle");
        app
    };
    let area = Rect::new(0, 0, 40, 10);
    assert_eq!(searched().find_marks(area).len(), 6);
    let mut app = searched();
    app.find.due = false;
    assert!(app.find_marks(area).is_empty(), "no marks before the pause");
    let mut app = searched();
    app.find.total = 0;
    assert!(
        app.find_marks(area).is_empty(),
        "no marks with no kept match"
    );
    let mut app = searched();
    app.find.open = false;
    assert!(
        app.find_marks(area).is_empty(),
        "no marks with the bar closed"
    );
}

#[test]
fn find_lost_ends_only_a_running_scan_on_an_open_bar() {
    let mut app = attached(40, 10);
    // Open bar, no fetch on the wire: nothing is lost.
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    app.find_lost();
    assert!(!app.find.incomplete);
    assert_eq!(app.notices.newest(), None);
}

#[test]
fn reconcile_keeps_the_equal_match_then_moves_after_the_old_place_and_wraps() {
    let mut app = attached(40, 10);
    app.find.matches = std::collections::BTreeMap::from([
        (0, vec![match_at(0, 1, 0)]),
        (1, vec![match_at(1, 2, 0)]),
        (2, vec![match_at(2, 1, 0)]),
        (3, vec![match_at(3, 1, 0)]),
    ]);
    app.find.total = 4;
    // The old match is gone from page 1: the next match after its place
    // is on page 2, not the other line at the same place or page 0.
    app.find.reveal = true;
    app.find.reconcile(match_at(1, 1, 0));
    assert_eq!(app.find.current.as_ref().map(|m| m.anchor.page), Some(2));
    assert!(!app.find.reveal);
    // An equal match stays current and keeps a pending reveal.
    app.find.reveal = true;
    app.find.reconcile(match_at(2, 1, 0));
    assert_eq!(app.find.current.as_ref().map(|m| m.anchor.page), Some(2));
    assert!(app.find.reveal);
    // Nothing after the old place: the scan wraps to the first match.
    app.find.reconcile(match_at(4, 9, 0));
    assert_eq!(app.find.current.as_ref().map(|m| m.anchor.page), Some(0));
    assert!(!app.find.reveal);
}

#[test]
fn remap_selected_keeps_the_equal_match_then_moves_after_and_wraps() {
    let mut app = attached(40, 10);
    app.find.matches = std::collections::BTreeMap::from([
        (0, vec![match_at(0, 1, 0)]),
        (1, vec![match_at(1, 2, 0)]),
        (2, vec![match_at(2, 1, 0)]),
        (3, vec![match_at(3, 1, 0)]),
    ]);
    app.find.total = 4;
    app.find.show_results(0);
    let selected = |app: &App| {
        app.find
            .results_at()
            .map(|(at, _)| at)
            .unwrap_or(usize::MAX)
    };
    // The old match is gone from page 1: the next match after its place
    // is on page 2, not page 0.
    app.find.remap_selected(match_at(1, 1, 0), 10);
    assert_eq!(selected(&app), 2);
    // An equal match stays selected.
    app.find.remap_selected(match_at(2, 1, 0), 10);
    assert_eq!(selected(&app), 2);
    // Nothing after the old place: the selection wraps to the first.
    app.find.remap_selected(match_at(4, 9, 0), 10);
    assert_eq!(selected(&app), 0);
    // No matches left: the selection is the top.
    app.find.matches.clear();
    app.find.total = 0;
    app.find.remap_selected(match_at(0, 1, 0), 10);
    assert_eq!(selected(&app), 0);
}
