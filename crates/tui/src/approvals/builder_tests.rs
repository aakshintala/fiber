//! Tests for the panel builder: marks follow the segments' byte ranges,
//! right-aligned rows keep at least one space, and spots sit on their
//! segment's cells.

use ratatui::style::{Modifier, Style};

use super::Builder;
use crate::approvals::Title;
use crate::approvals::form::Spot;
use crate::format::width;
use crate::theme::{Role, style};

/// "Question" bold in secondary.
fn head() -> Style {
    style(Role::Secondary).add_modifier(Modifier::BOLD)
}

/// Body text in secondary.
fn body() -> Style {
    style(Role::Secondary)
}

/// Dim text.
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// The placeholder's first character with the cursor on it: dim and reversed.
fn first() -> Style {
    Style::default()
        .add_modifier(Modifier::DIM)
        .add_modifier(Modifier::REVERSED)
}

/// Marks are sorted, never overlap, lie on char boundaries and inside the
/// line.
fn assert_valid(lines: &[String], marks: &[crate::approvals::Mark]) {
    let mut at: Option<(usize, usize)> = None;
    for mark in marks {
        assert!(mark.line < lines.len(), "{mark:?}");
        let line = &lines[mark.line];
        assert!(mark.bytes.start <= mark.bytes.end, "{mark:?}");
        assert!(mark.bytes.end <= line.len(), "{mark:?}");
        assert!(line.is_char_boundary(mark.bytes.start), "{mark:?}");
        assert!(line.is_char_boundary(mark.bytes.end), "{mark:?}");
        if let Some((line_at, end_at)) = at {
            assert!(
                mark.line > line_at || (mark.line == line_at && mark.bytes.start >= end_at),
                "{mark:?} after {line_at}:{end_at}"
            );
        }
        at = Some((mark.line, mark.bytes.end));
    }
}

#[test]
fn a_row_marks_each_styled_segment_at_its_byte_range() {
    let mut built = Builder::new();
    // "▸ " is 4 bytes, "1 " 2, "[x] " 4, "hub" 3.
    let line = built.row(&[
        ("▸ ", body()),
        ("1 ", dim()),
        ("[x] ", body()),
        ("hub", head()),
    ]);
    assert_eq!(line, 0);
    let panel = built.finish();
    assert_eq!(panel.lines, ["▸ 1 [x] hub"]);
    assert_eq!(
        panel
            .marks
            .iter()
            .map(|mark| (mark.line, mark.bytes.clone(), mark.style))
            .collect::<Vec<_>>(),
        [
            (0, 0..4, body()),
            (0, 4..6, dim()),
            (0, 6..10, body()),
            (0, 10..13, head()),
        ]
    );
    assert_valid(&panel.lines, &panel.marks);
}

#[test]
fn a_row_with_multi_byte_glyphs_marks_every_state() {
    // Every tab state and mark in one sweep: answered, current, arrows.
    let mut built = Builder::new();
    let line = built.row(&[
        ("← ", dim()),
        ("✔ Scope", style(Role::Accent)),
        ("  ", Style::default()),
        ("☐ Tests", body()),
        ("  →", dim()),
    ]);
    assert_eq!(line, 0);
    let panel = built.finish();
    assert_eq!(panel.lines, ["← ✔ Scope  ☐ Tests  →"]);
    assert_valid(&panel.lines, &panel.marks);
    // "← " is 4 bytes, "✔ Scope" 9, "  " 2, "☐ Tests" 9, "  →" 5.
    let ranges: Vec<_> = panel.marks.iter().map(|mark| mark.bytes.clone()).collect();
    assert_eq!(ranges, [0..4, 4..13, 15..24, 24..29]);
}

#[test]
fn the_placeholder_with_the_cursor_on_its_row_is_three_adjacent_marks() {
    let mut built = Builder::new();
    // "  3 ✎ " is 8 bytes, "T" 1, the rest 41.
    let line = built.row(&[
        ("  3 ✎ ", dim()),
        ("T", first()),
        ("ype an answer, alone or with the options", dim()),
    ]);
    built.here(line);
    built.caret(line, 8);
    let panel = built.finish();
    assert_eq!(
        panel.lines,
        ["  3 ✎ Type an answer, alone or with the options"]
    );
    assert_eq!(panel.cursor, Some(0));
    assert_eq!(panel.caret, Some((0, 8)));
    let marks: Vec<_> = panel
        .marks
        .iter()
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..8, dim()), (8..9, first()), (9..49, dim())]);
    assert_valid(&panel.lines, &panel.marks);
}

#[test]
fn an_empty_or_plain_segment_leaves_no_mark() {
    let mut built = Builder::new();
    built.row(&[
        ("", body()),
        ("x", Style::default()),
        ("", Style::default()),
    ]);
    built.row(&[("y", Style::default())]);
    let panel = built.finish();
    assert_eq!(panel.lines, ["x", "y"]);
    assert!(panel.marks.is_empty(), "{:?}", panel.marks);
}

#[test]
fn control_characters_in_segments_draw_as_spaces() {
    let mut built = Builder::new();
    built.row(&[("a\x1b[31mb\nc", body())]);
    let panel = built.finish();
    assert_eq!(panel.lines, ["a [31mb c"]);
    assert!(
        panel
            .lines
            .iter()
            .all(|line| !line.chars().any(|ch| ch.is_control())),
        "{:?}",
        panel.lines
    );
    assert_eq!(panel.marks.len(), 1);
    assert_valid(&panel.lines, &panel.marks);
}

#[test]
fn rows_across_states_keep_valid_marks() {
    let prefixes = ["▸ ", "  "];
    let ticks = ["( )", "(•)", "[ ]", "[x]"];
    let labels = ["hub", "tui ✔", "名前"];
    let descriptions = ["", "the session host", "← side"];
    let mut built = Builder::new();
    for prefix in prefixes {
        for tick in ticks {
            for label in labels {
                for description in descriptions {
                    let mut parts = vec![
                        (prefix, body()),
                        ("1 ", dim()),
                        (tick, body()),
                        (" ", Style::default()),
                        (label, head()),
                    ];
                    if !description.is_empty() {
                        parts.push(("  ", Style::default()));
                        parts.push((description, dim()));
                    }
                    built.row(&parts);
                }
            }
        }
    }
    built.blank();
    let panel = built.finish();
    assert_eq!(
        panel.lines.len(),
        prefixes.len() * ticks.len() * labels.len() * descriptions.len() + 1
    );
    assert_valid(&panel.lines, &panel.marks);
    assert!(panel.lines.last().is_some_and(String::is_empty));
}

#[test]
fn a_right_aligned_row_keeps_one_space_at_and_below_the_fit() {
    // Left "ab" and right "cd": 4 cells together.
    for (w, want) in [(3u16, "ab cd"), (4, "ab cd"), (5, "ab cd"), (6, "ab  cd")] {
        let mut built = Builder::new();
        let line = built.right_aligned(&[("ab", body())], &[("cd", dim())], w);
        assert_eq!(line, 0);
        let panel = built.finish();
        assert_eq!(panel.lines, [want], "width {w}");
        // The right part's mark ends at the line's end.
        let right = panel
            .marks
            .iter()
            .find(|mark| mark.style == dim())
            .unwrap_or_else(|| panic!("a dim mark at width {w}: {:?}", panel.marks));
        assert_eq!(right.bytes.end, panel.lines[0].len(), "width {w}");
        assert_valid(&panel.lines, &panel.marks);
    }
}

#[test]
fn a_right_aligned_row_marks_both_parts() {
    let mut built = Builder::new();
    built.right_aligned(&[("ab", body())], &[("cd", dim())], 10);
    let panel = built.finish();
    assert_eq!(panel.lines, ["ab      cd"]);
    let marks: Vec<_> = panel
        .marks
        .iter()
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(marks, [(0..2, body()), (8..10, dim())]);
}

#[test]
fn a_column_spot_sits_on_its_segments_cells() {
    let mut built = Builder::new();
    // "名 " is 3 cells, "tab" 3.
    let line = built.row(&[("名 ", dim()), ("tab", body())]);
    let from = u16::try_from(width("名 ")).unwrap_or(u16::MAX);
    let to = from.saturating_add(u16::try_from(width("tab")).unwrap_or(u16::MAX));
    built.spot_cols(line, (from, to), Spot::Tab(1));
    let panel = built.finish();
    assert_eq!(panel.spots.len(), 1);
    assert_eq!(panel.spots[0].line, 0);
    assert_eq!(panel.spots[0].cols, Some((3, 6)));
    assert_eq!(panel.spots[0].spot, Spot::Tab(1));
}

#[test]
fn a_whole_line_spot_covers_the_row() {
    let mut built = Builder::new();
    let line = built.row(&[("hi", body())]);
    built.spot(line, Spot::Words);
    let panel = built.finish();
    assert_eq!(panel.spots.len(), 1);
    assert_eq!(panel.spots[0].cols, None);
}

#[test]
fn the_title_draws_the_session_count_place_and_hint() {
    let mut built = Builder::new();
    let line = built.title(
        &Title {
            session: "main".to_owned(),
            place: Some((1, 2)),
            questions: Some(3),
        },
        100,
    );
    assert_eq!(line, 0);
    let panel = built.finish();
    let left = "Question from main · 3 questions, any can be skipped · 1 of 2";
    let right = "Esc: chat about this";
    let pad = 100usize
        .saturating_sub(width(left).saturating_add(width(right)))
        .max(1);
    assert_eq!(panel.lines, [format!("{left}{}{right}", " ".repeat(pad))]);
    let marks: Vec<_> = panel
        .marks
        .iter()
        .map(|mark| (mark.bytes.clone(), mark.style))
        .collect();
    assert_eq!(
        marks,
        [
            (0..8, head()),
            (8..18, body()),
            (18..53, body()),
            (53..63, head()),
            (63 + pad..63 + pad + right.len(), dim()),
        ]
    );
    assert_valid(&panel.lines, &panel.marks);
}

#[test]
fn the_title_without_a_place_or_count_reads_as_one_question() {
    let mut built = Builder::new();
    built.title(
        &Title {
            session: "s_1".to_owned(),
            place: None,
            questions: None,
        },
        100,
    );
    let panel = built.finish();
    assert!(
        panel.lines[0].starts_with("Question from s_1"),
        "{}",
        panel.lines[0]
    );
    assert!(!panel.lines[0].contains("questions"), "{}", panel.lines[0]);
    assert!(!panel.lines[0].contains(" of "), "{}", panel.lines[0]);
    assert!(
        panel.lines[0].ends_with("Esc: chat about this"),
        "{}",
        panel.lines[0]
    );
}

#[test]
fn the_title_without_more_waiting_has_no_place_suffix() {
    let mut built = Builder::new();
    built.title(
        &Title {
            session: "main".to_owned(),
            place: None,
            questions: Some(1),
        },
        100,
    );
    let panel = built.finish();
    assert!(panel.lines[0].contains("1 questions"), "{}", panel.lines[0]);
    assert!(!panel.lines[0].contains(" of "), "{}", panel.lines[0]);
}
