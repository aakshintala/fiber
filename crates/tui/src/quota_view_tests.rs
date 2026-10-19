//! The `/quota` view draws one row per entry of the `quota` answer, in
//! its order, with the panel card's figures and bars.

use contract::events::QuotaInfo;
use serde_json::json;

use super::{frame, resets_in};
use crate::swapped::{Ink, List};
use crate::theme::Role;

const NOW_MS: u64 = 1_700_000_000_000;
const NOTICE_AT: f64 = 80.0;

fn quota(value: serde_json::Value) -> Vec<QuotaInfo> {
    serde_json::from_value(value).unwrap()
}

fn window(name: &str, used: f64, resets_in_min: u64) -> serde_json::Value {
    json!({"name": name, "percent_used": used,
        "resets_at": NOW_MS + resets_in_min * 60_000})
}

fn rows() -> Vec<QuotaInfo> {
    quota(json!([
        {"provider": "codex", "credential": "default", "windows": [
            window("5h", 38.0, 192),
            window("weekly", 69.0, 3120),
        ]},
        {"provider": "claude", "credential": "default",
            "windows": [window("weekly", 35.0, 0)], "unreachable": true},
        {"provider": "gone", "credential": "default", "unreachable": true},
    ]))
}

/// The frame's text rows: each row's cells joined.
fn text(frame: &crate::swapped::Frame) -> Vec<String> {
    frame
        .rows
        .iter()
        .map(|cells| {
            cells
                .iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect()
}

fn show(rows: &[QuotaInfo], fetching: bool) -> crate::swapped::Frame {
    frame(rows, NOTICE_AT, fetching, List::default(), 160, NOW_MS)
}

#[test]
fn the_reset_time_rounds_minutes_down() {
    assert_eq!(resets_in(59), "59m");
    assert_eq!(resets_in(60), "1h0m");
    assert_eq!(resets_in(61), "1h1m");
    assert_eq!(resets_in(192), "3h12m");
    assert_eq!(resets_in(1439), "23h59m");
    assert_eq!(resets_in(1440), "1d0h");
    assert_eq!(resets_in(1441), "1d0h");
    assert_eq!(resets_in(3120), "2d4h");
}

#[test]
fn empty_names_what_is_missing() {
    let frame = show(&[], false);
    assert_eq!(frame.title, "Quota");
    assert_eq!(frame.command, "/quota");
    assert_eq!(text(&frame), ["No provider reports quota"]);
    assert_eq!(
        frame.rows[0][0],
        ("No provider reports quota".to_owned(), None, Ink::Muted)
    );
}

#[test]
fn fetching_marks_the_first_row_over_the_cached_answer() {
    let frame = show(&rows()[..1], true);
    let lines = text(&frame);
    assert_eq!(lines[0], "fetching");
    assert_eq!(frame.rows[0][0].2, Ink::Muted);
    assert!(lines[1].contains("codex · default"), "{lines:?}");
}

#[test]
fn entries_read_provider_then_credential_with_their_windows() {
    let frame = show(&rows(), false);
    let lines = text(&frame);
    assert_eq!(frame.title, "Quota");
    assert_eq!(frame.command, "/quota");
    assert!(
        !lines.iter().any(|line| line.contains("fetching")),
        "{lines:?}"
    );
    let codex = lines
        .iter()
        .position(|line| line == "codex · default")
        .unwrap();
    assert_eq!(frame.rows[codex][0].2, Ink::Heading);
    assert_eq!(frame.rows[codex][1].2, Ink::Muted);
    // The figures are the remaining shares, as the panel card draws them.
    assert_eq!(lines[codex + 1], "5h  62%");
    assert!(lines[codex + 3].contains("resets in 3h12m"), "{lines:?}");
    assert_eq!(lines[codex + 4], "weekly  31%");
    assert!(lines[codex + 6].contains("resets in 2d4h"), "{lines:?}");
    // Each reset time reads on the row after its window's bar.
    assert!(lines[codex + 2].contains('▆'), "{lines:?}");
    assert!(lines[codex + 5].contains('▆'), "{lines:?}");
    // Entries are separated by one blank row.
    let claude = lines
        .iter()
        .position(|line| line == "claude · default")
        .unwrap();
    assert_eq!(lines[claude - 1], "");
    // An unreachable entry shows its last value and the mark in error.
    let mark = lines
        .iter()
        .position(|line| line.contains("unreachable"))
        .unwrap();
    assert_eq!(lines[mark], "65% · unreachable");
    assert_eq!(frame.rows[mark][0].2, Ink::Plain);
    assert_eq!(frame.rows[mark][1].2, Ink::Muted);
    assert_eq!(frame.rows[mark][2].2, Ink::Role(Role::Error));
    // An unreachable entry with no last value shows the mark alone.
    let gone = lines
        .iter()
        .position(|line| line == "gone · default")
        .unwrap();
    assert_eq!(lines[gone + 1], "unreachable");
    assert_eq!(frame.rows[gone + 1][0].2, Ink::Role(Role::Error));
}

#[test]
fn little_left_reads_in_error() {
    let rows = quota(json!([
        {"provider": "codex", "credential": "default",
            "windows": [window("weekly", 95.0, 0)]},
    ]));
    let frame = show(&rows, false);
    let lines = text(&frame);
    assert_eq!(lines[1], "weekly  5%");
    assert_eq!(frame.rows[1][1].2, Ink::Role(Role::Error));
    assert_eq!(frame.rows[2][0].2, Ink::Role(Role::Error));
}

#[test]
fn the_bar_caps_at_forty_cells() {
    let rows = quota(json!([
        {"provider": "codex", "credential": "default",
            "windows": [window("weekly", 38.0, 0)]},
    ]));
    for (width, cells) in [(160u16, 40), (100, 40), (20, 20)] {
        let frame = frame(&rows, NOTICE_AT, false, List::default(), width, NOW_MS);
        let bar = &frame.rows[2];
        let drawn: String = bar.iter().map(|(text, _, _)| text.as_str()).collect();
        assert_eq!(drawn.chars().count(), cells, "width {width}");
        assert_eq!(bar.len(), 2);
    }
}

#[test]
fn a_reset_window_reads_reset_with_a_full_bar() {
    let rows = quota(json!([
        {"provider": "codex", "credential": "default",
            "windows": [{"name": "weekly", "reset": true}]},
    ]));
    let frame = show(&rows, false);
    let lines = text(&frame);
    assert_eq!(lines[1], "weekly  reset");
    assert_eq!(lines[2].chars().count(), 40);
    assert!(!lines[2].contains(' '), "{lines:?}");
}

#[test]
fn credit_reads_its_figure_and_bar() {
    let rows = quota(json!([
        {"provider": "codex", "credential": "default",
            "credit": {"remaining": 4.2, "limit": 10.0}},
    ]));
    let frame = show(&rows, false);
    let lines = text(&frame);
    assert_eq!(lines[1], "credit  42%");
    assert!(lines[2].contains('▆'), "{lines:?}");
}
