//! Tests for a tool group's drawing.

use contract::events::{CallStatus, FileChange};
use ratatui::style::Modifier;
use serde_json::json;

use super::{Call, Group, Section, Thought};
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
        started: true,
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
        started: true,
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
        started: true,
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
        started: true,
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
            started: true,
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
    assert_eq!(plus.style.fg, crate::markdown::style(Role::Added).fg);
    let minus = rows[0]
        .0
        .spans
        .iter()
        .find(|span| span.content == "−83")
        .expect("the removed count");
    assert_eq!(minus.style.fg, crate::markdown::style(Role::Removed).fg);
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
    // One below and at the cut: the ellipsis appears, ▸ keeps the last column.
    for width in [exact - 1, exact - 2] {
        let (rows, _) = summary_row(&group, false, width);
        let text = rows[0].0.to_string();
        assert!(text.contains('…'), "{width}: {text}");
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
