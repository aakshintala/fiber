//! Tests for the page index: the cut rule, seq ranges and the window.

use contract::{ActionId, Seq};

use super::{Cut, Index, PAGE_LINES, Page};

/// Pushes one line, shown on a card or not.
fn push(index: &mut Index, seq: u64, kind: &str, action: Option<&str>, shown: bool) -> Cut {
    let action = action.map(|id| ActionId(id.to_owned()));
    index.push(Seq(seq), kind, action.as_ref(), shown)
}

/// Feeds `kinds` with consecutive seqs from `seq`, each with `action` and
/// shown, returning every cut that was not `Cut::None`.
fn feed(index: &mut Index, seq: &mut u64, lines: &[(&str, Option<&str>)]) -> Vec<(u64, Cut)> {
    let mut cuts = Vec::new();
    for (kind, action) in lines {
        let cut = push(index, *seq, kind, *action, true);
        if cut != Cut::None {
            cuts.push((*seq, cut));
        }
        *seq += 1;
    }
    cuts
}

/// `count` lines that never cut: usage records.
fn filler(index: &mut Index, seq: &mut u64, count: usize) {
    for _ in 0..count {
        assert_eq!(push(index, *seq, "usage_recorded", None, true), Cut::None);
        *seq += 1;
    }
}

/// A page from `first` to `last` holding `lines`, no rows counted.
fn page(first: u64, last: u64, lines: usize) -> Page {
    Page {
        first_seq: Seq(first),
        last_seq: Seq(last),
        lines,
        rows: 0,
    }
}

/// One step that calls one tool, without text.
const TOOL_STEP: &[(&str, Option<&str>)] = &[
    ("step_started", None),
    ("assistant_message_started", Some("a_m")),
    ("tool_call_requested", Some("a_t")),
    ("assistant_message_completed", Some("a_m")),
    ("tool_call_completed", Some("a_t")),
];

/// One step that replies with text.
const TEXT_STEP: &[(&str, Option<&str>)] = &[
    ("step_started", None),
    ("assistant_message_started", Some("a_m")),
    ("text_completed", Some("a_m")),
    ("assistant_message_completed", Some("a_m")),
];

#[test]
fn fewer_than_page_lines_never_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES - 1);
    // The step_started finds PAGE_LINES - 1 lines held.
    assert!(feed(&mut index, &mut seq, TEXT_STEP).is_empty());
    assert_eq!(index.pages().len(), 1);
}

#[test]
fn a_step_that_opens_with_text_cuts_at_its_step_started() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    // Exactly PAGE_LINES lines held: the step_started is a candidate.
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    let at = PAGE_LINES as u64;
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
    assert_eq!(
        index.pages(),
        &[page(0, at - 1, PAGE_LINES), page(at, at + 3, 4)]
    );
}

#[test]
fn a_step_that_opens_with_a_tool_call_never_cuts() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let cuts = feed(&mut index, &mut seq, TOOL_STEP);
    assert_eq!(cuts, vec![(PAGE_LINES as u64, Cut::Candidate)]);
    // A text in the same step later does not confirm the discarded
    // candidate.
    assert!(feed(&mut index, &mut seq, &[("text_completed", Some("a_m"))]).is_empty());
    assert_eq!(index.pages().len(), 1);
    // The next step that opens with text cuts.
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
    assert_eq!(index.pages().len(), 2);
}

#[test]
fn reasoning_before_text_moves_with_the_step() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let at = seq;
    let cuts = feed(
        &mut index,
        &mut seq,
        &[
            ("step_started", None),
            ("assistant_message_started", Some("a_m")),
            ("reasoning_started", Some("a_m")),
            ("reasoning_completed", Some("a_m")),
            ("text_completed", Some("a_m")),
        ],
    );
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 4, Cut::AtCandidate)]);
    let pages = index.pages();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].last_seq, Seq(at - 1));
    assert_eq!(pages[0].lines, PAGE_LINES);
    assert_eq!(pages[1].first_seq, Seq(at));
    assert_eq!(pages[1].lines, 5);
}

#[test]
fn never_while_a_call_is_in_flight() {
    let mut index = Index::default();
    let mut seq = 0;
    // A call requested and not yet completed.
    feed(
        &mut index,
        &mut seq,
        &[("tool_call_requested", Some("a_long"))],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    assert!(feed(&mut index, &mut seq, TEXT_STEP).is_empty());
    assert_eq!(index.pages().len(), 1);
    // Once it completes, the next text step cuts.
    feed(
        &mut index,
        &mut seq,
        &[("tool_call_completed", Some("a_long"))],
    );
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn a_call_completed_after_its_turn_ends_blocks_the_next_turns_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    // The call's completion follows its turn's end.
    feed(
        &mut index,
        &mut seq,
        &[
            ("turn_started", None),
            ("tool_call_requested", Some("a_late")),
            ("turn_completed", None),
        ],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    assert!(feed(&mut index, &mut seq, &[("turn_started", None)]).is_empty());
    assert!(feed(&mut index, &mut seq, TEXT_STEP).is_empty());
    feed(
        &mut index,
        &mut seq,
        &[("tool_call_completed", Some("a_late"))],
    );
    // Completed: the next step that opens with text cuts.
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn a_call_two_turns_old_no_longer_blocks() {
    let mut index = Index::default();
    let mut seq = 0;
    // A cancelled turn leaves its call unmatched.
    feed(
        &mut index,
        &mut seq,
        &[
            ("turn_started", None),
            ("tool_call_requested", Some("a_cut")),
            ("turn_completed", None),
        ],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    // The next turn keeps it: its completion may yet come.
    assert!(feed(&mut index, &mut seq, &[("turn_started", None)]).is_empty());
    feed(&mut index, &mut seq, &[("turn_completed", None)]);
    // The turn after drops it.
    let at = seq;
    assert_eq!(
        feed(&mut index, &mut seq, &[("turn_started", None)]),
        vec![(at, Cut::Here)]
    );
}

#[test]
fn reasoning_in_flight_blocks_a_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    feed(
        &mut index,
        &mut seq,
        &[("turn_started", None), ("reasoning_started", Some("a_r"))],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    assert!(feed(&mut index, &mut seq, TEXT_STEP).is_empty());
    assert!(feed(&mut index, &mut seq, &[("turn_started", None)]).is_empty());
    // Completed within the next turn: that turn's next text step cuts.
    feed(
        &mut index,
        &mut seq,
        &[("reasoning_completed", Some("a_r"))],
    );
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn an_open_permission_blocks_a_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    // The call completes; its permission request is still open.
    feed(
        &mut index,
        &mut seq,
        &[
            ("turn_started", None),
            ("tool_call_requested", Some("a_p")),
            ("permission_requested", Some("a_p")),
            ("tool_call_completed", Some("a_p")),
        ],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    assert!(feed(&mut index, &mut seq, TEXT_STEP).is_empty());
    feed(
        &mut index,
        &mut seq,
        &[("permission_resolved", Some("a_p"))],
    );
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn reasoning_that_joins_an_open_group_discards_the_candidate() {
    let mut index = Index::default();
    let mut seq = 0;
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    filler(&mut index, &mut seq, PAGE_LINES);
    // The tool step leaves its group open; the next step thinks first, and
    // the thinking joins that group.
    feed(&mut index, &mut seq, TOOL_STEP);
    let at = seq;
    let cuts = feed(
        &mut index,
        &mut seq,
        &[
            ("step_started", None),
            ("assistant_message_started", Some("a_m2")),
            ("reasoning_started", Some("a_r2")),
            ("reasoning_completed", Some("a_r2")),
            ("text_completed", Some("a_m2")),
        ],
    );
    assert_eq!(cuts, vec![(at, Cut::Candidate)]);
    assert_eq!(index.pages().len(), 1);
    // The text closed the group: the next step that opens with text cuts.
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn text_shown_on_no_card_confirms_nothing() {
    let mut index = Index::default();
    let mut seq = 0;
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    feed(&mut index, &mut seq, TOOL_STEP);
    filler(&mut index, &mut seq, PAGE_LINES);
    let at = seq;
    assert_eq!(
        push(&mut index, at, "step_started", None, false),
        Cut::Candidate
    );
    // An empty part shows nothing, so the open group goes on.
    assert_eq!(
        push(&mut index, at + 1, "text_completed", Some("a_m"), false),
        Cut::None
    );
    assert_eq!(
        push(
            &mut index,
            at + 2,
            "tool_call_requested",
            Some("a_t2"),
            true
        ),
        Cut::None
    );
    assert_eq!(index.pages().len(), 1);
    seq = at + 3;
    feed(
        &mut index,
        &mut seq,
        &[("tool_call_completed", Some("a_t2"))],
    );
    // A step whose text shows cuts.
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
}

#[test]
fn turn_started_cuts_at_once() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    // A candidate the turn's end leaves unconfirmed stays in its page.
    let cuts = feed(
        &mut index,
        &mut seq,
        &[("step_started", None), ("turn_completed", None)],
    );
    assert_eq!(cuts, vec![(PAGE_LINES as u64, Cut::Candidate)]);
    let at = seq;
    let cuts = feed(&mut index, &mut seq, &[("turn_started", None)]);
    assert_eq!(cuts, vec![(at, Cut::Here)]);
    let pages = index.pages();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].lines, PAGE_LINES + 2);
    assert_eq!(pages[1].first_seq, Seq(at));
    assert_eq!(pages[1].lines, 1);
    // Never with a call in flight.
    feed(
        &mut index,
        &mut seq,
        &[("tool_call_requested", Some("a_x"))],
    );
    filler(&mut index, &mut seq, PAGE_LINES);
    assert!(feed(&mut index, &mut seq, &[("turn_started", None)]).is_empty());
}

#[test]
fn turn_started_below_page_lines_does_not_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES - 1);
    assert!(feed(&mut index, &mut seq, &[("turn_started", None)]).is_empty());
    // The line makes PAGE_LINES; the next turn cuts.
    let at = seq;
    assert_eq!(
        feed(&mut index, &mut seq, &[("turn_started", None)]),
        vec![(at, Cut::Here)]
    );
}

#[test]
fn a_later_step_replaces_an_empty_candidate() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let first = seq;
    let cuts = feed(&mut index, &mut seq, &[("step_started", None)]);
    assert_eq!(cuts, vec![(first, Cut::Candidate)]);
    let at = seq;
    let cuts = feed(&mut index, &mut seq, TEXT_STEP);
    assert_eq!(cuts, vec![(at, Cut::Candidate), (at + 2, Cut::AtCandidate)]);
    assert_eq!(index.pages()[0].last_seq, Seq(at - 1));
    assert_eq!(index.pages()[1].first_seq, Seq(at));
}

#[test]
fn pages_cover_every_line_once_and_contiguously() {
    let mut index = Index::default();
    let mut seq = 5;
    let first = seq;
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    for step in 0..200 {
        if step % 3 == 0 {
            feed(&mut index, &mut seq, TOOL_STEP);
        } else {
            feed(&mut index, &mut seq, TEXT_STEP);
        }
        if step % 50 == 49 {
            feed(
                &mut index,
                &mut seq,
                &[("turn_completed", None), ("turn_started", None)],
            );
        }
    }
    let pages = index.pages();
    assert!(pages.len() > 5);
    assert_eq!(pages.first().map(|page| page.first_seq), Some(Seq(first)));
    assert_eq!(pages.last().map(|page| page.last_seq), Some(Seq(seq - 1)));
    for pair in pages.windows(2) {
        assert_eq!(pair[0].last_seq.0 + 1, pair[1].first_seq.0);
    }
    for page in pages {
        let span = usize::try_from(page.last_seq.0 - page.first_seq.0 + 1).unwrap_or(0);
        assert_eq!(page.lines, span);
    }
    let total: usize = pages.iter().map(|page| page.lines).sum();
    assert_eq!(total, usize::try_from(seq - first).unwrap_or(0));
    // Every closed page holds at least PAGE_LINES lines, about one page.
    for page in &pages[..pages.len() - 1] {
        assert!(page.lines >= PAGE_LINES);
        assert!(page.lines < PAGE_LINES + 2 * TOOL_STEP.len() + 3);
    }
}

#[test]
fn the_page_holding_a_seq_is_found() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    let boundary = PAGE_LINES as u64;
    assert_eq!(index.page_of(Seq(0)), Some(0));
    assert_eq!(index.page_of(Seq(boundary - 1)), Some(0));
    assert_eq!(index.page_of(Seq(boundary)), Some(1));
    assert_eq!(index.page_of(Seq(boundary + 1)), None);
}

/// An index of pages with these row counts.
fn with_rows(rows: &[usize]) -> Index {
    let mut index = Index::default();
    let mut seq = 0;
    for (at, count) in rows.iter().enumerate() {
        if at > 0 {
            filler(&mut index, &mut seq, PAGE_LINES);
            assert_eq!(push(&mut index, seq, "turn_started", None, true), Cut::Here);
            seq += 1;
        }
        index.set_rows(at, *count);
    }
    index
}

#[test]
fn totals_and_start_rows_are_prefix_sums() {
    let index = with_rows(&[3, 0, 5, 2]);
    assert_eq!(index.total(), 10);
    assert_eq!(index.starts(), vec![0, 3, 3, 8]);
}

#[test]
fn the_window_is_one_screen_above_and_below() {
    // Pages of 10 rows: 0..10, 10..20, ... 90..100.
    let index = with_rows(&[10; 10]);
    // top 40, h 10: rows [30, 60), pages 3, 4 and 5.
    assert_eq!(index.window(40, 10), 3..6);
    // One row either way moves an edge across a page boundary.
    assert_eq!(index.window(41, 10), 3..7);
    assert_eq!(index.window(39, 10), 2..6);
    // Clamped at the top and the bottom.
    assert_eq!(index.window(0, 10), 0..2);
    assert_eq!(index.window(90, 10), 8..10);
    // Nothing on a screen with no rows.
    assert_eq!(index.window(40, 0), 4..4);
}

#[test]
fn a_page_with_no_rows_is_outside_every_window() {
    let index = with_rows(&[10, 0, 10]);
    let window = index.window(0, 5);
    assert_eq!(window, 0..1);
    assert!(index.intersects(2, 0, 20));
    assert!(!index.intersects(1, 0, 20));
}

#[test]
fn turn_completed_discards_a_candidate_cut() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let at = seq;
    assert_eq!(
        push(&mut index, at, "step_started", None, true),
        Cut::Candidate
    );
    assert!(index.pending());
    assert_eq!(
        push(&mut index, at + 1, "turn_completed", None, true),
        Cut::None
    );
    assert!(!index.pending());
    // The step's text opens nothing: the candidate died with the turn.
    assert_eq!(
        push(&mut index, at + 2, "text_completed", Some("a_m"), true),
        Cut::None
    );
    assert_eq!(index.pages().len(), 1);
}

#[test]
fn nothing_pends_on_a_fresh_index() {
    assert!(!Index::default().pending());
}

#[test]
fn page_of_holds_only_lines_on_a_page() {
    // The fresh index's open page holds no lines: nothing is on it.
    let empty = Index::default();
    assert_eq!(empty.page_of(Seq(0)), None);
    assert_eq!(empty.page_of(Seq(9)), None);
    // Both sides of a cut between two pages holding lines.
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    let edge = PAGE_LINES as u64;
    assert_eq!(index.page_of(Seq(edge - 1)), Some(0));
    assert_eq!(index.page_of(Seq(edge)), Some(1));
    assert_eq!(index.page_of(Seq(edge + 1)), None);
}

#[test]
fn thinking_shown_opens_a_group_for_later_thinking() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let mut at = seq;
    assert_eq!(
        push(&mut index, at, "step_started", None, true),
        Cut::Candidate
    );
    at += 1;
    // Shown thinking opens a group; completing it only ends the flight.
    assert_eq!(
        push(&mut index, at, "reasoning_started", Some("a_r"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "reasoning_completed", Some("a_r"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "step_started", None, true),
        Cut::Candidate
    );
    at += 1;
    // The thinking joins the open group, so the candidate is discarded and
    // the step's text cuts nothing.
    assert_eq!(
        push(&mut index, at, "reasoning_started", Some("a_r2"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "reasoning_completed", Some("a_r2"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "text_completed", Some("a_m"), true),
        Cut::None
    );
    assert_eq!(index.pages().len(), 1);
}

#[test]
fn a_call_shown_opens_a_group_for_later_thinking() {
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    let mut at = seq;
    assert_eq!(
        push(&mut index, at, "tool_call_requested", Some("a_t"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "tool_call_completed", Some("a_t"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "step_started", None, true),
        Cut::Candidate
    );
    at += 1;
    // The thinking joins the call's open group, shown on no card or not,
    // so the candidate is discarded and the step's text cuts nothing.
    assert_eq!(
        push(&mut index, at, "reasoning_started", Some("a_r"), false),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "reasoning_completed", Some("a_r"), true),
        Cut::None
    );
    at += 1;
    assert_eq!(
        push(&mut index, at, "text_completed", Some("a_m"), true),
        Cut::None
    );
    assert_eq!(index.pages().len(), 1);
}

#[test]
fn locate_finds_the_page_and_offset_of_a_row() {
    let index = with_rows(&[3, 0, 5, 2]);
    // (row, the page drawing it and the row's offset in it)
    let cases = [
        (0, Some((0, 0))),
        (2, Some((0, 2))),
        // Page 1 draws no row, so the row after page 0's last is page 2's.
        (3, Some((2, 0))),
        (7, Some((2, 4))),
        (8, Some((3, 0))),
        (9, Some((3, 1))),
        (10, None),
    ];
    for (row, expected) in cases {
        assert_eq!(index.locate(row), expected, "row {row}");
    }
}

#[test]
fn start_is_the_sum_of_the_rows_before() {
    let index = with_rows(&[3, 0, 5, 2]);
    let starts: Vec<usize> = (0..4).map(|at| index.start(at)).collect();
    assert_eq!(starts, index.starts());
    assert_eq!(index.start(4), 10, "past the last page is the total");
}

#[test]
fn set_rows_bumps_the_page_revision() {
    let mut index = Index::default();
    assert_eq!(index.revision(0), 0);
    index.set_rows(0, 10);
    assert_eq!(index.revision(0), 1);
    // A recount bumps it whether or not the count changed.
    index.set_rows(0, 10);
    assert_eq!(index.revision(0), 2);
}

#[test]
fn a_new_page_starts_at_revision_zero() {
    let mut index = Index::default();
    index.set_rows(0, 10);
    let mut seq = 1u64;
    filler(&mut index, &mut seq, PAGE_LINES);
    push(&mut index, seq, "turn_started", None, true);
    assert_eq!(index.pages().len(), 2);
    assert_eq!(index.revision(1), 0);
}

#[test]
fn a_window_near_the_top_counts_only_the_pages_it_walks() {
    let mut index = Index::default();
    let mut seq = 0;
    for _ in 0..6 {
        filler(&mut index, &mut seq, PAGE_LINES);
        feed(&mut index, &mut seq, TEXT_STEP);
    }
    let pages = index.pages().len();
    assert!(pages > 4);
    for at in 0..pages {
        index.set_rows(at, 10);
    }
    crate::work::take();
    assert_eq!(index.window(0, 5), 0..1);
    // `total` scans every page once; the walk itself stops at the second.
    assert_eq!(crate::work::take().index_pages, pages + 2);
}
