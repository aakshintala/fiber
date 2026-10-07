//! One turn's card: the prompt bubble, replies, steering, tool groups with
//! their ledger, thinking, and the ▣ line (`docs/tui.md`, "Turns", "Tool
//! groups and the ledger", "Thinking").
//!
//! Live, a reply's deltas stream before its durable lines are written, so
//! the fold places an item when it first sees it: reasoning by its own
//! action, a reply by its first text, a call by the raw arguments that
//! announced it. A replayed log has no deltas and lands in the same places.

use std::collections::HashMap;

use contract::Envelope;
use contract::events::{
    CallStatus, FileChange, ReasoningCompleted, TextCompleted, TextDelta, ToolCallArgumentsDelta,
    ToolCallCompleted, ToolCallRequested, TurnCompleted,
};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use serde_json::Value;

use crate::app::{Target, read};
use crate::format::{self, Kinds};

/// One drawn line and what clicking it opens.
pub(crate) type Row = (Line<'static>, Option<Target>);

/// What the fold keeps across turns.
#[derive(Debug, Default)]
pub(crate) struct Fold {
    /// The next target id.
    next: usize,
    /// Whether a new group starts with its ledger open: the last Ctrl+O.
    pub(crate) ledgers: bool,
}

impl Fold {
    fn id(&mut self) -> usize {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }
}

/// One item inside a card.
#[derive(Debug)]
enum Entry {
    /// One text part of a reply, updated as deltas arrive.
    Reply { action: String, text: String },
    /// A steering message.
    Steer(String),
    /// A tool group, by its index in the turn's groups.
    Group(usize),
}

/// Everything between two pieces of assistant text.
#[derive(Debug)]
pub(crate) struct Group {
    id: usize,
    /// Whether its ledger is open.
    pub(crate) open: bool,
    first: u64,
    last: u64,
    sections: Vec<Section>,
    /// Calls the model is still emitting.
    streaming: Vec<Streaming>,
}

/// One step's part of a group.
#[derive(Debug)]
struct Section {
    step: u64,
    thoughts: Vec<Thought>,
    calls: Vec<Call>,
}

/// One thinking block.
#[derive(Debug)]
struct Thought {
    id: usize,
    action: String,
    text: String,
    started: u64,
    ended: Option<u64>,
    open: bool,
}

/// One tool call.
#[derive(Debug)]
struct Call {
    id: usize,
    action: String,
    name: String,
    arguments: Value,
    /// How it ended; `None` while it runs.
    status: Option<CallStatus>,
    changes: Vec<FileChange>,
    /// What opening the call shows.
    detail: String,
    open: bool,
    /// A `permission_requested` for it is open.
    asking: bool,
}

/// A call still streaming: its message, position, name and raw text.
#[derive(Debug)]
struct Streaming {
    message: String,
    index: u32,
    name: Option<String>,
    text: String,
}

/// One turn's card.
#[derive(Debug)]
pub(crate) struct Turn {
    prompts: Vec<String>,
    entries: Vec<Entry>,
    groups: Vec<Group>,
    /// The group new non-text items join, until the next reply.
    open_group: Option<usize>,
    started: u64,
    ended: Option<(TurnCompleted, u64)>,
    calls: u64,
    step: u64,
    /// The latest assistant message.
    message: Option<String>,
    /// How many non-empty `text_completed` each message has had.
    parts: HashMap<String, usize>,
    /// The turn's usage, delegates' copies included.
    pub(crate) spend: format::Spend,
}

impl Turn {
    /// A turn started at `ts` by `prompts`.
    pub(crate) fn new(prompts: Vec<String>, ts: u64) -> Self {
        Self {
            prompts,
            entries: Vec::new(),
            groups: Vec::new(),
            open_group: None,
            started: ts,
            ended: None,
            calls: 0,
            step: 0,
            message: None,
            parts: HashMap::new(),
            spend: format::Spend::default(),
        }
    }

    /// Whether the turn is still running.
    pub(crate) fn is_open(&self) -> bool {
        self.ended.is_none()
    }

    /// Closes the card.
    pub(crate) fn complete(&mut self, done: TurnCompleted, ts: u64) {
        self.ended = Some((done, ts));
    }

    /// A steering message, in place; it does not end a group.
    pub(crate) fn steer(&mut self, text: String) {
        self.entries.push(Entry::Steer(text));
    }

    /// `step_started`.
    pub(crate) fn step_started(&mut self) {
        self.step = self.step.saturating_add(1);
    }

    /// `assistant_message_started`.
    pub(crate) fn message_started(&mut self, action: &str) {
        self.message = Some(action.to_owned());
    }

    /// `assistant_message_completed`: whatever it was still emitting is no
    /// longer in flight; false when nothing was.
    pub(crate) fn message_completed(&mut self, action: &str) -> bool {
        let mut dropped = false;
        for group in self.groups_mut() {
            let before = group.streaming.len();
            group.streaming.retain(|call| call.message != action);
            dropped |= group.streaming.len() != before;
        }
        dropped
    }

    /// `assistant_message_delta`: appends to the message's reply, or starts
    /// a new one once a group has opened after it; false when empty.
    pub(crate) fn text_delta(&mut self, action: &str, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let mut found = None;
        for entry in self.entries.iter_mut().rev() {
            match entry {
                Entry::Reply { action: has, text } if has == action => {
                    found = Some(text);
                    break;
                }
                Entry::Group(_) => break,
                Entry::Reply { .. } | Entry::Steer(_) => {}
            }
        }
        match found {
            Some(reply) => reply.push_str(text),
            None => self.reply(action, text.to_owned()),
        }
        true
    }

    /// `text_completed`: the message's next text part replaces the reply
    /// streamed for it, or is a new reply. An empty part shows nothing,
    /// and is false.
    pub(crate) fn text_completed(&mut self, action: &str, text: String) -> bool {
        if text.is_empty() {
            return false;
        }
        let part = self.parts.entry(action.to_owned()).or_default();
        let nth = *part;
        *part = part.saturating_add(1);
        let found = self
            .entries
            .iter_mut()
            .filter_map(|entry| match entry {
                Entry::Reply { action: has, text } if has == action => Some(text),
                Entry::Reply { .. } | Entry::Steer(_) | Entry::Group(_) => None,
            })
            .nth(nth);
        match found {
            Some(reply) => *reply = text,
            None => self.reply(action, text),
        }
        true
    }

    /// A new reply, which ends the open group.
    fn reply(&mut self, action: &str, text: String) {
        self.entries.push(Entry::Reply {
            action: action.to_owned(),
            text,
        });
        self.open_group = None;
    }

    /// `tool_call_arguments_delta` under message `action`.
    pub(crate) fn arguments_delta(
        &mut self,
        action: &str,
        delta: ToolCallArgumentsDelta,
        ts: u64,
        fold: &mut Fold,
    ) {
        self.message = Some(action.to_owned());
        for group in self.groups_mut() {
            if let Some(call) = group
                .streaming
                .iter_mut()
                .find(|call| call.message == action && call.index == delta.index)
            {
                call.text.push_str(&delta.text);
                if delta.name.is_some() {
                    call.name = delta.name;
                }
                group.touch(ts);
                return;
            }
        }
        let group = self.group(ts, fold);
        group.streaming.push(Streaming {
            message: action.to_owned(),
            index: delta.index,
            name: delta.name,
            text: delta.text,
        });
    }

    /// `reasoning_started`: a thinking block joins the open group; false
    /// when it already has.
    pub(crate) fn reasoning_started(&mut self, action: &str, ts: u64, fold: &mut Fold) -> bool {
        if self
            .groups_mut()
            .any(|group| group.thought(action).is_some())
        {
            return false;
        }
        let step = self.step;
        let id = fold.id();
        let group = self.group(ts, fold);
        group.section(step).thoughts.push(Thought {
            id,
            action: action.to_owned(),
            text: String::new(),
            started: ts,
            ended: None,
            open: false,
        });
        true
    }

    /// `reasoning_delta`; false when the block was never started here.
    pub(crate) fn reasoning_delta(&mut self, action: &str, text: &str, ts: u64) -> bool {
        self.groups_mut().any(|group| {
            group.thought(action).is_some_and(|thought| {
                thought.text.push_str(text);
                true
            }) && group.touched(ts)
        })
    }

    /// `reasoning_completed`; false when the block was never started here.
    pub(crate) fn reasoning_completed(&mut self, action: &str, text: String, ts: u64) -> bool {
        let mut text = Some(text);
        self.groups_mut().any(|group| {
            group.thought(action).is_some_and(|thought| {
                thought.text = text.take().unwrap_or_default();
                thought.ended = Some(ts);
                true
            }) && group.touched(ts)
        })
    }

    /// `tool_call_requested`: the call joins the group whose raw arguments
    /// announced it, the lowest index of the latest message, else the open
    /// group.
    pub(crate) fn call_requested(
        &mut self,
        action: &str,
        requested: ToolCallRequested,
        ts: u64,
        fold: &mut Fold,
    ) {
        self.calls = self.calls.saturating_add(1);
        let message = self.message.clone();
        let announced = self
            .groups
            .iter()
            .enumerate()
            .flat_map(|(at, group)| {
                group
                    .streaming
                    .iter()
                    .filter(|call| Some(&call.message) == message.as_ref())
                    .map(move |call| (call.index, at))
            })
            .min();
        let step = self.step;
        let id = fold.id();
        let group = match announced.and_then(|(index, at)| Some(index).zip(self.groups.get_mut(at)))
        {
            Some((index, group)) => {
                group
                    .streaming
                    .retain(|call| Some(&call.message) != message.as_ref() || call.index != index);
                group.touch(ts);
                group
            }
            None => self.group(ts, fold),
        };
        let arguments = match requested.repair {
            Some(repair) => Value::Object(repair.repaired),
            None => requested.arguments,
        };
        group.section(step).calls.push(Call {
            id,
            action: action.to_owned(),
            name: requested.name,
            arguments,
            status: None,
            changes: Vec::new(),
            detail: String::new(),
            open: false,
            asking: false,
        });
    }

    /// `tool_call_started`; false when the call is not in this turn.
    pub(crate) fn call_started(&mut self, action: &str, ts: u64) -> bool {
        self.groups_mut()
            .any(|group| group.call(action).is_some() && group.touched(ts))
    }

    /// `tool_call_completed`; false when the call is not in this turn.
    pub(crate) fn call_completed(
        &mut self,
        action: &str,
        done: &ToolCallCompleted,
        ts: u64,
    ) -> bool {
        self.groups_mut().any(|group| {
            group.call(action).is_some_and(|call| {
                call.status = Some(done.status);
                call.changes = done.changes.clone().unwrap_or_default();
                call.detail = format::detail(done);
                true
            }) && group.touched(ts)
        })
    }

    /// `permission_requested` (`asking`) or `permission_resolved` for a
    /// call; false when the call is not in this turn.
    pub(crate) fn permission(&mut self, action: &str, asking: bool) -> bool {
        self.groups_mut().any(|group| {
            group.call(action).is_some_and(|call| {
                call.asking = asking;
                true
            })
        })
    }

    /// The open group, or a new one started at `ts`.
    fn group(&mut self, ts: u64, fold: &mut Fold) -> &mut Group {
        let at = match self.open_group {
            Some(at) => at,
            None => {
                self.groups.push(Group {
                    id: fold.id(),
                    open: fold.ledgers,
                    first: ts,
                    last: ts,
                    sections: Vec::new(),
                    streaming: Vec::new(),
                });
                let at = self.groups.len().saturating_sub(1);
                self.entries.push(Entry::Group(at));
                self.open_group = Some(at);
                at
            }
        };
        #[expect(
            clippy::indexing_slicing,
            reason = "`open_group` only ever holds an index into `groups`"
        )]
        let group = &mut self.groups[at];
        group.touch(ts);
        group
    }

    /// The card's groups.
    pub(crate) fn groups_mut(&mut self) -> impl Iterator<Item = &mut Group> {
        self.groups.iter_mut()
    }

    /// Toggles what `target` opens; false when it is not in this turn.
    pub(crate) fn toggle(&mut self, target: Target) -> bool {
        self.groups_mut().any(|group| group.toggle(target))
    }

    /// The card's lines at `width`.
    pub(crate) fn rows(&self, width: u16, out: &mut Vec<Row>) {
        for prompt in &self.prompts {
            format::bubble(prompt, width, out);
        }
        for entry in &self.entries {
            match entry {
                Entry::Reply { text, .. } => {
                    for line in text.split('\n') {
                        out.push((Line::raw(line.to_owned()), None));
                    }
                }
                Entry::Steer(text) => out.push((Line::raw(format!("steer · {text}")), None)),
                Entry::Group(at) => {
                    if let Some(group) = self.groups.get(*at) {
                        group.rows(self.is_open() && self.open_group == Some(*at), out);
                    }
                }
            }
        }
        if let Some((done, ts)) = &self.ended {
            let closing = format::closing(
                done,
                ts.saturating_sub(self.started),
                self.calls,
                &self.spend.usage(),
            );
            out.push((format::dim(closing), None));
        }
    }
}

/// Folds a line about one action into `turns`; false when it changed no
/// card.
pub(crate) fn fold_action(
    turns: &mut [Turn],
    fold: &mut Fold,
    envelope: &Envelope,
    action: &str,
) -> bool {
    let ts = envelope.ts;
    // Completions may follow their turn's end, so they search back
    // through every card; streaming goes only to the open one.
    match envelope.kind.as_str() {
        "tool_call_started" => turns
            .iter_mut()
            .rev()
            .any(|turn| turn.call_started(action, ts)),
        "tool_call_completed" => read!(envelope, ToolCallCompleted).is_some_and(|done| {
            turns
                .iter_mut()
                .rev()
                .any(|turn| turn.call_completed(action, &done, ts))
        }),
        "permission_requested" | "permission_resolved" => {
            let asking = envelope.kind == "permission_requested";
            turns
                .iter_mut()
                .rev()
                .any(|turn| turn.permission(action, asking))
        }
        "reasoning_completed" => read!(envelope, ReasoningCompleted).is_some_and(|done| {
            let mut text = Some(done.text);
            turns
                .iter_mut()
                .rev()
                .any(|turn| turn.reasoning_completed(action, text.take().unwrap_or_default(), ts))
        }),
        kind => {
            let Some(turn) = turns.last_mut().filter(|turn| turn.is_open()) else {
                return false;
            };
            match kind {
                "assistant_message_started" => {
                    turn.message_started(action);
                    false
                }
                "assistant_message_completed" => turn.message_completed(action),
                "assistant_message_delta" => read!(envelope, TextDelta)
                    .is_some_and(|delta| turn.text_delta(action, &delta.text)),
                "text_completed" => read!(envelope, TextCompleted)
                    .is_some_and(|done| turn.text_completed(action, done.text)),
                "tool_call_arguments_delta" => {
                    read!(envelope, ToolCallArgumentsDelta).is_some_and(|delta| {
                        turn.arguments_delta(action, delta, ts, fold);
                        true
                    })
                }
                "reasoning_started" => turn.reasoning_started(action, ts, fold),
                "reasoning_delta" => read!(envelope, TextDelta)
                    .is_some_and(|delta| turn.reasoning_delta(action, &delta.text, ts)),
                "tool_call_requested" => {
                    read!(envelope, ToolCallRequested).is_some_and(|requested| {
                        turn.call_requested(action, requested, ts, fold);
                        true
                    })
                }
                _ => false,
            }
        }
    }
}

impl Group {
    /// Widens the group's span to `ts`.
    fn touch(&mut self, ts: u64) {
        self.first = self.first.min(ts);
        self.last = self.last.max(ts);
    }

    /// [`Self::touch`], as a condition that holds.
    fn touched(&mut self, ts: u64) -> bool {
        self.touch(ts);
        true
    }

    /// The section for `step`, the last one when it is that step's.
    fn section(&mut self, step: u64) -> &mut Section {
        if self.sections.last().is_none_or(|last| last.step != step) {
            self.sections.push(Section {
                step,
                thoughts: Vec::new(),
                calls: Vec::new(),
            });
        }
        let at = self.sections.len().saturating_sub(1);
        #[expect(clippy::indexing_slicing, reason = "a section was pushed above")]
        &mut self.sections[at]
    }

    fn thought(&mut self, action: &str) -> Option<&mut Thought> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.thoughts.iter_mut())
            .find(|thought| thought.action == action)
    }

    fn call(&mut self, action: &str) -> Option<&mut Call> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.calls.iter_mut())
            .find(|call| call.action == action)
    }

    fn calls(&self) -> impl Iterator<Item = &Call> {
        self.sections
            .iter()
            .flat_map(|section| section.calls.iter())
    }

    fn thoughts(&self) -> impl Iterator<Item = &Thought> {
        self.sections
            .iter()
            .flat_map(|section| section.thoughts.iter())
    }

    fn toggle(&mut self, target: Target) -> bool {
        let flag = match target {
            Target::Group(id) => (self.id == id).then_some(&mut self.open),
            Target::Thought(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.thoughts.iter_mut())
                .find(|thought| thought.id == id)
                .map(|thought| &mut thought.open),
            Target::Call(id) => self
                .sections
                .iter_mut()
                .flat_map(|section| section.calls.iter_mut())
                .find(|call| call.id == id)
                .map(|call| &mut call.open),
        };
        flag.map(|open| *open = !*open).is_some()
    }

    /// Whether the group holds calls, and so a ledger.
    pub(crate) fn has_calls(&self) -> bool {
        self.calls().next().is_some()
    }

    /// What its calls did, by kind, and how often the model thought.
    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds::default();
        let mut paths = Vec::new();
        let mut edits = 0u64;
        for call in self.calls() {
            match format::kind(&call.name, &call.arguments) {
                format::Kind::Read => kinds.read = kinds.read.saturating_add(1),
                format::Kind::Search => kinds.searched = kinds.searched.saturating_add(1),
                format::Kind::Ran => kinds.ran = kinds.ran.saturating_add(1),
                format::Kind::Other => kinds.other = kinds.other.saturating_add(1),
                format::Kind::Edit => {
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
    fn rows(&self, running: bool, out: &mut Vec<Row>) {
        if !running && !self.has_calls() {
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
        if running {
            let flight: Vec<String> = self
                .calls()
                .filter(|call| call.status.is_none())
                .map(|call| {
                    format::label(&call.name, &format::summary(&call.name, &call.arguments))
                })
                .chain(
                    self.streaming
                        .iter()
                        .map(|call| format::label(call.name.as_deref().unwrap_or("…"), &call.text)),
                )
                .collect();
            if !flight.is_empty() {
                parts.push(flight.join(", "));
            }
            if let Some(thought) = self.thoughts().filter(|t| t.ended.is_none()).last() {
                parts.push(match format::heading(&thought.text, true) {
                    Some(heading) => format!("Thinking: {heading}"),
                    None => "Thinking".to_owned(),
                });
            }
        } else {
            parts.extend(format::seconds(self.last.saturating_sub(self.first)));
        }
        if parts.is_empty() {
            return;
        }
        out.push((
            format::dim(format!("• {}", parts.join(" · "))),
            Some(Target::Group(self.id)),
        ));
        // An open approval shows its call whatever the group's own state.
        if self.open || self.calls().any(|call| call.asking) {
            self.ledger(out);
        }
    }

    /// One row per call, split by step: the step's number in the gutter on
    /// its first row, its thinking first.
    fn ledger(&self, out: &mut Vec<Row>) {
        for section in &self.sections {
            let mut gutter = format!("{:>3} ", section.step);
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
                    format::label(&call.name, &format::summary(&call.name, &call.arguments))
                );
                let (added, removed) = call.changes.iter().fold((0u64, 0u64), |(a, r), c| {
                    (a.saturating_add(c.added), r.saturating_add(c.removed))
                });
                if !call.changes.is_empty() {
                    row.push_str(&format!(" +{added} −{removed}"));
                }
                match call.status {
                    None => row.push_str(" · running"),
                    Some(CallStatus::Completed) => {}
                    Some(CallStatus::Failed) => row.push_str(" · failed"),
                    Some(CallStatus::Denied) => row.push_str(" · denied"),
                    Some(CallStatus::Cancelled) => row.push_str(" · cancelled"),
                }
                let line = if call.changes.is_empty() {
                    format::dim(row)
                } else {
                    Line::styled(row, Style::default().add_modifier(Modifier::BOLD))
                };
                out.push((line, Some(Target::Call(call.id))));
                if call.open {
                    format::opened(&call.detail, GAP, out);
                }
            }
        }
    }
}

/// The gutter left blank.
const GAP: &str = "    ";

/// A thought's line after `gutter`, and its text when open.
fn thought_rows(thought: &Thought, gutter: &str, out: &mut Vec<Row>) {
    let span = thought
        .ended
        .map(|ended| ended.saturating_sub(thought.started));
    let line = format::thought(gutter, &thought.text, span);
    out.push((line, Some(Target::Thought(thought.id))));
    if thought.open {
        format::opened(&thought.text, &" ".repeat(gutter.len()), out);
    }
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
