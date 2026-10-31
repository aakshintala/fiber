//! The `/context` view: estimated context categories and largest tool results
//! (`docs/tui.md`, "Swapped views").

use std::collections::BTreeMap;

use contract::Envelope;
use log::Rate;

use crate::swapped::{Frame, Ink, List, Spot, about, compact};
use crate::theme::Role;

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
    /// Whether a `preamble_built` named the preamble sizes: without one
    /// — a rewind, a fork or a workspace change continues another log —
    /// a zero size is unknown, not known zero, and the view shows the
    /// total alone (`docs/tui.md`, "Swapped views").
    preamble: bool,
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
                self.preamble = false;
                self.system = 0;
                self.tools = 0;
                self.results = 0;
                self.largest.clear();
                self.names.clear();
                self.forked = envelope
                    .payload
                    .get("forked_from")
                    .is_some_and(|forked_from| !forked_from.is_null());
            }
            "preamble_built" => {
                self.preamble = true;
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

/// The categories' estimated tokens capped in display order: `None`
/// until a rate is available, and when no `preamble_built` named the
/// preamble sizes — then the view shows the total alone, since a zero
/// size without a build is unknown, not known zero.
pub(crate) fn categories(
    fold: &ContextFold,
    rate: Rate,
    total: u64,
) -> Option<[(Category, u64); 4]> {
    if !fold.preamble {
        return None;
    }
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

/// A category's colour on the bar and its rows (`docs/tui.md`,
/// "Swapped views").
fn role_of(category: Category) -> Role {
    match category {
        Category::SystemPrompt => Role::Accent,
        Category::ToolDefinitions => Role::Info,
        Category::ToolResults => Role::Error,
        Category::Messages => Role::Text,
    }
}

/// The bar as coloured runs: one █ run per non-empty category in its
/// role, ▁ in `rule` for the room left, one │ in `attention` at the
/// handoff point. The bar spans `span` tokens in `cells` cells; with
/// no handoff point the frame passes the window and no trigger, so no
/// marker draws. No run when `cells` or `span` is 0.
pub(crate) fn bar(
    fill: &[(Category, u64)],
    span: u64,
    trigger: Option<u64>,
    cells: usize,
) -> Vec<(String, Role)> {
    if cells == 0 || span == 0 {
        return Vec::new();
    }
    let count = u128::try_from(cells).unwrap_or(u128::MAX);
    let denominator = u128::from(span);
    let mut painted = vec![Role::Rule; cells];
    let mut start = 0usize;
    let mut cumulative = 0u64;
    for (category, tokens) in fill {
        if *tokens == 0 {
            continue;
        }
        cumulative = cumulative.saturating_add(*tokens);
        let mut end = (u128::from(cumulative) * count / denominator)
            .min(count)
            .try_into()
            .unwrap_or(cells);
        // A non-empty category rounds to no cell still draws one, so
        // every non-empty category keeps its run.
        if end <= start {
            end = (start + 1).min(cells);
        }
        for role in painted
            .iter_mut()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            *role = role_of(*category);
        }
        start = end;
    }
    if let Some(trigger) = trigger {
        let marker = (u128::from(trigger) * count / denominator)
            .min(count.saturating_sub(1))
            .try_into()
            .unwrap_or(cells.saturating_sub(1));
        if let Some(role) = painted.get_mut(marker) {
            *role = Role::Attention;
        }
    }
    let mut runs: Vec<(String, Role)> = Vec::new();
    for role in painted {
        let glyph = if role == Role::Attention {
            '│'
        } else if role == Role::Rule {
            '▁'
        } else {
            '█'
        };
        match runs.last_mut() {
            Some((text, last)) if *last == role => text.push(glyph),
            _ => runs.push((glyph.to_string(), role)),
        }
    }
    runs
}

/// The automatic handoff point in force: known from the session's own
/// `preamble_built`, known off when that build sets none, or unknown
/// when the session logged no `preamble_built` of its own. A rewind, a
/// fork or a workspace change continues another log, whose handoff point
/// the client never learns (`docs/tui.md`, "Swapped views").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// The handoff runs at this context size in tokens.
    At(u64),
    /// Automatic handoff is off.
    Off,
    /// No own `preamble_built` named one.
    Unknown,
}

/// The current context size, model window and automatic handoff point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sized {
    /// The total context in tokens.
    pub(crate) total: u64,
    /// The model's context window in tokens.
    pub(crate) window: u64,
    /// The automatic handoff point in force.
    pub(crate) trigger: Handoff,
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
        let mut tail = format!(" · {percent}% of the {} window", compact(sized.window));
        if let Handoff::At(point) = sized.trigger {
            tail.push_str(&format!(" · automatic handoff at {}", compact(point)));
        }
        rows.push(indented(vec![
            (
                format!("{} tokens", compact(sized.total)),
                None,
                Ink::Heading,
            ),
            (tail, None, Ink::Muted),
        ]));
        let breakdown = categories(fold, rate, sized.total);
        let fill = breakdown.unwrap_or([
            (Category::SystemPrompt, 0),
            (Category::ToolDefinitions, 0),
            (Category::ToolResults, 0),
            (Category::Messages, sized.total),
        ]);
        // The bar spans to a known handoff point, not the window; with
        // no handoff point it spans the window and draws no marker. A
        // window of 0 draws no bar.
        let trigger = match sized.trigger {
            Handoff::At(point) => Some(point),
            Handoff::Off | Handoff::Unknown => None,
        };
        let span = trigger.unwrap_or(sized.window);
        // The bar leaves the indent's two columns.
        let cells = usize::from(width).saturating_sub(2);
        let runs = if sized.window == 0 {
            Vec::new()
        } else {
            bar(&fill, span, trigger, cells)
        };
        let mut bar_row = vec![("  ".to_owned(), None, Ink::Plain)];
        bar_row.extend(
            runs.iter()
                .map(|(text, role)| (text.clone(), None, Ink::Role(*role))),
        );
        rows.push(bar_row);
        if let Handoff::At(point) = sized.trigger {
            // The label ends under the marker, in attention; the pad
            // counts the indent's two columns.
            let mut end = 0usize;
            for (text, role) in &runs {
                end += text.chars().count();
                if *role == Role::Attention {
                    break;
                }
            }
            let (pad, label) = if runs.is_empty() {
                (String::new(), String::new())
            } else {
                let label = format!("handoff {}", compact(point));
                (
                    " ".repeat(
                        end.saturating_add(2)
                            .saturating_sub(crate::format::width(&label)),
                    ),
                    label,
                )
            };
            rows.push(vec![
                (pad, None, Ink::Plain),
                (label, None, Ink::Role(Role::Attention)),
            ]);
        }
        if breakdown.is_some() {
            for (category, tokens) in fill {
                let share = if sized.total == 0 {
                    0
                } else {
                    u128::from(tokens) * 100 / u128::from(sized.total)
                };
                rows.push(indented(vec![
                    ("■ ".to_owned(), None, Ink::Role(role_of(category))),
                    (format!("{:<20}", label(category)), None, Ink::Plain),
                    (format!("~{} {}%", compact(tokens), share), None, Ink::Muted),
                ]));
            }
            below.push(
                "Sizes are approximate: bytes at the session's own tokens-per-byte rate."
                    .to_owned(),
            );
        } else {
            below.push("Breakdown after a request without images.".to_owned());
        }
        if fold.forked {
            rows.push(indented(row(
                "history before the fork counts under messages",
                Ink::Muted,
            )));
        }
        // Only a known-off handoff draws the off row: an unknown one —
        // a continuation with no `preamble_built` of its own — draws
        // neither a marker nor a row.
        if sized.trigger == Handoff::Off {
            rows.push(indented(row("automatic handoff off", Ink::Muted)));
        }
        rows.push(indented(row("Largest tool results", Ink::Heading)));
        if fold.largest.is_empty() {
            rows.push(indented(row("  none since the last handoff", Ink::Muted)));
        } else {
            for result in &fold.largest {
                let size = rate.tokens(result.bytes).map_or_else(
                    || format!("{} bytes", about(result.bytes)),
                    |tokens| format!("~{} tokens", about(tokens)),
                );
                rows.push(indented(vec![
                    ("  ".to_owned(), None, Ink::Plain),
                    (result.tool.clone(), None, Ink::Muted),
                    ("  ".to_owned(), None, Ink::Plain),
                    (size, None, Ink::Plain),
                ]));
            }
        }
    } else {
        rows.push(indented(row(
            "The context shows after the session's first request.".to_owned(),
            Ink::Muted,
        )));
    }
    Frame {
        title: "Context".to_owned(),
        command: "/context".to_owned(),
        rows,
        list,
        below,
        field: None,
        footer: String::new(),
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

fn row(text: impl Into<String>, ink: Ink) -> Vec<(String, Option<Spot>, Ink)> {
    vec![(text.into(), None, ink)]
}

/// A body row indented two columns, as the breakdown draws
/// (`docs/tui.md`, "Swapped views").
fn indented(mut row: Vec<(String, Option<Spot>, Ink)>) -> Vec<(String, Option<Spot>, Ink)> {
    row.insert(0, ("  ".to_owned(), None, Ink::Plain));
    row
}

fn byte_count(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "context_view_tests.rs"]
mod tests;
