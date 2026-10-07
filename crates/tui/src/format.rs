//! The figures a card prints: durations, token counts, money and the
//! usage behind them, the ▣ line, the kinds a group's summary line counts,
//! a call's arguments in brief, thinking headings and the prompt bubble (`docs/tui.md`, "Turns", "Tool groups and the ledger",
//! "Thinking").

use std::collections::BTreeMap;

use contract::GenerationId;
use contract::events::{CallStatus, ToolCallCompleted, TurnCompleted, TurnOutcome, UsageRecorded};
use contract::shapes::{ContentPart, Tokens, Usage};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::turn::Row;

/// A span of milliseconds, truncated to whole seconds: `38s` under a
/// minute, `4m 05s` under an hour, else `1h 02m`.
pub(crate) fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    }
}

/// A token count: exact under 1,000, then thousands and millions to one
/// decimal, rounded half up. A count that rounds to 1000.0k reads `1.0M`.
pub(crate) fn tokens(n: u64) -> String {
    if n < 1000 {
        return count(n, "token", "tokens");
    }
    let tenths_k = n.saturating_add(50) / 100;
    if tenths_k < 10_000 {
        return format!("{}.{}k tokens", tenths_k / 10, tenths_k % 10);
    }
    let tenths_m = n.saturating_add(50_000) / 100_000;
    format!("{}.{}M tokens", tenths_m / 10, tenths_m % 10)
}

/// US dollars to two decimals; an amount above zero that rounds to `$0.00`
/// reads `<$0.01`.
pub(crate) fn money(dollars: f64) -> String {
    let shown = format!("{dollars:.2}");
    if dollars > 0.0 && shown == "0.00" {
        "<$0.01".to_owned()
    } else {
        format!("${shown}")
    }
}

/// `n` and its noun, singular for one.
pub(crate) fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What a group's calls did, by kind, and how often the model thought.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Kinds {
    /// `read` calls.
    pub(crate) read: u64,
    /// Shell calls that ran `grep`, `rg` or `find`.
    pub(crate) searched: u64,
    /// Files edited: distinct paths in `changes`, else `edit` and `write`
    /// calls.
    pub(crate) edited: u64,
    /// Lines added by the edits.
    pub(crate) added: u64,
    /// Lines removed by the edits.
    pub(crate) removed: u64,
    /// Other shell calls.
    pub(crate) ran: u64,
    /// Calls to any other tool.
    pub(crate) other: u64,
    /// Thinking blocks.
    pub(crate) thoughts: u64,
}

impl Kinds {
    /// "Read 2 files, edited 1 file +3 −1, ran 1 command, thought once":
    /// kinds with no calls left out, the first letter capitalised. Empty
    /// when nothing was done.
    pub(crate) fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.read > 0 {
            parts.push(format!("read {}", count(self.read, "file", "files")));
        }
        if self.searched > 0 {
            parts.push(format!(
                "searched {}",
                count(self.searched, "pattern", "patterns")
            ));
        }
        if self.edited > 0 {
            let mut part = format!("edited {}", count(self.edited, "file", "files"));
            if self.added > 0 || self.removed > 0 {
                part.push_str(&format!(" +{} −{}", self.added, self.removed));
            }
            parts.push(part);
        }
        if self.ran > 0 {
            parts.push(format!("ran {}", count(self.ran, "command", "commands")));
        }
        if self.other > 0 {
            parts.push(count(self.other, "other call", "other calls"));
        }
        match self.thoughts {
            0 => {}
            1 => parts.push("thought once".to_owned()),
            2 => parts.push("thought twice".to_owned()),
            n => parts.push(format!("thought {n} times")),
        }
        capitalise(&parts.join(", "))
    }
}

/// `text` with its first letter upper case.
fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A line's heading text: a markdown heading or a wholly bold line, its
/// markers stripped.
fn marked(line: &str) -> Option<&str> {
    let line = line.trim();
    let text = if line.starts_with('#') {
        line.trim_start_matches('#')
    } else {
        line.strip_prefix("**")?.strip_suffix("**")?
    };
    Some(text.trim()).filter(|text| !text.is_empty())
}

/// A thinking block's first heading, or with `latest` its last one so
/// far; failing that its first non-empty line.
pub(crate) fn heading(text: &str, latest: bool) -> Option<String> {
    let found = if latest {
        text.lines().rev().find_map(marked)
    } else {
        text.lines().find_map(marked)
    };
    found.or_else(|| first_line(text)).map(str::to_owned)
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

/// How many columns `text` takes on screen.
pub(crate) fn width(text: &str) -> usize {
    Span::raw(text).width()
}

/// `text` wrapped at word boundaries into rows at most `max` columns wide;
/// a word wider than a row is broken where it reaches the edge. Each line
/// of `text` starts a row.
pub(crate) fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for word in line.split(' ') {
            let gap = usize::from(!row.is_empty());
            if !row.is_empty() && width(&row) + gap + width(word) > max {
                rows.push(std::mem::take(&mut row));
            } else if !row.is_empty() {
                row.push(' ');
            }
            for ch in word.chars() {
                let mut buf = [0u8; 4];
                let cell = width(ch.encode_utf8(&mut buf));
                if !row.is_empty() && width(&row) + cell > max {
                    rows.push(std::mem::take(&mut row));
                }
                row.push(ch);
            }
        }
        rows.push(row);
    }
    rows
}

/// What a call is, for the summary line.
pub(crate) enum Kind {
    /// `read`.
    Read,
    /// `edit` or `write`.
    Edit,
    /// A shell `grep`, `rg` or `find`.
    Search,
    /// Any other shell command.
    Ran,
    /// Any other tool.
    Other,
}

pub(crate) fn kind(name: &str, arguments: &Value) -> Kind {
    match name {
        "read" => Kind::Read,
        "edit" | "write" => Kind::Edit,
        "shell" => match argument(arguments, "command")
            .and_then(|command| command.split_whitespace().next())
        {
            Some("grep" | "rg" | "find") => Kind::Search,
            Some(_) | None => Kind::Ran,
        },
        _ => Kind::Other,
    }
}

fn argument<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}

/// A call's arguments in brief: the path a file tool names, the command a
/// shell runs, else the arguments as compact JSON, or raw text that was
/// not JSON.
pub(crate) fn summary(name: &str, arguments: &Value) -> String {
    let key = match name {
        "read" | "edit" | "write" => Some("path"),
        "shell" => Some("command"),
        _ => None,
    };
    if let Some(text) = key.and_then(|key| argument(arguments, key)) {
        return text.to_owned();
    }
    if let Value::String(raw) = arguments {
        raw.clone()
    } else {
        arguments.to_string()
    }
}

/// `name` and its arguments, or the name alone.
pub(crate) fn label(name: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {arguments}")
    }
}

/// What opening a call shows: a failed call's error, a denial's reason,
/// an edit's diff, else the content's text.
pub(crate) fn detail(done: &ToolCallCompleted) -> String {
    if let (CallStatus::Failed, Some(error)) = (done.status, &done.error) {
        return error.message.clone();
    }
    if let (CallStatus::Denied, Some(reason)) = (done.status, &done.reason) {
        return reason.clone();
    }
    if let Some(diff) = done
        .details
        .as_ref()
        .and_then(|details| details.get("diff"))
        .and_then(Value::as_str)
    {
        return diff.to_owned();
    }
    done.content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A span of milliseconds, left out under a second.
pub(crate) fn seconds(ms: u64) -> Option<String> {
    (ms >= 1000).then(|| duration(ms))
}

pub(crate) fn dim(text: String) -> Line<'static> {
    Line::styled(text, Style::default().add_modifier(Modifier::DIM))
}

/// The prompt as a tinted block on the right, at most 70% of `width`, with
/// a column of padding each side.
pub(crate) fn bubble(text: &str, columns: u16, out: &mut Vec<Row>) {
    if text.trim().is_empty() {
        return;
    }
    let max = (usize::from(columns).saturating_mul(7) / 10).max(3);
    let rows = wrap(text, max.saturating_sub(2));
    let wide = rows.iter().map(|row| width(row)).max().unwrap_or(0);
    // debt: a fixed tint until themes land (#685), when the theme's bubble
    // colour replaces it.
    let tint = Style::default().bg(Color::DarkGray);
    for row in rows {
        let pad = " ".repeat(wide.saturating_sub(width(&row)));
        let span = Span::styled(format!(" {row}{pad} "), tint);
        out.push((Line::from(span).right_aligned(), None));
    }
}

/// The latest `usage_recorded` per `generation_id`, so a copy of a copy, or
/// a correction, is still one call (`docs/events.md`, `usage_recorded`).
/// `loop` folds the same lines for its own totals; `contract` holds no
/// behaviour, so this client keeps its own fold of what it draws.
#[derive(Debug, Default)]
pub(crate) struct Spend {
    calls: BTreeMap<GenerationId, UsageRecorded>,
}

impl Spend {
    /// Keeps `line`, replacing any earlier line with its `generation_id`.
    pub(crate) fn record(&mut self, line: &UsageRecorded) {
        self.calls.insert(line.generation_id.clone(), line.clone());
    }

    /// Whether a line with `id` is kept here.
    pub(crate) fn holds(&self, id: &GenerationId) -> bool {
        self.calls.contains_key(id)
    }

    /// The docs' `usage` shape over the lines kept.
    pub(crate) fn usage(&self) -> Usage {
        let mut tokens = Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        };
        let mut billed = false;
        let mut cost = None;
        let mut subscription_cost = 0.0;
        for call in self.calls.values() {
            tokens.input = tokens.input.saturating_add(call.tokens.input);
            tokens.cache_read = tokens.cache_read.saturating_add(call.tokens.cache_read);
            tokens.output = tokens.output.saturating_add(call.tokens.output);
            for (lifetime, n) in &call.tokens.cache_write {
                let total = tokens.cache_write.entry(lifetime.clone()).or_default();
                *total = total.saturating_add(*n);
            }
            if call.subscription == Some(true) {
                subscription_cost += call.cost.unwrap_or(0.0);
            } else {
                billed = true;
                if let Some(known) = call.cost {
                    *cost.get_or_insert(0.0) += known;
                }
            }
        }
        Usage {
            tokens,
            // `docs/events.md`, `usage`: 0 with no billed call, null when
            // billed calls exist and none had a known cost.
            cost: if billed { cost } else { Some(0.0) },
            subscription_cost,
        }
    }
}

/// "▣ completed · 38s · 12 calls · 18.2k tokens · $0.41 · $1.10 on
/// subscription", each zero figure left out.
pub(crate) fn closing(done: &TurnCompleted, ms: u64, calls: u64, usage: &Usage) -> String {
    let mut parts = vec![
        match done.outcome {
            TurnOutcome::Completed => "▣ completed",
            TurnOutcome::Interrupted => "▣ interrupted",
            TurnOutcome::Failed => "▣ failed",
        }
        .to_owned(),
    ];
    if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
        parts.push(error.message.clone());
    }
    parts.extend(seconds(ms));
    if calls > 0 {
        parts.push(count(calls, "call", "calls"));
    }
    let total = usage
        .tokens
        .cache_write
        .values()
        .fold(usage.tokens.input, |sum, n| sum.saturating_add(*n))
        .saturating_add(usage.tokens.cache_read)
        .saturating_add(usage.tokens.output);
    if total > 0 {
        parts.push(tokens(total));
    }
    if let Some(cost) = usage.cost.filter(|cost| *cost > 0.0) {
        parts.push(money(cost));
    }
    if usage.subscription_cost > 0.0 {
        parts.push(format!(
            "{} on subscription",
            money(usage.subscription_cost)
        ));
    }
    parts.join(" · ")
}

/// "+ Thought: Plan the fix · 22s" after `gutter`, dim; the span left
/// out when unknown or under a second.
pub(crate) fn thought(gutter: &str, text: &str, span: Option<u64>) -> Line<'static> {
    let mut line = format!("{gutter}+ Thought");
    if let Some(found) = heading(text, false) {
        line.push_str(&format!(": {found}"));
    }
    if let Some(span) = span.and_then(seconds) {
        line.push_str(&format!(" · {span}"));
    }
    dim(line)
}

/// An opened item's text under it, a dim line each.
pub(crate) fn opened(text: &str, indent: &str, out: &mut Vec<Row>) {
    for line in text.split('\n') {
        out.push((dim(format!("{indent}{line}")), None));
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
