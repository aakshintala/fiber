//! The `/context` view: estimated context categories and largest tool results
//! (`docs/tui.md`, "Swapped views").

use std::collections::BTreeMap;

use contract::Envelope;
use log::Rate;

use crate::swapped::{Frame, List, about};

/// The number of largest tool results shown.
pub(crate) const LARGEST_SHOWN: usize = 5;

/// A context category in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    /// Bytes from the system prompt.
    SystemPrompt,
    /// Serialized tool definitions.
    ToolDefinitions,
    /// Text bytes returned by tools.
    ToolResults,
    /// The rest of the context.
    Messages,
}

/// The context sizes folded from the attached session's events.
#[derive(Debug, Default)]
pub(crate) struct ContextFold {
    system: u64,
    tools: u64,
    results: u64,
    largest: Vec<Largest>,
    names: BTreeMap<String, String>,
    forked: bool,
}

impl ContextFold {
    /// Folds preamble sizes, tool results and handoffs from the session stream.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        match envelope.kind.as_str() {
            "session_started" => {
                self.forked = envelope
                    .payload
                    .get("forked_from")
                    .is_some_and(|forked_from| !forked_from.is_null());
            }
            "preamble_built" => {
                self.system = envelope
                    .payload
                    .get("system_prompt")
                    .and_then(serde_json::Value::as_str)
                    .map_or(0, |prompt| byte_count(prompt.len()));
                self.tools = envelope
                    .payload
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|tool| {
                        tool.get("deferred").and_then(serde_json::Value::as_bool) == Some(false)
                    })
                    .filter_map(|tool| tool.get("definition"))
                    .fold(0u64, |total, definition| {
                        total.saturating_add(byte_count(definition.to_string().len()))
                    });
            }
            "tool_call_requested" => {
                if let (Some(action), Some(name)) = (
                    envelope.action_id.as_ref(),
                    envelope
                        .payload
                        .get("name")
                        .and_then(serde_json::Value::as_str),
                ) {
                    self.names.insert(action.0.clone(), name.to_owned());
                }
            }
            "tool_call_completed" => self.tool_completed(envelope),
            "handoff_completed"
                if envelope
                    .payload
                    .get("outcome")
                    .and_then(serde_json::Value::as_str)
                    == Some("completed") =>
            {
                self.results = 0;
                self.largest.clear();
                self.names.clear();
            }
            _ => {}
        }
    }

    fn tool_completed(&mut self, envelope: &Envelope) {
        let bytes = envelope
            .payload
            .get("content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .fold(0u64, |total, text| {
                total.saturating_add(byte_count(text.len()))
            });
        self.results = self.results.saturating_add(bytes);
        let tool = envelope
            .action_id
            .as_ref()
            .and_then(|action| self.names.remove(&action.0))
            .unwrap_or_else(|| "tool".to_owned());
        self.keep_largest(Largest { tool, bytes });
    }

    fn keep_largest(&mut self, result: Largest) {
        self.largest.push(result);
        self.largest
            .sort_by_key(|largest| std::cmp::Reverse(largest.bytes));
        self.largest.truncate(LARGEST_SHOWN);
    }
}

/// A largest tool result, in text bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Largest {
    /// The tool name, or `tool` when it is unknown.
    pub(crate) tool: String,
    /// The result's text size in bytes.
    pub(crate) bytes: u64,
}

/// The categories' estimated tokens capped in display order.
pub(crate) fn categories(
    fold: &ContextFold,
    rate: Rate,
    total: u64,
) -> Option<[(Category, u64); 4]> {
    let mut remaining = total;
    let mut estimate = |bytes| {
        let tokens = rate.tokens(bytes)?.min(remaining);
        remaining = remaining.saturating_sub(tokens);
        Some(tokens)
    };
    let system = estimate(fold.system)?;
    let tools = estimate(fold.tools)?;
    let results = estimate(fold.results)?;
    Some([
        (Category::SystemPrompt, system),
        (Category::ToolDefinitions, tools),
        (Category::ToolResults, results),
        (Category::Messages, remaining),
    ])
}

/// The category fills, free cells and handoff marker.
pub(crate) fn bar(
    fill: &[(Category, u64)],
    window: u64,
    trigger: Option<u64>,
    cells: usize,
) -> String {
    if cells == 0 || window == 0 {
        return String::new();
    }
    let count = u128::try_from(cells).unwrap_or(u128::MAX);
    let denominator = u128::from(window);
    let mut symbols = vec!['░'; cells];
    let mut start = 0usize;
    let mut cumulative = 0u64;
    for (category, tokens) in fill {
        cumulative = cumulative.saturating_add(*tokens);
        let end = (u128::from(cumulative) * count / denominator)
            .min(count)
            .try_into()
            .unwrap_or(cells);
        for symbol in symbols
            .iter_mut()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            *symbol = glyph(*category);
        }
        start = end;
    }
    if let Some(trigger) = trigger {
        let marker = (u128::from(trigger) * count / denominator)
            .min(count.saturating_sub(1))
            .try_into()
            .unwrap_or(cells.saturating_sub(1));
        if let Some(symbol) = symbols.get_mut(marker) {
            *symbol = '│';
        }
    }
    symbols.into_iter().collect()
}

fn glyph(category: Category) -> char {
    match category {
        Category::SystemPrompt => '█',
        Category::ToolDefinitions => '▓',
        Category::ToolResults => '▒',
        Category::Messages => '▆',
    }
}

/// The current context size, model window and automatic handoff point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sized {
    /// The total context in tokens.
    pub(crate) total: u64,
    /// The model's context window in tokens.
    pub(crate) window: u64,
    /// The automatic handoff point in tokens, when enabled.
    pub(crate) trigger: Option<u64>,
}

/// The context rows and footer for the swapped view.
pub(crate) fn frame(
    fold: &ContextFold,
    rate: Rate,
    sized: Option<Sized>,
    list: List,
    width: u16,
) -> Frame {
    let mut rows = Vec::new();
    let mut below = Vec::new();
    if let Some(sized) = sized {
        let percent = if sized.window == 0 {
            0
        } else {
            u128::from(sized.total) * 100 / u128::from(sized.window)
        };
        rows.push(row(format!(
            "context  {} of {} tokens · {percent}%",
            about(sized.total),
            about(sized.window)
        )));
        let breakdown = categories(fold, rate, sized.total);
        let fill = breakdown.unwrap_or([
            (Category::SystemPrompt, 0),
            (Category::ToolDefinitions, 0),
            (Category::ToolResults, 0),
            (Category::Messages, sized.total),
        ]);
        rows.push(row(bar(
            &fill,
            sized.window,
            sized.trigger,
            usize::from(width),
        )));
        if breakdown.is_some() {
            for (category, tokens) in fill {
                rows.push(row(format!(
                    "{} {}  {} tokens",
                    glyph(category),
                    label(category),
                    about(tokens)
                )));
            }
            below.push(
                "Sizes are approximate: bytes at the session's own tokens-per-byte rate."
                    .to_owned(),
            );
        } else {
            below.push("Breakdown after a request without images.".to_owned());
        }
        if fold.forked {
            rows.push(row("history before the fork counts under messages"));
        }
        rows.push(row(match sized.trigger {
            Some(trigger) => format!(
                "│ handoff at {} tokens: Fiber writes a summary and the work continues in a fresh context",
                about(trigger)
            ),
            None => "automatic handoff off".to_owned(),
        }));
        rows.push(row("largest tool results"));
        if fold.largest.is_empty() {
            rows.push(row("  none since the last handoff"));
        } else {
            for result in &fold.largest {
                let size = rate.tokens(result.bytes).map_or_else(
                    || format!("{} bytes", about(result.bytes)),
                    |tokens| format!("~{} tokens", about(tokens)),
                );
                rows.push(row(format!("  {}  {size}", result.tool)));
            }
        }
    } else {
        rows.push(row(
            "The context shows after the session's first request.".to_owned()
        ));
    }
    Frame {
        title: "Context".to_owned(),
        rows,
        list,
        below,
        field: None,
        footer: "↑↓ scroll · Esc close".to_owned(),
    }
}

fn label(category: Category) -> &'static str {
    match category {
        Category::SystemPrompt => "system prompt",
        Category::ToolDefinitions => "tool definitions",
        Category::ToolResults => "tool results",
        Category::Messages => "messages",
    }
}

fn row(text: impl Into<String>) -> Vec<(String, Option<crate::swapped::Spot>)> {
    vec![(text.into(), None)]
}

fn byte_count(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "context_view_tests.rs"]
mod tests;
