//! Tests for the usage view's fold and frame (`docs/tui.md`, "Swapped views").

use super::{UsageFold, budget_row, frame};
use crate::swapped::{List, render};
use contract::events::{DelegateStarted, UsageRecorded};
use contract::shapes::Tokens;
use contract::{GenerationId, JobId, SessionId, TurnId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn recorded(
    generation: &str,
    model: &str,
    input: u64,
    cost: Option<f64>,
    origin: Option<&str>,
) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(generation.to_owned()),
        model: model.to_owned(),
        tokens: Tokens {
            input,
            cache_read: 5_678,
            cache_write: [("5m".to_owned(), 90), ("1h".to_owned(), 10)]
                .into_iter()
                .collect(),
            output: 1_000,
        },
        input_bytes: 0,
        input_media: None,
        web_searches: None,
        cost,
        subscription: None,
        extension: None,
        origin_session_id: origin.map(|id| SessionId(id.to_owned())),
        reviewer: None,
        warming: None,
    }
}

fn lines(fold: &UsageFold, budget: Option<f64>) -> Vec<String> {
    frame(fold, budget, List::default())
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect()
}

fn text(fold: &UsageFold, budget: Option<f64>) -> String {
    lines(fold, budget).join("\n")
}

fn turn(id: &str) -> TurnId {
    TurnId(id.to_owned())
}

fn delegate(job: &str, session: &str, model: &str) -> DelegateStarted {
    DelegateStarted {
        job_id: JobId(job.to_owned()),
        delegate_session_id: SessionId(session.to_owned()),
        harness: "fiber".to_owned(),
        model: model.to_owned(),
        workspace: "/workspace".to_owned(),
        worktree: None,
        forked_from: None,
    }
}

#[test]
fn an_entry_is_one_row_with_compact_counts() {
    // The doc's numbers: cache kinds show only when non-zero.
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(
        Some(turn("t1")),
        UsageRecorded {
            generation_id: GenerationId("g1".to_owned()),
            model: "m".to_owned(),
            tokens: Tokens {
                input: 120_000,
                cache_read: 0,
                cache_write: Default::default(),
                output: 15_000,
            },
            input_bytes: 0,
            input_media: None,
            web_searches: None,
            cost: Some(1.35),
            subscription: None,
            extension: None,
            origin_session_id: None,
            reviewer: None,
            warming: None,
        },
    );
    let frame = frame(&fold, None, List::default());
    let headings: Vec<&str> = frame
        .rows
        .iter()
        .filter(|row| {
            row.iter()
                .any(|(_, _, ink)| *ink == crate::swapped::Ink::Heading)
        })
        .map(|row| row[0].0.as_str())
        .collect();
    assert_eq!(headings, ["By turn", "By model", "By delegate"]);
    let got = lines(&fold, None);
    assert!(
        got.contains(&"turn 1 · 120k in · 15k out · $1.35 billed · $0.00 subscription".to_owned())
    );
}

#[test]
fn a_turn_with_cache_reads_adds_the_cache_kinds() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(
        Some(turn("t1")),
        recorded("g1", "m", 120_000, Some(1.35), None),
    );
    let got = lines(&fold, None);
    assert!(got.contains(
        &"turn 1 · 120k in · 1k out · 6k cache read · 100 cache write · $1.35 billed · $0.00 subscription"
            .to_owned()
    ));
}

#[test]
fn the_budget_row_is_last_dim_and_single_spaced() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(Some(turn("t1")), recorded("g1", "m", 1, Some(0.1), None));
    let frame = frame(&fold, Some(5.0), List::default());
    let last = frame.rows.last().expect("a last row");
    assert_eq!(last[0].0, "budget left $4.90 of $5.00");
    assert_eq!(last[0].2, crate::swapped::Ink::Muted);
}

#[test]
fn a_replacement_is_latest_in_every_total_but_stays_in_its_first_turn() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.turn_started(turn("t2"));
    fold.job_started(JobId("j1".to_owned()), "first delegate".to_owned());
    fold.delegate_started(&delegate("j1", "s_delegate", "model/old"));
    fold.job_started(JobId("j2".to_owned()), "second delegate".to_owned());
    fold.delegate_started(&delegate("j2", "s_other", "model/new"));
    fold.recorded(
        Some(turn("t1")),
        recorded("g1", "z/model", 10, Some(0.25), Some("s_delegate")),
    );
    fold.recorded(
        Some(turn("t2")),
        recorded("g1", "a/model", 20, Some(0.5), Some("s_other")),
    );

    let got = text(&fold, None);
    assert_eq!(
        got.matches(
            "20 in · 1k out · 6k cache read · 100 cache write · $0.50 billed · $0.00 subscription"
        )
        .count(),
        3
    );
    assert!(got.contains("turn 1 · 20 in"));
    assert!(!got.contains("turn 2"));
    assert!(got.contains("a/model"));
    assert!(!got.contains("z/model"));
    assert!(got.contains("◆ second delegate · model/new"));
    assert!(!got.contains("◆ first delegate · model/old"));
}

#[test]
fn delegate_copies_use_the_started_label() {
    let mut fold = UsageFold::default();
    fold.job_started(JobId("j1".to_owned()), "review the diff".to_owned());
    fold.delegate_started(&delegate("j1", "s_delegate", "openai/gpt-y"));
    fold.recorded(
        None,
        recorded("g1", "openai/gpt-y", 1, Some(0.1), Some("s_delegate")),
    );
    assert!(text(&fold, None).contains("◆ review the diff · openai/gpt-y"));
}

#[test]
fn an_origin_without_a_delegate_started_line_uses_its_session_id() {
    let mut fold = UsageFold::default();
    fold.recorded(None, recorded("g1", "m", 1, Some(0.1), Some("s_unknown")));
    assert!(text(&fold, None).contains("session s_unknown"));
}

#[test]
fn extension_calls_without_an_origin_belong_to_this_session() {
    let mut fold = UsageFold::default();
    let mut call = recorded("g1", "m", 1, Some(0.1), None);
    call.extension = Some("reviewer".to_owned());
    fold.recorded(None, call);
    assert!(text(&fold, None).contains("By delegate\nthis session · "));
}

#[test]
fn calls_without_a_known_turn_are_listed_last() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(Some(turn("t1")), recorded("g1", "m", 1, Some(0.1), None));
    fold.recorded(None, recorded("g2", "m", 2, Some(0.2), None));
    fold.recorded(
        Some(turn("unknown")),
        recorded("g3", "m", 3, Some(0.3), None),
    );
    let got = lines(&fold, None);
    let outside = got
        .iter()
        .position(|line| line.starts_with("outside a turn · "))
        .expect("outside a turn is shown");
    let models = got
        .iter()
        .position(|line| line == "By model")
        .expect("models follow turns");
    assert!(outside > 0);
    assert!(outside < models);
    assert_eq!(
        got.get(outside).map(String::as_str),
        Some(
            "outside a turn · 5 in · 2k out · 11k cache read · 200 cache write · $0.50 billed · $0.00 subscription"
        )
    );
    assert_eq!(
        got.iter()
            .filter(|line| line.starts_with("outside a turn"))
            .count(),
        1
    );
    assert!(!got.iter().any(|line| line.starts_with("turn unknown")));
}

#[test]
fn turn_numbers_follow_start_arrival_order() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t2"));
    fold.turn_started(turn("t1"));
    fold.recorded(Some(turn("t2")), recorded("g1", "m", 2, Some(0.1), None));
    fold.recorded(Some(turn("t1")), recorded("g2", "m", 1, Some(0.1), None));
    let got = lines(&fold, None);
    let first = got
        .iter()
        .position(|line| line.starts_with("turn 1 · "))
        .expect("the first arrival is numbered first");
    let second = got
        .iter()
        .position(|line| line.starts_with("turn 2 · "))
        .expect("the second arrival is numbered second");
    assert!(first < second);
    assert_eq!(
        got.get(first).map(String::as_str),
        Some(
            "turn 1 · 2 in · 1k out · 6k cache read · 100 cache write · $0.10 billed · $0.00 subscription"
        )
    );
}

#[test]
fn models_are_listed_in_ascending_order() {
    let mut fold = UsageFold::default();
    fold.recorded(None, recorded("g1", "z/model", 1, Some(0.1), None));
    fold.recorded(None, recorded("g2", "a/model", 2, Some(0.2), None));
    let got = lines(&fold, None);
    let first = got.iter().position(|line| line.starts_with("a/model · "));
    let second = got.iter().position(|line| line.starts_with("z/model · "));
    assert!(first < second);
}

#[test]
fn each_entry_is_one_row_after_its_group_heading() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(Some(turn("t1")), recorded("g1", "m", 1, Some(0.1), None));
    let got = lines(&fold, None);
    for (heading, entry) in [
        ("By turn", "turn 1 · "),
        ("By model", "m · "),
        ("By delegate", "this session · "),
    ] {
        let at = got.iter().position(|line| line == heading).expect(heading);
        assert!(
            got.get(at + 1).is_some_and(|line| line.starts_with(entry)),
            "{entry} follows {heading}: {got:?}"
        );
    }
    assert!(!got.iter().any(|line| line == "session"));
}

#[test]
fn null_billed_cost_reads_unknown() {
    let mut fold = UsageFold::default();
    fold.recorded(None, recorded("g1", "m", 1, None, None));
    assert!(text(&fold, None).contains(" · unknown billed · $0.00 subscription"));
}

#[test]
fn budget_left_counts_only_known_billed_cost_and_never_goes_below_zero() {
    let cases = [
        (Some(1.25), "$3.75 of $5.00"),
        (Some(5.0), "$0.00 of $5.00"),
        (Some(7.0), "$0.00 of $5.00"),
        (None, "$5.00 of $5.00"),
    ];
    for (cost, expected) in cases {
        let mut fold = UsageFold::default();
        fold.recorded(None, recorded("g1", "m", 1, cost, None));
        assert!(text(&fold, Some(5.0)).contains(&format!("budget left {expected}")));
    }
}

#[test]
fn budget_row_takes_the_billed_amount() {
    let mut rows = Vec::new();
    budget_row(&mut rows, Some(5.0), 1.25);
    assert_eq!(rows[0][0].0, "budget left $3.75 of $5.00");
    assert_eq!(rows[0][0].2, crate::swapped::Ink::Muted);
}

#[test]
fn subscription_cost_does_not_reduce_the_budget_left() {
    let mut fold = UsageFold::default();
    let mut call = recorded("g1", "m", 1, Some(4.0), None);
    call.subscription = Some(true);
    fold.recorded(None, call);
    assert!(text(&fold, Some(5.0)).contains("budget left $5.00 of $5.00"));
}

#[test]
fn no_budget_means_no_budget_row() {
    let mut fold = UsageFold::default();
    fold.recorded(None, recorded("g1", "m", 1, Some(0.1), None));
    assert!(!text(&fold, None).contains("budget left"));
}

#[test]
fn an_empty_fold_says_no_model_calls_then_its_budget() {
    let fold = UsageFold::default();
    assert_eq!(
        lines(&fold, Some(5.0)),
        ["No model calls yet.", "budget left $5.00 of $5.00"]
    );
}

#[test]
fn an_empty_fold_without_a_budget_only_says_no_model_calls() {
    let fold = UsageFold::default();
    assert_eq!(lines(&fold, None), ["No model calls yet."]);
}

#[test]
fn the_usage_view_snapshot_is_rendered_through_the_shared_frame() {
    let mut fold = UsageFold::default();
    fold.turn_started(turn("t1"));
    fold.recorded(
        Some(turn("t1")),
        recorded("g1", "anthropic/claude-x", 1_234, Some(0.12), None),
    );
    let frame = frame(&fold, Some(5.0), List::default());
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&frame, area, &mut buffer, &mut targets);
    let screen = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("usage_80x24", screen);
}
