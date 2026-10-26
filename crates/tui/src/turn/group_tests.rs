//! Tests for a tool group's drawing.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use contract::events::{CallStatus, FileChange};
use ratatui::style::Modifier;
use serde_json::json;

use super::{Call, CallResult, Group, Section, Streaming, Thought, Timing};
use crate::app::Target;
use crate::ext::cache::Cache;
use crate::ext::{Bound, Failure, Handle, Input, RawSpan, Renderer, Status as ExtStatus};
use crate::theme::Role;

#[test]
fn an_empty_group_has_no_ledger() {
    assert!(!super::Group::default().has_ledger());
}

fn read_call(id: usize, path: &str) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "read".to_owned(),
        arguments: json!({"path": path}),
        status: Some(CallStatus::Completed),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn search_call(id: usize, command: &str) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "shell".to_owned(),
        arguments: json!({"command": command}),
        status: Some(CallStatus::Completed),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn edit_call(id: usize, path: &str, added: u64, removed: u64) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "edit".to_owned(),
        arguments: json!({"path": path}),
        status: Some(CallStatus::Completed),
        changes: vec![FileChange {
            path: path.to_owned(),
            added,
            removed,
        }],
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn running_call(id: usize, name: &str, arguments: serde_json::Value) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: name.to_owned(),
        arguments,
        status: None,
        timing: Timing::Started(0),
        ..Call::default()
    }
}

/// The doc mock's group: 34 reads, 84 searches, 24 edits at +175 −83,
/// three thoughts, 4m 12s.
fn doc_group() -> Group {
    let mut sections = vec![Section {
        step: 3,
        ..Section::default()
    }];
    for n in 0..34 {
        sections[0].calls.push(read_call(n, &format!("f{n:02}.rs")));
    }
    for n in 0..84 {
        sections[0]
            .calls
            .push(search_call(100 + n, &format!("rg pattern{n}")));
    }
    sections[0].calls.push(edit_call(200, "e00.rs", 175, 83));
    for n in 1..24 {
        sections[0].calls.push(Call {
            id: 200 + n,
            action: format!("a_e{n}"),
            name: "edit".to_owned(),
            arguments: json!({"path": format!("e{n:02}.rs")}),
            status: Some(CallStatus::Completed),
            timing: Timing::Started(0),
            ..Call::default()
        });
    }
    for n in 0..3 {
        sections[0].thoughts.push(Thought {
            id: n,
            action: format!("t{n}"),
            text: "# Heading".to_owned(),
            started: 0,
            ended: Some(1_000),
            ..Thought::default()
        });
    }
    Group {
        key: Some("a_0".to_owned()),
        first: 0,
        last: 252_000,
        sections,
        ..Group::default()
    }
}

fn summary_row(
    group: &Group,
    running: bool,
    width: u16,
) -> (Vec<crate::turn::Row>, Vec<crate::rows::RowText>) {
    let mut out = crate::rows::Rows::default();
    group.rows(running, width, &crate::image::Layout::default(), &mut out);
    out.into_parts()
}

fn is_dim(span: &ratatui::text::Span<'_>) -> bool {
    span.style.add_modifier.contains(Modifier::DIM)
}

#[test]
fn the_summary_row_matches_the_doc_mock_in_text_and_order() {
    let group = doc_group();
    let width = 120u16;
    let (rows, _) = summary_row(&group, false, width);
    assert_eq!(rows.len(), 1);
    let text = rows[0].0.to_string();
    let left = "● Read 34 files, searched 84 patterns, edited 24 files +175 −83, thought 3 times";
    let right = "4m 12s ▸";
    let filler =
        " ".repeat(usize::from(width) - crate::format::width(left) - crate::format::width(right));
    assert_eq!(text, format!("{left}{filler}{right}"));
    // The right part ends at the row's last cell.
    assert!(text.ends_with("4m 12s ▸"));
    assert_eq!(crate::format::width(&text), usize::from(width));
    // Every cell is dim but the counts, which are dim in their colours.
    for span in &rows[0].0.spans {
        assert!(is_dim(span), "{span:?} is dim");
    }
    let plus = rows[0]
        .0
        .spans
        .iter()
        .find(|span| span.content == "+175")
        .expect("the added count");
    assert_eq!(plus.style.fg, crate::theme::style(Role::Added).fg);
    let minus = rows[0]
        .0
        .spans
        .iter()
        .find(|span| span.content == "−83")
        .expect("the removed count");
    assert_eq!(minus.style.fg, crate::theme::style(Role::Removed).fg);
}

#[test]
fn a_long_summary_is_cut_with_ellipsis_keeping_the_right_part() {
    let group = doc_group();
    let left = "● Read 34 files, searched 84 patterns, edited 24 files +175 −83, thought 3 times";
    let right = "4m 12s ▸";
    let exact = u16::try_from(crate::format::width(left) + 1 + crate::format::width(right))
        .unwrap_or(u16::MAX);
    // Exactly at the edge: one filler space, no ellipsis.
    let (rows, _) = summary_row(&group, false, exact);
    assert_eq!(rows[0].0.to_string(), format!("{left} {right}"));
    assert!(rows[0].0.to_string().ends_with('▸'));
    // One below and at the cut: one "…" appears, ▸ keeps the last column.
    for width in [exact - 1, exact - 2] {
        let (rows, _) = summary_row(&group, false, width);
        let text = rows[0].0.to_string();
        // One truncation only: the left part keeps `width − right − 2`
        // cells, then "…", then one space and the right part.
        let kept = usize::from(width) - crate::format::width(right) - 2;
        assert_eq!(
            text,
            format!("{}… {right}", crate::format::cut(left, kept)),
            "{width}"
        );
        assert_eq!(text.matches('…').count(), 1, "{width}: {text}");
        assert!(text.ends_with("4m 12s ▸"), "{width}: {text}");
        assert_eq!(crate::format::width(&text), usize::from(width), "{width}");
    }
    // One above: no ellipsis, two filler spaces.
    let (rows, _) = summary_row(&group, false, exact + 1);
    let text = rows[0].0.to_string();
    assert!(!text.contains('…'), "{text}");
    assert!(text.ends_with("4m 12s ▸"), "{text}");
    assert_eq!(crate::format::width(&text), usize::from(exact + 1));
}

#[test]
fn a_width_under_the_right_part_cuts_the_whole_row() {
    let group = doc_group();
    let right_width = crate::format::width("4m 12s ▸");
    let width = u16::try_from(right_width + 1).unwrap_or(u16::MAX);
    let (rows, _) = summary_row(&group, false, width);
    assert_eq!(rows.len(), 1);
    let text = rows[0].0.to_string();
    assert_eq!(crate::format::width(&text), usize::from(width));
    assert!(text.contains('…'), "{text}");
}

#[test]
fn a_running_group_marks_its_bullet_to_spin() {
    let mut group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 5_000,
        sections: vec![Section {
            step: 1,
            calls: vec![running_call(1, "read", json!({"path": "a.rs"}))],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, texts) = summary_row(&group, true, 60);
    assert_eq!(rows.len(), 1);
    let dot = &rows[0].0.spans[0];
    assert_eq!(dot.content.as_ref(), "●");
    assert_eq!(dot.style.fg, Some(Role::Attention.color()));
    assert!(
        !dot.style.add_modifier.contains(Modifier::DIM),
        "{dot:?} is not dim"
    );
    assert_eq!(texts[0].spin, Some(0));
    // The duration shows while running too.
    assert!(
        rows[0].0.to_string().contains("5s"),
        "{:?}",
        rows[0].0.to_string()
    );
    // Finished: the dot is dim and nothing spins.
    group.sections[0].calls[0].status = Some(CallStatus::Completed);
    let (rows, texts) = summary_row(&group, false, 60);
    let dot = &rows[0].0.spans[0];
    assert_eq!(dot.content.as_ref(), "●");
    assert!(is_dim(dot), "{dot:?} is dim");
    assert_eq!(dot.style.fg, None);
    assert_eq!(texts[0].spin, None);
}

#[test]
fn the_toggle_opens_with_the_ledger_or_an_asking_call() {
    let mut group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 1_000,
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, _) = summary_row(&group, false, 60);
    assert!(
        rows[0].0.to_string().ends_with('▸'),
        "{:?}",
        rows[0].0.to_string()
    );
    group.open = true;
    let (rows, _) = summary_row(&group, false, 60);
    assert!(
        rows[0].0.to_string().ends_with('▾'),
        "{:?}",
        rows[0].0.to_string()
    );
    group.open = false;
    group.sections[0].calls[0].asking = true;
    let (rows, _) = summary_row(&group, false, 60);
    assert!(
        rows[0].0.to_string().ends_with('▾'),
        "{:?}",
        rows[0].0.to_string()
    );
}

#[test]
fn a_streaming_thought_ends_the_body_italic() {
    let group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 1_000,
        sections: vec![Section {
            step: 1,
            thoughts: vec![Thought {
                id: 1,
                action: "t".to_owned(),
                text: "# Fixing the lock".to_owned(),
                started: 0,
                ended: None,
                ..Thought::default()
            }],
            // A call, so the thought suffix draws on a summary row.
            calls: vec![read_call(2, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, _) = summary_row(&group, true, 80);
    assert_eq!(rows.len(), 1);
    let thinking = rows[0]
        .0
        .spans
        .iter()
        .find(|span| span.content.contains("Thinking:"))
        .expect("the thinking suffix");
    assert_eq!(thinking.content.as_ref(), "Thinking: Fixing the lock");
    assert!(is_dim(thinking));
    assert!(
        thinking.style.add_modifier.contains(Modifier::ITALIC),
        "{thinking:?} is italic"
    );
}

/// A thinking-only group draws its thinking lines while running too:
/// no summary row, no spinner, no duration or toggle, dim and italic
/// throughout (`docs/tui.md`, "Thinking").
#[test]
fn a_running_thinking_only_group_draws_its_thought_lines() {
    let thought = |id: usize, text: &str, ended: Option<u64>| Thought {
        id,
        action: format!("t{id}"),
        text: text.to_owned(),
        started: 0,
        ended,
        ..Thought::default()
    };
    let group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 1_000,
        sections: vec![Section {
            step: 1,
            thoughts: vec![
                thought(1, "# Fixing the lock", None),
                thought(2, "# Finding the lock", Some(22_000)),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, texts) = summary_row(&group, true, 60);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0.to_string(), "Thinking: Fixing the lock");
    assert_eq!(rows[1].0.to_string(), "+ Thought: Finding the lock · 22s");
    for row in &rows {
        let text = row.0.to_string();
        assert!(!text.contains('●'), "{text} has no dot");
        assert!(!text.contains('▸') && !text.contains('▾'), "{text}");
        for span in &row.0.spans {
            assert!(is_dim(span), "{span:?} is dim");
        }
    }
    assert!(
        texts.iter().all(|text| text.spin.is_none()),
        "nothing spins"
    );
}

/// A group with a call still streaming is not thinking-only: while
/// running it draws a summary row with the call in flight.
#[test]
fn a_group_with_a_call_in_flight_draws_a_summary_row() {
    let group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 1_000,
        sections: vec![Section {
            step: 1,
            thoughts: vec![Thought {
                id: 1,
                action: "t".to_owned(),
                text: "# Fixing the lock".to_owned(),
                started: 0,
                ended: None,
                ..Thought::default()
            }],
            ..Section::default()
        }],
        streaming: vec![Streaming {
            message: "a_m".to_owned(),
            index: 0,
            name: None,
            text: "{\"pa".to_owned(),
        }],
        ..Group::default()
    };
    let (rows, texts) = summary_row(&group, true, 60);
    assert_eq!(rows.len(), 1);
    let text = rows[0].0.to_string();
    assert!(text.starts_with("● "), "{text}");
    assert!(text.contains("… {\"pa"), "{text}");
    assert!(text.ends_with('▸'), "{text}");
    assert_eq!(texts[0].spin, Some(0));
}

#[test]
fn a_failed_model_call_count_follows_the_kinds() {
    let group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 0,
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            failed: vec![("rate_limited".to_owned(), 2)],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, _) = summary_row(&group, false, 60);
    let text = rows[0].0.to_string();
    assert!(
        text.starts_with("● Read 1 file · 1 failed model call"),
        "{text}"
    );
    assert!(text.ends_with("0s ▸"), "{text}");
}

#[test]
fn an_empty_body_draws_no_summary_row() {
    let group = Group::default();
    let (rows, _) = summary_row(&group, false, 60);
    assert!(rows.is_empty());
    let (rows, _) = summary_row(&group, true, 60);
    assert!(rows.is_empty());
}

fn shell_call(id: usize, command: &str, exit: Option<i32>, lines: u64, content: &str) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "shell".to_owned(),
        arguments: json!({"command": command}),
        status: Some(CallStatus::Completed),
        result: Some(Box::new(CallResult {
            content: content.to_owned(),
            ..CallResult::default()
        })),
        exit,
        lines,
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn failed_call(id: usize) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "shell".to_owned(),
        arguments: json!({"command": "cargo t"}),
        status: Some(CallStatus::Failed),
        result: Some(Box::new(CallResult {
            headline: "nonzero_exit: exit 101".to_owned(),
            ..CallResult::default()
        })),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn denied_call(id: usize) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "web_fetch".to_owned(),
        arguments: json!({"url": "u"}),
        status: Some(CallStatus::Denied),
        result: Some(Box::new(CallResult {
            headline: "not allowed".to_owned(),
            ..CallResult::default()
        })),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn cancelled_call(id: usize) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "custom".to_owned(),
        arguments: json!("raw text"),
        status: Some(CallStatus::Cancelled),
        result: Some(Box::new(CallResult {
            content: "x".to_owned(),
            ..CallResult::default()
        })),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

/// `call` with its result's content replaced.
fn with_content(mut call: Call, content: &str) -> Call {
    let result = call
        .result
        .get_or_insert_with(|| Box::new(CallResult::default()));
    result.content = content.to_owned();
    call
}

/// `call` with its result's headline replaced.
fn with_headline(mut call: Call, headline: &str) -> Call {
    let result = call
        .result
        .get_or_insert_with(|| Box::new(CallResult::default()));
    result.headline = headline.to_owned();
    call
}

/// `call` with its result's details replaced by `details` JSON text.
fn with_details(mut call: Call, details: serde_json::Value) -> Call {
    let result = call
        .result
        .get_or_insert_with(|| Box::new(CallResult::default()));
    result.details = Some(details.to_string());
    call
}

/// `call` opened.
fn with_open(mut call: Call) -> Call {
    call.open = true;
    call
}

fn ask_call(id: usize) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "ask_user".to_owned(),
        arguments: json!({"questions": [{}, {}]}),
        status: None,
        timing: Timing::Started(0),
        ..Call::default()
    }
}

/// The ledger rows of an open group at `width`: the summary row first.
fn ledger_rows(group: &Group, width: u16) -> Vec<ratatui::text::Line<'static>> {
    let (rows, _) = summary_row(group, false, width);
    assert!(!rows.is_empty());
    rows.into_iter().skip(1).map(|row| row.0).collect()
}

fn cells(line: &ratatui::text::Line<'_>) -> usize {
    crate::format::width(&line.to_string())
}

/// The doc mock's ledger at width 76: the step's number right-aligned in
/// five columns, the glyph, the kind padded to nine, the target, and the
/// result right-aligned to the last cell.
#[test]
fn the_ledger_matches_the_doc_mock_column_for_column() {
    let group = Group {
        key: Some("a_0".to_owned()),
        first: 0,
        last: 1_000,
        open: true,
        sections: vec![
            Section {
                step: 3,
                thoughts: vec![Thought {
                    id: 1,
                    action: "t".to_owned(),
                    text: "# Finding the flaky lock".to_owned(),
                    started: 0,
                    ended: Some(8_000),
                    ..Thought::default()
                }],
                calls: vec![
                    Call {
                        lines: 212,
                        ..read_call(2, "crates/hub/src/lock.rs")
                    },
                    Call {
                        lines: 9,
                        ..search_call(3, "rg -n try_lock")
                    },
                ],
                ..Section::default()
            },
            Section {
                step: 4,
                calls: vec![
                    edit_call(4, "crates/hub/src/lock.rs", 12, 4),
                    running_call(5, "shell", json!({"command": "cargo test -p hub lock"})),
                ],
                ..Section::default()
            },
        ],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 76);
    assert_eq!(rows.len(), 5);
    let text: Vec<String> = rows.iter().map(ToString::to_string).collect();
    assert_eq!(text[0], "    3 + Thought: Finding the flaky lock · 8s");
    // A step's later rows carry a blank six-column gutter, and every row
    // is exactly 76 cells with its result at the last cell.
    let cases = [
        ("      ", '✓', "read", "crates/hub/src/lock.rs", "212 lines"),
        ("      ", '✓', "search", "rg -n try_lock", "9 matches"),
        // Step 4's first row carries its number in the gutter.
        ("    4 ", '✓', "edit", "crates/hub/src/lock.rs", "+12 −4"),
        ("      ", '○', "shell", "cargo test -p hub lock", "running"),
    ];
    for (index, (gutter, glyph, kind, target, result)) in cases.into_iter().enumerate() {
        let row = &text[index + 1];
        let kind_cell = format!("{kind:<9}");
        let gap = " ".repeat(
            76 - 6 - 1 - 1 - 9 - crate::format::width(target) - 2 - crate::format::width(result),
        );
        assert_eq!(
            *row,
            format!("{gutter}{glyph} {kind_cell}{target}{gap}  {result}"),
            "{kind}"
        );
        assert_eq!(cells(&rows[index + 1]), 76);
    }
    assert!(text[4].starts_with("      ○ shell"));
    // The glyphs: ✓ in success, ○ in attention.
    let success = crate::theme::style(Role::Success);
    let attention = crate::theme::style(Role::Attention);
    let info = crate::theme::style(Role::Info);
    for row in &rows[1..4] {
        assert_eq!(row.spans[1].style.fg, success.fg, "{row:?}");
        assert_eq!(row.spans[3].style.fg, info.fg, "{row:?}");
    }
    assert_eq!(rows[4].spans[1].style.fg, attention.fg);
    // The gutter is dim; the kind is in info; the target is the text
    // colour; the edit's counts are added and removed, every other
    // result dim.
    for row in &rows[1..] {
        assert!(is_dim(&row.spans[0]), "{row:?} gutter dim");
    }
    assert_eq!(rows[1].spans[4].style.fg, None);
    assert!(!is_dim(&rows[1].spans[4]), "the target is the text colour");
    assert!(is_dim(&rows[1].spans[6]), "212 lines dim");
    assert_eq!(
        rows[3].spans[6].style.fg,
        crate::theme::style(Role::Added).fg
    );
    assert_eq!(
        rows[3].spans[8].style.fg,
        crate::theme::style(Role::Removed).fg
    );
    let running = &rows[4].spans[6];
    assert_eq!(running.content.as_ref(), "running");
    assert_eq!(running.style.fg, attention.fg);
}

/// Every glyph: ○ and "running" in attention, ✓ in success, ⊘ dim when
/// cancelled, ✗ dim for failed and denied, and "?" in attention on a cut
/// turn.
#[test]
fn ledger_glyphs_name_what_each_call_is_doing() {
    let attention = crate::theme::style(Role::Attention);
    let success = crate::theme::style(Role::Success);
    let section = |calls: Vec<Call>| Section {
        step: 1,
        calls,
        ..Section::default()
    };
    // Running, completed, cancelled, failed and denied.
    let group = Group {
        open: true,
        sections: vec![section(vec![
            running_call(1, "read", json!({"path": "a.rs"})),
            read_call(2, "b.rs"),
            cancelled_call(3),
            failed_call(4),
            denied_call(5),
        ])],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 76);
    assert_eq!(rows.len(), 5);
    let glyph = |row: &ratatui::text::Line<'static>| row.spans[1].clone();
    assert_eq!(glyph(&rows[0]).content.as_ref(), "○");
    assert_eq!(glyph(&rows[0]).style.fg, attention.fg);
    assert_eq!(glyph(&rows[1]).content.as_ref(), "✓");
    assert_eq!(glyph(&rows[1]).style.fg, success.fg);
    assert_eq!(glyph(&rows[2]).content.as_ref(), "⊘");
    assert!(is_dim(&glyph(&rows[2])));
    assert_eq!(glyph(&rows[3]).content.as_ref(), "✗");
    assert!(is_dim(&glyph(&rows[3])));
    assert_eq!(glyph(&rows[4]).content.as_ref(), "✗");
    assert!(is_dim(&glyph(&rows[4])));
    // A call that started and never completed may have run.
    let group = Group {
        open: true,
        cut: true,
        sections: vec![section(vec![running_call(
            1,
            "shell",
            json!({"command": "./smoke.sh"}),
        )])],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 76);
    assert_eq!(rows[0].spans[1].content.as_ref(), "?");
    assert_eq!(rows[0].spans[1].style.fg, attention.fg);
    assert!(
        rows[0]
            .to_string()
            .ends_with("? may have run; not run again")
    );
}

/// Every result branch: reads count lines, searches matches, listings
/// paths, edits colour their counts, commands name their exit, and
/// anything else shows its first line or "done".
#[test]
fn ledger_results_name_what_each_call_did() {
    let section = |calls: Vec<Call>| Section {
        step: 1,
        calls,
        ..Section::default()
    };
    let group = Group {
        open: true,
        sections: vec![section(vec![
            Call {
                lines: 1,
                ..read_call(1, "a.rs")
            },
            Call {
                lines: 2,
                ..read_call(2, "b.rs")
            },
            Call {
                lines: 1,
                ..search_call(3, "rg x")
            },
            Call {
                lines: 4,
                ..Call {
                    id: 4,
                    action: "a_4".to_owned(),
                    name: "shell".to_owned(),
                    arguments: json!({"command": "fd x"}),
                    status: Some(CallStatus::Completed),
                    timing: Timing::Started(0),
                    ..Call::default()
                }
            },
            shell_call(5, "cargo test", Some(0), 2, "ok\nmore"),
            // A shell with no process shows its first line, or "done".
            shell_call(6, "echo hi", None, 0, "a\nb"),
            shell_call(7, "true", None, 0, ""),
            shell_call(8, "echo hi", None, 0, "\n\nfirst\nsecond"),
            failed_call(9),
            denied_call(10),
            cancelled_call(11),
            ask_call(12),
            edit_call(13, "e.rs", 2, 0),
            // An edit with no changes shows its first line like any
            // other call.
            {
                let mut call = edit_call(14, "f.rs", 0, 0);
                call.changes = vec![];
                with_content(call, "wrote it")
            },
        ])],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 100);
    let result_of = |row: &ratatui::text::Line<'static>| {
        row.spans[6..]
            .iter()
            .map(|span| span.content.as_ref().to_owned())
            .collect::<String>()
    };
    let results: Vec<String> = rows.iter().map(result_of).collect();
    assert_eq!(
        results,
        vec![
            "1 line",
            "2 lines",
            "1 match",
            "4 paths",
            "exit 0 · 2 lines",
            "a",
            "done",
            "first",
            "nonzero_exit: exit 101",
            "denied",
            "cancelled",
            "waiting on you",
            "+2 −0",
            "wrote it",
        ]
    );
    // Singulars read as one; every result but the waiting ask and the
    // edit's counts is dim.
    for (index, row) in rows.iter().enumerate() {
        if index == 11 || index == 12 {
            continue;
        }
        for span in &row.spans[6..] {
            assert!(is_dim(span), "{span:?} is dim");
        }
    }
    assert_eq!(
        rows[12].spans[6].style.fg,
        crate::theme::style(Role::Added).fg
    );
    // An ask waiting on the person is in attention.
    assert_eq!(
        rows[11].spans[6].style.fg,
        crate::theme::style(Role::Attention).fg
    );
}

/// A failed call with no error names no headline, so its ledger row
/// falls back to the first line of its content.
#[test]
fn a_failed_call_with_no_error_falls_back_to_its_content_on_the_ledger_row() {
    let group = Group {
        key: Some("a_0".to_owned()),
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![with_content(
                with_headline(failed_call(1), ""),
                "boom\nsecond",
            )],
            ..Section::default()
        }],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 100);
    assert_eq!(rows.len(), 1);
    let result: String = rows[0].spans[6..]
        .iter()
        .map(|span| span.content.as_ref().to_owned())
        .collect();
    assert_eq!(result.trim_end(), "boom");
}

/// The target is cut with "…" to what the result leaves: one column more
/// shows one target cell more.
#[test]
fn a_long_target_is_cut_to_what_its_result_leaves() {
    let target = "src/some/very/long/file/name/that/runs/on.rs";
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![Call {
                lines: 5,
                ..read_call(1, target)
            }],
            ..Section::default()
        }],
        ..Group::default()
    };
    // "5 lines" is 7 cells: the target's room is width − 26.
    let row_at = |width: u16| ledger_rows(&group, width)[0].to_string();
    let (narrow, wide) = (row_at(40), row_at(41));
    assert_eq!(cells(&ledger_rows(&group, 40)[0]), 40);
    assert_eq!(cells(&ledger_rows(&group, 41)[0]), 41);
    assert!(narrow.ends_with("5 lines") && wide.ends_with("5 lines"));
    let room = |width: usize| width - 26;
    assert!(narrow.contains(&format!("{}…", crate::format::cut(target, room(40) - 1))));
    assert!(wide.contains(&format!("{}…", crate::format::cut(target, room(41) - 1))));
}

/// With no room for the target it is left out and the result is cut
/// with "…" instead. "12345 lines" is 11 cells, so at 31 the target
/// is one "…", at 30 the whole result keeps the row's end, and at 25
/// the result itself is cut.
#[test]
fn a_long_result_without_target_room_is_cut_itself() {
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![Call {
                lines: 12345,
                ..read_call(1, "a/very/long/target/name.rs")
            }],
            ..Section::default()
        }],
        ..Group::default()
    };
    // With no room for the target it is left out: at 31 the target is
    // one "…", at 30 the whole result keeps the row's end.
    let rows = ledger_rows(&group, 31);
    assert!(
        rows[0].to_string().ends_with("…  12345 lines"),
        "{}",
        rows[0].to_string()
    );
    assert!(!rows[0].to_string().contains("target"));
    assert_eq!(cells(&rows[0]), 31);
    let rows = ledger_rows(&group, 30);
    assert!(
        rows[0].to_string().ends_with("  12345 lines"),
        "{}",
        rows[0].to_string()
    );
    assert!(!rows[0].to_string().contains('…'));
    assert!(!rows[0].to_string().contains("target"));
    assert_eq!(cells(&rows[0]), 30);
    // Step 1's first row carries its number in the gutter.
    let rows = ledger_rows(&group, 25);
    assert_eq!(rows[0].to_string(), "    1 ✓ read       12345…");
    assert_eq!(cells(&rows[0]), 25);
}

/// No ledger row is wider than its width, down to one column; a step of
/// 99,999 keeps the six-column gutter.
#[test]
fn ledger_rows_never_pass_their_width() {
    let group = Group {
        open: true,
        sections: vec![
            Section {
                step: 99_999,
                thoughts: vec![Thought {
                    id: 1,
                    action: "t".to_owned(),
                    text: "# Look".to_owned(),
                    started: 0,
                    ended: Some(1_000),
                    ..Thought::default()
                }],
                calls: vec![
                    Call {
                        lines: 212,
                        ..read_call(2, "crates/hub/src/lock.rs")
                    },
                    running_call(3, "shell", json!({"command": "cargo test -p hub lock"})),
                ],
                ..Section::default()
            },
            Section {
                step: 100_000,
                calls: vec![read_call(4, "a.rs")],
                ..Section::default()
            },
        ],
        ..Group::default()
    };
    for width in [1u16, 10, 19, 20, 21, 40, 76] {
        let rows = ledger_rows(&group, width);
        for row in &rows {
            assert!(cells(row) <= usize::from(width), "{width}: {row:?}");
        }
    }
    let rows = ledger_rows(&group, 76);
    assert!(rows[0].to_string().starts_with("99999 "));
    assert!(rows[1].to_string().starts_with("      ✓ read"));
    // Past five digits the gutter grows, and the row is still cut to
    // the width.
    assert!(rows[3].to_string().starts_with("100000 "));
    // At step 100000 and width 6 the renderer's row is cut to the
    // width too, and its links shift by the gutter actually drawn:
    // every row is at most 6 cells, and every link lands on the cells
    // drawn.
    let group = Group {
        open: true,
        sections: vec![
            Section {
                step: 100_000,
                calls: vec![read_call(4, "a.rs")],
                ..Section::default()
            },
            Section {
                step: 1_000_000,
                calls: vec![read_call(5, "b.rs")],
                ..Section::default()
            },
        ],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(vec![vec![
            raw("ab"),
            RawSpan {
                text: "hel".to_owned(),
                link: Some("https://example.com".to_owned()),
                ..RawSpan::default()
            },
            raw("lo"),
        ]])),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 6, Some(&bound));
    assert_eq!(rows.len(), texts.len());
    for ((row, _), text) in rows.iter().zip(texts.iter()) {
        let taken = cells(row);
        assert!(taken <= 6, "{row:?} is {taken} cells at width 6");
        for (range, _) in &text.links {
            assert!(
                usize::from(range.end) <= taken,
                "{range:?} lands past the {taken} cells drawn"
            );
        }
    }
}

/// Thinking rows are dim and italic, cut to the width: "○ Thinking" while
/// the thought streams, "+ Thought" once ended. A thinking-only group's
/// line reads the same without the "○ ".
#[test]
fn thinking_rows_stream_and_end_in_italic() {
    let thought = |id: usize, text: &str, ended: Option<u64>| Thought {
        id,
        action: format!("t{id}"),
        text: text.to_owned(),
        started: 0,
        ended,
        ..Thought::default()
    };
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 2,
            thoughts: vec![
                thought(1, "# Fixing the lock", None),
                thought(2, "# Finding the lock", Some(22_000)),
            ],
            // A call, so the thoughts draw as ledger rows.
            calls: vec![read_call(3, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    // A thinking-only group draws no summary row: its lines are the rows.
    let (rows, _) = summary_row(
        &Group {
            sections: vec![Section {
                step: 2,
                thoughts: vec![thought(1, "# Fixing the lock", None)],
                ..Section::default()
            }],
            ..Group::default()
        },
        false,
        60,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.to_string(), "Thinking: Fixing the lock");
    // In the ledger the streaming row carries the "○ ", and the
    // section's first row its number in the gutter.
    let rows = ledger_rows(&group, 60);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].to_string(), "    2 ○ Thinking: Fixing the lock");
    assert_eq!(
        rows[1].to_string(),
        "      + Thought: Finding the lock · 22s"
    );
    for row in &rows[..2] {
        for span in &row.spans {
            assert!(is_dim(span), "{span:?} is dim");
        }
    }
    // Only the thinking text is italic; the gutter is dim alone.
    for row in &rows[..2] {
        assert!(
            row.spans[1..]
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::ITALIC)),
            "{row:?} is italic"
        );
        assert!(
            !row.spans[0].style.add_modifier.contains(Modifier::ITALIC),
            "gutter is not italic"
        );
    }
    // A 200-character heading is cut to the width with "…", in the ledger
    // and on the thinking-only line.
    let long = format!("# {}", "h".repeat(198));
    assert_eq!(crate::format::width(&long), 200);
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 2,
            thoughts: vec![thought(1, &long, Some(1_000))],
            // A call, so the thought draws as a ledger row.
            calls: vec![read_call(2, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 60);
    assert_eq!(cells(&rows[0]), 60);
    assert!(rows[0].to_string().contains('…'));
    // The thinking-only line is cut the same way, without the "○ ".
    let (rows, _) = summary_row(
        &Group {
            sections: vec![Section {
                step: 2,
                thoughts: vec![thought(1, &long, Some(1_000))],
                ..Section::default()
            }],
            ..Group::default()
        },
        false,
        60,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(cells(&rows[0].0), 60);
    assert!(rows[0].0.to_string().contains('…'));
    assert!(!rows[0].0.to_string().contains('○'));
}

/// Thinking rows without a heading show the bare form: "○ Thinking"
/// while streaming, "+ Thought · <span>" once ended.
#[test]
fn thinking_rows_without_a_heading_show_the_bare_form() {
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 2,
            thoughts: vec![
                Thought {
                    id: 1,
                    action: "t1".to_owned(),
                    text: "".to_owned(),
                    started: 0,
                    ended: None,
                    ..Thought::default()
                },
                Thought {
                    id: 2,
                    action: "t2".to_owned(),
                    text: "   ".to_owned(),
                    started: 0,
                    ended: Some(2_000),
                    ..Thought::default()
                },
            ],
            calls: vec![read_call(3, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let rows = ledger_rows(&group, 60);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].to_string(), "    2 ○ Thinking");
    assert_eq!(rows[1].to_string(), "      + Thought · 2s");
}

fn open_read_call(id: usize, lines: usize) -> Call {
    let content = (1..=lines)
        .map(|n| format!("L{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    Call {
        id,
        action: format!("a_{id}"),
        name: "read".to_owned(),
        arguments: json!({"path": "notes.txt"}),
        status: Some(CallStatus::Completed),
        result: Some(Box::new(CallResult {
            details: Some(json!({"start_line": 1}).to_string()),
            content,
            ..CallResult::default()
        })),
        lines: lines as u64,
        timing: Timing::Started(0),
        open: true,
        ..Call::default()
    }
}

/// The rows an opened call draws under its ledger row.
fn opened_rows(group: &Group, width: u16) -> Vec<String> {
    let (rows, _) = summary_row(group, false, width);
    rows.into_iter()
        .skip(2)
        .map(|row| row.0.to_string())
        .collect()
}

fn opened_group(call: Call) -> Group {
    Group {
        key: Some("a_0".to_owned()),
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![call],
            ..Section::default()
        }],
        ..Group::default()
    }
}

/// An opened read shows its lines numbered from the first line read:
/// 19 lines draw whole, 20 draw with no "more" row, and 21 draw 20 rows
/// then "… 1 more line (Enter to view)" dim.
#[test]
fn an_opened_call_is_capped_at_twenty_lines() {
    for (lines, shown, more) in [
        (19usize, 19usize, None),
        (20, 20, None),
        (21, 20, Some("… 1 more line (Enter to view)")),
    ] {
        let rows = opened_rows(&opened_group(open_read_call(1, lines)), 76);
        assert_eq!(rows.len(), shown + usize::from(more.is_some()), "{lines}");
        assert_eq!(rows[0], "         1 L0001");
        assert_eq!(rows[shown - 1], format!("        {:>2} L{shown:04}", shown));
        match more {
            None => assert!(!rows.iter().any(|row| row.contains('…')), "{lines}"),
            Some(last) => assert_eq!(rows[shown], format!("        {last}")),
        }
        for row in &rows {
            assert!(row.starts_with("        "), "{row:?} indented eight");
        }
    }
    // 300 lines draw 20 rows, then "… 280 more lines".
    let rows = opened_rows(&opened_group(open_read_call(1, 300)), 76);
    assert_eq!(rows.len(), 21);
    assert_eq!(rows[19], "        20 L0020");
    assert_eq!(rows[20], "        … 280 more lines (Enter to view)");
    assert!(!rows.iter().any(|row| row.contains("L0021")));
}

/// An opened edit draws its diff with the headers dropped: the `+`
/// span keeps its added colour, body text is not dim, and 26 logical
/// lines cap at 20 with "… 6 more lines (Enter to view)".
#[test]
fn an_opened_diff_is_capped_with_its_colours() {
    let mut diff =
        "diff --git a/a.rs b/a.rs\nindex 123..456 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,14 +1,13 @@\n-old\n+new\n"
            .to_owned();
    for n in 1..=23 {
        diff.push_str(&format!(" context {n}\n"));
    }
    let call = with_open(with_details(
        edit_call(1, "a.rs", 13, 12),
        json!({"diff": diff}),
    ));
    let (rows, _) = summary_row(&opened_group(call), false, 76);
    let shown: Vec<_> = rows.into_iter().skip(2).collect();
    assert_eq!(shown.len(), 21);
    assert_eq!(
        shown[20].0.to_string(),
        "        … 6 more lines (Enter to view)"
    );
    // The diff's own headers never draw.
    assert!(
        !shown
            .iter()
            .any(|row| ["diff --git", "index ", "---", "+++"]
                .iter()
                .any(|header| row.0.to_string().contains(header))),
        "{shown:?}"
    );
    let added = shown
        .iter()
        .find(|row| row.0.to_string().contains("+new"))
        .expect("the added row");
    assert_eq!(
        added.0.spans[2].style.fg,
        Some(crate::theme::Role::Added.color()),
        "{added:?} keeps its colour"
    );
    // Body text is not dim; the hunk row and the gutters are.
    for row in &shown[1..20] {
        for span in row.0.spans.iter().skip(2) {
            assert!(
                !span.style.add_modifier.contains(Modifier::DIM),
                "{span:?} body not dim"
            );
        }
    }
}

/// An opened failure shows its headline first, then its view; empty
/// output reads "no output" dim.
#[test]
fn an_opened_call_shows_its_error_or_no_output() {
    let call = with_open(with_headline(failed_call(1), "ambiguous_match: boom"));
    let rows = opened_rows(&opened_group(call), 76);
    assert_eq!(rows, vec!["        ambiguous_match: boom".to_owned()]);
    let call = with_open(shell_call(2, "true", Some(0), 0, ""));
    let rows = opened_rows(&opened_group(call), 76);
    assert_eq!(rows, vec!["        no output".to_owned()]);
}

/// An opened thought shows all of its text: thoughts are not capped.
#[test]
fn an_opened_thought_is_not_capped() {
    let text = (1..=30)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 1,
            thoughts: vec![Thought {
                id: 1,
                action: "t".to_owned(),
                text,
                started: 0,
                ended: Some(1_000),
                open: true,
            }],
            calls: vec![read_call(2, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, _) = summary_row(&group, false, 76);
    let shown: Vec<String> = rows
        .into_iter()
        .skip(2)
        .map(|row| row.0.to_string())
        .collect();
    assert_eq!(shown.len(), 31);
    assert_eq!(shown[0], "        line 1");
    assert_eq!(shown[29], "        line 30");
    assert!(!shown.iter().any(|row| row.contains('…')));
}

fn write_call(id: usize, path: &str) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "write".to_owned(),
        arguments: json!({"path": path}),
        status: Some(CallStatus::Completed),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn missing_call(id: usize, name: &str) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: name.to_owned(),
        arguments: json!({}),
        status: Some(CallStatus::Completed),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

fn missing_edit_call(id: usize) -> Call {
    Call {
        id,
        action: format!("a_{id}"),
        name: "edit".to_owned(),
        arguments: json!({}),
        status: Some(CallStatus::Completed),
        timing: Timing::Started(0),
        ..Call::default()
    }
}

/// Two distinct paths plus one call with no path count three: the `+`
/// in `kinds` adds both operands, so `-` (1) and `*` (2) fail.
#[test]
fn kinds_add_missing_paths_to_distinct_paths() {
    let group = Group {
        key: Some("a_0".to_owned()),
        first: 0,
        last: 0,
        sections: vec![Section {
            step: 1,
            calls: vec![
                read_call(1, "r1.rs"),
                read_call(2, "r2.rs"),
                missing_call(3, "read"),
                edit_call(4, "e1.rs", 0, 0),
                edit_call(5, "e2.rs", 0, 0),
                missing_edit_call(6),
                write_call(7, "w1.rs"),
                write_call(8, "w2.rs"),
                missing_call(9, "write"),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let width = 80u16;
    let (rows, _) = summary_row(&group, false, width);
    assert_eq!(rows.len(), 1);
    let left = "● Read 3 files, edited 3 files, wrote 3 files";
    let right = "0s ▸";
    let filler =
        " ".repeat(usize::from(width) - crate::format::width(left) - crate::format::width(right));
    assert_eq!(rows[0].0.to_string(), format!("{left}{filler}{right}"));
}

/// A repeated path counts once, then the missing call still adds one:
/// deleting the `!` dedupe counts 0 distinct plus 1 missing (1 file),
/// not 3 files.
#[test]
fn kinds_dedupe_repeated_paths_before_adding_missing() {
    let group = Group {
        key: Some("a_0".to_owned()),
        first: 0,
        last: 0,
        sections: vec![Section {
            step: 1,
            calls: vec![
                read_call(1, "r1.rs"),
                read_call(2, "r1.rs"),
                read_call(3, "r2.rs"),
                missing_call(4, "read"),
                edit_call(5, "e1.rs", 0, 0),
                edit_call(6, "e1.rs", 0, 0),
                edit_call(7, "e2.rs", 0, 0),
                missing_edit_call(8),
                write_call(9, "w1.rs"),
                write_call(10, "w1.rs"),
                write_call(11, "w2.rs"),
                missing_call(12, "write"),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let width = 80u16;
    let (rows, _) = summary_row(&group, false, width);
    assert_eq!(rows.len(), 1);
    let left = "● Read 3 files, edited 3 files, wrote 3 files";
    let right = "0s ▸";
    let filler =
        " ".repeat(usize::from(width) - crate::format::width(left) - crate::format::width(right));
    assert_eq!(rows[0].0.to_string(), format!("{left}{filler}{right}"));
    // Without the missing calls the same duplicates still count two.
    let group = Group {
        key: Some("a_0".to_owned()),
        first: 0,
        last: 0,
        sections: vec![Section {
            step: 1,
            calls: vec![
                write_call(1, "w1.rs"),
                write_call(2, "w1.rs"),
                write_call(3, "w2.rs"),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let (rows, _) = summary_row(&group, false, width);
    assert_eq!(rows.len(), 1);
    let left = "● Wrote 2 files";
    let right = "0s ▸";
    let filler =
        " ".repeat(usize::from(width) - crate::format::width(left) - crate::format::width(right));
    assert_eq!(rows[0].0.to_string(), format!("{left}{filler}{right}"));
}

/// At `right_width + 2 <= max < right_width * 2` the summary keeps the
/// right part and cuts the left with "…": `*` would take the
/// whole-row cut and lose the duration.
#[test]
fn a_narrow_summary_keeps_the_right_part() {
    let group = Group {
        key: Some("a_1".to_owned()),
        first: 0,
        last: 0,
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    // Right part "0s ▸" is 4 cells: `+ 2` admits 6, `* 2` needs 8.
    assert_eq!(summary_row(&group, false, 7).0[0].0.to_string(), "●… 0s ▸");
    assert_eq!(summary_row(&group, false, 6).0[0].0.to_string(), "… 0s ▸");
}

/// `fit` keeps what fits, and cut rows end in exactly one "…": a wide
/// character at the edge leaves a filled column, never a short row.
#[test]
fn fit_cuts_spans_to_its_width_with_one_ellipsis() {
    use ratatui::text::Span;
    // What fits is kept whole with no ellipsis.
    let kept = super::fit(vec![Span::raw("ab")], 2);
    assert_eq!(kept, vec![Span::raw("ab")]);
    // A cut inside a span keeps `max − 1` cells, then one "…".
    let cut = super::fit(vec![Span::raw("abcde")], 4);
    assert_eq!(cut.len(), 3);
    assert_eq!(cut[0].content.as_ref(), "abc");
    assert_eq!(cut[1].content.as_ref(), "…");
    assert_eq!(super::spans_width(&cut), 4);
    assert_eq!(cut.iter().filter(|span| span.content == "…").count(), 1);
    // A cut on a span edge still ends in one "…", not a bare row.
    let edge = super::fit(vec![Span::raw("ab"), Span::raw("cd")], 2);
    assert_eq!(edge[0].content.as_ref(), "a");
    assert_eq!(edge[1].content.as_ref(), "…");
    assert_eq!(super::spans_width(&edge), 2);
    // A wide character at the edge leaves one column unused: it is
    // filled, so the cut row still fills its width.
    let wide = super::fit(vec![Span::raw("ab界cd")], 4);
    assert_eq!(wide[0].content.as_ref(), "ab");
    assert_eq!(wide[1].content.as_ref(), "…");
    assert_eq!(wide[2].content.as_ref(), " ");
    assert_eq!(super::spans_width(&wide), 4);
}

/// A wide target cut at its edge fills the unused column: every ledger
/// row is exactly its width, and the result keeps the last cell.
#[test]
fn a_wide_target_keeps_the_result_at_the_last_cell() {
    let group = Group {
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![Call {
                lines: 5,
                ..read_call(1, "ab界cd/ef/gh.rs")
            }],
            ..Section::default()
        }],
        ..Group::default()
    };
    for width in 20..=76u16 {
        let rows = ledger_rows(&group, width);
        assert_eq!(rows.len(), 1);
        assert_eq!(cells(&rows[0]), usize::from(width), "{width}");
        // Below 26 columns the target has no room and the result
        // itself is cut; from 26 on it keeps the last cell.
        if width >= 26 {
            assert!(rows[0].to_string().ends_with("5 lines"), "{width}");
        }
    }
}

/// A target exactly filling its room is kept whole with no padding:
/// `>=` would cut it to room − 1 plus "…".
#[test]
fn pad_target_keeps_a_target_that_fills_its_room() {
    assert_eq!(super::pad_target("abcd", 4), "abcd");
    assert_eq!(super::pad_target("ab", 4), "ab  ");
    assert_eq!(super::pad_target("abcde", 4), "abc…");
    // A wide character at the edge leaves one column unused: it is
    // filled, so the result keeps its columns.
    assert_eq!(super::pad_target("ab界cd", 4), "ab… ");
    assert_eq!(crate::format::width(&super::pad_target("ab界cd", 4)), 4);
}

/// A call-view target opens nothing: the call stays as it was.
#[test]
fn set_open_ignores_a_call_view_target() {
    let mut group = opened_group(open_read_call(1, 2));
    assert!(group.calls().next().is_some_and(|call| call.open));
    assert!(!group.set_open(&crate::app::Target::CallView(1), true));
    assert!(group.calls().next().is_some_and(|call| call.open));
    assert!(!group.set_open(&crate::app::Target::CallView(1), false));
    assert!(group.calls().next().is_some_and(|call| call.open));
}

/// A test renderer: what slots it fills, what it was called with, what
/// its built-in calls got, and what it answers.
#[derive(Debug, Default)]
struct Fake {
    fills: RefCell<Vec<String>>,
    calls: RefCell<Vec<(String, u16, String)>>,
    builtins: RefCell<Vec<(u16, String)>>,
    script: RefCell<Script>,
}

/// What [`Fake`] answers a render with.
#[derive(Debug, Clone)]
enum Script {
    /// These lines.
    Replace(Vec<Vec<RawSpan>>),
    /// This failure.
    Fail(String),
    /// The built-in row at the width less the badge, then the badge.
    Decorate { badge: Vec<RawSpan> },
}

impl Default for Script {
    fn default() -> Self {
        Script::Replace(vec![vec![raw("X")]])
    }
}

/// A plain span of `text`.
fn raw(text: &str) -> RawSpan {
    RawSpan {
        text: text.to_owned(),
        ..RawSpan::default()
    }
}

impl Renderer for Fake {
    fn name(&self) -> &str {
        "fake"
    }

    fn fills(&self, slot: &str) -> bool {
        self.fills.borrow().iter().any(|filled| filled == slot)
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> crate::ext::Decision {
        crate::ext::tests::decide(self.name(), || -> Result<Vec<Vec<RawSpan>>, Failure> {
            self.calls
                .borrow_mut()
                .push((slot.to_owned(), width, format!("{input:?}")));
            match self.script.borrow().clone() {
                Script::Replace(lines) => Ok(lines),
                Script::Fail(message) => Err(Failure {
                    kind: crate::ext::FailureKind::HookFailed,
                    extension: "fake".to_owned(),
                    message,
                }),
                Script::Decorate { badge } => {
                    let trimming: usize = badge
                        .iter()
                        .map(|span| crate::format::width(&span.text))
                        .sum();
                    let trimming = u16::try_from(trimming).unwrap_or(u16::MAX);
                    let built = builtin(input, width.saturating_sub(trimming));
                    self.builtins
                        .borrow_mut()
                        .push((width.saturating_sub(trimming), format!("{input:?}")));
                    let mut row = built.into_iter().next().unwrap_or_default();
                    row.extend(badge);
                    Ok(vec![row])
                }
            }
        })
    }
}

/// A seam holding `fake`, drawing page 0.
fn seam(fake: &Rc<Fake>, flat: bool) -> Bound {
    let handle = Handle::create();
    handle.set(fake.clone());
    Bound {
        handle,
        cache: Arc::new(Cache::default()),
        page: crate::ext::cache::PageKey {
            session: contract::SessionId("s".into()),
            index: 0,
        },
        ctx: crate::ext::SessionCtx {
            session_id: contract::SessionId("s".into()),
            halves: None,
        },
        flat,
    }
}

/// `group` drawn at `width`, through `seam` when one is bound. The
/// group draws open: the seam covers the ledger rows.
fn draw(
    group: &Group,
    width: u16,
    seam: Option<&Bound>,
) -> (Vec<super::super::Row>, Vec<crate::rows::RowText>) {
    let mut group = group.clone();
    group.open = true;
    let mut out = crate::rows::Rows::default();
    if let Some(seam) = seam {
        out = out.with_seam(seam.clone());
    }
    group.rows(false, width, &crate::image::Layout::default(), &mut out);
    out.into_parts()
}

/// The byte-identity fixtures: every result shape the built-in ledger
/// row draws, one image call, and one opened call.
fn identity_group() -> Group {
    Group {
        key: Some("a_0".to_owned()),
        open: true,
        sections: vec![
            Section {
                step: 1,
                calls: vec![
                    read_call(1, "a.rs"),
                    search_call(2, "rg x"),
                    edit_call(3, "e.rs", 2, 1),
                    running_call(4, "shell", json!({"command": "sleep 1"})),
                    cancelled_call(5),
                    denied_call(6),
                    failed_call(7),
                    with_content(with_headline(failed_call(8), ""), "boom\nsecond"),
                    ask_call(9),
                    answered_ask_call(10),
                ],
                ..Section::default()
            },
            Section {
                step: 2,
                calls: vec![
                    shell_call(11, "cargo test", Some(0), 2, "ok\nmore"),
                    with_open(read_call(12, "b.rs")),
                    Call {
                        images: vec![crate::image::Part {
                            session: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                            path: "pic.png".to_owned(),
                            mime_type: "image/png".to_owned(),
                            width: 4,
                            height: 4,
                        }],
                        ..read_call(13, "c.rs")
                    },
                ],
                ..Section::default()
            },
        ],
        ..Group::default()
    }
}

/// An `ask_user` call whose form the person answered, folded through
/// the real request and resolution: the only way the suite builds one,
/// because the answer's outcome stays inside `answers`.
fn answered_ask_call(id: usize) -> Call {
    use super::super::{Fold, Turn};
    let mut fold = Fold::default();
    let mut turn = Turn::new(Vec::new(), 0);
    turn.call_requested(
        "a_1",
        contract::events::ToolCallRequested {
            name: "ask_user".to_owned(),
            arguments: json!({"questions": [{}, {}]}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
        0,
        &mut fold,
    );
    let mut turns = vec![turn];
    for (kind, payload) in [
        (
            "interaction_requested",
            json!({"request_id": "r_1", "kind": "form",
                "action_ids": ["a_1"],
                "fields": [{"header": "Base", "question": "Which branch?"}]}),
        ),
        (
            "interaction_resolved",
            json!({"request_id": "r_1", "by": "person",
                "answers": [{"labels": ["dev"]}]}),
        ),
    ] {
        assert!(
            super::super::answers::fold(
                &mut turns,
                &contract::Envelope {
                    kind: kind.to_owned(),
                    session_id: contract::SessionId("s".to_owned()),
                    ts: 0,
                    schema_version: contract::SCHEMA_VERSION,
                    turn_id: None,
                    action_id: None,
                    seq: None,
                    payload: payload.as_object().cloned().unwrap_or_default(),
                },
            ),
            "the {kind} line folds"
        );
    }
    let mut call = turns
        .iter()
        .flat_map(|turn| turn.groups())
        .flat_map(|group| group.sections.iter())
        .flat_map(|section| section.calls.iter())
        .find(|call| call.action == "a_1")
        .cloned()
        .expect("the answered ask stays in its turn");
    call.id = id;
    call.action = format!("a_{id}");
    call
}

/// A renderer that fills nothing draws exactly the built-in rows, at
/// widths from a wide terminal down to five columns, and is never
/// called.
#[test]
fn a_fills_nothing_renderer_draws_byte_identical_rows() {
    let group = identity_group();
    let cut = Group {
        cut: true,
        sections: vec![Section {
            step: 1,
            calls: vec![running_call(1, "shell", json!({"command": "sleep 1"}))],
            ..Section::default()
        }],
        ..Group::default()
    };
    for width in [120u16, 80, 40, 25, 19, 12, 5] {
        for group in [&group, &cut] {
            let (plain_rows, plain_texts) = draw(group, width, None);
            let fake = Rc::new(Fake::default());
            let bound = seam(&fake, false);
            let (seam_rows, seam_texts) = draw(group, width, Some(&bound));
            assert_eq!(seam_rows, plain_rows, "width {width}");
            assert_eq!(seam_texts, plain_texts, "width {width}");
            assert!(fake.calls.borrow().is_empty(), "width {width}");
            assert!(fake.builtins.borrow().is_empty(), "width {width}");
        }
    }
}

/// A renderer that fills the slot replaces the row's text after the
/// six-column gutter: the step's number shows on the section's first
/// row only, the row opens the call, and the tail is the pad.
#[test]
fn a_renderer_replaces_the_row_after_the_gutter() {
    let group = Group {
        sections: vec![Section {
            step: 4,
            calls: vec![read_call(1, "a.rs"), read_call(2, "b.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 80, Some(&bound));
    // The summary row first, then one ledger row per call.
    assert_eq!(rows.len(), 3);
    let (rows, texts) = (&rows[1..], &texts[1..]);
    let first = format!("{:>5} X{}", 4, " ".repeat(80 - 7));
    let second = format!("{}X{}", " ".repeat(6), " ".repeat(80 - 7));
    assert_eq!(rows[0].0.to_string(), first);
    assert_eq!(rows[1].0.to_string(), second);
    assert_eq!(rows[0].1, Some(Target::Call(1)));
    assert_eq!(rows[1].1, Some(Target::Call(2)));
    assert_eq!(texts[0].tail, 80 - 7);
    assert!(texts[0].links.is_empty());
}

/// The tool's own slot beats the catch-all: a shell call renders
/// `ledger_row:shell` and never `ledger_row`, while a read call
/// renders `ledger_row` after its own slot misses.
#[test]
fn the_tool_slot_beats_the_catch_all() {
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![
                shell_call(1, "cargo test", Some(0), 2, "ok\nmore"),
                read_call(2, "a.rs"),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned(), "ledger_row:shell".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, _) = draw(&group, 80, Some(&bound));
    assert_eq!(rows.len(), 3);
    let calls = fake.calls.borrow();
    assert_eq!(calls[0].0, "ledger_row:shell");
    assert_eq!(calls[1].0, "ledger_row");
}

/// A fake filling only the catch-all serves every tool under it.
#[test]
fn the_catch_all_serves_every_tool() {
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![
                shell_call(1, "cargo test", Some(0), 2, "ok\nmore"),
                read_call(2, "a.rs"),
            ],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, _) = draw(&group, 80, Some(&bound));
    assert_eq!(rows.len(), 3);
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|(slot, _, _)| slot == "ledger_row"));
}

/// A decorator draws the built-in row plus its badge, uncut: the fake
/// calls the built-in with the width less the badge's two cells, the
/// built-in text equals the ledger's own row output cell for cell,
/// and the row's text is that text plus the badge.
#[test]
fn a_decorator_draws_the_built_in_row_plus_its_badge() {
    crate::work::take();
    let call = read_call(1, "a.rs");
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![call.clone()],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Decorate {
            badge: vec![raw(" +")],
        }),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, _) = draw(&group, 80, Some(&bound));
    assert_eq!(rows.len(), 2);
    let rows = &rows[1..];
    // The terminal less the six-column gutter is 74; the badge takes
    // two, so the built-in draws at 72.
    let builtins = fake.builtins.borrow();
    assert_eq!(builtins.len(), 1);
    assert_eq!(builtins[0].0, 72);
    let output = super::first_result_line(call.content());
    let row = group.ledger_input(&call, &output, false);
    let built = group.builtin_row(&row, 72);
    let built_text: String = built.iter().map(|span| span.content.as_ref()).collect();
    // The row is the gutter, the built-in text, the badge, and the
    // pad: the built-in text cell for cell.
    let taken = 6 + crate::format::width(&built_text) + 2;
    let text = rows[0].0.to_string();
    assert_eq!(
        text,
        format!("    1 {built_text} +{}", " ".repeat(80 - taken))
    );
    // One render, one cache miss: the built-in call touches no cache.
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(crate::work::take().renderer_calls, 1);
}

/// The renderer gets the row width less the six-column gutter: 74 at
/// 80 columns, and 0 when the gutter leaves nothing.
#[test]
fn the_renderer_gets_the_width_less_the_gutter() {
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    for (max, width) in [(80u16, 74u16), (6, 0), (5, 0)] {
        let fake = Rc::new(Fake {
            fills: RefCell::new(vec!["ledger_row".to_owned()]),
            ..Fake::default()
        });
        let bound = seam(&fake, false);
        let (rows, _) = draw(&group, max, Some(&bound));
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 1, "width {max}");
        assert_eq!(calls[0].1, width, "width {max}");
        for row in &rows {
            assert!(crate::format::width(&row.0.to_string()) <= usize::from(max));
        }
    }
}

/// The gutter edge keeps its branch at six columns: below six the row
/// is the gutter cut to the width, and at six and above the gutter
/// precedes the fitted line. The equal case takes the gutter-plus-line
/// branch; the gutter leaves the line nothing there, so the row is the
/// gutter alone, while at seven the line's cell follows it.
#[test]
fn the_gutter_edge_keeps_its_branch_at_six_columns() {
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    // Five columns: the gutter cut to the width, no rendered text.
    let (rows, texts) = draw(&group, 5, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "    1");
    assert_eq!(texts[1].tail, 0);
    assert!(texts[1].links.is_empty());
    // Six columns: the equal case takes the gutter-plus-line branch.
    let (rows, texts) = draw(&group, 6, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "    1 ");
    assert_eq!(texts[1].tail, 0);
    assert!(texts[1].links.is_empty());
    // Seven columns: the gutter plus the fitted line's one cell.
    let (rows, texts) = draw(&group, 7, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "    1 X");
    assert_eq!(texts[1].tail, 0);
    assert!(texts[1].links.is_empty());
}

/// At step 100000 the gutter is seven cells, so at width 6 the row is
/// the gutter cut to the width, and the row fits (#2785).
#[test]
fn the_six_digit_gutter_at_width_six_is_cut_to_the_width() {
    let group = Group {
        sections: vec![Section {
            step: 100000,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, _) = draw(&group, 6, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "100000");
    assert_eq!(cells(&rows[1].0), 6);
}

/// At step 100000 the gutter is seven cells, so a renderer link shifts
/// right by seven, not six: at width 20 the link's 0..3 cells land on
/// 7..10 (#2785).
#[test]
fn a_wide_gutter_shifts_extension_links_by_its_own_cells() {
    let group = Group {
        sections: vec![Section {
            step: 100000,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(vec![vec![
            RawSpan {
                text: "hel".to_owned(),
                link: Some("https://example.com".to_owned()),
                ..RawSpan::default()
            },
            raw("lo"),
        ]])),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 20, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert!(rows[1].0.to_string().starts_with("100000 hel"));
    assert_eq!(cells(&rows[1].0), 20);
    assert_eq!(
        texts[1].links,
        vec![(7..10, "https://example.com".to_owned())]
    );
}

/// At a width equal to the gutter the row keeps the fitted line: with
/// `<`, width 7 at step 100000's 7-cell gutter takes the gutter-plus-line
/// branch, and the link shifts by seven; `<=` would cut the line away.
/// Through a draw the gutter leaves the line nothing there, so the row is
/// the gutter only with no links, while width 8 keeps the link (#2785).
#[test]
fn the_gutter_equal_width_keeps_the_fitted_line_and_its_link() {
    let line = crate::ext::spans::FittedLine {
        line: ratatui::text::Line::from(vec![ratatui::text::Span::raw("ab")]),
        links: vec![(0..2, "https://example.com".to_owned())],
        pad: 0,
    };
    let (row, text) = super::gutter_row("100000 ".to_owned(), 7, &line, 7);
    assert_eq!(row.to_string(), "100000 ab");
    assert_eq!(text.tail, 0);
    assert_eq!(text.links, vec![(7..9, "https://example.com".to_owned())]);
    // The other side of the boundary keeps the line too.
    let (row, text) = super::gutter_row("100000 ".to_owned(), 7, &line, 8);
    assert_eq!(row.to_string(), "100000 ab");
    assert_eq!(text.links, vec![(7..9, "https://example.com".to_owned())]);
    // Through a draw at width 7 the gutter leaves the line nothing, so
    // the row is the gutter only with no links; at width 8 the link
    // stays, shifted by seven.
    let group = Group {
        sections: vec![Section {
            step: 100000,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(vec![vec![
            RawSpan {
                text: "hel".to_owned(),
                link: Some("https://example.com".to_owned()),
                ..RawSpan::default()
            },
            raw("lo"),
        ]])),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 7, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "100000 ");
    assert_eq!(cells(&rows[1].0), 7);
    assert_eq!(texts[1].tail, 0);
    assert!(texts[1].links.is_empty());
    let (rows, texts) = draw(&group, 8, Some(&bound));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.to_string(), "100000 h");
    assert_eq!(cells(&rows[1].0), 8);
    assert_eq!(
        texts[1].links,
        vec![(7..8, "https://example.com".to_owned())]
    );
}

/// An error, a wrong shape, and a refused link each fall back to the
/// built-in row, and the failure is cached: two draws call the fake
/// once per row.
#[test]
fn a_bad_render_falls_back_to_the_built_in_row() {
    let group = identity_group();
    let scripts = [
        Script::Fail("boom".to_owned()),
        Script::Replace(vec![vec![RawSpan {
            text: "X".to_owned(),
            fg: Some("no-such-role".to_owned()),
            ..RawSpan::default()
        }]]),
        Script::Replace(vec![vec![raw("ab\u{7}")]]),
        Script::Replace(vec![vec![RawSpan {
            text: "X".to_owned(),
            fg: Some("#ff00zz".to_owned()),
            ..RawSpan::default()
        }]]),
        Script::Replace(vec![vec![RawSpan {
            text: "here".to_owned(),
            link: Some("https://exa mple.com".to_owned()),
            ..RawSpan::default()
        }]]),
    ];
    let (plain_rows, plain_texts) = draw(&group, 80, None);
    for script in scripts {
        let fake = Rc::new(Fake {
            fills: RefCell::new(vec!["ledger_row".to_owned()]),
            script: RefCell::new(script),
            ..Fake::default()
        });
        let bound = seam(&fake, false);
        let (rows, texts) = draw(&group, 80, Some(&bound));
        assert_eq!(rows, plain_rows);
        assert_eq!(texts, plain_texts);
        draw(&group, 80, Some(&bound));
        let ledger = plain_rows
            .iter()
            .filter(|row| matches!(row.1, Some(Target::Call(_))))
            .count();
        assert_eq!(fake.calls.borrow().len(), ledger);
    }
}

/// Several lines draw several rows with a blank gutter after the
/// first; zero lines draw no row for the call while its opened detail
/// still draws.
#[test]
fn several_lines_draw_several_rows_and_none_draws_none() {
    let group = Group {
        sections: vec![Section {
            step: 4,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(vec![vec![raw("one")], vec![raw("two")]])),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 80, Some(&bound));
    assert_eq!(rows.len(), 3);
    let (rows, texts) = (&rows[1..], &texts[1..]);
    assert_eq!(
        rows[0].0.to_string(),
        format!("{:>5} one{}", 4, " ".repeat(80 - 9))
    );
    assert_eq!(
        rows[1].0.to_string(),
        format!("{}two{}", " ".repeat(6), " ".repeat(80 - 9))
    );
    for (row, text) in rows.iter().zip(texts.iter()) {
        assert_eq!(row.1, Some(Target::Call(1)));
        assert_eq!(text.tail, 80 - 9);
    }
    // Zero lines: the ledger row is gone but the opened detail stays.
    let open = Group {
        open: true,
        sections: vec![Section {
            step: 1,
            calls: vec![with_open(read_call(2, "b.rs"))],
            ..Section::default()
        }],
        ..Group::default()
    };
    let empty = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(Vec::new())),
        ..Fake::default()
    });
    let bound = seam(&empty, false);
    let (rows, _) = draw(&open, 80, Some(&bound));
    let (plain_rows, _) = draw(&open, 80, None);
    // The summary row, then the opened detail: the ledger row between
    // them is gone.
    assert!(!rows.is_empty());
    assert_eq!(rows[0], plain_rows[0]);
    assert_eq!(rows[1..], plain_rows[2..]);
}

/// The renderer input folds the call's events: what status the call is
/// in, its exit and lines, its headline's code and message, its
/// answer, its duration, and whether the page is flat.
#[test]
fn ledger_input_folds_the_call_events() {
    let group = Group::default();
    // A call that never started is pending; nothing ran.
    let pending = Call {
        id: 1,
        action: "a_1".to_owned(),
        name: "shell".to_owned(),
        arguments: json!({"command": "sleep 1"}),
        ..Call::default()
    };
    let row = group.ledger_input(&pending, "", false);
    assert_eq!(row.name, "shell");
    assert_eq!(row.arguments, &json!({"command": "sleep 1"}));
    assert_eq!(row.status, ExtStatus::Pending);
    assert_eq!(row.exit, None);
    assert_eq!(row.duration_ms, None);
    assert_eq!(row.lines, 0);
    assert!(row.changes.is_empty());
    assert_eq!(row.error, None);
    assert_eq!(row.output, "");
    assert_eq!(row.answer, None);
    assert!(!row.flat);
    // A started call with no completion is running.
    let running = running_call(2, "shell", json!({"command": "sleep 1"}));
    let row = group.ledger_input(&running, "", false);
    assert_eq!(row.status, ExtStatus::Running);
    assert_eq!(row.duration_ms, None);
    // A completed shell carries its exit and its lines.
    let done = Call {
        timing: Timing::Took(2500),
        ..shell_call(3, "cargo test", Some(0), 2, "ok\nmore")
    };
    let output = super::first_result_line(done.content());
    let row = group.ledger_input(&done, &output, false);
    assert_eq!(row.status, ExtStatus::Completed);
    assert_eq!(row.exit, Some(0));
    assert_eq!(row.lines, 2);
    assert_eq!(row.duration_ms, Some(2500));
    assert_eq!(row.output, "ok");
    // A failed call's headline splits at the first ": " into its
    // code and message.
    let failed = with_content(
        with_headline(failed_call(4), "nonzero_exit: x: y"),
        "out\nmore",
    );
    let output = super::first_result_line(failed.content());
    let row = group.ledger_input(&failed, &output, false);
    assert_eq!(row.status, ExtStatus::Failed);
    assert_eq!(
        row.error,
        Some(crate::ext::RowError {
            code: "nonzero_exit",
            message: "x: y",
        })
    );
    assert_eq!(row.output, "out");
    // A failed call with no headline names no code: the row ends
    // with its output.
    let bare = with_content(with_headline(failed_call(5), ""), "boom\nsecond");
    let output = super::first_result_line(bare.content());
    let row = group.ledger_input(&bare, &output, false);
    assert_eq!(row.status, ExtStatus::Failed);
    assert_eq!(
        row.error,
        Some(crate::ext::RowError {
            code: "",
            message: "",
        })
    );
    assert_eq!(row.output, "boom");
    // A denied call and a cancelled call carry no error: the built-in
    // row names the denial and the cancellation itself.
    for (call, status) in [
        (denied_call(6), ExtStatus::Denied),
        (cancelled_call(7), ExtStatus::Cancelled),
    ] {
        let output = super::first_result_line(call.content());
        let row = group.ledger_input(&call, &output, false);
        assert_eq!(row.status, status);
        assert_eq!(row.error, None);
    }
    // A started call of a cut turn may have run: its status is
    // unknown.
    let cut = Group {
        cut: true,
        ..Group::default()
    };
    let row = cut.ledger_input(&running, "", false);
    assert_eq!(row.status, ExtStatus::Unknown);
    // An answered form carries the person's answer.
    let answered = answered_ask_call(8);
    let output = super::first_result_line(answered.content());
    let row = group.ledger_input(&answered, &output, false);
    assert_eq!(row.answer, Some("answered"));
    let waiting = ask_call(9);
    let output = super::first_result_line(waiting.content());
    let row = group.ledger_input(&waiting, &output, false);
    assert_eq!(row.answer, None);
    // An edit carries its counts.
    let edit = edit_call(10, "e.rs", 2, 1);
    let output = super::first_result_line(edit.content());
    let row = group.ledger_input(&edit, &output, true);
    assert_eq!(row.status, ExtStatus::Completed);
    assert!(row.flat);
    assert_eq!(row.changes.len(), 1);
    assert_eq!(row.changes[0].added, 2);
    assert_eq!(row.changes[0].removed, 1);
}

/// An extension link's range moves right by the six gutter cells, on
/// the first row and under a blank gutter; a link the width cuts away
/// leaves no range.
#[test]
fn extension_links_shift_right_by_the_gutter() {
    let linked = vec![vec![
        raw("ab"),
        RawSpan {
            text: "hel".to_owned(),
            link: Some("https://example.com".to_owned()),
            ..RawSpan::default()
        },
        raw("lo"),
    ]];
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs"), read_call(2, "b.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        script: RefCell::new(Script::Replace(linked)),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (_, texts) = draw(&group, 80, Some(&bound));
    assert_eq!(texts.len(), 3);
    for text in &texts[1..] {
        assert_eq!(text.links, vec![(8..11, "https://example.com".to_owned())]);
    }
    // At eight columns the gutter leaves "ab": the link's whole
    // text is cut away, so the row links nowhere.
    let (_, texts) = draw(&group, 8, Some(&bound));
    assert_eq!(texts.len(), 3);
    for text in &texts[1..] {
        assert!(text.links.is_empty(), "{text:?}");
    }
}

/// The drawn text is the span texts plus the pad: `to_string` minus
/// the tail is the text.
#[test]
fn the_drawn_text_is_the_span_texts_plus_the_pad() {
    let group = Group {
        sections: vec![Section {
            step: 1,
            calls: vec![read_call(1, "a.rs")],
            ..Section::default()
        }],
        ..Group::default()
    };
    let fake = Rc::new(Fake {
        fills: RefCell::new(vec!["ledger_row".to_owned()]),
        ..Fake::default()
    });
    let bound = seam(&fake, false);
    let (rows, texts) = draw(&group, 20, Some(&bound));
    assert_eq!(rows.len(), 2);
    let (rows, texts) = (&rows[1..], &texts[1..]);
    let text = rows[0].0.to_string();
    assert_eq!(texts[0].tail, 20 - 7);
    assert_eq!(&text[..text.len() - texts[0].tail as usize], "    1 X");
    assert!(texts[0].links.is_empty());
}
