//! Tests for `usage_recorded` in the window of history (`docs/tui.md`,
//! "History and paging"): a usage line finds its turn without walking
//! every turn, and counts a page again only when its ▣ line's rows
//! change, so opening a session folds each line with the same work
//! however long the session is.

use std::path::PathBuf;

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
    fn turn(&mut self, turn: usize, generation: &str) {
        self.line(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
        );
        let message = format!("a_m{turn}");
        self.line("step_started", None, json!({}));
        self.line("assistant_message_started", Some(&message), json!({}));
        let text = "the session log holds every durable line ".repeat(100);
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
        log.turn(turn, &generation);
    }
    log.lines
}

/// The work folding `lines` in one batch did, as the loop folds a batch
/// of hub lines, and how many pages the session has.
fn fold_work(lines: &[Envelope]) -> (crate::work::Work, usize) {
    let mut app = App::new(PathBuf::new());
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
            assert_eq!(work.lines, lines.len(), "{case}");
            // Each reply renders once, to count its rows.
            assert_eq!(work.markdown_renders, turns, "{case}");
            // Each page is counted once: when it closes, and the open one
            // at the batch's end. A usage line counts no page again.
            assert!(work.page_counts <= pages, "{pages} pages; {case}");
            // Each turn draws its rows for its page's count, and a cut
            // draws the cards on either side of it once more.
            assert!(work.turn_rows <= turns + 2 * pages, "{case}");
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
    for turn in 0..12 {
        log.turn(turn, &format!("g{turn}"));
    }
    let mut pages = folded(&log.lines, 30);
    assert!(!pages.closed.is_empty(), "the first page closed");
    assert!(pages.part(0).is_some(), "the first page holds its cards");
    // A correction for the first turn as large as its ▣ line can grow:
    // the line wraps onto more rows, and the page is counted again.
    let before = pages.recounts;
    log.usage("g0", 987_654_321, json!(12.5));
    let late = log
        .lines
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("a line was logged"));
    pages.apply(&late);
    assert_eq!(pages.recounts - before, 1);
    let fresh = folded(&log.lines, 30);
    assert_eq!(rows(&pages), rows(&fresh));
    assert_eq!(pages.rows(), fresh.rows());
    // The same correction again leaves every row where it is: no page is
    // counted again, and the page's revision still moves, so search reads
    // its text again.
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
    assert!(pages.index().revision(0) > revision);
    let fresh = folded(&log.lines, 30);
    assert_eq!(rows(&pages), rows(&fresh));
    assert_eq!(pages.rows(), fresh.rows());
}
