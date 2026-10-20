//! Tests for the page index: the cut rule, seq ranges and the window.

use contract::{ActionId, Seq, SessionId};

use super::{Cut, Index, PAGE_LINES, Page};

/// The session every line here folds under: one segment, so no page
/// spans a continuation point.
fn session() -> SessionId {
    SessionId("s_aaaaaaaaaaaaaaaa".to_owned())
}

/// Pushes one line, shown on a card or not.
fn push(index: &mut Index, seq: u64, kind: &str, action: Option<&str>, shown: bool) -> Cut {
    let action = action.map(|id| ActionId(id.to_owned()));
    index.push(&session(), Seq(seq), kind, action.as_ref(), shown)
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
        session: session(),
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
    assert_eq!(index.page_of(&session(), Seq(0)), Some(0));
    assert_eq!(index.page_of(&session(), Seq(boundary - 1)), Some(0));
    assert_eq!(index.page_of(&session(), Seq(boundary)), Some(1));
    assert_eq!(index.page_of(&session(), Seq(boundary + 1)), None);
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
    assert_eq!(empty.page_of(&session(), Seq(0)), None);
    assert_eq!(empty.page_of(&session(), Seq(9)), None);
    // Both sides of a cut between two pages holding lines.
    let mut index = Index::default();
    let mut seq = 0;
    filler(&mut index, &mut seq, PAGE_LINES);
    feed(&mut index, &mut seq, &[("turn_started", None)]);
    let edge = PAGE_LINES as u64;
    assert_eq!(index.page_of(&session(), Seq(edge - 1)), Some(0));
    assert_eq!(index.page_of(&session(), Seq(edge)), Some(1));
    assert_eq!(index.page_of(&session(), Seq(edge + 1)), None);
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
fn set_rows_leaves_the_text_revision_alone() {
    let mut index = Index::default();
    assert_eq!(index.revision(0), 0);
    // Geometry alone never moves the revision: a recount, a reload, or
    // an edge coming off keeps what the search scanned.
    index.set_rows(0, 10);
    assert_eq!(index.revision(0), 0);
    index.set_rows(0, 10);
    assert_eq!(index.revision(0), 0);
    index.set_rows(0, 4);
    assert_eq!(index.revision(0), 0);
    // A missing page stays harmless.
    index.set_rows(9, 10);
    assert_eq!(index.revision(9), 0);
}

#[test]
fn invalidate_moves_the_text_revision() {
    let mut index = Index::default();
    index.set_rows(0, 10);
    index.invalidate(0);
    assert_eq!(index.revision(0), 1);
    // Geometry after the invalidation keeps the bumped revision.
    index.set_rows(0, 4);
    assert_eq!(index.revision(0), 1);
    index.invalidate(0);
    assert_eq!(index.revision(0), 2);
    // A missing page stays harmless.
    index.invalidate(9);
    assert_eq!(index.revision(9), 0);
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
fn a_window_rebuilds_prefixes_once_then_queries_are_free() {
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
    // One rebuild visits every page once; the window's binary searches
    // then read the cache.
    assert_eq!(crate::work::take().index_pages, pages);
    // Every later query is free while no row changes: every page holds
    // ten rows, so the fourth starts at row 30.
    crate::work::take();
    assert_eq!(index.start(3), 30);
    assert_eq!(index.total(), pages * 10);
    assert_eq!(index.locate(35), Some((3, 5)));
    // Repeated window queries read the same cache: binary searches over
    // cached prefixes visit no page, whatever the range.
    assert_eq!(index.window(0, 5), 0..1);
    assert_eq!(index.window(30, 5), 2..4);
    assert_eq!(index.window(pages * 10, 0), pages..pages);
    assert_eq!(crate::work::take().index_pages, 0);
}

#[test]
fn a_noop_set_rows_rebuilds_nothing() {
    let index = with_rows(&[3, 0, 5, 2]);
    crate::work::take();
    assert_eq!(index.total(), 10);
    let built = crate::work::take().index_pages;
    // Setting the rows a page already has marks nothing dirty.
    let mut index = index;
    index.set_rows(2, 5);
    crate::work::take();
    assert_eq!(index.total(), 10);
    assert_eq!(index.start(3), 8);
    assert_eq!(crate::work::take().index_pages, 0);
    assert!(built > 0);
}

#[test]
fn fresh_empty_and_zero_row_pages_query_safely() {
    // A fresh index draws nothing: every query is free and empty.
    let index = Index::default();
    crate::work::take();
    assert_eq!(index.total(), 0);
    assert_eq!(index.start(0), 0);
    assert_eq!(index.locate(0), None);
    assert_eq!(index.window(0, 5), 1..1);
    assert_eq!(crate::work::take().index_pages, 0);
    // Pages drawing no row at all, and zeros trailing real rows.
    let index = with_rows(&[0, 0, 0]);
    assert_eq!(index.total(), 0);
    assert_eq!(index.locate(0), None);
    assert_eq!(index.window(0, 5), 3..3);
    let index = with_rows(&[5, 0, 0]);
    assert_eq!(index.total(), 5);
    assert_eq!(index.locate(4), Some((0, 4)));
    assert_eq!(index.locate(5), None);
    assert_eq!(index.window(0, 2), 0..1);
}

#[test]
fn an_appended_page_rebuilds_prefixes_once() {
    let mut index = with_rows(&[3, 0, 5, 2]);
    crate::work::take();
    assert_eq!(index.total(), 10);
    assert_eq!(crate::work::take().index_pages, 4);
    // One more page: the next query rebuilds once for all five.
    let mut seq = 100u64;
    filler(&mut index, &mut seq, PAGE_LINES);
    push(&mut index, seq, "turn_started", None, true);
    index.set_rows(4, 7);
    crate::work::take();
    assert_eq!(index.total(), 17);
    assert_eq!(index.start(4), 10);
    assert_eq!(index.locate(16), Some((4, 6)));
    assert_eq!(crate::work::take().index_pages, 5);
}

#[test]
fn window_boundaries_are_exact() {
    // Prefixes [0, 10, 10, 20, 30]: page 1 draws no row, ending where it
    // starts.
    let index = with_rows(&[10, 0, 10, 10]);
    // A zero-height window on the shared edge finds the page past it:
    // page 1 ends at 10, never past it, and page 2 starts there. The
    // empty range keeps the lower bound, never inverts it.
    assert_eq!(index.window(10, 0), 2..2);
    // A page ending exactly at the window's lower edge stays out, while
    // one starting exactly at its upper edge does too.
    assert_eq!(index.window(20, 10), 2..4);
    assert_eq!(index.window(0, 10), 0..3);
    // The whole conversation, zero-row page included.
    assert_eq!(index.window(10, 10), 0..4);
}

#[test]
fn prefixes_saturate_rather_than_subtract() {
    // A saturated total stays saturated: no page after the overflow
    // undercounts by subtraction.
    let index = with_rows(&[usize::MAX, 1]);
    assert_eq!(index.total(), usize::MAX);
    assert_eq!(index.start(1), usize::MAX);
    assert_eq!(index.locate(usize::MAX - 1), Some((0, usize::MAX - 1)));
    assert_eq!(index.locate(usize::MAX), None);
    // Saturation clamps the window's upper edge to the total, and a top
    // past every row finds no page past the last one.
    assert_eq!(index.window(0, 5), 0..1);
    assert_eq!(index.window(usize::MAX, 0), 2..2);
    // Duplicate prefixes from pages drawing no row locate past them.
    let index = with_rows(&[3, 0, 0, 5]);
    assert_eq!(index.total(), 8);
    assert_eq!(index.start(3), 3);
    assert_eq!(index.locate(3), Some((3, 0)));
    // A zero-height window on the duplicated edge is empty, never
    // inverted.
    assert_eq!(index.window(3, 0), 3..3);
    // Out of range in both directions.
    assert_eq!(index.locate(8), None);
    assert_eq!(index.start(99), 8);
}

/// The ancestor's session: a second segment numbering from zero, like a
/// real chain.
fn ancestor() -> SessionId {
    SessionId("s_bbbbbbbbbbbbbbbb".to_owned())
}

/// One page-cutting chunk of `session` lines from `seq`: fillers, then a
/// `turn_started` that cuts the next page.
fn chunk(index: &mut Index, session: &SessionId, seq: &mut u64) {
    for _ in 0..PAGE_LINES {
        index.push(session, Seq(*seq), "usage_recorded", None, true);
        *seq += 1;
    }
    index.push(session, Seq(*seq), "turn_started", None, true);
    *seq += 1;
}

/// Every line on every page resolves to its own page under its session:
/// the lookup searches only the named session's runs.
fn check_runs(index: &Index) {
    for (at, page) in index.pages().iter().enumerate() {
        if page.lines == 0 {
            continue;
        }
        let mut seq = page.first_seq.0;
        while seq <= page.last_seq.0 {
            assert_eq!(
                index.page_of(&page.session, Seq(seq)),
                Some(at),
                "session {} seq {seq}",
                page.session.0
            );
            seq += 1;
        }
    }
}

/// `page_of` searches only the named session's runs: both segments number
/// from zero, so a bare `seq` match would land on the wrong run. The
/// ancestor runs twice, so the lookup must walk every matching run, not
/// just the first.
#[test]
fn page_of_searches_only_the_named_sessions_run() {
    let mut index = Index::default();
    let mut aseq = 0u64;
    chunk(&mut index, &ancestor(), &mut aseq);
    chunk(&mut index, &ancestor(), &mut aseq);
    let mut oseq = 0u64;
    chunk(&mut index, &session(), &mut oseq);
    chunk(&mut index, &session(), &mut oseq);
    chunk(&mut index, &ancestor(), &mut aseq);
    check_runs(&index);
    // The two segments share their seqs, yet resolve apart.
    let seam = index
        .pages()
        .iter()
        .position(|page| page.session == session())
        .expect("the own run");
    assert!(seam > 0, "the ancestor runs first");
    assert_eq!(index.page_of(&ancestor(), Seq(0)), Some(0));
    assert_eq!(index.page_of(&session(), Seq(0)), Some(seam));
    // The ancestor's later run sits past the own run.
    let later = index
        .page_of(&ancestor(), Seq(aseq.saturating_sub(1)))
        .expect("the ancestor's last seq");
    assert!(later > seam, "the second ancestor run follows the own run");
    // Unknown sessions and seqs past every page miss.
    assert_eq!(
        index.page_of(&SessionId("s_cccccccccccccccc".to_owned()), Seq(0)),
        None
    );
    assert_eq!(index.page_of(&ancestor(), Seq(aseq)), None);
    assert_eq!(index.page_of(&session(), Seq(oseq)), None);
}

/// `prepend_from` carries the runs with the pages and shifts the fold's
/// own runs by the carried count, so both sides still resolve. Carrying
/// nothing, or carrying from an index that recorded no run, keeps the
/// index as it was.
#[test]
fn prepend_from_shifts_the_runs_it_keeps() {
    let mut other = Index::default();
    let mut aseq = 0u64;
    chunk(&mut other, &ancestor(), &mut aseq);
    chunk(&mut other, &ancestor(), &mut aseq);
    let mut index = Index::default();
    let mut oseq = 0u64;
    chunk(&mut index, &session(), &mut oseq);
    chunk(&mut index, &session(), &mut oseq);
    let own_pages = index.pages().len();
    let carried = other.pages().len();
    index.prepend_from(&other, carried);
    assert_eq!(index.pages().len(), carried + own_pages);
    check_runs(&index);
    let seam = index
        .pages()
        .iter()
        .position(|page| page.session == session())
        .expect("the own run");
    assert_eq!(seam, carried, "the carried runs land ahead of the fold's");
    assert_eq!(index.page_of(&ancestor(), Seq(0)), Some(0));
    assert_eq!(index.page_of(&session(), Seq(0)), Some(carried));
    // Carrying nothing, or carrying from an index with no run, is a no-op.
    let len = index.pages().len();
    index.prepend_from(&other, 0);
    assert_eq!(index.pages().len(), len);
    index.prepend_from(&Index::default(), 1);
    assert_eq!(index.pages().len(), len);
    check_runs(&index);
}

/// An empty open page takes only a changed session: carrying the
/// ancestor runs lands them ahead of the fold's empty open page, and
/// the next own line starts its run there without a cut. Kills the
/// `!=` to `==` mutant, which would leave the own line on the
/// ancestor's run. This is the land path: a fresh fold's open page is
/// empty with runs already carried.
#[test]
fn an_empty_open_page_takes_only_a_changed_session() {
    let mut other = Index::default();
    let mut aseq = 0u64;
    chunk(&mut other, &ancestor(), &mut aseq);
    let carried = other.pages().len();
    let mut index = Index::default();
    index.prepend_from(&other, carried);
    assert_eq!(
        index.pages().len(),
        carried + 1,
        "the carried runs land ahead of the empty open page"
    );
    let cut = index.push(&session(), Seq(0), "usage_recorded", None, true);
    assert_eq!(cut, Cut::None, "nothing folded yet, so nothing cuts");
    assert_eq!(
        index.pages().last().map(|page| page.session.clone()),
        Some(session()),
        "the empty open page takes the pushed session"
    );
    assert_eq!(
        index.page_of(&session(), Seq(0)),
        Some(carried),
        "the own run starts past the carried runs"
    );
}

/// A page holding no lines holds no seq: its range may still name it,
/// but nothing is on it. Kills the `>` to `>=` mutant on the
/// `page.lines > 0` guard, which would seat a seq on an empty page.
#[test]
fn page_of_skips_a_page_holding_no_lines() {
    let mut index = Index::default();
    push(&mut index, 0, "usage_recorded", None, true);
    // A page covering 1..=5 that holds no lines: its range still names
    // it, but no line folded there.
    index.pages.push(Page {
        session: session(),
        first_seq: Seq(1),
        last_seq: Seq(5),
        lines: 0,
        rows: 0,
    });
    index.revisions.push(0);
    assert_eq!(index.page_of(&session(), Seq(0)), Some(0));
    assert_eq!(
        index.page_of(&session(), Seq(3)),
        None,
        "no line folded on the empty page"
    );
}

/// `prepend_from` stops before the run starting at the boundary: a run
/// whose first page is `len` stays with the fold. Kills the `<` to
/// `<=` mutant on the `segment.first < len` take, which would carry a
/// run with no carried page in it.
#[test]
fn prepend_from_stops_before_the_run_at_the_boundary() {
    let mut other = Index::default();
    let mut aseq = 0u64;
    chunk(&mut other, &ancestor(), &mut aseq);
    let mut bseq = 0u64;
    chunk(&mut other, &session(), &mut bseq);
    assert_eq!(other.pages().len(), 4, "two pages per chunk");
    let mut index = Index::default();
    let mut oseq = 0u64;
    chunk(&mut index, &session(), &mut oseq);
    let own_pages = index.pages().len();
    // `len` is the own run's first page: exactly the second run's start.
    index.prepend_from(&other, 2);
    assert_eq!(index.pages().len(), 2 + own_pages);
    assert_eq!(
        index.segments.len(),
        2,
        "only the ancestor run carries; the run starting at the boundary stays"
    );
    check_runs(&index);
}
