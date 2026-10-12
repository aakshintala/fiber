//! A tool group: everything between two pieces of assistant text, by step,
//! with its thinking blocks, calls, failed model calls and the calls still
//! streaming (`docs/tui.md`, "Tool groups and the ledger"). This module
//! draws it: its summary line and ledger.

use contract::events::{CallStatus, FileChange};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::target_id;
use crate::app::Target;
use crate::format::{Kind, Kinds, count, dim, heading, kind, label, opened, summary};
use crate::markdown::style;
use crate::rows::{RowText, Rows};
use crate::theme::Role;

/// Everything between two pieces of assistant text.
#[derive(Debug, Clone, Default)]
pub(crate) struct Group {
    /// The first call's or thought's action, used as a stable target across reloads.
    pub(crate) key: Option<String>,
    /// Whether its ledger is open.
    pub(crate) open: bool,
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) sections: Vec<Section>,
    /// Calls the model is still emitting.
    pub(crate) streaming: Vec<Streaming>,
    /// Its turn was cut short.
    pub(crate) cut: bool,
}

/// One step's part of a group.
#[derive(Debug, Clone, Default)]
pub(crate) struct Section {
    pub(crate) step: u64,
    pub(crate) thoughts: Vec<Thought>,
    pub(crate) calls: Vec<Call>,
    /// Model calls that failed and were retried: code and attempt.
    pub(crate) failed: Vec<(String, u32)>,
}

/// One thinking block.
#[derive(Debug, Clone, Default)]
pub(crate) struct Thought {
    pub(crate) id: usize,
    pub(crate) action: String,
    pub(crate) text: String,
    pub(crate) started: u64,
    pub(crate) ended: Option<u64>,
    pub(crate) open: bool,
}

/// One tool call.
#[derive(Debug, Clone, Default)]
pub(crate) struct Call {
    pub(crate) id: usize,
    pub(crate) action: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
    /// How it ended; `None` while it runs.
    pub(crate) status: Option<CallStatus>,
    pub(crate) changes: Vec<FileChange>,
    /// What opening the call shows.
    pub(crate) detail: String,
    /// The process's exit code, when it ran one
    /// (`tool_call_completed`'s `process.exit_code`).
    pub(crate) exit: Option<i32>,
    /// How many lines the result's text parts hold, a trailing newline
    /// adding none (`str::lines`).
    pub(crate) lines: u64,
    /// The image parts of its result: each draws as its one line under
    /// its ledger row (`docs/tui.md`, "Images").
    pub(crate) images: Vec<crate::image::Part>,
    pub(crate) open: bool,
    /// A `permission_requested` for it is open.
    pub(crate) asking: bool,
    /// It had `tool_call_started`.
    pub(crate) started: bool,
    /// The question form it raised.
    pub(crate) asked: Option<super::answers::Asked>,
}

/// A call still streaming: its message, position, name and raw text.
#[derive(Debug, Clone)]
pub(crate) struct Streaming {
    pub(crate) message: String,
    pub(crate) index: u32,
    pub(crate) name: Option<String>,
    pub(crate) text: String,
}

impl Group {
    /// Widens the group's span to `ts`.
    pub(in crate::turn) fn touch(&mut self, ts: u64) {
        self.first = self.first.min(ts);
        self.last = self.last.max(ts);
    }

    /// [`Self::touch`], as a condition that holds.
    pub(in crate::turn) fn touched(&mut self, ts: u64) -> bool {
        self.touch(ts);
        true
    }

    /// The section for `step`, the last one when it is that step's.
    pub(in crate::turn) fn section(&mut self, step: u64) -> &mut Section {
        if self.sections.last().is_none_or(|last| last.step != step) {
            self.sections.push(Section {
                step,
                ..Section::default()
            });
        }
        let at = self.sections.len().saturating_sub(1);
        #[expect(clippy::indexing_slicing, reason = "a section was pushed above")]
        &mut self.sections[at]
    }

    pub(in crate::turn) fn thought(&mut self, action: &str) -> Option<&mut Thought> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.thoughts.iter_mut())
            .find(|thought| thought.action == action)
    }

    pub(in crate::turn) fn call(&mut self, action: &str) -> Option<&mut Call> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.calls.iter_mut())
            .find(|call| call.action == action)
    }

    pub(crate) fn calls(&self) -> impl Iterator<Item = &Call> {
        self.sections
            .iter()
            .flat_map(|section| section.calls.iter())
    }

    pub(crate) fn thoughts(&self) -> impl Iterator<Item = &Thought> {
        self.sections
            .iter()
            .flat_map(|section| section.thoughts.iter())
    }

    pub(in crate::turn) fn set_open(&mut self, target: &Target, open: bool) -> bool {
        let flag = match target {
            Target::Group(id) => self
                .key
                .as_deref()
                .is_some_and(|key| target_id(key) == *id)
                .then_some(&mut self.open),
            Target::Thought(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.thoughts.iter_mut())
                .find(|thought| thought.id == *id)
                .map(|thought| &mut thought.open),
            Target::Call(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.calls.iter_mut())
                .find(|call| call.id == *id)
                .map(|call| &mut call.open),
            Target::Login
            | Target::Image(_)
            | Target::Note(_)
            | Target::Orphans(_)
            | Target::Copy { .. } => None,
        };
        flag.map(|flag| *flag = open).is_some()
    }

    /// Whether the group holds calls or failed model calls, and so a
    /// ledger.
    pub(crate) fn has_ledger(&self) -> bool {
        self.calls().next().is_some() || self.failed() > 0
    }

    /// How many model calls failed in the group.
    pub(crate) fn failed(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.failed.len())
            .sum()
    }
}

/// The diff of a call that changed a file, as [`opened`] draws text, each
/// line added or removed in the added or removed colour.
/// An opened call's output, error or diff under its row, indented eight
/// columns: the first 20 logical lines, then "… N more lines" dim, or
/// "no output" when empty. Diff lines keep their added and removed
/// colours. The cap counts logical lines, not wrapped rows: a long line
/// still wraps through `Rows::pad` (`docs/tui.md`, "Tool groups and the
/// ledger").
const OPENED_LINES: usize = 20;

fn opened_call(text: &str, diff: bool, out: &mut Rows) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        out.push((dim(format!("{INDENT}no output")), None));
        return;
    }
    for line in lines.iter().take(OPENED_LINES) {
        let mut row = dim(format!("{INDENT}{line}"));
        if diff {
            let role = if line.starts_with('+') {
                Some(Role::Added)
            } else if line.starts_with('-') {
                Some(Role::Removed)
            } else {
                None
            };
            if let Some(role) = role {
                row = row.patch_style(style(role));
            }
        }
        out.push((row, None));
    }
    let more = lines.len().saturating_sub(OPENED_LINES);
    if more > 0 {
        let more = u64::try_from(more).unwrap_or(u64::MAX);
        out.push((
            dim(format!(
                "{INDENT}… {}",
                count(more, "more line", "more lines")
            )),
            None,
        ));
    }
}
impl Group {
    /// What its calls did, by kind, and how often the model thought.
    /// Read, edited and written files count distinct `path` argument
    /// strings per kind; a call with no string `path` counts as one
    /// file of its own. The added and removed totals sum the `changes`
    /// of the `edit` calls.
    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds::default();
        let mut read_paths: Vec<&str> = Vec::new();
        let mut read_missing = 0u64;
        let mut edit_paths: Vec<&str> = Vec::new();
        let mut edit_missing = 0u64;
        let mut write_paths: Vec<&str> = Vec::new();
        let mut write_missing = 0u64;
        for call in self.calls() {
            match kind(&call.name, &call.arguments) {
                Kind::Read => match call.arguments.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        if !read_paths.contains(&path) {
                            read_paths.push(path);
                        }
                    }
                    None => read_missing = read_missing.saturating_add(1),
                },
                Kind::Search => kinds.searched = kinds.searched.saturating_add(1),
                Kind::List => kinds.listed = kinds.listed.saturating_add(1),
                Kind::Shell => kinds.ran = kinds.ran.saturating_add(1),
                Kind::Delegate => kinds.delegates = kinds.delegates.saturating_add(1),
                Kind::Jobs => kinds.jobs = kinds.jobs.saturating_add(1),
                Kind::Ask(questions) => kinds.asked = kinds.asked.saturating_add(questions),
                Kind::Other => kinds.called = kinds.called.saturating_add(1),
                Kind::Edit => {
                    match call.arguments.get("path").and_then(Value::as_str) {
                        Some(path) => {
                            if !edit_paths.contains(&path) {
                                edit_paths.push(path);
                            }
                        }
                        None => edit_missing = edit_missing.saturating_add(1),
                    }
                    for change in &call.changes {
                        kinds.added = kinds.added.saturating_add(change.added);
                        kinds.removed = kinds.removed.saturating_add(change.removed);
                    }
                }
                Kind::Write => match call.arguments.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        if !write_paths.contains(&path) {
                            write_paths.push(path);
                        }
                    }
                    None => write_missing = write_missing.saturating_add(1),
                },
            }
        }
        kinds.read = read_paths.len() as u64 + read_missing;
        kinds.edited = edit_paths.len() as u64 + edit_missing;
        kinds.wrote = write_paths.len() as u64 + write_missing;
        kinds.thoughts = self.thoughts().count() as u64;
        kinds
    }

    /// Its lines. A group with no call is its thinking, one line
    /// a block; otherwise a summary line, and the ledger when open. The
    /// summary line is one row in `width` cells, whatever runs: the dot,
    /// the kinds in their fixed order, and, right-aligned, the group's
    /// duration and ▸ while closed or ▾ while open. What does not fit is
    /// cut with "…", so the duration and the toggle stay put. A call's
    /// images draw as their one line each under its ledger row
    /// (`docs/tui.md`, "Images").
    pub(crate) fn rows(
        &self,
        running: bool,
        width: u16,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        // A group with no call is its thinking, streaming or ended: a
        // thinking-only group never draws a summary row, even while the
        // model thinks (`docs/tui.md`, "Thinking").
        if !self.has_ledger() && self.streaming.is_empty() {
            for thought in self.thoughts() {
                thought_rows(thought, "", width, out);
            }
            return;
        }
        // The body: the kinds with ", thought …", then each joined by
        // " · ": the failed model call count, the in-flight targets while
        // running, and the streaming thought's heading. Dim throughout,
        // the heading italic too (`docs/tui.md`, "Tool groups and the
        // ledger", "Turns").
        let mut body: Vec<Span<'static>> = self.kinds().summary();
        let join = |body: &mut Vec<Span<'static>>| {
            if !body.is_empty() {
                body.push(dim_span(" · ".to_owned()));
            }
        };
        let failed = self.failed() as u64;
        if failed > 0 {
            join(&mut body);
            body.push(dim_span(count(
                failed,
                "failed model call",
                "failed model calls",
            )));
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
                join(&mut body);
                body.push(dim_span(flight.join(", ")));
            }
            if let Some(thought) = self.thoughts().filter(|t| t.ended.is_none()).last() {
                join(&mut body);
                let text = match heading(&thought.text, true) {
                    Some(heading) => format!("Thinking: {heading}"),
                    None => "Thinking".to_owned(),
                };
                body.push(Span::styled(
                    text,
                    Style::default()
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
        // An empty body draws no row, as before.
        if body.is_empty() {
            return;
        }
        // A running group's dot spins on the working line's tick in the
        // attention colour; a finished group's dot is dim, as is the
        // whole line (`docs/tui.md`, "The working line", "Turns").
        let mut left = Vec::new();
        if running {
            left.push(Span::styled("●", style(Role::Attention)));
        } else {
            left.push(dim_span("●".to_owned()));
        }
        left.push(dim_span(" ".to_owned()));
        left.extend(body);
        // The duration and the toggle, right-aligned and dim. The toggle
        // follows the ledger's draw, not a search draw's (`docs/tui.md`,
        // "Search").
        let draws = self.open || self.calls().any(|call| call.asking);
        let right = vec![dim_span(format!(
            "{} {}",
            crate::format::duration(self.last.saturating_sub(self.first)),
            if draws { "▾" } else { "▸" },
        ))];
        // The row fills `width` exactly; what does not fit is cut from
        // the left part with "…", so the right part keeps its columns.
        let max = usize::from(width);
        let right_width = spans_width(&right);
        let left_width = spans_width(&left);
        let row = if left_width + 1 + right_width <= max {
            let mut row = left;
            row.push(dim_span(" ".repeat(max - left_width - right_width)));
            row.extend(right);
            row
        } else if max >= right_width + 2 {
            // One truncation: `fit` emits the row's single "…", so the
            // right part keeps its columns.
            let mut row = fit(left, max - right_width - 1);
            row.push(dim_span(" ".to_owned()));
            row.extend(right);
            row
        } else {
            let mut row = left;
            row.push(dim_span(" ".to_owned()));
            row.extend(right);
            fit(row, max)
        };
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
        // A row exactly `inner` wide is never split, and the spin column
        // moves with the card's pad.
        out.push_text(
            (
                Line::from(row),
                self.key.as_deref().map(|key| Target::Group(target_id(key))),
            ),
            text,
        );
        // An open approval shows its call whatever the group's own state.
        // A group with no key has no target, and draws as it is on a
        // search draw too: a match there is one nothing could reveal
        // (`docs/tui.md`, "Search").
        match self.key.as_deref().map(|key| Target::Group(target_id(key))) {
            Some(target) => {
                if out.open_scope(target, draws) {
                    self.ledger(width, layout, out);
                }
                out.end_scope();
            }
            None => {
                if draws {
                    self.ledger(width, layout, out);
                }
            }
        }
    }

    /// One row per call, split by step: the step's number right-aligned in
    /// a five-column dim gutter on its first row, then a glyph, the call's
    /// kind in `info` padded to nine columns, its target, and its result
    /// right-aligned to end at `width` (`docs/tui.md`, "Tool groups and
    /// the ledger"). The model calls that failed come first, then the
    /// step's thinking. Each call's images draw under its row, before its
    /// opened detail.
    fn ledger(&self, width: u16, layout: &crate::image::Layout, out: &mut Rows) {
        let max = usize::from(width);
        for section in &self.sections {
            let mut gutter = format!("{:>5} ", section.step);
            for (code, attempt) in &section.failed {
                let gutter = std::mem::replace(&mut gutter, GUTTER_BLANK.to_owned());
                let row = format!("{gutter}model call failed · {code} · attempt {attempt}");
                out.push((dim(row), None));
            }
            for thought in &section.thoughts {
                thought_rows(
                    thought,
                    &std::mem::replace(&mut gutter, GUTTER_BLANK.to_owned()),
                    width,
                    out,
                );
            }
            for call in &section.calls {
                let gutter = std::mem::replace(&mut gutter, GUTTER_BLANK.to_owned());
                let kind = kind(&call.name, &call.arguments);
                // The glyph: what the call is doing, or how it ended. A
                // call that started and never completed may have run.
                let (glyph, glyph_style) = match call.status {
                    None if self.cut && call.started => ("?", style(Role::Attention)),
                    None => ("○", style(Role::Attention)),
                    Some(CallStatus::Completed) => ("✓", style(Role::Success)),
                    Some(CallStatus::Cancelled) => {
                        ("⊘", Style::default().add_modifier(Modifier::DIM))
                    }
                    Some(CallStatus::Failed) | Some(CallStatus::Denied) => {
                        ("✗", Style::default().add_modifier(Modifier::DIM))
                    }
                };
                let result = self.result(call);
                // Six columns of gutter, the glyph, one space and nine of
                // kind leave the target and the result `max - 19` cells.
                // The target is cut with "…" to what the result leaves;
                // with no room for even one cell the target is left out
                // and the result is cut instead.
                let room = max.saturating_sub(19 + spans_width(&result));
                let mut row = vec![
                    dim_span(gutter),
                    Span::styled(glyph, glyph_style),
                    Span::raw(" "),
                    Span::styled(format!("{:<9}", kind.word()), style(Role::Info)),
                ];
                if room >= 1 {
                    row.push(Span::raw(pad_target(
                        &summary(&call.name, &call.arguments),
                        room,
                    )));
                    row.push(Span::raw("  "));
                    row.extend(result);
                } else {
                    row.push(Span::raw("  "));
                    row.extend(fit(result, max.saturating_sub(19)));
                }
                // Below twenty columns the gutter, the glyph, the kind,
                // the gap and one result cell do not fit. The row is cut
                // to `width` with "…" as its last step, so no row is
                // wider than `width` whatever the step number.
                let row = fit(row, max);
                out.push((Line::from(row), Some(Target::Call(call.id))));
                crate::turn::images::call_rows(&call.images, width, layout, out);
                if out.open_scope(Target::Call(call.id), call.open) {
                    opened_call(&call.detail, !call.changes.is_empty(), out);
                }
                out.end_scope();
            }
        }
    }

    /// A call's result: what the ledger row ends with (`docs/tui.md`,
    /// "Tool groups and the ledger"). An edit's counts are in the added
    /// and removed colours; every other result is dim, except a running
    /// call and an ask waiting on the person, which are in `attention`.
    fn result(&self, call: &Call) -> Vec<Span<'static>> {
        // The person's answer to a form replaces the status.
        if let Some(answered) = call.asked.as_ref().and_then(|asked| asked.suffix()) {
            return vec![dim_span(
                answered.strip_prefix(" · ").unwrap_or(answered).to_owned(),
            )];
        }
        match call.status {
            None => {
                if self.cut && call.started {
                    return vec![dim_span("? may have run; not run again".to_owned())];
                }
                if matches!(kind(&call.name, &call.arguments), Kind::Ask(_)) {
                    return vec![Span::styled("waiting on you", style(Role::Attention))];
                }
                return vec![Span::styled("running", style(Role::Attention))];
            }
            Some(CallStatus::Cancelled) => return vec![dim_span("cancelled".to_owned())],
            Some(CallStatus::Denied) => return vec![dim_span("denied".to_owned())],
            Some(CallStatus::Failed) => {
                let first = call.detail.lines().next().unwrap_or("").to_owned();
                return vec![dim_span(first)];
            }
            Some(CallStatus::Completed) => {}
        }
        if matches!(kind(&call.name, &call.arguments), Kind::Edit | Kind::Write)
            && !call.changes.is_empty()
        {
            let (added, removed) = call.changes.iter().fold((0u64, 0u64), |(a, r), c| {
                (a.saturating_add(c.added), r.saturating_add(c.removed))
            });
            return vec![
                Span::styled(format!("+{added}"), style(Role::Added)),
                Span::raw(" "),
                Span::styled(format!("−{removed}"), style(Role::Removed)),
            ];
        }
        match kind(&call.name, &call.arguments) {
            Kind::Read => vec![dim_span(count(call.lines, "line", "lines"))],
            Kind::Search => vec![dim_span(count(call.lines, "match", "matches"))],
            Kind::List => vec![dim_span(count(call.lines, "path", "paths"))],
            Kind::Shell if call.exit.is_some() => {
                let exit = call.exit.unwrap_or(0);
                vec![dim_span(format!(
                    "exit {exit} · {}",
                    count(call.lines, "line", "lines")
                ))]
            }
            Kind::Edit
            | Kind::Write
            | Kind::Shell
            | Kind::Delegate
            | Kind::Jobs
            | Kind::Ask(_)
            | Kind::Other => vec![dim_span(first_result_line(&call.detail))],
        }
    }
}

/// The gutter left blank: six columns, as `"{:>5} "` is.
const GUTTER_BLANK: &str = "      ";

/// An opened call or thought's indent: eight columns (`docs/tui.md`,
/// "Tool groups and the ledger").
const INDENT: &str = "        ";

/// A dim span.
fn dim_span(text: String) -> Span<'static> {
    Span::styled(text, Style::default().add_modifier(Modifier::DIM))
}

/// How many cells `spans` take on screen.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| crate::format::width(&span.content))
        .sum()
}

/// `spans` cut to at most `max` cells, styles kept: when anything is
/// left out the row ends in exactly one "…", so a cut row still fills
/// its width and what follows keeps its columns. Content keeps at most
/// `max − 1` cells, and a wide character's unused column is filled in
/// the cut span's style (`docs/tui.md`, "Tool groups and the ledger").
fn fit(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= max {
        return spans;
    }
    let mut out = Vec::new();
    let mut taken = 0usize;
    let room = max.saturating_sub(1);
    let mut style = Style::default();
    for span in spans {
        style = span.style;
        let cells = crate::format::width(&span.content);
        if taken + cells <= room {
            taken += cells;
            out.push(span);
            continue;
        }
        let kept = crate::format::cut(&span.content, room.saturating_sub(taken));
        taken += crate::format::width(&kept);
        if !kept.is_empty() {
            out.push(Span::styled(kept, style));
        }
        break;
    }
    out.push(Span::styled("…", style));
    out.push(Span::styled(
        " ".repeat(max.saturating_sub(taken + 1)),
        style,
    ));
    out
}

/// A thought's row after `gutter`, dim and italic, cut to `width` with
/// "…", and its text when open: "○ Thinking: <latest heading>" while
/// the thought streams, "+ Thought: <first heading> · <span>" once
/// ended. With no gutter the row is a thinking-only group's line, without
/// the "○ ". An opened thought's text sits eight columns in inside the
/// ledger and at the line's own indent outside it (`docs/tui.md`,
/// "Thinking", "Tool groups and the ledger").
fn thought_rows(thought: &Thought, gutter: &str, width: u16, out: &mut Rows) {
    let max = usize::from(width);
    let row = match thought.ended {
        None => {
            let heading = heading(&thought.text, true)
                .map(|found| format!(": {found}"))
                .unwrap_or_default();
            let mark = if gutter.is_empty() {
                "Thinking"
            } else {
                "○ Thinking"
            };
            vec![
                dim_span(gutter.to_owned()),
                Span::styled(
                    format!("{mark}{heading}"),
                    Style::default()
                        .add_modifier(Modifier::DIM)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]
        }
        Some(_) => {
            let span = thought
                .ended
                .map(|ended| ended.saturating_sub(thought.started));
            let mut line = crate::format::thought("", &thought.text, span);
            // The line's dim lives on its spans from here, italic too.
            let base = line.style;
            let mut text = std::mem::take(&mut line.spans);
            for span in &mut text {
                span.style = base.patch(span.style).add_modifier(Modifier::ITALIC);
            }
            let mut spans = vec![dim_span(gutter.to_owned())];
            spans.extend(text);
            spans
        }
    };
    out.push((Line::from(fit(row, max)), Some(Target::Thought(thought.id))));
    if out.open_scope(Target::Thought(thought.id), thought.open) {
        let indent = if gutter.is_empty() { "" } else { INDENT };
        opened(&thought.text, indent, out);
    }
    out.end_scope();
}

/// `target` cut with "…" to `room` cells, else padded with spaces to it,
/// so the result ends at the row's last cell. A wide character at the
/// edge can leave one column unused: the cut is padded to `room` from
/// its actual width, not its allowance.
fn pad_target(target: &str, room: usize) -> String {
    let cells = crate::format::width(target);
    if cells > room {
        let kept = crate::format::cut(target, room.saturating_sub(1));
        let fill = room.saturating_sub(crate::format::width(&kept) + 1);
        format!("{kept}…{}", " ".repeat(fill))
    } else {
        format!("{target}{}", " ".repeat(room - cells))
    }
}

/// The first non-empty line of a completed call's output, or "done" when
/// there is none.
fn first_result_line(detail: &str) -> String {
    detail
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("done")
        .to_owned()
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
