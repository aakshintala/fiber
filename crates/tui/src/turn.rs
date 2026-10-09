//! One turn's card: the prompt bubble, replies, steering, tool groups with
//! their ledger, thinking, and the ▣ line (`docs/tui.md`, "Turns", "Tool
//! groups and the ledger", "Thinking").
//!
//! Live, a reply's deltas stream before its durable lines are written, so
//! the fold places an item when it first sees it: reasoning by its own
//! action, a reply by its first text, a call by the raw arguments that
//! announced it. A replayed log has no deltas and lands in the same places.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use contract::Envelope;
use contract::events::{
    AssistantMessageCompleted, InputItem, MessageOutcome, ReasoningCompleted, RetryScheduled,
    SteeringApplied, TextCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallCompleted,
    ToolCallRequested, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use jiff::tz::TimeZone;
use ratatui::text::Line;
use serde_json::Value;

use crate::app::{Target, read, text_of};
use crate::format;
use crate::markdown;
use crate::rows::Rows;

mod answers;
pub(crate) mod crash;
mod group;
mod handoff;

#[cfg(test)]
use group::Section;
use group::{Call, Streaming};
pub(crate) use group::{Group, Thought};

/// One drawn line and what clicking it opens.
pub(crate) type Row = (Line<'static>, Option<Target>);

/// The same action names the same target in a live turn and a page replay.
pub(crate) fn target_id(action: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    action.hash(&mut hasher);
    usize::try_from(hasher.finish()).unwrap_or(usize::MAX)
}

/// What the fold keeps across turns.
#[derive(Debug, Clone, Default)]
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
    /// What the fold knows of the process and its jobs.
    crash: crash::Crash,
}

impl Fold {
    pub(crate) fn id(&mut self) -> usize {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }
}

/// One item inside a card.
#[derive(Debug, Clone)]
pub(crate) enum Entry {
    /// One text part of a reply, updated as deltas arrive.
    Reply {
        action: String,
        reply: markdown::Reply,
    },
    /// A steering message.
    Steer(String),
    /// A tool group, by its index in the turn's groups.
    Group(usize),
    /// A line that came while the turn ran.
    Aside(crash::Aside),
    /// A handoff's band: what follows is the second card.
    Band(handoff::Band),
    /// The person's answers to a question form.
    Answers(answers::Answered),
}

/// One turn's card.
#[derive(Debug, Clone, Default)]
pub(crate) struct Turn {
    prompts: Vec<String>,
    pub(crate) entries: Vec<Entry>,
    groups: Vec<Group>,
    /// The group new non-text items join, until the next reply.
    open_group: Option<usize>,
    started: u64,
    /// The time of the last line folded into it.
    last: u64,
    ended: Option<(Ending, u64)>,
    calls: u64,
    step: u64,
    /// The message whose raw arguments arrived last.
    message: Option<String>,
    /// How many non-empty `text_completed` each message has had.
    parts: HashMap<String, usize>,
    /// The turn's usage, delegates' copies included.
    pub(crate) spend: format::Spend,
    /// A failed model call waiting to retry, with the number of the attempt
    /// about to be made.
    retry: Option<(RetryScheduled, u32)>,
    /// The `assistant_message_started` lines of the model request in flight:
    /// a start after a failed call continues the count, any other restarts it.
    attempts: u32,
}

/// How a turn ended.
#[derive(Debug, Clone)]
enum Ending {
    /// Its `turn_completed`.
    Done(TurnCompleted),
    /// Fiber stopped before it completed.
    CutShort,
}

impl Turn {
    /// A turn started at `ts` by `prompts`.
    pub(crate) fn new(prompts: Vec<String>, ts: u64) -> Self {
        Self {
            prompts,
            started: ts,
            last: ts,
            ..Self::default()
        }
    }

    /// The part of a running turn on a page that begins at its step `step`:
    /// no prompt bubble, and the step count it has reached.
    pub(crate) fn part(step: u64) -> Self {
        Self {
            step,
            ..Self::default()
        }
    }

    /// Ends the open group at a page cut.
    pub(crate) fn end_group(&mut self) {
        self.open_group = None;
    }

    /// Whether the turn is still running.
    pub(crate) fn is_open(&self) -> bool {
        self.ended.is_none()
    }

    /// Closes the card.
    pub(crate) fn complete(&mut self, done: TurnCompleted, ts: u64) {
        self.ended = Some((Ending::Done(done), ts));
    }

    /// Restores the whole-turn totals on the page that holds its ending.
    pub(crate) fn summary(
        &mut self,
        started: u64,
        calls: u64,
        spend: &format::Spend,
        ended: Option<&(TurnCompleted, u64)>,
    ) {
        self.started = started;
        self.calls = calls;
        self.spend = spend.clone();
        if let Some((done, ts)) = ended {
            self.ended = Some((Ending::Done(done.clone()), *ts));
        }
    }

    /// A steering message, in place; it does not end a group.
    pub(crate) fn steer(&mut self, text: String) {
        self.entries.push(Entry::Steer(text));
    }

    /// `step_started`.
    pub(crate) fn step_started(&mut self) {
        self.step = self.step.saturating_add(1);
        self.attempts = 0;
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
    pub(crate) fn text_delta(&mut self, action: &str, text: &str, fold: &mut Fold) -> bool {
        if text.is_empty() {
            return false;
        }
        if self.note_text(action, text, false) {
            return true;
        }
        let mut found = None;
        for entry in self.entries.iter_mut().rev() {
            match entry {
                Entry::Reply { action: has, reply } if has == action => {
                    found = Some(reply);
                    break;
                }
                Entry::Group(_) | Entry::Band(_) => break,
                Entry::Reply { .. } | Entry::Steer(_) | Entry::Aside(_) | Entry::Answers(_) => {}
            }
        }
        match found {
            Some(reply) => reply.push(text),
            None => self.reply(action, text.to_owned(), fold),
        }
        true
    }

    /// `text_completed`: the message's next text part replaces the reply
    /// streamed for it, or is a new reply. An empty part shows nothing,
    /// and is false.
    pub(crate) fn text_completed(&mut self, action: &str, text: String, fold: &mut Fold) -> bool {
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
                Entry::Reply { action: has, reply } if has == action => Some(reply),
                Entry::Reply { .. }
                | Entry::Steer(_)
                | Entry::Group(_)
                | Entry::Aside(_)
                | Entry::Band(_)
                | Entry::Answers(_) => None,
            })
            .nth(nth);
        match found {
            Some(reply) => reply.set(text),
            None => self.reply(action, text, fold),
        }
        true
    }

    /// A new reply, which ends the open group.
    fn reply(&mut self, action: &str, text: String, fold: &mut Fold) {
        self.entries.push(Entry::Reply {
            action: action.to_owned(),
            reply: markdown::Reply::new(text, fold.id()),
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
        let group = self.group(ts, fold);
        group.key.get_or_insert_with(|| action.to_owned());
        group.section(step).thoughts.push(Thought {
            id: target_id(action),
            action: action.to_owned(),
            started: ts,
            ..Thought::default()
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
        group.key.get_or_insert_with(|| action.to_owned());
        let arguments = match requested.repair {
            Some(repair) => Value::Object(repair.repaired),
            None => requested.arguments,
        };
        group.section(step).calls.push(Call {
            id: target_id(action),
            action: action.to_owned(),
            name: requested.name,
            arguments,
            ..Call::default()
        });
    }

    /// `tool_call_started`; false when the call is not in this turn.
    pub(crate) fn call_started(&mut self, action: &str, ts: u64) -> bool {
        self.groups_mut().any(|group| {
            group.call(action).is_some_and(|call| {
                call.started = true;
                true
            }) && group.touched(ts)
        })
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
                    open: fold.ledgers,
                    first: ts,
                    ..Group::default()
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
    pub(crate) fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.iter()
    }

    /// The card's groups, to change.
    pub(crate) fn groups_mut(&mut self) -> impl Iterator<Item = &mut Group> {
        self.groups.iter_mut()
    }

    /// Toggles what `target` opens, returning its new state when found.
    pub(crate) fn toggle(&mut self, target: Target) -> Option<bool> {
        match target {
            Target::Group(id) => {
                let group = self
                    .groups
                    .iter_mut()
                    .find(|group| group.key.as_deref().is_some_and(|key| target_id(key) == id))?;
                group.open = !group.open;
                Some(group.open)
            }
            Target::Thought(id) => {
                let thought = self
                    .groups
                    .iter_mut()
                    .flat_map(|group| group.sections.iter_mut())
                    .flat_map(|section| section.thoughts.iter_mut())
                    .find(|thought| thought.id == id)?;
                thought.open = !thought.open;
                Some(thought.open)
            }
            Target::Call(id) => {
                let call = self
                    .groups
                    .iter_mut()
                    .flat_map(|group| group.sections.iter_mut())
                    .flat_map(|section| section.calls.iter_mut())
                    .find(|call| call.id == id)?;
                call.open = !call.open;
                Some(call.open)
            }
            Target::Note(id) => self.entries.iter_mut().find_map(|entry| match entry {
                Entry::Band(band) => band.toggle(Target::Note(id)),
                Entry::Reply { .. }
                | Entry::Steer(_)
                | Entry::Group(_)
                | Entry::Aside(_)
                | Entry::Answers(_) => None,
            }),
            Target::Orphans(id) => self.entries.iter_mut().find_map(|entry| match entry {
                Entry::Aside(aside) => aside.toggle(Target::Orphans(id)),
                Entry::Reply { .. }
                | Entry::Steer(_)
                | Entry::Group(_)
                | Entry::Band(_)
                | Entry::Answers(_) => None,
            }),
            Target::Login | Target::Copy { .. } => None,
        }
    }

    /// Sets what `target` opens to `open`; false when it is not in this turn.
    pub(crate) fn set_open(&mut self, target: &Target, open: bool) -> bool {
        self.groups
            .iter_mut()
            .any(|group| group.set_open(target, open))
            || self.entries.iter_mut().any(|entry| match entry {
                Entry::Aside(aside) => aside.set_open(target, open),
                Entry::Band(band) => band.set_open(target, open),
                Entry::Reply { .. } | Entry::Steer(_) | Entry::Group(_) | Entry::Answers(_) => {
                    false
                }
            })
    }

    /// `assistant_message_started`: one more attempt of the request.
    fn message_started(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
    }

    /// `retry_scheduled`: the retry pends, and the open group counts the
    /// call that failed.
    fn retry(&mut self, retry: RetryScheduled, ts: u64, fold: &mut Fold) {
        let step = self.step;
        let code = format::code(&retry.code);
        let failed = self.attempts;
        self.group(ts, fold)
            .section(step)
            .failed
            .push((code, failed));
        self.retry = Some((retry, failed.saturating_add(1)));
    }

    /// The card's lines at `width`, each prompt bubble with the local time
    /// of day under it (`docs/tui.md`, "Turns").
    pub(crate) fn rows(&self, width: u16, zone: &TimeZone, out: &mut Rows) {
        for prompt in &self.prompts {
            let before = out.len();
            crate::bubble::rows(prompt, width, out);
            if out.len() > before
                && let Some(time) = crate::local_time::time_of_day(self.started, zone)
            {
                out.push((format::dim(time).right_aligned(), None));
            }
        }
        for entry in &self.entries {
            match entry {
                Entry::Reply { reply, .. } => reply.rows(width, out),
                Entry::Steer(text) => out.push((Line::raw(format!("steer · {text}")), None)),
                Entry::Group(at) => {
                    if let Some(group) = self.groups.get(*at) {
                        group.rows(self.is_open() && self.open_group == Some(*at), out);
                    }
                }
                Entry::Aside(aside) => aside.rows(out),
                Entry::Band(band) => band.rows(out),
                Entry::Answers(answered) => answered.rows(out),
            }
        }
        if let Some((ending, ts)) = &self.ended {
            let head = match ending {
                Ending::Done(done) => {
                    if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
                        format::failure(error, out);
                    }
                    match done.outcome {
                        TurnOutcome::Completed => "▣ completed",
                        TurnOutcome::Interrupted => "▣ interrupted",
                        TurnOutcome::Failed => "▣ failed",
                    }
                }
                Ending::CutShort => "▣ cut short: Fiber stopped",
            };
            let ms = ts.saturating_sub(self.started);
            let closing = format::closing(head, ms, self.calls, &self.spend.usage());
            out.push((format::dim(closing), None));
        } else if let Some((retry, attempt)) = &self.retry {
            out.push((format::retry(retry, *attempt), None));
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
    // The request ends at a handoff, whose note request is its own, and at a
    // call that completed; a failed call is the request's retry.
    let request_ended = kind == "handoff_started"
        || (kind == "assistant_message_completed"
            && read!(envelope, AssistantMessageCompleted)
                .is_some_and(|done| done.outcome != MessageOutcome::Failed));
    if request_ended && let Some(turn) = open(turns) {
        turn.attempts = 0;
    }
    let changed = match kind {
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
        "assistant_message_started" => {
            if let Some(turn) = open(turns) {
                turn.message_started();
            }
            false
        }
        "retry_scheduled" => read!(envelope, RetryScheduled).is_some_and(|retry| {
            open(turns).is_some_and(|turn| {
                turn.retry(retry, ts, fold);
                true
            })
        }),
        "mcp_server_failed" | "fiber_started" | "fiber_exited" | "job_started"
        | "job_completed" => crash::fold(turns, fold, envelope),
        "preamble_built" | "handoff_started" | "handoff_completed" | "context_nudged" => {
            handoff::fold(turns, fold, envelope)
        }
        // Interaction lines name their calls in the payload, not the
        // envelope.
        "interaction_requested" | "interaction_resolved" => answers::fold(turns, envelope),
        _ => action.is_some_and(|action| fold_action(turns, fold, envelope, action)),
    };
    if let Some(turn) = open(turns) {
        turn.last = turn.last.max(ts);
    }
    changed
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
                    .is_some_and(|delta| turn.text_delta(action, &delta.text, fold)),
                "text_completed" => read!(envelope, TextCompleted)
                    .is_some_and(|done| turn.text_completed(action, done.text, fold)),
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

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
