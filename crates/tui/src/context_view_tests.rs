//! Tests for the context breakdown's fold, estimates, bar and frame
//! (`docs/tui.md`, "Swapped views").

use super::{Category, ContextFold, Handoff, LARGEST_SHOWN, Sized, bar, categories, frame};
use crate::swapped::{Ink, List, render};
use crate::theme::Role;
use contract::Envelope;
use contract::{ActionId, SCHEMA_VERSION, SessionId};
use log::{Rate, RateFold};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

fn envelope(kind: &str, action: Option<&str>, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId("s_1".to_owned()),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

fn preamble(system: &str, tools: Vec<Value>, trigger: Option<u64>) -> Envelope {
    let mut payload = json!({
        "reason": "start", "model": "model/m", "context_window": 100,
        "system_prompt": system, "tools": tools,
        "tool_choice": "auto", "cache_lifetime": "5m",
    });
    if let Some(trigger) = trigger {
        payload["trigger_at"] = json!(trigger);
    }
    envelope("preamble_built", None, payload)
}

fn tool(name: &str, definition: Value, deferred: bool) -> Value {
    json!({"name": name, "definition": definition, "deferred": deferred})
}

fn requested(action: &str, name: &str) -> Envelope {
    envelope(
        "tool_call_requested",
        Some(action),
        json!({"name": name, "arguments": {}}),
    )
}

fn completed(action: Option<&str>, content: Vec<Value>) -> Envelope {
    envelope(
        "tool_call_completed",
        action,
        json!({"status": "completed", "content": content}),
    )
}

fn text_part(text: String) -> Value {
    json!({"type": "text", "text": text})
}

fn rate() -> Rate {
    let mut fold = RateFold::default();
    fold.fold(&envelope(
        "preamble_built",
        None,
        json!({"model": "model/m"}),
    ));
    fold.fold(&envelope(
        "assistant_message_started",
        Some("rate-action"),
        json!({}),
    ));
    fold.fold(&envelope(
        "usage_recorded",
        Some("rate-action"),
        json!({
            "generation_id": "g_1", "model": "model/m",
            "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 0},
            "input_bytes": 1, "cost": 0.0,
        }),
    ));
    fold.rate()
}

fn fold_with_sizes(system: usize, tool_definition_bytes: usize, results: usize) -> ContextFold {
    let mut fold = ContextFold::default();
    fold.fold(&preamble(&"s".repeat(system), Vec::new(), Some(80)));
    // Use the fold's byte totals directly here so the category cap cases do
    // not depend on JSON object formatting.
    fold.system = u64::try_from(system).unwrap();
    fold.tools = u64::try_from(tool_definition_bytes).unwrap();
    fold.results = u64::try_from(results).unwrap();
    fold
}

fn row(frame: &crate::swapped::Frame, prefix: &str) -> Option<String> {
    frame.rows.iter().find_map(|row| {
        let text = row
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<String>();
        text.starts_with(prefix).then_some(text)
    })
}

fn text(fold: &ContextFold, rate: Rate, sized: Option<Sized>, width: u16) -> String {
    let frame = frame(fold, rate, sized, List::default(), width);
    frame
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_doc_numbers_draw_bold_sizes_a_role_bar_and_an_attention_label() {
    // 268,000 of a 1,000,000 window with the handoff at 400,000: the
    // bar spans to the handoff point, so the marker sits on its end.
    let fold = fold_with_sizes(100_000, 50_000, 118_000);
    let sized = Sized {
        total: 268_000,
        window: 1_000_000,
        trigger: Handoff::At(400_000),
    };
    let frame = frame(&fold, rate(), Some(sized), List::default(), 80);
    // The body is indented two columns: "268k tokens" bold, then the
    // dim tail, and the bar leaves the indent's two columns.
    assert_eq!(
        frame.rows[0][1],
        ("268k tokens".to_owned(), None, Ink::Heading)
    );
    assert_eq!(
        frame.rows[0][2],
        (
            " · 26% of the 1M window · automatic handoff at 400k".to_owned(),
            None,
            Ink::Muted
        )
    );
    // One █ run per non-empty category in its role, ▁ for the rest,
    // one │ in attention on the last cell.
    let runs: Vec<(usize, Ink)> = frame.rows[1]
        .iter()
        .skip(1)
        .map(|(text, _, ink)| (text.chars().count(), *ink))
        .collect();
    assert_eq!(
        runs,
        [
            (19, Ink::Role(Role::Accent)),
            (10, Ink::Role(Role::Info)),
            (23, Ink::Role(Role::Error)),
            (25, Ink::Role(Role::Rule)),
            (1, Ink::Role(Role::Attention)),
        ]
    );
    let bar: String = frame.rows[1]
        .iter()
        .map(|(text, _, _)| text.as_str())
        .collect();
    assert_eq!(
        bar,
        format!(
            "  {}{}{}{}│",
            "█".repeat(19),
            "█".repeat(10),
            "█".repeat(23),
            "▁".repeat(25)
        )
    );
    // The label ends under the marker, in attention.
    assert_eq!(
        frame.rows[2]
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<String>(),
        format!("{}handoff 400k", " ".repeat(68))
    );
    assert_eq!(
        frame.rows[2].last(),
        Some(&("handoff 400k".to_owned(), None, Ink::Role(Role::Attention)))
    );
    // One row per category: the mark in its colour, the padded name,
    // the size and its share dim.
    let categories: Vec<String> = frame.rows[3..7]
        .iter()
        .map(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        categories,
        [
            format!("  ■ {:<20}~100k 37%", "system prompt"),
            format!("  ■ {:<20}~50k 18%", "tool definitions"),
            format!("  ■ {:<20}~118k 44%", "tool results"),
            format!("  ■ {:<20}~0 0%", "messages"),
        ]
    );
    assert_eq!(frame.rows[3][1].2, Ink::Role(Role::Accent));
    assert_eq!(frame.rows[4][1].2, Ink::Role(Role::Info));
    assert_eq!(frame.rows[5][1].2, Ink::Role(Role::Error));
    assert_eq!(frame.rows[6][1].2, Ink::Role(Role::Text));
    assert_eq!(frame.rows[3][3].2, Ink::Muted);
    // Then the bold largest heading.
    assert_eq!(
        frame.rows[7][1],
        ("Largest tool results".to_owned(), None, Ink::Heading)
    );
}

#[test]
fn categories_sum_to_the_total_with_messages_as_the_remainder() {
    let fold = fold_with_sizes(4, 5, 6);
    assert_eq!(
        categories(&fold, rate(), 20),
        Some([
            (Category::SystemPrompt, 4),
            (Category::ToolDefinitions, 5),
            (Category::ToolResults, 6),
            (Category::Messages, 5),
        ])
    );
}

#[test]
fn system_estimate_past_the_total_is_capped_and_leaves_no_other_tokens() {
    let fold = fold_with_sizes(12, 5, 6);
    assert_eq!(
        categories(&fold, rate(), 5),
        Some([
            (Category::SystemPrompt, 5),
            (Category::ToolDefinitions, 0),
            (Category::ToolResults, 0),
            (Category::Messages, 0),
        ])
    );
}

#[test]
fn tool_estimate_past_the_remainder_is_capped_before_results() {
    let fold = fold_with_sizes(3, 10, 5);
    assert_eq!(
        categories(&fold, rate(), 8),
        Some([
            (Category::SystemPrompt, 3),
            (Category::ToolDefinitions, 5),
            (Category::ToolResults, 0),
            (Category::Messages, 0),
        ])
    );
}

#[test]
fn result_estimate_past_the_remainder_is_capped_before_messages() {
    let fold = fold_with_sizes(3, 2, 10);
    assert_eq!(
        categories(&fold, rate(), 8),
        Some([
            (Category::SystemPrompt, 3),
            (Category::ToolDefinitions, 2),
            (Category::ToolResults, 3),
            (Category::Messages, 0),
        ])
    );
}

#[test]
fn categories_are_absent_until_a_rate_is_available() {
    let fold = fold_with_sizes(4, 5, 6);
    assert_eq!(categories(&fold, Rate::default(), 20), None);
}

#[test]
fn categories_are_absent_without_a_preamble_even_with_a_rate() {
    // A continuation with no `preamble_built` of its own (see #3756):
    // once its prompt supplies a rate, the empty fold's unknown sizes
    // must not render as zero-valued categories.
    let fold = ContextFold::default();
    assert_eq!(categories(&fold, rate(), 20), None);
    let sized = Some(Sized {
        total: 20,
        window: 100,
        trigger: Handoff::Unknown,
    });
    let shown = text(&fold, rate(), sized, 80);
    assert!(!shown.contains("system prompt"), "{shown}");
    assert!(!shown.contains("tool definitions"), "{shown}");
    // No category row draws: each one starts with its "■ " glyph, which
    // the "Largest tool results" heading never carries.
    assert!(!shown.contains("■ "), "{shown}");
}

#[test]
fn an_empty_preamble_draws_known_zero_categories() {
    // The other side of the unknown-sizes gate: a `preamble_built`
    // with empty sizes is known zero, so the breakdown still draws.
    let mut fold = ContextFold::default();
    fold.fold(&envelope(
        "preamble_built",
        None,
        json!({"model": "model/m"}),
    ));
    let sized = Some(Sized {
        total: 20,
        window: 100,
        trigger: Handoff::Off,
    });
    let shown = text(&fold, rate(), sized, 80);
    assert!(shown.contains("system prompt"), "{shown}");
    assert!(shown.contains("automatic handoff off"), "{shown}");
}

#[test]
fn preamble_sizes_use_utf8_bytes_and_only_non_deferred_definitions() {
    let mut fold = ContextFold::default();
    fold.fold(&preamble(
        "é",
        vec![
            tool("read", json!({"a": 1}), false),
            tool("deferred", json!({"b": 2}), true),
        ],
        Some(80),
    ));
    assert_eq!(fold.system, 2);
    assert_eq!(fold.tools, 7);
}

#[test]
fn a_zero_width_bar_is_empty() {
    assert!(bar(&[], 100, None, 0).is_empty());
}

#[test]
fn a_total_past_the_window_fills_every_cell() {
    let fill = [(Category::Messages, 5)];
    assert_eq!(bar(&fill, 3, None, 4), [("████".to_owned(), Role::Text)]);
}

#[test]
fn a_zero_total_leaves_every_cell_free() {
    let fill = [(Category::Messages, 0)];
    assert_eq!(bar(&fill, 100, None, 4), [("▁▁▁▁".to_owned(), Role::Rule)]);
}

#[test]
fn a_zero_window_draws_no_bar_or_marker() {
    let fill = [(Category::Messages, 100)];
    assert!(bar(&fill, 0, Some(50), 4).is_empty());
}

#[test]
fn the_handoff_marker_uses_the_trigger_cell() {
    let fill = [(Category::Messages, 100)];
    assert_eq!(
        bar(&fill, 100, Some(25), 8),
        [
            ("██".to_owned(), Role::Text),
            ("│".to_owned(), Role::Attention),
            ("█████".to_owned(), Role::Text),
        ]
    );
}

#[test]
fn a_trigger_past_the_window_marks_the_last_cell() {
    let fill = [(Category::Messages, 100)];
    assert_eq!(
        bar(&fill, 4, Some(5), 3),
        [
            ("██".to_owned(), Role::Text),
            ("│".to_owned(), Role::Attention),
        ]
    );
}

#[test]
fn no_trigger_draws_no_handoff_marker() {
    let fill = [(Category::Messages, 50)];
    assert_eq!(
        bar(&fill, 100, None, 4),
        [("██".to_owned(), Role::Text), ("▁▁".to_owned(), Role::Rule),]
    );
}

#[test]
fn a_nonzero_category_keeps_one_cell() {
    // Too small to round to a cell still draws one, so every
    // non-empty category keeps its run.
    let fill = [(Category::SystemPrompt, 1), (Category::Messages, 1000)];
    assert_eq!(
        bar(&fill, 140_000, None, 80),
        [
            ("█".to_owned(), Role::Accent),
            ("█".to_owned(), Role::Text),
            ("▁".repeat(78), Role::Rule),
        ]
    );
}

#[test]
fn a_category_boundary_on_a_cell_edge_ends_before_the_next_cell() {
    let fill = [
        (Category::SystemPrompt, 1),
        (Category::ToolDefinitions, 3),
        (Category::ToolResults, 0),
        (Category::Messages, 0),
    ];
    assert_eq!(
        bar(&fill, 4, None, 4),
        [
            ("█".to_owned(), Role::Accent),
            ("███".to_owned(), Role::Info),
        ]
    );
}

#[test]
fn largest_results_show_five_largest_and_keep_arrival_order_for_ties() {
    let mut fold = ContextFold::default();
    for (at, bytes) in [10, 9, 8, 7, 6, 11, 11].into_iter().enumerate() {
        let action = format!("a{at}");
        fold.fold(&requested(&action, &format!("tool-{at}")));
        fold.fold(&completed(
            Some(&action),
            vec![text_part("x".repeat(bytes))],
        ));
    }
    assert_eq!(LARGEST_SHOWN, 5);
    let shown = text(
        &fold,
        Rate::default(),
        Some(Sized {
            total: 100,
            window: 100,
            trigger: Handoff::At(80),
        }),
        80,
    );
    let positions: Vec<usize> = ["tool-5", "tool-6", "tool-0", "tool-1", "tool-2"]
        .iter()
        .map(|tool| shown.find(tool).expect("largest tool is shown"))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!shown.contains("tool-3"));
    assert!(!shown.contains("tool-4"));
}

#[test]
fn a_completed_handoff_clears_tool_results_and_the_largest_list() {
    let mut fold = ContextFold::default();
    fold.fold(&requested("a1", "read"));
    fold.fold(&completed(Some("a1"), vec![text_part("result".to_owned())]));
    fold.fold(&envelope(
        "handoff_completed",
        None,
        json!({"outcome": "completed"}),
    ));
    assert_eq!(fold.results, 0);
    assert!(fold.largest.is_empty());
}

#[test]
fn failed_and_cancelled_handoffs_keep_tool_results_and_the_largest_list() {
    for outcome in ["failed", "cancelled"] {
        let mut fold = ContextFold::default();
        fold.fold(&requested("a1", "read"));
        fold.fold(&completed(Some("a1"), vec![text_part("result".to_owned())]));
        fold.fold(&envelope(
            "handoff_completed",
            None,
            json!({"outcome": outcome}),
        ));
        assert_eq!(fold.results, 6, "{outcome}");
        assert_eq!(fold.largest.len(), 1, "{outcome}");
    }
}

#[test]
fn result_names_come_from_the_request_and_unseen_requests_are_tools() {
    let mut fold = ContextFold::default();
    fold.fold(&requested("known", "read"));
    fold.fold(&completed(
        Some("known"),
        vec![text_part("first".to_owned())],
    ));
    fold.fold(&completed(
        Some("unseen"),
        vec![text_part("second".to_owned())],
    ));
    let shown = text(
        &fold,
        Rate::default(),
        Some(Sized {
            total: 100,
            window: 100,
            trigger: Handoff::Unknown,
        }),
        80,
    );
    assert!(shown.contains("read  5 bytes"));
    assert!(shown.contains("tool  6 bytes"));
}

#[test]
fn image_parts_add_no_bytes_to_tool_results() {
    let mut fold = ContextFold::default();
    fold.fold(&completed(
        None,
        vec![
            text_part("abc".to_owned()),
            json!({"type": "image", "path": "artifacts/i.png", "mime_type": "image/png",
                "width": 20, "height": 20}),
        ],
    ));
    assert_eq!(fold.results, 3);
}

#[test]
fn no_preamble_or_context_total_says_when_context_appears() {
    let no_preamble = ContextFold::default();
    let mut with_preamble = ContextFold::default();
    with_preamble.fold(&preamble("system", Vec::new(), Some(80)));
    for fold in [&no_preamble, &with_preamble] {
        let frame = frame(fold, Rate::default(), None, List::default(), 80);
        assert_eq!(
            frame.rows,
            [vec![
                ("  ".to_owned(), None, Ink::Plain),
                (
                    "The context shows after the session's first request.".to_owned(),
                    None,
                    Ink::Muted
                )
            ]]
        );
    }
}

#[test]
fn without_a_rate_the_view_draws_one_fill_and_says_when_to_break_down() {
    let mut fold = fold_with_sizes(4, 5, 6);
    fold.fold(&requested("a1", "read"));
    fold.fold(&completed(Some("a1"), vec![text_part("123456".to_owned())]));
    let frame = frame(
        &fold,
        Rate::default(),
        Some(Sized {
            total: 20,
            window: 100,
            trigger: Handoff::At(80),
        }),
        List::default(),
        12,
    );
    assert!(row(&frame, "  20 tokens").is_some());
    assert_eq!(
        frame
            .rows
            .iter()
            .find(|row| row.iter().any(|(_, _, ink)| matches!(ink, Ink::Role(_))))
            .map(|row| row
                .iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()),
        Some("  ██▁▁▁▁▁▁▁│".to_owned())
    );
    assert_eq!(frame.below, ["Breakdown after a request without images."]);
    assert!(
        text(
            &fold,
            Rate::default(),
            Some(Sized {
                total: 20,
                window: 100,
                trigger: Handoff::At(80),
            }),
            80
        )
        .contains("6 bytes")
    );
}

#[test]
fn with_a_rate_the_view_shows_each_category_and_sizes_results_in_tokens() {
    let mut fold = fold_with_sizes(4, 5, 6);
    fold.fold(&requested("a1", "read"));
    fold.fold(&completed(Some("a1"), vec![text_part("123456".to_owned())]));
    let frame = frame(
        &fold,
        rate(),
        Some(Sized {
            total: 20,
            window: 100,
            trigger: Handoff::At(80),
        }),
        List::default(),
        80,
    );
    let shown = text(
        &fold,
        rate(),
        Some(Sized {
            total: 20,
            window: 100,
            trigger: Handoff::At(80),
        }),
        80,
    );
    for category in [
        "system prompt",
        "tool definitions",
        "tool results",
        "messages",
    ] {
        assert!(shown.contains(category), "{shown}");
    }
    assert!(shown.contains("~4 20%"), "{shown}");
    assert!(shown.contains("~5 25%"), "{shown}");
    assert!(shown.contains("~11 55%"), "{shown}");
    assert!(shown.contains("read  ~6 tokens"), "{shown}");
    assert_eq!(frame.title, "Context");
    assert_eq!(frame.command, "/context");
    assert_eq!(frame.footer, "");
    assert_eq!(
        frame.below,
        ["Sizes are approximate: bytes at the session's own tokens-per-byte rate."]
    );
}

#[test]
fn tool_results_draw_in_error() {
    // The only coverage of the tool-result `error` colour: no pty case
    // can reach it, because the scripted provider reports no rate.
    let fold = fold_with_sizes(4, 5, 6);
    let frame = frame(
        &fold,
        rate(),
        Some(Sized {
            total: 20,
            window: 100,
            trigger: Handoff::At(80),
        }),
        List::default(),
        80,
    );
    let results = frame
        .rows
        .iter()
        .find(|row| {
            row.iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>()
                .contains("tool results")
        })
        .expect("the tool results row");
    assert_eq!(results[1], ("■ ".to_owned(), None, Ink::Role(Role::Error)));
}

#[test]
fn a_missing_trigger_says_automatic_handoff_is_off() {
    let fold = fold_with_sizes(4, 5, 6);
    let shown = text(
        &fold,
        rate(),
        Some(Sized {
            total: 20,
            window: 100,
            trigger: Handoff::Off,
        }),
        80,
    );
    assert!(shown.contains("automatic handoff off"));
    assert!(!shown.contains("handoff at"));
}

#[test]
fn an_unknown_handoff_draws_the_window_with_no_marker_and_no_off_row() {
    // A continuation with no `preamble_built` of its own (see #3756):
    // the total draws against the inherited window, with no handoff
    // tail, no marker label and no off row.
    let fold = ContextFold::default();
    let sized = Some(Sized {
        total: 50,
        window: 200,
        trigger: Handoff::Unknown,
    });
    let frame = frame(&fold, Rate::default(), sized, List::default(), 80);
    let shown = text(&fold, Rate::default(), sized, 80);
    assert!(
        shown.contains("50 tokens · 25% of the 200 window"),
        "{shown}"
    );
    // No tail, no marker label and no off row: each carries "handoff "
    // with a trailing word, which the largest-results row never has.
    assert!(!shown.contains("handoff "), "{shown}");
    assert!(
        !shown.contains("The context shows after the session's first request."),
        "{shown}"
    );
    // The bar still draws against the window, and the total alone.
    assert!(shown.contains('█'), "{shown}");
    assert!(!shown.contains("system prompt"), "{shown}");
    assert_eq!(frame.below, ["Breakdown after a request without images."]);
    assert!(shown.contains("none since the last handoff"), "{shown}");
}

#[test]
fn the_bar_row_stays_present_and_row_count_does_not_depend_on_width() {
    let fold = fold_with_sizes(4, 5, 6);
    let sized = Some(Sized {
        total: 20,
        window: 100,
        trigger: Handoff::At(80),
    });
    let zero = frame(&fold, rate(), sized, List::default(), 0);
    let wide = frame(&fold, rate(), sized, List::default(), 80);
    assert_eq!(zero.rows.len(), wide.rows.len());
    assert_eq!(
        zero.rows.get(1).map(|cells| cells
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<String>()),
        Some("  ".to_owned())
    );
    assert!(wide.rows.iter().any(|cells| {
        cells
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<String>()
            .contains('█')
    }));
}

#[test]
fn a_fork_notes_that_inherited_history_counts_under_messages() {
    let mut fold = ContextFold::default();
    fold.fold(&envelope(
        "session_started",
        None,
        json!({"workspace": "/w", "variables": {},
            "forked_from": {"session_id": "s_parent", "seq": 8}}),
    ));
    fold.fold(&preamble("system", Vec::new(), Some(80)));
    let shown = text(
        &fold,
        rate(),
        Some(Sized {
            total: 100,
            window: 100,
            trigger: Handoff::At(80),
        }),
        80,
    );
    assert!(shown.contains("history before the fork counts under messages"));
}

#[test]
fn the_context_view_snapshot_is_rendered_through_the_shared_frame() {
    let mut fold = ContextFold::default();
    fold.fold(&preamble(
        "system prompt",
        vec![tool("read", json!({"type": "object"}), false)],
        Some(80),
    ));
    fold.fold(&requested("a1", "read"));
    fold.fold(&completed(
        Some("a1"),
        vec![text_part("tool result".to_owned())],
    ));
    let view = frame(
        &fold,
        rate(),
        Some(Sized {
            total: 61_234,
            window: 200_000,
            trigger: Handoff::At(140_000),
        }),
        List::default(),
        80,
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&view, area, &mut buffer, &mut targets);
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
    insta::assert_snapshot!("context_80x24", screen);
}
