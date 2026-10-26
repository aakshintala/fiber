//! The figures a card prints: durations, token counts, money and the
//! usage behind them, the ▣ line, the kinds a summary line counts,
//! a call's arguments in brief and thinking headings (`docs/tui.md`,
//! "Turns", "Tool groups and the ledger", "Thinking"). A tool
//! group's summary line and ledger are drawn in `crate::turn::group`.

use std::collections::BTreeMap;

use contract::ErrorCode;
use contract::GenerationId;
use contract::events::{CallStatus, RetryScheduled, ToolCallCompleted, UsageRecorded};
use contract::shapes::{ContentPart, Failure, Usage};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::app::Target;
use crate::markdown::style;
use crate::rows::{Join, Rows};
use crate::theme::Role;

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

/// Billed spend against a budget (`docs/loop.md`, "Spending budget"): a
/// set budget draws `"X of Y"`, a cost still `null` reading `"unknown"`.
/// With no budget there is no comparison to draw, so `None`: each caller
/// keeps its own no-budget fallback.
pub(crate) fn spend_against_budget(cost: Option<f64>, budget: Option<f64>) -> Option<String> {
    let budget = budget?;
    Some(match cost {
        Some(cost) => format!("{} of {}", money(cost), money(budget)),
        None => format!("unknown of {}", money(budget)),
    })
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
/// Read, edited and written files count distinct `path` arguments per
/// kind; kinds with no calls are left out of the summary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Kinds {
    /// `read` calls, distinct paths.
    pub(crate) read: u64,
    /// Shell calls that ran `grep` or `rg`.
    pub(crate) searched: u64,
    /// Shell calls that ran `find`, `ls` or `fd`.
    pub(crate) listed: u64,
    /// `edit` calls, distinct paths.
    pub(crate) edited: u64,
    /// `write` calls, distinct paths.
    pub(crate) wrote: u64,
    /// Lines added by the `edit` calls.
    pub(crate) added: u64,
    /// Lines removed by the `edit` calls.
    pub(crate) removed: u64,
    /// Other shell calls.
    pub(crate) ran: u64,
    /// `delegate_spawn` and `delegate_fork` calls.
    pub(crate) delegates: u64,
    /// `jobs` calls.
    pub(crate) jobs: u64,
    /// Questions `ask_user` asked.
    pub(crate) asked: u64,
    /// Calls to any other tool.
    pub(crate) called: u64,
    /// Thinking blocks.
    pub(crate) thoughts: u64,
}

/// A dim span.
fn dim_span(text: String) -> Span<'static> {
    Span::styled(text, Style::default().add_modifier(Modifier::DIM))
}

impl Kinds {
    /// The kinds in the doc's fixed order, each dim, with the edit's
    /// totals in the added and removed colours: "Read 2 files, searched
    /// 1 pattern, edited 1 file +2 −1, ran 1 command, thought 4 times".
    /// Kinds with no calls are left out; the first letter is capitalised.
    /// Empty when nothing was done.
    pub(crate) fn summary(&self) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        let mut first = true;
        let push = |out: &mut Vec<Span<'static>>, first: &mut bool, text: String| {
            let mut text = text;
            if *first {
                text = capitalise(&text);
                *first = false;
            } else {
                out.push(dim_span(", ".to_owned()));
            }
            out.push(dim_span(text));
        };
        if self.read > 0 {
            push(
                &mut out,
                &mut first,
                format!("read {}", count(self.read, "file", "files")),
            );
        }
        if self.searched > 0 {
            push(
                &mut out,
                &mut first,
                format!("searched {}", count(self.searched, "pattern", "patterns")),
            );
        }
        if self.listed > 0 {
            push(
                &mut out,
                &mut first,
                format!("listed {}", count(self.listed, "directory", "directories")),
            );
        }
        if self.edited > 0 {
            push(
                &mut out,
                &mut first,
                format!("edited {}", count(self.edited, "file", "files")),
            );
            if self.added > 0 || self.removed > 0 {
                out.push(dim_span(" ".to_owned()));
                out.push(Span::styled(
                    format!("+{}", self.added),
                    style(Role::Added).add_modifier(Modifier::DIM),
                ));
                out.push(dim_span(" ".to_owned()));
                out.push(Span::styled(
                    format!("−{}", self.removed),
                    style(Role::Removed).add_modifier(Modifier::DIM),
                ));
            }
        }
        if self.wrote > 0 {
            push(
                &mut out,
                &mut first,
                format!("wrote {}", count(self.wrote, "file", "files")),
            );
        }
        if self.ran > 0 {
            push(
                &mut out,
                &mut first,
                format!("ran {}", count(self.ran, "command", "commands")),
            );
        }
        if self.delegates > 0 {
            push(
                &mut out,
                &mut first,
                format!("started {}", count(self.delegates, "delegate", "delegates")),
            );
        }
        if self.jobs > 0 {
            push(
                &mut out,
                &mut first,
                format!("checked {}", count(self.jobs, "job", "jobs")),
            );
        }
        if self.asked > 0 {
            push(
                &mut out,
                &mut first,
                format!("asked {}", count(self.asked, "question", "questions")),
            );
        }
        if self.called > 0 {
            push(
                &mut out,
                &mut first,
                format!("called {}", count(self.called, "tool", "tools")),
            );
        }
        match self.thoughts {
            0 => {}
            1 => push(&mut out, &mut first, "thought once".to_owned()),
            n => push(&mut out, &mut first, format!("thought {n} times")),
        }
        out
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

pub(crate) use crate::text::width;

/// `text` cut to at most `max` columns.
pub(crate) fn cut(text: &str, max: usize) -> String {
    crate::text::cut(text, max, "")
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
        let mut taken = 0usize;
        let mut join = Join::Break;
        // Words split only at a standalone `" "` grapheme, so a
        // grapheme containing a space (such as `" \u{301}"`) stays
        // inside its word instead of being torn apart.
        let mut words: Vec<String> = vec![String::new()];
        crate::text::each_piece(line, |piece, _| {
            if piece == " " {
                words.push(String::new());
            } else if let Some(word) = words.last_mut() {
                word.push_str(piece);
            }
            true
        });
        for word in &words {
            // Each word's pieces are measured once here; the row width
            // is carried, so no growing string is ever remeasured. The
            // pieces are kept as byte ranges into the word, since the
            // borrow a piece carries cannot be stored.
            let mut pieces: Vec<(usize, usize, usize)> = Vec::new();
            let mut word_width = 0usize;
            crate::text::each_piece(word, |piece, at| {
                let cell = crate::text::measured(piece);
                word_width = word_width.saturating_add(cell);
                pieces.push((at, piece.len(), cell));
                true
            });
            // The space before the word counts only after another word.
            if !row.is_empty() && taken.saturating_add(1).saturating_add(word_width) > max {
                rows.push((std::mem::take(&mut row), join));
                taken = 0;
                join = Join::WrapSpace;
            } else if !row.is_empty() {
                row.push(' ');
                taken = taken.saturating_add(1);
            }
            for (at, len, cell) in pieces {
                if !row.is_empty() && taken.saturating_add(cell) > max {
                    rows.push((std::mem::take(&mut row), join));
                    taken = 0;
                    join = Join::Wrap;
                }
                row.push_str(&word[at..at.saturating_add(len)]);
                taken = taken.saturating_add(cell);
            }
        }
        rows.push((row, join));
    }
    rows
}

/// What a call is, for the summary line and the ledger's kind word.
/// `find`, `ls` and `fd` list directories; only `grep` and `rg`
/// search (`research/tui-prototype`, `kind_of`).
pub(crate) enum Kind {
    /// `read`.
    Read,
    /// A shell `grep` or `rg`.
    Search,
    /// A shell `find`, `ls` or `fd`.
    List,
    /// `edit`.
    Edit,
    /// `write`.
    Write,
    /// Any other shell command.
    Shell,
    /// `delegate_spawn` or `delegate_fork`.
    Delegate,
    /// `jobs`.
    Jobs,
    /// `ask_user`, with how many questions it asked.
    Ask(u64),
    /// Any other tool.
    Other,
}

impl Kind {
    /// The ledger's kind word: the call's kind in `info`, padded to nine
    /// columns by the draw site (`docs/tui.md`, "Tool groups and the
    /// ledger").
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Kind::Read => "read",
            Kind::Search => "search",
            Kind::List => "list",
            Kind::Edit => "edit",
            Kind::Write => "write",
            Kind::Shell => "shell",
            Kind::Delegate => "delegate",
            Kind::Jobs => "jobs",
            Kind::Ask(_) => "ask",
            Kind::Other => "other",
        }
    }
}

pub(crate) fn kind(name: &str, arguments: &Value) -> Kind {
    match name {
        "read" => Kind::Read,
        "edit" => Kind::Edit,
        "write" => Kind::Write,
        "delegate_spawn" | "delegate_fork" => Kind::Delegate,
        "jobs" => Kind::Jobs,
        "shell" => match argument(arguments, "command")
            .and_then(|command| command.split_whitespace().next())
        {
            Some("grep" | "rg") => Kind::Search,
            Some("find" | "ls" | "fd") => Kind::List,
            Some(_) | None => Kind::Shell,
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
/// shell runs, an `ask_user` call's question count, else the first useful
/// string field, a short count of an array field, or nothing. Never JSON:
/// the running line already holds the raw text until the call is requested
/// (`docs/tui.md`, "Tool groups and the ledger").
pub(crate) fn summary(name: &str, arguments: &Value) -> String {
    if let Some(text) = match name {
        "read" | "edit" | "write" => argument(arguments, "path"),
        "shell" => argument(arguments, "command"),
        _ => None,
    } {
        return text.to_owned();
    }
    if let Kind::Ask(questions) = kind(name, arguments) {
        return count(questions, "question", "questions");
    }
    if let Value::Object(arguments) = arguments {
        // The first useful string field, in the order tools take them;
        // an empty value names nothing, so it is skipped.
        for key in [
            "query", "url", "pattern", "name", "text", "title", "session", "id",
        ] {
            if let Some(text) = arguments.get(key).and_then(Value::as_str) {
                let text = oneline(text);
                if !text.is_empty() {
                    return text;
                }
            }
        }
        // Else a short count of the first array field with anything in it.
        for (key, value) in arguments {
            if let Some(list) = value.as_array().filter(|list| !list.is_empty()) {
                return format!("{} {key}", list.len());
            }
        }
    }
    String::new()
}

/// `text` on one line: every run of whitespace, newlines included, reads
/// as one space, with nothing left at either end.
fn oneline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `name` and its arguments, or the name alone.
pub(crate) fn label(name: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {arguments}")
    }
}

/// A failed call's "<code>: <message>", a denial's reason, else empty:
/// the first row an opened call draws (`docs/tui.md`, "Tool groups and
/// the ledger").
pub(crate) fn headline(done: &ToolCallCompleted) -> String {
    if let (CallStatus::Failed, Some(error)) = (done.status, &done.error) {
        return format!("{}: {}", code(&error.code), error.message);
    }
    if let (CallStatus::Denied, Some(reason)) = (done.status, &done.reason) {
        return reason.clone();
    }
    String::new()
}

/// The result's text parts joined with "\n": images, PDFs and unknown
/// parts are not text and stay out.
pub(crate) fn content_text(done: &ToolCallCompleted) -> String {
    done.content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
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
        log::usage(self.calls.values())
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

/// One answered row's parts for the panel and the card: the header
/// padded to 13 display cells, at least one trailing space, and the
/// answer as `ask_user`'s result writes it (`docs/tui.md`, "A question
/// form").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnswerParts {
    /// The header escaped and padded to 13 display cells, at least one
    /// trailing space; a wider header keeps one space after it.
    pub(crate) header: String,
    /// The chosen labels then the quoted words, or "skipped".
    pub(crate) text: String,
    /// Whether the question was skipped.
    pub(crate) skipped: bool,
}

/// [`AnswerParts`] for `header` and `said`: the padded header for the
/// panel and the card, the answer text as the result writes it.
pub(crate) fn answer_parts(header: &str, said: Option<(&[String], Option<&str>)>) -> AnswerParts {
    let header = escaped(header);
    let pad = 13usize.saturating_sub(width(&header)).max(1);
    let (text, skipped) = said_text(said);
    AnswerParts {
        header: format!("{header}{}", " ".repeat(pad)),
        text,
        skipped,
    }
}

/// The answer text as `ask_user`'s result writes it and whether it is a
/// skip: the chosen labels and then the typed text, joined by `, `. A
/// header or label keeps JSON's escapes without quotes and the text is a
/// JSON string, so the row stays one line.
fn said_text(said: Option<(&[String], Option<&str>)>) -> (String, bool) {
    match said {
        None => ("skipped".to_owned(), true),
        Some((labels, text)) => {
            let mut parts: Vec<String> = labels.iter().map(|label| escaped(label)).collect();
            parts.extend(text.map(quoted));
            (parts.join(", "), false)
        }
    }
}

/// `text` as a JSON string, quotes included: the note on a whole form
/// draws it bold in the turn's card.
pub(crate) fn quoted(text: &str) -> String {
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
