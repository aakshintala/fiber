//! Tests for `usage_recorded` in the window of history (`docs/tui.md`,
//! "History and paging"): a usage line finds its turn without walking
//! every turn, and counts a page again only when its ▣ line's rows
//! change, so opening a session folds each line with the same work
//! however long the session is.

use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::Pages;
use crate::app::App;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// A session's durable lines, numbered from 0.
#[derive(Default)]
struct Log {
    lines: Vec<Envelope>,
}

impl Log {
    fn line(&mut self, kind: &str, action: Option<&str>, payload: Value) {
        let seq = u64::try_from(self.lines.len()).unwrap_or(u64::MAX);
        self.lines.push(Envelope {
            kind: kind.to_owned(),
            session_id: SessionId(SESSION.to_owned()),
            ts: 1_000 + seq * 700,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| ActionId(id.to_owned())),
            seq: Some(Seq(seq)),
            payload: payload.as_object().cloned().unwrap_or_default(),
        });
    }

    fn usage(&mut self, generation: &str, input: u64, cost: Value) {
        self.line(
            "usage_recorded",
            Some("a_m"),
            json!({"generation_id": generation, "model": "fake/m",
                "tokens": {"input": input, "cache_read": 0, "cache_write": {}, "output": 40},
                "input_bytes": 0, "cost": cost}),
        );
    }

    /// One turn as the attach benchmark's log holds it: a prompt, one
    /// long single-paragraph reply and its usage line, under
    /// `generation`.
    fn turn(&mut self, turn: usize, generation: &str, repeats: usize) {
        self.line(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
        );
        let message = format!("a_m{turn}");
        self.line("step_started", None, json!({}));
        self.line("assistant_message_started", Some(&message), json!({}));
        let text = "the session log holds every durable line ".repeat(repeats);
        self.line("text_completed", Some(&message), json!({"text": text}));
        self.usage(generation, 100, Value::Null);
        self.line(
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        self.line("turn_completed", None, json!({"outcome": "completed"}));
    }
}

/// `turns` turns, each usage line under one generation id when `shared`,
/// as a provider that repeats its ids writes them, else one id a turn.
fn session(turns: usize, shared: bool) -> Vec<Envelope> {
    let mut log = Log::default();
    for turn in 0..turns {
        let generation = if shared {
            "resp_1".to_owned()
        } else {
            format!("resp_{turn}")
        };
        log.turn(turn, &generation, 100);
    }
    log.lines
}

/// The work folding `lines` in one batch did, as the loop folds a batch
/// of hub lines, and how many pages the session has.
fn fold_work(lines: &[Envelope]) -> (crate::work::Work, usize) {
    let mut app = App::new(None);
    app.set_size(60, 12);
    app.attach(SessionId(SESSION.to_owned()));
    crate::work::take();
    app.begin_batch();
    for line in lines {
        app.on_line(Line::Session(line.clone()));
    }
    app.end_batch();
    (crate::work::take(), app.pages().page_count())
}

#[test]
fn folding_a_session_does_the_same_work_for_each_line() {
    for shared in [true, false] {
        for turns in [8, 32] {
            let lines = session(turns, shared);
            let (work, pages) = fold_work(&lines);
            let case = format!("{turns} turns, shared ids {shared}: {work:?}");
            // Each reply draws once for its page's count, and a page
            // cut draws one reply again on its scratch pad.
            assert!(work.reply_renders >= turns, "{case}");
            assert!(work.reply_renders <= turns + pages, "{case}");
            // Each turn draws once for its page's count, and a cut
            // draws the cards on either side of it on scratch pads.
            assert!(work.turn_rows >= turns, "{case}");
            assert!(work.turn_rows <= turns + 3 * pages, "{case}");
            // Each page is counted once: when it closes, and the open one
            // at the batch's end. A usage line counts no page again.
            assert_eq!(work.page_counts, pages, "{case}");
            // The batch's end settles once: one prefix rebuild visits
            // every page, and the settle's totals and window then read
            // the cache.
            assert_eq!(work.index_pages, pages, "{case}");
            // A usage line looks its turn up once, never walking the
            // turns before it.
            assert_eq!(work.usage_summaries, turns, "{case}");
        }
    }
}

/// Every page's row count, from the index.
fn rows(pages: &Pages) -> Vec<usize> {
    pages.index().pages().iter().map(|page| page.rows).collect()
}

/// `lines` folded into fresh pages at `width`.
fn folded(lines: &[Envelope], width: u16) -> Pages {
    let mut pages = Pages::new(width);
    for line in lines {
        pages.apply(line);
    }
    pages
}

#[test]
fn a_late_usage_line_on_a_held_page_counts_its_rows_exactly() {
    // Turns enough to close the first page, each with its own id. The
    // closed first page still holds its cards: no window trims the pages
    // here.
    let mut log = Log::default();
    for turn in 0..10 {
        log.turn(turn, &format!("g{turn}"), 3);
    }
    let mut pages = folded(&log.lines, 30);
    assert!(pages.residency.len() > 0, "the first page closed");
    assert!(pages.part(0).is_some(), "the first page holds its cards");
    // A correction for the first turn as large as its ▣ line can grow:
    // at the card's inner width the line already took two padded rows,
    // and still takes two, so no page is counted again yet every row is
    // where a fresh fold puts it (`docs/tui.md`, "Turns"). Its text
    // changed, so the page's revision moves with it.
    let before = pages.recounts;
    let revision = pages.index().revision(0);
    log.usage("g0", 987_654_321, json!(12.5));
    let late = log
        .lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    pages.apply(&late);
    assert_eq!(pages.recounts, before);
    assert!(pages.index().revision(0) > revision);
    let fresh = folded(&log.lines, 30);
    assert_eq!(rows(&pages), rows(&fresh));
    assert_eq!(pages.rows(), fresh.rows());
    // The same correction again changes nothing: no page is counted
    // again, and the page's revision stays, so the search keeps the
    // scan it holds.
    let before = pages.recounts;
    let revision = pages.index().revision(0);
    log.usage("g0", 987_654_321, json!(12.5));
    let again = log
        .lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    pages.apply(&again);
    assert_eq!(pages.recounts, before);
    assert_eq!(pages.index().revision(0), revision);
    let fresh = folded(&log.lines, 30);
    assert_eq!(rows(&pages), rows(&fresh));
    assert_eq!(pages.rows(), fresh.rows());
    // A correction that changes the ▣ line's text but not its rows
    // counts no page again, yet the revision still moves, so the search
    // reads its text again.
    let before = pages.recounts;
    log.usage("g0", 987_654_321, json!(12.6));
    let changed = log
        .lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    pages.apply(&changed);
    assert_eq!(pages.recounts, before);
    assert!(pages.index().revision(0) > revision);
    let fresh = folded(&log.lines, 30);
    assert_eq!(rows(&pages), rows(&fresh));
    assert_eq!(pages.rows(), fresh.rows());
}

/// Folding one turn counts each step exactly once: seven lines on one
/// page, so every kept counter has the number a `*=` or `-=` mutant
/// cannot satisfy.
#[test]
fn folding_one_turn_counts_each_step_once() {
    let lines = session(1, false);
    assert_eq!(lines.len(), 7);
    let (work, pages) = fold_work(&lines);
    assert_eq!(pages, 1);
    let case = format!("{work:?}");
    // The reply draws once, for the batch-end count.
    assert_eq!(work.reply_renders, 1, "{case}");
    // The turn draws once, for the batch-end count.
    assert_eq!(work.turn_rows, 1, "{case}");
    // The open page counts once, at the batch's end.
    assert_eq!(work.page_counts, 1, "{case}");
    // The settle rebuilds the one page's prefix once, and every later
    // total and window reads the cache.
    assert_eq!(work.index_pages, 1, "{case}");
    // The usage line looks its turn up once.
    assert_eq!(work.usage_summaries, 1, "{case}");
}

#[test]
fn a_late_usage_line_on_a_dropped_page_counts_the_padded_closing_rows() {
    // At W = 20 the card draws its ▣ line at the inner width of 18:
    // "▣ completed · 1 call" is 20 cells, one row at W but two padded
    // rows. A late `usage_recorded` grows it further, and the dropped
    // page holding it adds exactly the padded difference
    // (`docs/tui.md`, "Turns", "History and paging").
    let mut lines = Vec::new();
    let mut seq = 0u64;
    {
        let mut line = |kind: &str, action: Option<&str>, payload: Value| {
            lines.push(Envelope {
                kind: kind.to_owned(),
                session_id: SessionId(SESSION.to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: action.map(|id| ActionId(id.to_owned())),
                seq: Some(Seq(seq)),
                payload: payload.as_object().cloned().unwrap_or_default(),
            });
            seq += 1;
        };
        for turn in 0..16 {
            line(
                "turn_started",
                None,
                json!({"input": [{"type": "message", "source": "driver",
                    "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
            );
            let call = format!("a_c{turn}");
            line(
                "tool_call_requested",
                Some(&call),
                json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
            );
            line(
                "tool_call_completed",
                Some(&call),
                json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
            );
            // No tokens and no cost: the ▣ line is the head and the call
            // count alone, while the generation still names its turn.
            line(
                "usage_recorded",
                Some(&call),
                json!({"generation_id": format!("g{turn}"), "model": "fake/m",
                    "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
                    "input_bytes": 0, "cost": Value::Null}),
            );
            line("turn_completed", None, json!({"outcome": "completed"}));
        }
    }
    let mut pages = folded(&lines, 20);
    assert!(pages.residency.len() > 0, "the first page closed");
    let head = pages.summaries.first().expect("the first turn's totals");
    let closing = head.closing().expect("a closing line");
    assert_eq!(closing.to_string(), "▣ completed · 1 call");
    assert_eq!(crate::view::rows(closing.clone(), 20), 1);
    assert_eq!(crate::rows::fitted(&closing, crate::surface::inset(20)), 2);
    pages.residency.drop_slot_for_test(0);
    lines.push(Envelope {
        kind: "usage_recorded".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId("a_c0".to_owned())),
        seq: Some(Seq(seq)),
        payload: json!({"generation_id": "g0", "model": "fake/m",
            "tokens": {"input": 987_654_321, "cache_read": 0, "cache_write": {}, "output": 40},
            "input_bytes": 0, "cost": 12.5})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    let late = lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    pages.apply(&late);
    let grown = pages.summaries.first().expect("the first turn's totals");
    let closing = grown.closing().expect("a closing line");
    assert!(crate::rows::fitted(&closing, crate::surface::inset(20)) > 2);
    let fresh = folded(&lines, 20);
    assert_eq!(rows(&pages), rows(&fresh));
}

#[test]
fn a_late_usage_line_with_new_rows_on_a_held_page_counts_it_again() {
    // The dropped-page padded closing, but with the first page still
    // holding its cards: the ▣ line grows past its old rows, so the
    // closed page counts again. This pins the `open ||` side of the
    // recount guard: with `&&` the page would take the dropped-page
    // arithmetic instead of counting, and `recounts` would stay.
    let mut lines = Vec::new();
    let mut seq = 0u64;
    {
        let mut line = |kind: &str, action: Option<&str>, payload: Value| {
            lines.push(Envelope {
                kind: kind.to_owned(),
                session_id: SessionId(SESSION.to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: action.map(|id| ActionId(id.to_owned())),
                seq: Some(Seq(seq)),
                payload: payload.as_object().cloned().unwrap_or_default(),
            });
            seq += 1;
        };
        for turn in 0..16 {
            line(
                "turn_started",
                None,
                json!({"input": [{"type": "message", "source": "driver",
                    "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
            );
            let call = format!("a_c{turn}");
            line(
                "tool_call_requested",
                Some(&call),
                json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
            );
            line(
                "tool_call_completed",
                Some(&call),
                json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
            );
            line(
                "usage_recorded",
                Some(&call),
                json!({"generation_id": format!("g{turn}"), "model": "fake/m",
                    "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
                    "input_bytes": 0, "cost": Value::Null}),
            );
            line("turn_completed", None, json!({"outcome": "completed"}));
        }
    }
    let mut pages = folded(&lines, 20);
    assert!(pages.residency.len() > 0, "the first page closed");
    assert!(
        pages.part(0).is_some(),
        "the first page still holds its cards"
    );
    let head = pages.summaries.first().expect("the first turn's totals");
    assert_eq!(head.closed_on, 0, "the first turn ends on the first page");
    lines.push(Envelope {
        kind: "usage_recorded".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId("a_c0".to_owned())),
        seq: Some(Seq(seq)),
        payload: json!({"generation_id": "g0", "model": "fake/m",
            "tokens": {"input": 987_654_321, "cache_read": 0, "cache_write": {}, "output": 40},
            "input_bytes": 0, "cost": 12.5})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    let late = lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    let before = pages.recounts;
    pages.apply(&late);
    assert_eq!(
        pages.recounts,
        before.saturating_add(1),
        "the closed page with new rows counts again"
    );
    let fresh = folded(&lines, 20);
    assert_eq!(rows(&pages), rows(&fresh));
}
