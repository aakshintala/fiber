//! Tests for whole-turn paging (`docs/tui.md`, "History and paging"): which
//! pages hold a turn at its boundaries, and that pinning a turn's pages and
//! releasing them are observable in the pinned set.

use super::{pages_for_turn, release_turn};
use crate::window::Pages;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// Pages cut inside turn 0 and at turn 2: page 0 holds turn 0's tail, page 1
/// holds turns 0 and 1, page 2 is the open page from turn 2 on.
fn split() -> (Vec<usize>, Vec<bool>) {
    (vec![0, 0, 2], vec![true, false, false])
}

#[test]
fn pages_for_turn_starts_before_and_ends_after_a_turn() {
    let (firsts, cuts) = split();
    // Turn 0 spans the cut pages; turn 1 ends page 1; turn 2 opens the last
    // page; a turn past every first still lands on the open page.
    assert_eq!(pages_for_turn(&firsts, &cuts, 3, 0), vec![0, 1]);
    assert_eq!(pages_for_turn(&firsts, &cuts, 3, 1), vec![1]);
    assert_eq!(pages_for_turn(&firsts, &cuts, 3, 2), vec![2]);
    assert_eq!(pages_for_turn(&firsts, &cuts, 3, 5), vec![2]);
}

#[test]
fn pages_for_turn_keeps_a_cut_pages_tail_with_its_turn() {
    // One turn across three pages: two step cuts inside turn 2.
    let firsts = vec![2, 2, 2];
    let cuts = vec![true, true, false];
    assert_eq!(pages_for_turn(&firsts, &cuts, 3, 2), vec![0, 1, 2]);
}

#[test]
fn release_turn_lets_its_pages_page_out_again() {
    let mut pages = Pages::new(80);
    pages.want(0);
    assert_eq!(pages.pinned(), 1);
    release_turn(&mut pages, 0);
    assert_eq!(pages.pinned(), 0);
}

/// One line of a session that cuts pages, numbered from `seq`.
fn envelope(
    seq: u64,
    kind: &str,
    action: Option<&str>,
    payload: serde_json::Value,
) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: seq.saturating_mul(1_000),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

fn turn_started(seq: &mut u64, out: &mut Vec<contract::Envelope>, text: &str) {
    out.push(envelope(
        *seq,
        "turn_started",
        None,
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    ));
    *seq = seq.saturating_add(1);
}

/// Two turns with enough lines between their starts to cut a page: the
/// second page opens on turn 1.
fn turn_cut_session() -> Vec<contract::Envelope> {
    let mut out = Vec::new();
    let mut seq = 0u64;
    turn_started(&mut seq, &mut out, "go");
    for _ in 0..70 {
        out.push(envelope(seq, "unknown", None, serde_json::json!({})));
        seq = seq.saturating_add(1);
    }
    out.push(envelope(
        seq,
        "turn_completed",
        None,
        serde_json::json!({"outcome": "completed"}),
    ));
    seq = seq.saturating_add(1);
    turn_started(&mut seq, &mut out, "next");
    out.push(envelope(
        seq,
        "turn_completed",
        None,
        serde_json::json!({"outcome": "completed"}),
    ));
    out
}

/// One turn with a confirmed step cut: the first page closes inside turn 0.
fn step_cut_session() -> Vec<contract::Envelope> {
    let mut out = Vec::new();
    let mut seq = 0u64;
    turn_started(&mut seq, &mut out, "go");
    for _ in 0..70 {
        out.push(envelope(seq, "unknown", None, serde_json::json!({})));
        seq = seq.saturating_add(1);
    }
    out.push(envelope(seq, "step_started", None, serde_json::json!({})));
    seq = seq.saturating_add(1);
    out.push(envelope(
        seq,
        "assistant_message_started",
        Some("a_m"),
        serde_json::json!({}),
    ));
    seq = seq.saturating_add(1);
    out.push(envelope(
        seq,
        "text_completed",
        Some("a_m"),
        serde_json::json!({"text": "reply"}),
    ));
    seq = seq.saturating_add(1);
    out.push(envelope(
        seq,
        "assistant_message_completed",
        Some("a_m"),
        serde_json::json!({"outcome": "completed"}),
    ));
    seq = seq.saturating_add(1);
    out.push(envelope(
        seq,
        "turn_completed",
        None,
        serde_json::json!({"outcome": "completed"}),
    ));
    out
}

fn fold_all(session: &[contract::Envelope]) -> Pages {
    let mut pages = Pages::new(80);
    for line in session {
        pages.apply(line);
    }
    pages
}

#[test]
fn page_first_reports_the_turn_each_page_opens_on() {
    let pages = fold_all(&turn_cut_session());
    assert!(pages.page_count() > 1, "no page cut");
    assert_eq!(pages.page_first(0), Some(0));
    // The cut at the second turn_started opens its page on turn 1, not 0.
    assert_eq!(pages.page_first(1), Some(1));
}

#[test]
fn page_cut_marks_a_page_closed_inside_its_turn() {
    let pages = fold_all(&step_cut_session());
    assert!(pages.page_count() > 1, "no page cut");
    assert!(pages.page_cut(0), "no page closed inside its turn");
}

#[test]
fn page_cut_is_clear_where_no_page_closed_inside_a_turn() {
    let pages = fold_all(&turn_cut_session());
    // A turn cut is not a step cut, and the open page never closed.
    assert!(!pages.page_cut(0), "a turn cut is not a step cut");
    assert!(
        !pages.page_cut(pages.page_count() - 1),
        "the open page never closed"
    );
}
