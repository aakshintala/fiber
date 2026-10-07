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
    CallStatus, FileChange, InputItem, ReasoningCompleted, RetryScheduled, SteeringApplied,
    TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallCompleted, ToolCallRequested,
    TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use ratatui::text::Line;
use serde_json::Value;

use crate::app::{Target, read, text_of};
use crate::format;

#[path = "crash.rs"]
pub(crate) mod crash;
#[path = "handoff.rs"]
mod handoff;

/// One drawn line and what clicking it opens.
pub(crate) type Row = (Line<'static>, Option<Target>);

/// What the fold keeps across turns.
#[derive(Debug, Default)]
pub(crate) struct Fold {
    /// The next target id.
    next: usize,
    /// Whether a new group starts with its ledger open: the last Ctrl+O.
    pub(crate) ledgers: bool,
    /// Lines outside any turn, each after the turns there were when it
    /// came.
    pub(crate) asides: Vec<(usize, crash::Aside)>,
    /// The context size an automatic handoff runs at, from the latest
    /// `preamble_built`.
    trigger_at: Option<u64>,
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
    /// A line that came while the turn ran.
    Aside(crash::Aside),
    /// A handoff's band: what follows is the second card.
    Band(handoff::Band),
}

/// Everything between two pieces of assistant text.
#[derive(Debug)]
pub(crate) struct Group {
    pub(crate) id: usize,
    /// Whether its ledger is open.
    pub(crate) open: bool,
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) sections: Vec<Section>,
    /// Calls the model is still emitting.
    pub(crate) streaming: Vec<Streaming>,
}

/// One step's part of a group.
#[derive(Debug)]
pub(crate) struct Section {
    pub(crate) step: u64,
    pub(crate) thoughts: Vec<Thought>,
    pub(crate) calls: Vec<Call>,
    /// Model calls that failed and were retried: code and attempt.
    pub(crate) failed: Vec<(String, u32)>,
}

/// One thinking block.
#[derive(Debug)]
pub(crate) struct Thought {
    pub(crate) id: usize,
    pub(crate) action: String,
    pub(crate) text: String,
    pub(crate) started: u64,
    pub(crate) ended: Option<u64>,
    pub(crate) open: bool,
}

/// One tool call.
#[derive(Debug)]
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
    pub(crate) open: bool,
    /// A `permission_requested` for it is open.
    pub(crate) asking: bool,
}

/// A call still streaming: its message, position, name and raw text.
#[derive(Debug)]
pub(crate) struct Streaming {
    pub(crate) message: String,
    pub(crate) index: u32,
    pub(crate) name: Option<String>,
    pub(crate) text: String,
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
    /// The message whose raw arguments arrived last.
    message: Option<String>,
    /// How many non-empty `text_completed` each message has had.
    parts: HashMap<String, usize>,
    /// The turn's usage, delegates' copies included.
    pub(crate) spend: format::Spend,
    /// A failed model call waiting to retry.
    retry: Option<RetryScheduled>,
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
            retry: None,
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
        if self.note_text(action, text, false) {
            return true;
        }
        let mut found = None;
        for entry in self.entries.iter_mut().rev() {
            match entry {
                Entry::Reply { action: has, text } if has == action => {
                    found = Some(text);
                    break;
                }
                Entry::Group(_) | Entry::Band(_) => break,
                Entry::Reply { .. } | Entry::Steer(_) | Entry::Aside(_) => {}
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
        if self.note_text(action, &text, true) {
            return true;
        }
        let part = self.parts.entry(action.to_owned()).or_default();
        let nth = *part;
        *part = part.saturating_add(1);
        let found = self
            .entries
            .iter_mut()
            .filter_map(|entry| match entry {
                Entry::Reply { action: has, text } if has == action => Some(text),
                Entry::Reply { .. }
                | Entry::Steer(_)
                | Entry::Group(_)
                | Entry::Aside(_)
                | Entry::Band(_) => None,
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
            || self.entries.iter_mut().any(|entry| match entry {
                Entry::Aside(aside) => aside.toggle(target),
                Entry::Band(band) => band.toggle(target),
                Entry::Reply { .. } | Entry::Steer(_) | Entry::Group(_) => false,
            })
    }

    /// `retry_scheduled`: the retry pends, and the open group counts the
    /// call that failed.
    fn retry(&mut self, retry: RetryScheduled, ts: u64, fold: &mut Fold) {
        let step = self.step;
        let code = format::code(&retry.code);
        let attempt = retry.attempt.saturating_sub(1);
        self.group(ts, fold)
            .section(step)
            .failed
            .push((code, attempt));
        self.retry = Some(retry);
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
                Entry::Aside(aside) => aside.rows(out),
                Entry::Band(band) => band.rows(out),
            }
        }
        if let Some((done, ts)) = &self.ended {
            let head = match done.outcome {
                TurnOutcome::Completed => "▣ completed",
                TurnOutcome::Interrupted => "▣ interrupted",
                TurnOutcome::Failed => "▣ failed",
            };
            if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
                format::failure(error, out);
            }
            let ms = ts.saturating_sub(self.started);
            let closing = format::closing(head, ms, self.calls, &self.spend.usage());
            out.push((format::dim(closing), None));
        } else if let Some(retry) = &self.retry {
            out.push((format::retry(retry), None));
        }
    }
}

/// Folds one line of the attached session's stream into `turns`; false
/// when it changed no card.
pub(crate) fn fold_line(turns: &mut Vec<Turn>, fold: &mut Fold, envelope: &Envelope) -> bool {
    let action = envelope.action_id.as_ref().map(|id| id.0.as_str());
    let ts = envelope.ts;
    let kind = envelope.kind.as_str();
    // A model call that got through ends the wait to retry.
    if (matches!(
        kind,
        "assistant_message_delta" | "text_completed" | "reasoning_started" | "turn_completed"
    ) || kind.starts_with("tool_call_"))
        && let Some(turn) = open(turns)
    {
        turn.retry = None;
    }
    match kind {
        "turn_started" => read!(envelope, TurnStarted).is_some_and(|started| {
            let prompts = started
                .input
                .iter()
                .filter_map(|input| {
                    if let InputItem::Message { content, .. } = input {
                        Some(text_of(content))
                    } else {
                        None
                    }
                })
                .collect();
            turns.push(Turn::new(prompts, ts));
            true
        }),
        "turn_completed" => read!(envelope, TurnCompleted).is_some_and(|done| {
            open(turns).is_some_and(|turn| {
                turn.complete(done, ts);
                true
            })
        }),
        "usage_recorded" => read!(envelope, UsageRecorded).is_some_and(|line| {
            // A line folds where its generation already is, so a late
            // correction updates a closed card; else into the open turn.
            let known = turns
                .iter()
                .rposition(|turn| turn.spend.holds(&line.generation_id));
            // A new call, not a correction, may size a handoff.
            let sized = known.is_none() && handoff::sized(turns, &line);
            let turn = match known {
                Some(at) => turns.get_mut(at),
                None => open(turns),
            };
            turn.is_some_and(|turn| {
                turn.spend.record(&line);
                true
            }) || sized
        }),
        "steering_applied" => read!(envelope, SteeringApplied).is_some_and(|applied| {
            open(turns).is_some_and(|turn| {
                turn.steer(text_of(&applied.content));
                true
            })
        }),
        "step_started" => {
            if let Some(turn) = open(turns) {
                turn.step_started();
            }
            false
        }
        "retry_scheduled" => read!(envelope, RetryScheduled).is_some_and(|retry| {
            open(turns).is_some_and(|turn| {
                turn.retry(retry, ts, fold);
                true
            })
        }),
        "mcp_server_failed" => crash::fold(turns, fold, envelope),
        "preamble_built" | "handoff_started" | "handoff_completed" | "context_nudged" => {
            handoff::fold(turns, fold, envelope)
        }
        _ => action.is_some_and(|action| fold_action(turns, fold, envelope, action)),
    }
}

/// The turn still running, if any.
fn open(turns: &mut [Turn]) -> Option<&mut Turn> {
    turns.last_mut().filter(|turn| turn.is_open())
}

/// Folds a line about one action into `turns`; false when it changed no
/// card.
fn fold_action(turns: &mut [Turn], fold: &mut Fold, envelope: &Envelope, action: &str) -> bool {
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
                failed: Vec::new(),
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
            Target::Login | Target::Note(_) => None,
        };
        flag.map(|open| *open = !*open).is_some()
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

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
