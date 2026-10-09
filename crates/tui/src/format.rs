//! The figures a card prints: durations, token counts, money and the
//! usage behind them, the ▣ line, a tool group's summary line and ledger,
//! the kinds a summary line counts, a call's arguments in brief and
//! thinking headings (`docs/tui.md`, "Turns", "Tool groups and
//! the ledger", "Thinking").

use std::collections::BTreeMap;

use contract::ErrorCode;
use contract::GenerationId;
use contract::events::{CallStatus, RetryScheduled, ToolCallCompleted, UsageRecorded};
use contract::shapes::{ContentPart, Failure, Tokens, Usage};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::app::Target;
use crate::markdown::style;
use crate::rows::{Join, Rows};
use crate::theme::Role;
use crate::turn::{Group, Thought, target_id};

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
    /// Questions `ask_user` asked.
    pub(crate) asked: u64,
    /// Calls to any other tool.
    pub(crate) other: u64,
    /// Thinking blocks.
    pub(crate) thoughts: u64,
}

impl Kinds {
    /// "Read 2 files, edited 1 file +3 −1, ran 1 command, asked 2
    /// questions, thought once":
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
        if self.asked > 0 {
            parts.push(format!(
                "asked {}",
                count(self.asked, "question", "questions")
            ));
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

/// `text` cut to at most `max` columns.
pub(crate) fn cut(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        if width(&out) + width(ch.encode_utf8(&mut buf)) > max {
            break;
        }
        out.push(ch);
    }
    out
}

/// `text` wrapped at word boundaries into rows at most `max` columns wide;
/// a word wider than a row is broken where it reaches the edge. Each line
/// of `text` starts a row.
pub(crate) fn wrap(text: &str, max: usize) -> Vec<String> {
    wrap_joined(text, max)
        .into_iter()
        .map(|(row, _)| row)
        .collect()
}

/// [`wrap`]'s rows, each with how it joins the row before: a row a line of
/// `text` starts is a break, one broken at a space joins with that space,
/// and one broken inside a word joins with nothing.
pub(crate) fn wrap_joined(text: &str, max: usize) -> Vec<(String, Join)> {
    let max = max.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut join = Join::Break;
        for word in line.split(' ') {
            // The space before the word counts only after another word.
            if !row.is_empty() && width(&row) + 1 + width(word) > max {
                rows.push((std::mem::take(&mut row), join));
                join = Join::WrapSpace;
            } else if !row.is_empty() {
                row.push(' ');
            }
            for ch in word.chars() {
                let mut buf = [0u8; 4];
                let cell = width(ch.encode_utf8(&mut buf));
                if !row.is_empty() && width(&row) + cell > max {
                    rows.push((std::mem::take(&mut row), join));
                    join = Join::Wrap;
                }
                row.push(ch);
            }
        }
        rows.push((row, join));
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
    /// `ask_user`, with how many questions it asked.
    Ask(u64),
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
        // A call whose `questions` is not a list of at least one is no
        // question asked.
        "ask_user" => arguments
            .get("questions")
            .and_then(Value::as_array)
            .filter(|questions| !questions.is_empty())
            .map_or(Kind::Other, |questions| Kind::Ask(questions.len() as u64)),
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

/// The latest `usage_recorded` per `generation_id`, so a copy of a copy, or
/// a correction, is still one call (`docs/events.md`, `usage_recorded`).
/// `loop` folds the same lines for its own totals; `contract` holds no
/// behaviour, so this client keeps its own fold of what it draws.
#[derive(Debug, Clone, Default)]
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
/// subscription" after `head`, each zero figure left out.
pub(crate) fn closing(head: &str, ms: u64, calls: u64, usage: &Usage) -> String {
    let mut parts = vec![head.to_owned()];
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

/// An error code as the log spells it.
pub(crate) fn code(code: &ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A failed turn's lines before its ▣ line: "✗ <message> · <code>", then,
/// when the provider said something, its words dim under it. A failed
/// login offers "log in" on the ✗ line.
pub(crate) fn failure(error: &Failure, out: &mut Rows) {
    // A failed login offers the login view (`docs/tui.md`, "Turns").
    let login = (error.code == ErrorCode::AuthenticationFailed).then_some(Target::Login);
    let line = format!("✗ {} · {}", error.message, code(&error.code));
    out.push((Line::raw(line), login));
    if let Some(provider) = &error.provider {
        let said = match provider.status {
            Some(status) => format!(
                "{name} said HTTP {status}: “{message}”",
                name = provider.name,
                message = provider.message
            ),
            None => format!(
                "{name} said: “{message}”",
                name = provider.name,
                message = provider.message
            ),
        };
        out.push((dim(said), None));
    }
}

/// "↻ Retrying in 4s · rate_limited · attempt 2 of 4": the wait rounded up
/// to whole seconds in `secs`. `attempt` is the one about to be made, of the
/// `last_attempt` the retry policy allows.
pub(crate) fn retry(retry: &RetryScheduled, attempt: u32, secs: u64) -> Line<'static> {
    Line::raw(format!(
        "↻ Retrying in {secs}s · {} · attempt {} of {}",
        code(&retry.code),
        attempt,
        retry.last_attempt
    ))
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
pub(crate) fn opened(text: &str, indent: &str, out: &mut Rows) {
    for line in text.split('\n') {
        out.push((dim(format!("{indent}{line}")), None));
    }
}

/// The diff of a call that changed a file, as [`opened`] draws text, each
/// line added or removed in the added or removed colour.
fn opened_diff(text: &str, indent: &str, out: &mut Rows) {
    for line in text.split('\n') {
        let role = if line.starts_with('+') {
            Some(Role::Added)
        } else if line.starts_with('-') {
            Some(Role::Removed)
        } else {
            None
        };
        let mut row = dim(format!("{indent}{line}"));
        if let Some(role) = role {
            row = row.patch_style(style(role));
        }
        out.push((row, None));
    }
}

impl Group {
    /// What its calls did, by kind, and how often the model thought.
    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds::default();
        let mut paths = Vec::new();
        let mut edits = 0u64;
        for call in self.calls() {
            match kind(&call.name, &call.arguments) {
                Kind::Read => kinds.read = kinds.read.saturating_add(1),
                Kind::Search => kinds.searched = kinds.searched.saturating_add(1),
                Kind::Ran => kinds.ran = kinds.ran.saturating_add(1),
                Kind::Ask(questions) => kinds.asked = kinds.asked.saturating_add(questions),
                Kind::Other => kinds.other = kinds.other.saturating_add(1),
                Kind::Edit => {
                    edits = edits.saturating_add(1);
                    for change in &call.changes {
                        kinds.added = kinds.added.saturating_add(change.added);
                        kinds.removed = kinds.removed.saturating_add(change.removed);
                        if !paths.contains(&&change.path) {
                            paths.push(&change.path);
                        }
                    }
                }
            }
        }
        kinds.edited = if paths.is_empty() {
            edits
        } else {
            paths.len() as u64
        };
        kinds.thoughts = self.thoughts().count() as u64;
        kinds
    }

    /// Its lines. A finished group with no call is its thinking, one line
    /// a block; otherwise a summary line, and the ledger when open.
    pub(crate) fn rows(&self, running: bool, out: &mut Rows) {
        if !running && !self.has_ledger() {
            for thought in self.thoughts() {
                thought_rows(thought, "", out);
            }
            return;
        }
        let mut parts = Vec::new();
        let kinds = self.kinds().summary();
        if !kinds.is_empty() {
            parts.push(kinds);
        }
        let failed = self.failed() as u64;
        if failed > 0 {
            parts.push(count(failed, "failed model call", "failed model calls"));
        }
        if running {
            let flight: Vec<String> = self
                .calls()
                .filter(|call| call.status.is_none())
                .map(|call| label(&call.name, &summary(&call.name, &call.arguments)))
                .chain(
                    self.streaming
                        .iter()
                        .map(|call| label(call.name.as_deref().unwrap_or("…"), &call.text)),
                )
                .collect();
            if !flight.is_empty() {
                parts.push(flight.join(", "));
            }
            if let Some(thought) = self.thoughts().filter(|t| t.ended.is_none()).last() {
                parts.push(match heading(&thought.text, true) {
                    Some(heading) => format!("Thinking: {heading}"),
                    None => "Thinking".to_owned(),
                });
            }
        } else {
            parts.extend(seconds(self.last.saturating_sub(self.first)));
        }
        if parts.is_empty() {
            return;
        }
        // A running summary line carries its spinner's cell: the draw
        // site spins it on the working line's tick, so keyed and keyless
        // running groups both move (`docs/tui.md`, "The working line").
        let text = if running {
            RowText {
                spin: Some(0),
                ..RowText::plain()
            }
        } else {
            RowText::plain()
        };
        out.push_text(
            (
                dim(format!("• {}", parts.join(" · "))),
                self.key.as_deref().map(|key| Target::Group(target_id(key))),
            ),
            text,
        );
        // An open approval shows its call whatever the group's own state.
        // A group with no key has no target, and draws as it is on a
        // search draw too: a match there is one nothing could reveal
        // (`docs/tui.md`, "Search").
        let draws = self.open || self.calls().any(|call| call.asking);
        match self.key.as_deref().map(|key| Target::Group(target_id(key))) {
            Some(target) => {
                if out.open_scope(target, draws) {
                    self.ledger(out);
                }
                out.end_scope();
            }
            None => {
                if draws {
                    self.ledger(out);
                }
            }
        }
    }

    /// One row per call, split by step: the step's number in the gutter on
    /// its first row, the model calls that failed first, then its thinking.
    fn ledger(&self, out: &mut Rows) {
        for section in &self.sections {
            let mut gutter = format!("{:>3} ", section.step);
            for (code, attempt) in &section.failed {
                let gutter = std::mem::replace(&mut gutter, GAP.to_owned());
                let row = format!("{gutter}model call failed · {code} · attempt {attempt}");
                out.push((dim(row), None));
            }
            for thought in &section.thoughts {
                thought_rows(
                    thought,
                    &std::mem::replace(&mut gutter, GAP.to_owned()),
                    out,
                );
            }
            for call in &section.calls {
                let mut row = format!(
                    "{}{}",
                    std::mem::replace(&mut gutter, GAP.to_owned()),
                    label(&call.name, &summary(&call.name, &call.arguments))
                );
                let (added, removed) = call.changes.iter().fold((0u64, 0u64), |(a, r), c| {
                    (a.saturating_add(c.added), r.saturating_add(c.removed))
                });
                // The person's answer to a form replaces the status.
                let status = match call.status {
                    None if self.cut && call.started => " · ? may have run; not run again",
                    None => " · running",
                    Some(CallStatus::Completed) => "",
                    Some(CallStatus::Failed) => " · failed",
                    Some(CallStatus::Denied) => " · denied",
                    Some(CallStatus::Cancelled) => " · cancelled",
                };
                let status = call
                    .asked
                    .as_ref()
                    .and_then(|asked| asked.suffix())
                    .unwrap_or(status);
                // A call that changed a file stands out: bold, its counts in
                // the added and removed colours (`docs/tui.md`, "Tool groups
                // and the ledger").
                let line = if call.changes.is_empty() {
                    row.push_str(status);
                    dim(row)
                } else {
                    Line::from(vec![
                        Span::raw(row),
                        Span::styled(format!(" +{added}"), style(Role::Added)),
                        Span::styled(format!(" −{removed}"), style(Role::Removed)),
                        Span::raw(status.to_owned()),
                    ])
                    .style(Style::default().add_modifier(Modifier::BOLD))
                };
                out.push((line, Some(Target::Call(call.id))));
                if out.open_scope(Target::Call(call.id), call.open) {
                    if call.changes.is_empty() {
                        opened(&call.detail, GAP, out);
                    } else {
                        opened_diff(&call.detail, GAP, out);
                    }
                }
                out.end_scope();
            }
        }
    }
}

/// The gutter left blank.
const GAP: &str = "    ";

/// A thought's line after `gutter`, and its text when open.
fn thought_rows(thought: &Thought, gutter: &str, out: &mut Rows) {
    let span = thought
        .ended
        .map(|ended| ended.saturating_sub(thought.started));
    let line = self::thought(gutter, &thought.text, span);
    out.push((line, Some(Target::Thought(thought.id))));
    if out.open_scope(Target::Thought(thought.id), thought.open) {
        opened(&thought.text, &" ".repeat(gutter.len()), out);
    }
    out.end_scope();
}

/// One question's answer as `ask_user`'s result writes it (`docs/tools.md`,
/// "The result"): `<header>: skipped` when `said` is `None`, else the chosen
/// labels and then the typed text, joined by `, `. A header or label keeps
/// JSON's escapes without quotes and the text is a JSON string, so the row
/// stays one line.
pub(crate) fn answer_row(header: &str, said: Option<(&[String], Option<&str>)>) -> String {
    let said = match said {
        None => "skipped".to_owned(),
        Some((labels, text)) => {
            let mut parts: Vec<String> = labels.iter().map(|label| escaped(label)).collect();
            parts.extend(text.map(quoted));
            parts.join(", ")
        }
    };
    format!("{}: {said}", escaped(header))
}

/// The note on a whole form, as `ask_user`'s result writes it: `note: `
/// and the note as a JSON string (`docs/tools.md`, "The result").
pub(crate) fn note_row(note: &str) -> String {
    format!("note: {}", quoted(note))
}

/// `text` as a JSON string, quotes included.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// `text` with JSON's escapes but without the quotes around it.
fn escaped(text: &str) -> String {
    let quoted = quoted(text);
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_owned()
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
