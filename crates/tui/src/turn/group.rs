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
use crate::format::{Kind, Kinds, count, cut, dim, heading, kind, label, opened, seconds, summary};
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

    /// Its lines. A finished group with no call is its thinking, one line
    /// a block; otherwise a summary line, and the ledger when open. The
    /// summary line is one row in `width` cells, whatever runs. A
    /// call's images draw as their one line each under its ledger row
    /// (`docs/tui.md`, "Images").
    pub(crate) fn rows(
        &self,
        running: bool,
        width: u16,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        if !running && !self.has_ledger() {
            for thought in self.thoughts() {
                thought_rows(thought, "", out);
            }
            return;
        }
        let mut parts = Vec::new();
        let kinds = Line::from(self.kinds().summary()).to_string();
        if !kinds.is_empty() {
            parts.push(kinds);
        }
        let failed = self.failed() as u64;
        if failed > 0 {
            parts.push(count(failed, "failed model call", "failed model calls"));
        }
        let elapsed = if running {
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
            None
        } else {
            seconds(self.last.saturating_sub(self.first))
        };
        // A finished group's duration stays whole at the end of the line:
        // what does not fit is cut from the descriptive part with "…", so
        // the duration is still there (`docs/tui.md`, "Tool groups and the
        // ledger").
        let body = parts.join(" · ");
        let suffix = elapsed.as_deref().map(|span| {
            if body.is_empty() {
                format!(" {span}")
            } else {
                format!(" · {span}")
            }
        });
        let head = if body.is_empty() {
            "•".to_owned()
        } else {
            format!("• {body}")
        };
        if body.is_empty() && suffix.is_none() {
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
        let mut line = match &suffix {
            Some(tail) => format!("{head}{tail}"),
            None => head.clone(),
        };
        // What does not fit is cut with "…", so the line never wraps
        // (`docs/tui.md`, "Tool groups and the ledger").
        let max = usize::from(width);
        if crate::format::width(&line) > max {
            match &suffix {
                Some(tail) => {
                    let keep =
                        max.saturating_sub(crate::format::width(tail) + crate::format::width("…"));
                    line = format!("{}…{tail}", cut(&head, keep));
                }
                None => line = format!("{}…", cut(&line, max.saturating_sub(1))),
            }
        }
        out.push_text(
            (
                dim(line),
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

    /// One row per call, split by step: the step's number in the gutter on
    /// its first row, the model calls that failed first, then its thinking.
    /// Each call's images draw under its row, before its opened detail.
    fn ledger(&self, width: u16, layout: &crate::image::Layout, out: &mut Rows) {
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
                crate::turn::images::call_rows(&call.images, width, layout, out);
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
    let line = crate::format::thought(gutter, &thought.text, span);
    out.push((line, Some(Target::Thought(thought.id))));
    if out.open_scope(Target::Thought(thought.id), thought.open) {
        opened(&thought.text, &" ".repeat(gutter.len()), out);
    }
    out.end_scope();
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
