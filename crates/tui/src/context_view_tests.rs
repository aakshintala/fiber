//! Tests for the context breakdown's fold, estimates, bar and frame
//! (`docs/tui.md`, "Swapped views").

use super::{Category, ContextFold, LARGEST_SHOWN, Sized, bar, categories, frame};
use crate::swapped::{Ink, List, render};
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
    assert_eq!(bar(&[], 100, None, 0), "");
}

#[test]
fn a_total_past_the_window_fills_every_cell() {
    let fill = [(Category::Messages, 5)];
    assert_eq!(bar(&fill, 3, None, 4), "▆▆▆▆");
}

#[test]
fn a_zero_total_leaves_every_cell_free() {
    let fill = [(Category::Messages, 0)];
    assert_eq!(bar(&fill, 100, None, 4), "░░░░");
}

#[test]
fn a_zero_window_draws_no_bar_or_marker() {
    let fill = [(Category::Messages, 100)];
    assert_eq!(bar(&fill, 0, Some(50), 4), "");
}

#[test]
fn the_handoff_marker_uses_the_trigger_cell() {
    let fill = [(Category::Messages, 100)];
    assert_eq!(bar(&fill, 100, Some(25), 8), "▆▆│▆▆▆▆▆");
}

#[test]
fn a_trigger_past_the_window_marks_the_last_cell() {
    let fill = [(Category::Messages, 100)];
    assert_eq!(bar(&fill, 4, Some(5), 3), "▆▆│");
}

#[test]
fn no_trigger_draws_no_handoff_marker() {
    let fill = [(Category::Messages, 50)];
    assert_eq!(bar(&fill, 100, None, 4), "▆▆░░");
}

#[test]
fn a_category_boundary_on_a_cell_edge_ends_before_the_next_cell() {
    let fill = [
        (Category::SystemPrompt, 1),
        (Category::ToolDefinitions, 3),
        (Category::ToolResults, 0),
        (Category::Messages, 0),
    ];
    assert_eq!(bar(&fill, 4, None, 4), "█▓▓▓");
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
            trigger: Some(80),
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
            trigger: None,
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
            [vec![(
                "The context shows after the session's first request.".to_owned(),
                None,
                Ink::Muted
            )]]
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
            trigger: Some(80),
        }),
        List::default(),
        12,
    );
    assert!(row(&frame, "context  ").is_some());
    assert_eq!(row(&frame, "▆"), Some("▆▆░░░░░░░│░░".to_owned()));
    assert_eq!(frame.below, ["Breakdown after a request without images."]);
    assert!(
        text(
            &fold,
            Rate::default(),
            Some(Sized {
                total: 20,
                window: 100,
                trigger: Some(80),
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
            trigger: Some(80),
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
            trigger: Some(80),
        }),
        80,
    );
    for category in [
        "█ system prompt",
        "▓ tool definitions",
        "▒ tool results",
        "▆ messages",
    ] {
        assert!(shown.contains(category), "{shown}");
    }
    assert!(shown.contains("read  ~6 tokens"), "{shown}");
    assert_eq!(frame.title, "Context");
    assert_eq!(frame.footer, "↑↓ scroll · Esc close");
    assert_eq!(
        frame.below,
        ["Sizes are approximate: bytes at the session's own tokens-per-byte rate."]
    );
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
            trigger: None,
        }),
        80,
    );
    assert!(shown.contains("automatic handoff off"));
    assert!(!shown.contains("handoff at"));
}

#[test]
fn the_bar_row_stays_present_and_row_count_does_not_depend_on_width() {
    let fold = fold_with_sizes(4, 5, 6);
    let sized = Some(Sized {
        total: 20,
        window: 100,
        trigger: Some(80),
    });
    let zero = frame(&fold, rate(), sized, List::default(), 0);
    let wide = frame(&fold, rate(), sized, List::default(), 80);
    assert_eq!(zero.rows.len(), wide.rows.len());
    assert_eq!(
        zero.rows.get(1).map(|cells| cells
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<String>()),
        Some(String::new())
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
            trigger: Some(80),
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
            trigger: Some(140_000),
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
