//! The rewind view's folded turns and chain pager (`docs/tui.md`, "Rewind").
//!
//! Owns the held history lines, the pager walking `forked_from`, the folded
//! turns with their tool calls, and the ten-second `history` wait. The view
//! cursor and the send state live in sibling modules; the parent coordinates
//! them through the methods below.

use std::time::{Duration, Instant};

use contract::events::{InputItem, ToolCallRequested, ToolCallStarted, TurnStarted};
use contract::shapes::{ContentPart, Origin, Point};
use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::Value;

use super::super::history_args;
use crate::app::read;

/// How long a `history` wait holds for its answer before ending as a
/// lost connection (`docs/tui.md`, "History and paging").
const HISTORY_WAIT: Duration = Duration::from_secs(10);

/// A `history` request waiting for its answer.
#[derive(Debug)]
struct Pending {
    /// The command id the answer names.
    id: String,
    /// When the request went out on the frame's clock; none before the
    /// first frame time, and then the wait carries no deadline.
    sent_at: Option<Instant>,
}

/// One step boundary: the point, and what the view says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Boundary {
    /// The log holding the point.
    pub session: SessionId,
    /// The point, in that log.
    pub seq: u64,
    /// Which boundary it is.
    pub kind: BoundKind,
    /// The step's text for an after-step boundary; empty elsewhere.
    pub summary: String,
}

/// Which boundary a point is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundKind {
    /// The turn's first line minus one.
    Start,
    /// Just after the person's message.
    AfterInput,
    /// Just after a step: the step that ended, one-based.
    AfterStep {
        /// The step that ended, one-based.
        step: u32,
    },
    /// The turn's last line.
    End,
}

/// One folded turn: its prompt and every step boundary it opens into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRow {
    /// The log holding the turn.
    pub session: SessionId,
    /// The turn's first line minus one: the row's key with its session.
    pub start: u64,
    /// The row's prompt: the first driver message's text, flattened.
    pub prompt: String,
    /// Whether the turn is a rewind's summary turn.
    pub summary: bool,
    /// Every step boundary, oldest first.
    pub bounds: Vec<Boundary>,
}

/// One tool call taking shape while its lines fold: what it declared.
#[derive(Debug)]
struct CallRec {
    session: SessionId,
    seq: u64,
    action: Option<ActionId>,
    effects: Vec<contract::shapes::Effect>,
    paths: Vec<String>,
}

/// A turn taking shape while its lines fold.
struct Building {
    session: SessionId,
    start: u64,
    after_input: u64,
    prompt: String,
    summary: bool,
    steps: Vec<Step>,
    end: Option<u64>,
}

/// One step taking shape: when it started and what it said.
struct Step {
    seq: u64,
    text: String,
}

/// The files written and the calls that may have changed files since the
/// chosen point, as folded from the held lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DetailModel {
    /// Paths of calls that declared writes, oldest first.
    pub writes: Vec<String>,
    /// Names of calls that declared executes, oldest first.
    pub executes: Vec<String>,
}

/// The folded turns and the chain pager: the held lines, the walk state,
/// the pager wait, and the folded turns with their tool calls.
#[derive(Debug, Default)]
pub(crate) struct Fold {
    /// The session the view opened on: every `history` goes to it, and
    /// it answers from its own read of the chain.
    serving: Option<SessionId>,
    /// The lines read so far, oldest first and contiguous within each
    /// segment, ancestors before the session.
    lines: Vec<Envelope>,
    /// Whether an answer held no line, or the held run reached its
    /// segment's first line: the current segment ended.
    start: bool,
    /// Whether the held run already followed its oldest segment's
    /// parent and the answer came back empty: the parent is unknown,
    /// so the walk stops instead of asking again.
    crossed: bool,
    /// Whether the walk ended: refused, failed, or rejected.
    ended: bool,
    /// The `history` request on the wire, if any.
    pending: Option<Pending>,
    /// The folded turns, oldest first.
    turns: Vec<TurnRow>,
    /// The tool calls' declared effects, oldest first.
    calls: Vec<CallRec>,
    /// The tool calls' names by session and action id.
    names: Vec<(SessionId, Option<ActionId>, String)>,
    /// Why the view shows no rows, or why a request failed.
    error: Option<String>,
}

impl Fold {
    /// An empty fold for `serving`.
    pub(crate) fn open(serving: SessionId) -> Self {
        Self {
            serving: Some(serving),
            ..Self::default()
        }
    }

    /// Whether `id` is the `history` request on the wire: only that
    /// answer folds, and only into the open rewind view.
    pub(crate) fn pending_is(&self, id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
    }

    /// Notes the `history` request on the wire.
    pub(crate) fn begin_wait(&mut self, id: String, sent_at: Option<Instant>) {
        self.pending = Some(Pending { id, sent_at });
    }

    /// Drops the `history` request: its late answer folds nowhere.
    pub(crate) fn abort_wait(&mut self) {
        self.pending = None;
    }

    /// The pending `history`'s deadline: what the frame wakes for.
    pub(crate) fn wait_deadline(&self) -> Option<Instant> {
        let sent_at = self.pending.as_ref()?.sent_at?;
        sent_at.checked_add(HISTORY_WAIT)
    }

    /// Whether the pending `history` wait passed its bound: at or past
    /// ten seconds on checked arithmetic, failing closed when the bound
    /// is unrepresentable.
    pub(crate) fn wait_due(&self, now: Instant) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let Some(sent_at) = pending.sent_at else {
            return false;
        };
        match sent_at.checked_add(HISTORY_WAIT) {
            Some(deadline) => now >= deadline,
            None => true,
        }
    }

    /// Settles the walk as refused or failed with `message`: the walk
    /// ends, and the view shows the message.
    pub(crate) fn fail_walk(&mut self, message: String) {
        self.pending = None;
        self.ended = true;
        self.error = Some(message);
    }

    /// Marks a refused page: the walk ends with the turns read so far.
    pub(crate) fn mark_ended(&mut self) {
        self.ended = true;
    }

    /// Puts `older` before the held lines. Refuses a line carrying no
    /// seq, an answer that leaves a gap within the segment (its last
    /// seq is not one below the first held line), or a new segment
    /// across the seam that the held run does not name, that names
    /// another session, or that reaches above the named point. Cut-off
    /// turns past each successor's point never fold, because the walk
    /// never asks past them and refuses them here.
    pub(crate) fn prepend(&mut self, older: Vec<Envelope>) -> Result<(), ()> {
        if older.is_empty() {
            self.start = true;
            if self.forked_from().is_some() {
                self.crossed = true;
            }
            return Ok(());
        }
        if older.iter().any(|line| line.seq.is_none()) {
            return Err(());
        }
        let crossed_seam = match (self.lines.first(), older.last()) {
            (Some(first), Some(newest)) => first.session_id != newest.session_id,
            _ => false,
        };
        if crossed_seam {
            let Some(forked) = self.forked_from() else {
                return Err(());
            };
            let newest = older.last().and_then(|line| line.seq);
            if newest.is_some_and(|seq| seq > forked.seq)
                || older
                    .iter()
                    .any(|line| line.session_id != forked.session_id)
            {
                return Err(());
            }
            self.start = false;
            self.crossed = false;
        } else if let (Some(first), Some(last)) = (
            self.lines.first().and_then(|line| line.seq),
            older.last().and_then(|line| line.seq),
        ) && last.0.saturating_add(1) != first.0
        {
            return Err(());
        }
        self.lines.splice(..0, older);
        if self.lines.first().and_then(|line| line.seq) == Some(Seq(0)) {
            self.start = true;
        }
        Ok(())
    }

    /// The parent the held run continues from, when it reached its
    /// segment's first line: the `forked_from` of the held
    /// `session_started`; `None` at the chain root or without one.
    fn forked_from(&self) -> Option<Point> {
        let first = self.lines.first()?;
        if first.kind != "session_started" || first.seq != Some(Seq(0)) {
            return None;
        }
        first
            .payload
            .get("forked_from")
            .and_then(|forked| serde_json::from_value(forked.clone()).ok())
    }

    /// Whether the walk reached the chain root: the read ended, or a
    /// segment ended with no onward parent.
    pub(crate) fn chain_done(&self) -> bool {
        self.ended || (self.start && (self.crossed || self.forked_from().is_none()))
    }

    /// The args of the next read: `to_seq` just below the first held
    /// line, or the parent the held run continues from when it reached
    /// its segment's first line. Past the seam every window names its
    /// segment. `None` with nothing held.
    pub(crate) fn next_args(&self) -> Option<Value> {
        let first = self.lines.first()?;
        let Some(serving) = &self.serving else {
            return None;
        };
        let first_seq = first.seq?.0;
        if first_seq == 0
            && let Some(forked) = self.forked_from().filter(|_| !self.crossed)
        {
            return Some(history_args(
                serving,
                &forked.session_id,
                None,
                Some(forked.seq.0),
            ));
        }
        Some(history_args(
            serving,
            &first.session_id,
            None,
            Some(first_seq.saturating_sub(1)),
        ))
    }

    /// Folds every held line into turns again: each `turn_started` one
    /// row, each step and the turn's end its boundaries, with every
    /// tool call before each boundary answered. Returns whether this
    /// is the first fold, which takes the default point.
    pub(crate) fn rebuild(&mut self) -> bool {
        let fresh = self.turns.is_empty() && self.calls.is_empty();
        let mut turns: Vec<TurnRow> = Vec::new();
        let mut current: Option<Building> = None;
        let mut started: Vec<(SessionId, Option<contract::ActionId>, u64)> = Vec::new();
        let mut completed: Vec<(SessionId, Option<contract::ActionId>, u64)> = Vec::new();
        self.calls.clear();
        self.names.clear();
        for line in &self.lines {
            let seq = line.seq.map(|seq| seq.0).unwrap_or(u64::MAX);
            match line.kind.as_str() {
                "turn_started" => {
                    if let Some(building) = current.take() {
                        turns.push(finish(building, &started, &completed));
                    }
                    let input = read_turn(line);
                    current = Some(Building {
                        session: line.session_id.clone(),
                        start: seq.saturating_sub(1),
                        after_input: seq,
                        prompt: prompt_of(&input),
                        summary: input
                            .iter()
                            .any(|item| matches!(item, InputItem::Rewind { .. })),
                        steps: Vec::new(),
                        end: None,
                    });
                }
                "step_started" => {
                    if let Some(building) = current.as_mut()
                        && building.session == line.session_id
                    {
                        building.steps.push(Step {
                            seq,
                            text: String::new(),
                        });
                    }
                }
                "text_completed" => {
                    if let Some(building) = current.as_mut()
                        && building.session == line.session_id
                        && let Some(step) = building.steps.last_mut()
                        && let Some(text) = line.payload.get("text").and_then(|text| text.as_str())
                    {
                        if !step.text.is_empty() {
                            step.text.push('\n');
                        }
                        step.text.push_str(text);
                    }
                }
                "tool_call_requested" => {
                    if let Some(requested) = read!(line, ToolCallRequested)
                        && let Some(action) = line.action_id.clone()
                    {
                        self.names.push((
                            line.session_id.clone(),
                            Some(action),
                            requested.name.clone(),
                        ));
                    }
                }
                "tool_call_started" => {
                    started.push((line.session_id.clone(), line.action_id.clone(), seq));
                    if let Some(call) = read!(line, ToolCallStarted) {
                        self.calls.push(CallRec {
                            session: line.session_id.clone(),
                            seq,
                            action: line.action_id.clone(),
                            effects: call.declared.effects.clone(),
                            paths: call.declared.paths.clone().unwrap_or_default(),
                        });
                    }
                }
                "tool_call_completed" => {
                    completed.push((line.session_id.clone(), line.action_id.clone(), seq));
                }
                "turn_completed" => {
                    if let Some(building) = current.as_mut()
                        && building.session == line.session_id
                    {
                        building.end = Some(seq);
                    }
                }
                _ => {}
            }
        }
        if let Some(building) = current.take() {
            turns.push(finish(building, &started, &completed));
        }
        self.turns = turns;
        self.error = None;
        fresh
    }

    /// The folded turns, oldest first.
    pub(crate) fn turns(&self) -> &[TurnRow] {
        &self.turns
    }

    /// Why the view shows no rows, or why a request failed.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// A turn's key by row: its session and start seq.
    pub(crate) fn turn_key(&self, at: usize) -> Option<(SessionId, u64)> {
        self.turns
            .get(at)
            .map(|turn| (turn.session.clone(), turn.start))
    }

    /// A turn's first boundary by row: the default point for its row.
    pub(crate) fn first_point(&self, at: usize) -> Option<(SessionId, u64)> {
        self.turns.get(at).and_then(|turn| {
            turn.bounds
                .first()
                .map(|bound| (bound.session.clone(), bound.seq))
        })
    }

    /// A boundary's point by rows: its turn's key with its own session
    /// and seq.
    pub(crate) fn bound_point(
        &self,
        at: usize,
        bound: usize,
    ) -> Option<((SessionId, u64), (SessionId, u64))> {
        self.turns.get(at).and_then(|turn| {
            turn.bounds.get(bound).map(|other| {
                (
                    (turn.session.clone(), turn.start),
                    (other.session.clone(), other.seq),
                )
            })
        })
    }

    /// The files written and the calls that may have changed files since
    /// the chosen point, folded from the held lines and calls.
    pub(crate) fn detail_model(&self, session: &SessionId, seq: u64) -> DetailModel {
        let mut after: Vec<&Envelope> = Vec::new();
        let mut past = false;
        for line in &self.lines {
            if !past {
                if line.session_id != *session {
                    continue;
                }
                if line.seq.is_none_or(|held| held.0 <= seq) {
                    continue;
                }
                past = true;
            }
            after.push(line);
        }
        let mut writes = Vec::new();
        let mut executes: Vec<String> = Vec::new();
        for line in after {
            let Some(call) = self.calls.iter().find(|call| {
                call.session == line.session_id && Some(call.seq) == line.seq.map(|held| held.0)
            }) else {
                continue;
            };
            if call.effects.contains(&contract::shapes::Effect::Writes) {
                for path in &call.paths {
                    if !writes.contains(path) {
                        writes.push(path.clone());
                    }
                }
            }
            if call.effects.contains(&contract::shapes::Effect::Executes) {
                let name = self
                    .names
                    .iter()
                    .find(|(at, action, _)| at == &call.session && action == &call.action)
                    .map(|(_, _, name)| name.clone())
                    .or_else(|| call.action.clone().map(|action| action.0));
                if let Some(name) = name
                    && !executes.contains(&name)
                {
                    executes.push(name);
                }
            }
        }
        DetailModel { writes, executes }
    }
}

/// A `turn_started` line's input, when it parses; empty when it does
/// not, so an unreadable turn still lists with no prompt.
fn read_turn(line: &Envelope) -> Vec<InputItem> {
    serde_json::from_value::<TurnStarted>(Value::Object(line.payload.clone()))
        .map(|started| started.input)
        .unwrap_or_default()
}

/// The row's prompt: the first driver message's text, flattened to one
/// line; empty when no driver message started the turn.
fn prompt_of(input: &[InputItem]) -> String {
    input
        .iter()
        .find_map(|item| {
            if let InputItem::Message {
                content, sender, ..
            } = item
                && sender.origin == Origin::Driver
            {
                Some(
                    content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text.as_str()),
                            ContentPart::Image { .. }
                            | ContentPart::Pdf(_)
                            | ContentPart::Unknown => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                None
            }
        })
        .map(|prompt| prompt.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// Whether every tool call started at or below `seq` in `session`
/// answered at or below it: the point's next line starts a turn or a
/// step only then.
fn answered(
    session: &SessionId,
    seq: u64,
    started: &[(SessionId, Option<contract::ActionId>, u64)],
    completed: &[(SessionId, Option<contract::ActionId>, u64)],
) -> bool {
    started
        .iter()
        .filter(|(at, _, from)| at == session && *from <= seq)
        .all(|(at, action, _)| {
            completed.iter().any(|(done_at, done_action, done)| {
                done_at == at && done_action == action && *done <= seq
            })
        })
}

/// One folded turn: its boundaries oldest first, each standing only
/// where every tool call before it answered.
fn finish(
    building: Building,
    started: &[(SessionId, Option<contract::ActionId>, u64)],
    completed: &[(SessionId, Option<contract::ActionId>, u64)],
) -> TurnRow {
    let mut bounds = vec![
        Boundary {
            session: building.session.clone(),
            seq: building.start,
            kind: BoundKind::Start,
            summary: String::new(),
        },
        Boundary {
            session: building.session.clone(),
            seq: building.after_input,
            kind: BoundKind::AfterInput,
            summary: String::new(),
        },
    ];
    // Every step but the last ends where the next step starts; the
    // last step ends where the turn does, when it did.
    let mut previous: Option<&Step> = None;
    for (index, step) in building.steps.iter().enumerate() {
        if let Some(ended) = previous {
            bounds.push(Boundary {
                session: building.session.clone(),
                seq: step.seq.saturating_sub(1),
                kind: BoundKind::AfterStep {
                    step: u32::try_from(index).unwrap_or(u32::MAX),
                },
                summary: ended.text.clone(),
            });
        }
        previous = Some(step);
    }
    if let Some(end) = building.end {
        // The last step ends where the turn does: its summary rides on
        // the end boundary, as every step's summary shows at its end
        // (`docs/tui.md`, "Rewind").
        let summary = previous.map(|ended| ended.text.clone()).unwrap_or_default();
        bounds.push(Boundary {
            session: building.session.clone(),
            seq: end,
            kind: BoundKind::End,
            summary,
        });
    }
    bounds.retain(|bound| answered(&bound.session, bound.seq, started, completed));
    TurnRow {
        session: building.session,
        start: building.start,
        prompt: building.prompt,
        summary: building.summary,
        bounds,
    }
}
