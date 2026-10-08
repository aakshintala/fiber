//! Tests for the page scanners and snippet builders
//! (`docs/tui.md`, "Search"): what a match is in a page's text.

use super::super::tests::{attached, count, search_all, text_turn};

#[test]
fn a_snippet_is_its_line_and_one_line_either_side_cut_to_400() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let long: String = format!("{}needle{}", "x".repeat(250), "x".repeat(250));
    let exact: String = format!("needle{}", "y".repeat(394));
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["the line before", &long, &exact, "the line after"],
    );
    assert!(search_all(&mut app, &log, "needle").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 2);
    // The long line is cut to 400 characters around the match.
    let first = &flat[0].snippet;
    assert_eq!(first.line.chars().count(), super::SNIPPET);
    assert_eq!(first.at, 197..203);
    assert_eq!(&first.line[first.at.start..first.at.end], "needle");
    assert_eq!(first.before, "the line before");
    assert_eq!(first.after, exact);
    // A line of exactly 400 characters stays whole.
    let second = &flat[1].snippet;
    assert_eq!(second.line, exact);
    assert_eq!(second.at, 0..6);
    assert_eq!(
        second.before,
        long.chars().take(super::SNIPPET).collect::<String>()
    );
    assert_eq!(second.after, "the line after");
}

#[test]
fn two_matches_on_lines_sharing_their_first_400_characters_stay_distinct() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let shared = "s".repeat(400);
    let one = format!("{shared}needle one");
    let two = format!("{shared}needle two");
    let end = format!("{}needle", "z".repeat(400));
    text_turn(&mut log, &mut seq, &mut app, &[&one, &two, &end]);
    assert!(search_all(&mut app, &log, "needle").is_empty());
    // Identity is the whole line's hash, never the cut snippet: all
    // three matches are kept, each cut around its own match.
    let flat = app.find.flat();
    assert_eq!(flat.len(), 3);
    assert_ne!(flat[0].anchor, flat[1].anchor);
    for kept in &flat {
        assert_eq!(kept.snippet.line.chars().count(), super::SNIPPET);
        assert_eq!(
            &kept.snippet.line[kept.snippet.at.start..kept.snippet.at.end],
            "needle"
        );
    }
    assert_eq!(flat[0].snippet.at, 390..396);
    assert_eq!(flat[1].snippet.at, 390..396);
    // A match at the line's end cuts the last 400 characters.
    assert_eq!(flat[2].snippet.at, 394..400);
    assert_ne!(flat[0].snippet.line, flat[1].snippet.line);
    assert_eq!(count(&app), "1 of 3");
}

#[test]
fn a_match_at_a_page_edge_has_no_neighbour_across_it() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["needle one", "needle two", "needle three"],
    );
    assert!(search_all(&mut app, &log, "needle").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 3);
    // The prompt bubble is the page's first logical line, so the first
    // match has a neighbour; the turn's end marker follows its last
    // reply, so the last reply's match has one after it.
    assert_eq!(flat[0].snippet.before, "go");
    assert_eq!(flat[0].snippet.after, "needle two");
    assert_eq!(flat[2].snippet.before, "needle two");
    assert_eq!(flat[2].snippet.after, "▣ completed");
    // The bubble is the page's first line and the end marker its last:
    // a match on either has no line past the edge.
    assert!(search_all(&mut app, &log, "go").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].snippet.before, "");
    assert_eq!(flat[0].snippet.after, "needle one");
    assert!(search_all(&mut app, &log, "completed").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].snippet.before, "needle three");
    assert_eq!(flat[0].snippet.after, "");
}
