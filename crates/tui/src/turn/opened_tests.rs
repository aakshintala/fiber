//! Tests for the shared diff renderer.

use contract::events::CallStatus;
use ratatui::style::Modifier;
use serde_json::{Value, json};

use super::rows;
use crate::rows::{RowText, Rows};
use crate::theme::Role;
use crate::turn::Row;
use crate::turn::group::{Call, CallResult};

/// The call's rows at `width`: text and target per row.
fn drawn(call: &Call, width: u16) -> (Vec<Row>, Vec<RowText>) {
    let mut out = Rows::default();
    rows(call, width, &mut out);
    out.into_parts()
}

/// A call with `details`, `headline` and `content`.
fn made_call(details: Option<Value>, headline: &str, content: &str) -> Call {
    Call {
        result: Some(Box::new(CallResult {
            details: details.map(|details| details.to_string()),
            headline: headline.to_owned(),
            content: content.to_owned(),
            ..CallResult::default()
        })),
        ..Call::default()
    }
}

/// Every row's text.
fn lines(parts: &(Vec<Row>, Vec<RowText>)) -> Vec<String> {
    parts.0.iter().map(|(line, _)| line.to_string()).collect()
}

/// Whether every row starts with the eight-column indent and fits
/// `width` cells.
fn indented_and_narrow(parts: &(Vec<Row>, Vec<RowText>), width: u16) {
    for (line, _) in &parts.0 {
        let text = line.to_string();
        assert!(text.starts_with("        "), "{text:?} indented eight");
        assert!(
            crate::format::width(&text) <= usize::from(width),
            "{text:?} fits {width}"
        );
    }
}

#[test]
fn an_edit_is_chosen_by_a_diff_string() {
    let call = made_call(
        Some(json!({"diff": "@@ -1 +1 @@\n-call foo(a);\n+call bar(a);\n"})),
        "",
        "result text",
    );
    let parts = drawn(&call, 76);
    indented_and_narrow(&parts, 76);
    let texts = lines(&parts);
    assert_eq!(texts[1], "        1 -call foo(a);");
    assert_eq!(texts[2], "        1 +call bar(a);");
    let removed = &parts.0[1].0.spans;
    assert_eq!(removed[2].style.fg, Some(Role::Removed.color()));
    let marked = removed
        .iter()
        .find(|span| span.content == "foo(a);")
        .expect("the differing word");
    assert!(marked.style.add_modifier.contains(Modifier::REVERSED));
    assert_eq!(marked.style.fg, Some(Role::Removed.color()));
}

#[test]
fn a_tool_named_anything_with_diff_is_an_edit() {
    let call = Call {
        name: "mytool".to_owned(),
        ..made_call(
            Some(json!({"diff": "@@ -1 +1 @@\n-a\n+b\n"})),
            "",
            "result text",
        )
    };
    let parts = drawn(&call, 76);
    let texts = lines(&parts);
    assert_eq!(texts[1], "        1 -a");
    assert_eq!(parts.0[1].0.spans[2].style.fg, Some(Role::Removed.color()));
}

#[test]
fn a_read_is_chosen_by_start_line_not_the_name() {
    let call = Call {
        name: "shell".to_owned(),
        ..made_call(Some(json!({"start_line": 5})), "", "a\nb")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        5 a", "        6 b"]);
}

#[test]
fn a_call_with_no_details_and_a_content_argument_shows_it_numbered_from_one() {
    let call = Call {
        name: "mytool".to_owned(),
        arguments: json!({"content": "x\ny"}),
        ..made_call(None, "", "ignored")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        1 x", "        2 y"]);
}

#[test]
fn a_write_whose_details_hold_only_file_seen_shows_its_content_argument() {
    let call = Call {
        name: "write".to_owned(),
        arguments: json!({"content": "w1\nw2"}),
        ..made_call(
            Some(json!({"file_seen": {"path": "/w/out.txt", "hash": 7}})),
            "",
            "Created /w/out.txt",
        )
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        1 w1", "        2 w2"]);
}

#[test]
fn a_call_named_write_with_unrecognised_details_shows_content() {
    let call = Call {
        name: "write".to_owned(),
        arguments: json!({"content": "argument"}),
        ..made_call(Some(json!({"bogus": 1})), "", "actual")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        actual"]);
}

#[test]
fn matches_beats_command() {
    let call = made_call(
        Some(json!({
            "command": "grep -n x a",
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 2, "text": "x", "spans": [[0, 1]]},
            ]}],
        })),
        "",
        "a:x\nExit code 0.",
    );
    let parts = drawn(&call, 76);
    let texts = lines(&parts);
    assert!(!texts.iter().any(|row| row.contains("$ ")), "{texts:?}");
    assert_eq!(texts[0], "        a");
    assert_eq!(texts[1], "        2 x");
    assert_eq!(texts[2], "        exit 0");
}

#[test]
fn unrecognised_details_do_not_draw_the_content_argument() {
    let call = Call {
        arguments: json!({"content": "argument"}),
        ..made_call(Some(json!([])), "", "actual")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        actual"]);
}

#[test]
fn command_without_exit_or_signal_is_plain() {
    let call = made_call(Some(json!({"command": "ls"})), "", "a\nb");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        a", "        b"]);
}

#[test]
fn command_with_a_string_exit_code_is_plain() {
    let call = made_call(Some(json!({"command": "ls", "exit_code": "0"})), "", "a");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        a"]);
}

#[test]
fn null_details_counts_as_none() {
    let call = Call {
        arguments: json!({"other": 1}),
        ..made_call(None, "", "hi")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        hi"]);
}

#[test]
fn a_diff_that_is_not_a_string_shows_content() {
    let call = made_call(Some(json!({"diff": 7})), "", "result");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        result"]);
}

#[test]
fn a_malformed_diff_does_not_fall_through_to_a_later_key() {
    let call = made_call(Some(json!({"diff": 7, "start_line": 3})), "", "result");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        result"]);
}

#[test]
fn a_start_line_that_is_not_a_number_shows_content() {
    let call = made_call(Some(json!({"start_line": "5"})), "", "result");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        result"]);
}

#[test]
fn matches_that_is_not_an_array_shows_content() {
    let call = made_call(Some(json!({"matches": {}})), "", "result");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        result"]);
}

#[test]
fn a_read_at_an_offset_numbers_from_the_offset() {
    let call = made_call(Some(json!({"start_line": 41})), "", "a\nb\nc");
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        41 a", "        42 b", "        43 c"]
    );
}

#[test]
fn gutter_width_is_the_largest_drawn_number() {
    let call = made_call(Some(json!({"start_line": 98})), "", "a\nb\nc\nd");
    let parts = drawn(&call, 76);
    let texts = lines(&parts);
    assert_eq!(texts[0], "         98 a");
    assert_eq!(texts[3], "        101 d");
    // The gutter span is right-aligned in three columns and one space.
    assert_eq!(parts.0[0].0.spans[1].content.as_ref(), " 98 ");
    assert!(
        parts.0[0].0.spans[1]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
}

#[test]
fn start_line_u64_max_does_not_overflow() {
    let call = made_call(Some(json!({"start_line": u64::MAX})), "", "a\nb");
    let parts = drawn(&call, 100);
    let texts = lines(&parts);
    assert_eq!(texts.len(), 2);
    assert!(texts[0].ends_with("a") && texts[1].ends_with("b"));
    assert_eq!(texts[0], texts[1].replace('b', "a"));
    indented_and_narrow(&parts, 100);
}

#[test]
fn a_write_numbers_from_one() {
    let call = Call {
        arguments: json!({"content": "l1\nl2\nl3"}),
        ..made_call(None, "", "created")
    };
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        1 l1", "        2 l2", "        3 l3"]
    );
}

#[test]
fn search_groups_keep_printed_order_with_bold_paths() {
    let call = made_call(
        Some(json!({
            "command": "grep -n x b a",
            "exit_code": 0,
            "matches": [
                {"path": "b", "lines": [{"line": 1, "text": "x", "spans": []}]},
                {"path": "a", "lines": [{"line": 9, "text": "y", "spans": []}]},
            ],
        })),
        "",
        "noise",
    );
    let parts = drawn(&call, 76);
    let texts = lines(&parts);
    assert_eq!(texts[0], "        b");
    assert_eq!(texts[2], "        a");
    for path in [0, 2] {
        assert!(
            parts.0[path].0.spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "{:?} bold",
            parts.0[path].0
        );
    }
}

#[test]
fn a_context_line_has_no_match_tint() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 1, "text": "before", "spans": []},
                {"line": 2, "text": "hit x", "spans": [[4, 5]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts)[1], "        1 before");
    assert!(
        parts.0[1]
            .0
            .spans
            .iter()
            .all(|span| span.style.bg.is_none()),
        "{:?}",
        parts.0[1].0
    );
    let tinted: Vec<_> = parts.0[2]
        .0
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(Role::Match.color()))
        .collect();
    assert_eq!(tinted.len(), 1);
    assert_eq!(tinted[0].content.as_ref(), "x");
}

#[test]
fn an_empty_matches_array_keeps_the_closing_line() {
    let call = made_call(
        Some(json!({"command": "grep -n x a", "exit_code": 0, "matches": []})),
        "",
        "Exit code 0.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        exit 0"]);
}

#[test]
fn spans_widen_on_the_match_tint() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 1, "text": "aéx", "spans": [[1, 3]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    let tinted: Vec<_> = parts.0[1]
        .0
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(Role::Match.color()))
        .collect();
    assert_eq!(tinted.len(), 1);
    assert_eq!(tinted[0].content.as_ref(), "é");
}

#[test]
fn overlapping_spans_merge() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 1, "text": "abcdefg", "spans": [[0, 4], [2, 6]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    let tinted: Vec<String> = parts.0[1]
        .0
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(Role::Match.color()))
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(tinted, vec!["abcdef".to_owned()]);
}

#[test]
fn reversed_out_of_range_and_off_boundary_spans_are_dropped() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 1, "text": "aé", "spans": [[2, 1], [0, 99], [2, 3], [0, 1]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    let tinted: Vec<String> = parts.0[1]
        .0
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(Role::Match.color()))
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(tinted, vec!["a".to_owned()]);
}

#[test]
fn search_draws_under_all_open_when_the_call_is_closed() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 1, "text": "x", "spans": [[0, 1]]},
            ]}],
        })),
        "",
        "",
    );
    // A search draw opens every collapsible body, open or not.
    let mut out = Rows::all_open();
    rows(&call, 76, &mut out);
    let (drawn, _) = out.into_parts();
    let texts: Vec<String> = drawn.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(texts, vec!["        a", "        1 x", "        exit 0"]);
}

#[test]
fn a_shell_call_shows_command_output_and_exit_zero_dim() {
    let call = made_call(
        Some(json!({"command": "cargo t", "exit_code": 0})),
        "",
        "ok\nExit code 0.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ cargo t", "        ok", "        exit 0"]
    );
    assert!(
        parts.0[2].0.style.add_modifier.contains(Modifier::DIM),
        "{:?}",
        parts.0[2].0
    );
}

#[test]
fn exit_one_is_in_removed() {
    let call = made_call(
        Some(json!({"command": "cargo t", "exit_code": 1})),
        "",
        "boom\nExit code 1.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ cargo t", "        boom", "        exit 1"]
    );
    assert_eq!(parts.0[2].0.spans[1].style.fg, Some(Role::Removed.color()));
}

#[test]
fn a_killed_shell_reads_killed_by_the_signal() {
    let call = made_call(
        Some(json!({"command": "sleep 9", "signal": "SIGKILL"})),
        "",
        "Killed by SIGKILL.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ sleep 9", "        killed by SIGKILL"]
    );
    assert_eq!(parts.0[1].0.spans[1].style.fg, Some(Role::Removed.color()));
}

#[test]
fn the_tools_exit_line_is_not_drawn_twice() {
    let call = made_call(
        Some(json!({"command": "cmd", "exit_code": 1})),
        "",
        "out\nExit code 1.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ cmd", "        out", "        exit 1"]
    );
}

#[test]
fn a_killed_shell_does_not_repeat_the_tools_killed_by_line() {
    let call = made_call(
        Some(json!({"command": "cmd", "signal": "SIGTERM"})),
        "",
        "out\nKilled by SIGTERM.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ cmd", "        out", "        killed by SIGTERM"]
    );
}

#[test]
fn a_closing_line_is_drawn_when_a_hook_rewrote_the_content() {
    let call = made_call(
        Some(json!({"command": "cmd", "exit_code": 0})),
        "",
        "hooked",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ cmd", "        hooked", "        exit 0"]
    );
}

#[test]
fn an_earlier_notice_line_stays_as_output() {
    let call = made_call(
        Some(json!({"command": "cmd", "exit_code": 0})),
        "",
        "Exit code 1.\nout",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        $ cmd",
            "        Exit code 1.",
            "        out",
            "        exit 0"
        ]
    );
}

#[test]
fn a_held_open_shell_keeps_the_notice_and_draws_one_closing_line() {
    let call = made_call(
        Some(json!({"command": "cmd", "exit_code": 0})),
        "",
        "out\nExit code 0.\nOutput was still held open.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        $ cmd",
            "        out",
            "        Output was still held open.",
            "        exit 0"
        ]
    );
}

#[test]
fn a_multi_line_command_prefixes_only_the_first_row() {
    let call = made_call(
        Some(json!({"command": "a\nb", "exit_code": 0})),
        "",
        "Exit code 0.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        $ a", "        b", "        exit 0"]
    );
}

#[test]
fn a_long_command_wraps_with_the_indent_on_every_row() {
    let command = "x".repeat(200);
    let call = made_call(
        Some(json!({"command": command, "exit_code": 0})),
        "",
        "Exit code 0.",
    );
    let parts = drawn(&call, 76);
    indented_and_narrow(&parts, 76);
    assert!(parts.0.len() > 3);
    // The shared wrap rule breaks after the space in "$ ", dropping it.
    let texts = lines(&parts);
    assert_eq!(texts[0], "        $");
    assert!(texts[1].starts_with("        xxx"));
}

#[test]
fn a_long_read_line_wraps_with_the_indent() {
    let call = made_call(
        Some(json!({"start_line": 1})),
        "",
        "word ".repeat(40).trim_end(),
    );
    let parts = drawn(&call, 76);
    indented_and_narrow(&parts, 76);
    assert!(parts.0.len() > 1);
}

#[test]
fn a_narrow_width_does_not_panic() {
    let call = made_call(Some(json!({"start_line": 1})), "", "a\nb");
    for width in [8u16, 0] {
        let parts = drawn(&call, width);
        for (line, _) in &parts.0 {
            assert!(
                crate::format::width(&line.to_string()) <= usize::from(width),
                "{line:?} fits {width}"
            );
        }
    }
}

#[test]
fn a_wrapped_row_survives_the_cards_pad() {
    let call = made_call(
        Some(json!({"start_line": 1})),
        "",
        "word ".repeat(40).trim_end(),
    );
    let mut out = Rows::default();
    rows(&call, 74, &mut out);
    // The card pads the ledger past its one-column padding.
    out.pad(0, 1, 74);
    let (drawn, _) = out.into_parts();
    assert!(drawn.len() > 1);
    for (line, _) in &drawn {
        // One card column, then the opened call's eight.
        assert!(line.to_string().starts_with("         "), "{line:?}");
    }
}

#[test]
fn a_wrapped_opened_line_copies_unwrapped() {
    let text = "word ".repeat(40);
    let text = text.trim_end();
    let call = made_call(Some(json!({"start_line": 1})), "", text);
    let (drawn, texts) = drawn(&call, 76);
    assert!(drawn.len() > 1);
    let logical = crate::logical::logical(&drawn, &texts);
    assert_eq!(logical.len(), 1);
    assert_eq!(logical[0].text, text);
}

#[test]
fn a_wrapped_opened_line_is_found_by_search_across_its_rows() {
    let text = "alpha beta gamma delta epsilon zeta eta theta";
    let call = made_call(Some(json!({"start_line": 1})), "", text);
    let (drawn, texts) = drawn(&call, 30);
    assert!(drawn.len() > 1);
    let logical = crate::logical::logical(&drawn, &texts);
    assert_eq!(logical.len(), 1);
    let hits = crate::logical::matches(&logical[0].text, "gamma delta");
    assert_eq!(hits.len(), 1);
    // The match crosses the rows the line wrapped onto.
    let Some(hit) = hits.first() else {
        return;
    };
    let rows: Vec<usize> = hit
        .clone()
        .filter_map(|at| {
            logical[0]
                .from
                .get(at)
                .copied()
                .flatten()
                .map(|(row, _)| row)
        })
        .collect();
    assert!(rows.iter().min() != rows.iter().max());
}

#[test]
fn a_failed_call_shows_its_headline_then_its_view() {
    let call = Call {
        status: Some(CallStatus::Failed),
        ..made_call(
            Some(json!({"command": "cmd", "exit_code": 1})),
            "nonzero_exit: exit 1",
            "out\nExit code 1.",
        )
    };
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        nonzero_exit: exit 1",
            "        $ cmd",
            "        out",
            "        exit 1"
        ]
    );
    assert!(
        parts.0[0].0.style.add_modifier.contains(Modifier::DIM),
        "{:?}",
        parts.0[0].0
    );
}

#[test]
fn a_denied_call_shows_its_reason_first() {
    let call = Call {
        status: Some(CallStatus::Denied),
        ..made_call(None, "not allowed", "")
    };
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        not allowed"]);
}

#[test]
fn json_content_is_indented_two_columns() {
    let call = made_call(None, "", r#"{"b":[1,2],"a":{"c":true}}"#);
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        {",
            "          \"a\": {",
            "            \"c\": true",
            "          },",
            "          \"b\": [",
            "            1,",
            "            2",
            "          ]",
            "        }"
        ]
    );
}

#[test]
fn bare_json_string_and_invalid_json_draw_verbatim() {
    let call = made_call(None, "", r#""hi""#);
    assert_eq!(lines(&drawn(&call, 76)), vec![r#"        "hi""#]);
    let call = made_call(None, "", "{oops");
    assert_eq!(lines(&drawn(&call, 76)), vec!["        {oops"]);
}

#[test]
fn empty_content_reads_no_output() {
    let call = made_call(None, "", "");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        no output"]);
    assert!(
        parts.0[0].0.style.add_modifier.contains(Modifier::DIM),
        "{:?}",
        parts.0[0].0
    );
}

#[test]
fn nineteen_twenty_and_twenty_one_lines_cap_at_twenty() {
    for (count, shown, more) in [
        (19usize, 19usize, None),
        (20, 20, None),
        (21, 20, Some("… 1 more line (Enter to view)")),
    ] {
        let text = (1..=count)
            .map(|n| format!("L{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let call = made_call(Some(json!({"start_line": 1})), "", &text);
        let parts = drawn(&call, 76);
        let texts = lines(&parts);
        assert_eq!(texts.len(), shown + usize::from(more.is_some()), "{count}");
        match more {
            None => assert!(!texts.iter().any(|row| row.contains('…')), "{count}"),
            Some(last) => assert_eq!(texts[shown], format!("        {last}")),
        }
    }
}

#[test]
fn the_cap_counts_the_headline_and_the_closing_line() {
    let output = (1..=18)
        .map(|n| format!("o{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = Call {
        status: Some(CallStatus::Failed),
        ..made_call(
            Some(json!({"command": "cmd", "exit_code": 1})),
            "nonzero_exit: exit 1",
            &format!("{output}\nExit code 1."),
        )
    };
    // Headline, command and 18 output rows draw; the rest is one more row.
    let parts = drawn(&call, 76);
    let texts = lines(&parts);
    assert_eq!(texts.len(), 21);
    assert_eq!(texts[0], "        nonzero_exit: exit 1");
    assert_eq!(texts[20], "        … 1 more line (Enter to view)");
    assert!(
        !texts
            .iter()
            .any(|row| row.contains("exit 1") && !row.contains("nonzero"))
    );
}

#[test]
fn the_gutter_and_more_row_are_dim_and_body_text_is_not() {
    let text = (1..=21)
        .map(|n| format!("L{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = made_call(Some(json!({"start_line": 1})), "", &text);
    let parts = drawn(&call, 76);
    assert_eq!(parts.0.len(), 21);
    for (line, _) in parts.0.iter().take(20) {
        assert!(
            line.spans[1].style.add_modifier.contains(Modifier::DIM),
            "{line:?} gutter dim"
        );
        assert!(
            line.spans
                .iter()
                .skip(2)
                .all(|span| { !span.style.add_modifier.contains(Modifier::DIM) }),
            "{line:?} body not dim"
        );
    }
    assert!(
        parts.0[20].0.style.add_modifier.contains(Modifier::DIM),
        "{:?} more row dim",
        parts.0[20].0
    );
}

#[test]
fn controls_are_stripped_and_tabs_expand_in_inline_text() {
    let call = made_call(None, "", "a\u{1b}b\tc\u{0}d");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        ab    cd"]);
}

#[test]
fn the_more_row_targets_the_call_view() {
    let text = (1..=21)
        .map(|n| format!("L{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = Call {
        id: 42,
        ..made_call(Some(json!({"start_line": 1})), "", &text)
    };
    let parts = drawn(&call, 76);
    assert_eq!(parts.0.len(), 21);
    assert_eq!(
        parts.0[20].1,
        Some(crate::app::Target::CallView(42)),
        "the more row"
    );
    assert!(
        parts.0[..20].iter().all(|(_, target)| target.is_none()),
        "no other row"
    );
    // Narrow enough to wrap the more row, its target stays on the
    // first piece alone.
    let parts = drawn(&call, 30);
    let aimed: Vec<usize> = parts
        .0
        .iter()
        .enumerate()
        .filter_map(|(at, (_, target))| {
            matches!(target, Some(crate::app::Target::CallView(42))).then_some(at)
        })
        .collect();
    assert_eq!(aimed.len(), 1);
    assert!(lines(&parts)[aimed[0]].contains("…"));
}

#[test]
fn logical_lines_skip_gutter_and_headline() {
    let call = made_call(Some(json!({"start_line": 1})), "boom", "a");
    let (drawn, texts) = drawn(&call, 76);
    let logical = crate::logical::logical(&drawn, &texts);
    assert_eq!(logical.len(), 2);
    assert_eq!(logical[0].text, "boom");
    assert_eq!(logical[1].text, "a");
}

#[test]
fn logical_lines_keep_unguttered_text() {
    let call = made_call(Some(json!({"diff": "-old\n+new\n"})), "", "x");
    let (drawn, texts) = drawn(&call, 76);
    let logical = crate::logical::logical(&drawn, &texts);
    assert_eq!(logical.len(), 2);
    assert_eq!(logical[0].text, "-old");
    assert_eq!(logical[1].text, "+new");
}

#[test]
fn match_row_draws_whole() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 7, "text": "ab cd", "spans": [[3, 4]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts)[1], "        7 ab cd");
}

#[test]
fn empty_spans_drop() {
    let call = made_call(
        Some(json!({
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 5, "text": "abcd", "spans": [[0, 0], [2, 3]]},
            ]}],
        })),
        "",
        "",
    );
    let parts = drawn(&call, 76);
    let tinted: Vec<String> = parts.0[1]
        .0
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(Role::Match.color()))
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(tinted, vec!["c".to_owned()]);
}

#[test]
fn held_open_without_closing_keeps_both_rows() {
    let call = made_call(
        Some(json!({"command": "cmd", "exit_code": 0})),
        "",
        "out\nOutput was still held open.",
    );
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        $ cmd",
            "        out",
            "        Output was still held open.",
            "        exit 0"
        ]
    );
}

/// A failed call without details still shows its headline, then its
/// content: the headline keeps it on the full view.
#[test]
fn a_failed_call_without_details_shows_headline_then_content() {
    let call = Call {
        status: Some(CallStatus::Failed),
        ..made_call(None, "nonzero_exit: exit 1", "out")
    };
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec!["        nonzero_exit: exit 1", "        out"]
    );
}

/// A closed call with no details and no content reads dim no output.
#[test]
fn a_closed_call_without_details_and_content_reads_no_output() {
    let call = made_call(None, "", "");
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        no output"]);
    assert!(parts.0[0].0.style.add_modifier.contains(Modifier::DIM));
}

/// Kept ranges keep the call on the full view: its long line wraps.
#[test]
fn a_kept_call_without_details_builds_the_plain_view() {
    let mut call = made_call(None, "", &"y".repeat(200));
    call.result
        .as_deref_mut()
        .expect("made_call sets a result")
        .kept = Some(vec![[0, 2]]);
    let parts = drawn(&call, 76);
    assert!(parts.0.len() > 1);
    assert!(lines(&parts)[0].starts_with("        y"));
}

#[test]
fn json_with_leading_whitespace_indents() {
    let call = made_call(None, "", "  {\"a\": [1]}");
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        {",
            "          \"a\": [",
            "            1",
            "          ]",
            "        }"
        ]
    );
}

/// Details that fail to parse fall back to the plain-content view,
/// never to the `content` argument.
#[test]
fn details_that_fail_to_parse_show_plain_content() {
    let mut call = made_call(None, "", "actual");
    call.arguments = json!({"content": "argument"});
    call.result
        .as_deref_mut()
        .expect("made_call sets a result")
        .details = Some("{oops".to_owned());
    let parts = drawn(&call, 76);
    assert_eq!(lines(&parts), vec!["        actual"]);
}

/// The swapped view copies a search's raw result text, not the
/// rendered match rows: the path header the draw adds never reaches
/// the clipboard.
#[test]
fn a_search_snapshot_copies_raw_content_not_rendered_matches() {
    let content = "a:x\nExit code 0.";
    let call = made_call(
        Some(json!({
            "command": "grep -n x a",
            "exit_code": 0,
            "matches": [{"path": "a", "lines": [
                {"line": 2, "text": "x", "spans": [[0, 1]]},
            ]}],
        })),
        "",
        content,
    );
    let snapshot = super::snapshot(std::iter::once(&call), call.id, 80).expect("the call draws");
    assert_eq!(snapshot.copy, content);
}

#[test]
fn json_array_indents() {
    let call = made_call(None, "", "[1,{\"a\":2}]");
    let parts = drawn(&call, 76);
    assert_eq!(
        lines(&parts),
        vec![
            "        [",
            "          1,",
            "          {",
            "            \"a\": 2",
            "          }",
            "        ]"
        ]
    );
}

/// A message call named `name` with `arguments`; `result` is its
/// headline and content, or none while it runs.
fn message_call(name: &str, arguments: Value, result: Option<(&str, &str)>) -> Call {
    Call {
        name: name.to_owned(),
        arguments,
        result: result.map(|(headline, content)| {
            Box::new(CallResult {
                headline: headline.to_owned(),
                content: content.to_owned(),
                ..CallResult::default()
            })
        }),
        ..Call::default()
    }
}

#[test]
fn a_delegate_message_shows_recipient_text_then_result() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": "line one\n\n- a\n- b"}),
        Some(("", "delivered")),
    );
    let parts = drawn(&call, 76);
    indented_and_narrow(&parts, 76);
    let texts = lines(&parts);
    assert_eq!(texts[0], "        to j_1");
    assert_eq!(texts[1].trim(), "line one");
    let list: Vec<&String> = texts[2..]
        .iter()
        .filter(|row| row.contains('a') || row.contains('b'))
        .collect();
    assert_eq!(list.len(), 2, "{texts:?}");
    assert!(
        list[0].trim_start().len() > 1 && list[0].contains("a"),
        "{texts:?}"
    );
    assert_eq!(texts.last().map(String::as_str), Some("        delivered"));
}

#[test]
fn a_session_message_reads_its_text_argument() {
    let call = message_call(
        "session_message",
        json!({"id": "s_1", "text": "hi", "message": "wrong key"}),
        Some(("", "sent")),
    );
    assert_eq!(
        lines(&drawn(&call, 76)),
        vec!["        to s_1", "        hi", "        sent"]
    );
    // `message` is the delegate tool's key, not this one's.
    let call = message_call(
        "session_message",
        json!({"id": "s_1", "message": "wrong key"}),
        Some(("", "sent")),
    );
    assert_eq!(
        lines(&drawn(&call, 76)),
        vec!["        to s_1", "        sent"]
    );
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "text": "wrong key"}),
        Some(("", "sent")),
    );
    assert_eq!(
        lines(&drawn(&call, 76)),
        vec!["        to j_1", "        sent"]
    );
}

#[test]
fn a_running_message_shows_recipient_and_text_and_no_empty_row() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": "stop"}),
        None,
    );
    assert_eq!(
        lines(&drawn(&call, 76)),
        vec!["        to j_1", "        stop"]
    );
}

#[test]
fn a_failed_message_keeps_its_error_first() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": "stop"}),
        Some(("not_found: no such job", "")),
    );
    assert_eq!(
        lines(&drawn(&call, 76)),
        vec![
            "        not_found: no such job",
            "        to j_1",
            "        stop",
        ]
    );
}

#[test]
fn a_message_call_missing_a_part_draws_the_other() {
    for message in [json!(""), json!(null), json!(7)] {
        let call = message_call(
            "delegate_message",
            json!({"id": "j_1", "message": message}),
            Some(("", "ok")),
        );
        assert_eq!(
            lines(&drawn(&call, 76)),
            vec!["        to j_1", "        ok"]
        );
    }
    for id in [json!(null), json!(""), json!(7)] {
        let call = message_call(
            "delegate_message",
            json!({"id": id, "message": "hi"}),
            Some(("", "ok")),
        );
        assert_eq!(lines(&drawn(&call, 76)), vec!["        hi", "        ok"]);
    }
    let call = message_call(
        "delegate_message",
        json!({"id": 1, "message": 2}),
        Some(("", "ok")),
    );
    assert_eq!(lines(&drawn(&call, 76)), vec!["        ok"]);
}

#[test]
fn another_tool_with_the_same_arguments_draws_no_message_rows() {
    let call = message_call(
        "custom",
        json!({"id": "j_1", "message": "hi"}),
        Some(("", "ok")),
    );
    assert_eq!(lines(&drawn(&call, 76)), vec!["        ok"]);
}

#[test]
fn a_long_message_is_cut_at_the_line_cap_and_hides_the_result() {
    let text: Vec<String> = (1..=30).map(|n| format!("row {n}")).collect();
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": text.join("\n\n")}),
        Some(("", "delivered")),
    );
    let texts = lines(&drawn(&call, 76));
    assert_eq!(texts.len(), super::OPENED_LINES + 1, "{texts:?}");
    assert_eq!(texts[0], "        to j_1");
    assert!(texts.iter().all(|row| !row.contains("delivered")));
    // 1 recipient, 59 text rows (30 paragraphs and 29 blanks) and the
    // result row make 61; 20 are shown.
    assert_eq!(
        texts[super::OPENED_LINES],
        "        … 41 more lines (Enter to view)"
    );
}

#[test]
fn a_message_of_exactly_the_cap_draws_no_more_row() {
    // The recipient and 19 text rows fill the 20 lines exactly.
    let text: Vec<String> = (1..=10).map(|n| format!("row {n}")).collect();
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": text.join("\n\n")}),
        None,
    );
    let texts = lines(&drawn(&call, 76));
    assert_eq!(texts.len(), 20, "{texts:?}");
    assert!(
        texts.iter().all(|row| !row.contains("more line")),
        "{texts:?}"
    );
    let text: Vec<String> = (1..=11).map(|n| format!("row {n}")).collect();
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": text.join("\n\n")}),
        None,
    );
    let texts = lines(&drawn(&call, 76));
    assert_eq!(texts.len(), 21, "{texts:?}");
    assert_eq!(texts[20], "        … 2 more lines (Enter to view)");
}

#[test]
fn message_text_renders_at_the_room_of_the_view() {
    // One 81-character word-spaced paragraph: no break before column 81.
    let paragraph = format!("{} {}", "a".repeat(40), "b".repeat(40));
    assert_eq!(paragraph.len(), 81);
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": paragraph}),
        None,
    );
    // Room 92: the paragraph stays one row.
    let wide = lines(&drawn(&call, 100));
    assert_eq!(wide.len(), 2, "{wide:?}");
    assert_eq!(wide[1], format!("        {paragraph}"));
    // Width 40: every row fits.
    let narrow = drawn(&call, 40);
    indented_and_narrow(&narrow, 40);
    assert!(lines(&narrow).len() > 2);
}

#[test]
fn a_message_prefix_holds_recipient_and_text_and_no_result_rows() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": "hello"}),
        Some(("", "delivered")),
    );
    let prefix: Vec<String> = super::prefix(&call, 76)
        .iter()
        .map(|body| body.cells().iter().map(|(text, _)| text.as_str()).collect())
        .collect();
    assert_eq!(prefix, vec!["to j_1", "hello"]);
}

#[test]
fn message_rows_strip_control_characters() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j\u{1b}[31m_1\u{7}", "message": "\u{1b}[31mhello\u{7}"}),
        Some(("", "ok")),
    );
    let inline = lines(&drawn(&call, 76));
    let swapped: Vec<String> = super::prefix(&call, 76)
        .iter()
        .map(|body| body.cells().iter().map(|(text, _)| text.as_str()).collect())
        .collect();
    assert!(!swapped.is_empty());
    for row in inline.iter().chain(swapped.iter()) {
        assert!(!row.contains('\u{1b}') && !row.contains('\u{7}'), "{row:?}");
    }
    assert!(
        inline.iter().any(|row| row.contains("[31mhello")),
        "{inline:?}"
    );
}

#[test]
fn a_message_view_never_panics_on_a_tiny_width() {
    let call = message_call(
        "delegate_message",
        json!({"id": "j_1", "message": "hello world"}),
        None,
    );
    // Width 0 draws nothing; a width past the indent reaches the message
    // rows, so the recipient row shows at the width that fits it.
    assert!(lines(&drawn(&call, 0)).is_empty());
    for width in 1..12 {
        let _ = drawn(&call, width);
    }
    let narrow = drawn(&call, 14);
    indented_and_narrow(&narrow, 14);
    let rows = lines(&narrow);
    assert_eq!(rows[0], "        to j_1", "{rows:?}");
    let text = rows[1..].join(" ");
    assert!(text.contains("hello") && text.contains("world"), "{rows:?}");
}
