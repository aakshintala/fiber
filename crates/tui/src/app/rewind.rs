//! The `/rewind` view (`docs/tui.md`, "Rewind"): the conversation's turns
//! back through the `forked_from` chain, each opening into its step
//! boundaries, with the point's files, calls and running jobs below.
//!
//! Opening the view sends one `history` for the attached session and
//! pages back through each ancestor the chain names, stopping each
//! segment at its successor's `forked_from.seq`, at most one request on
//! the wire. Every pending request carries the frame time it went out
//! at; past ten seconds (`docs/tui.md`, "History and paging") the frame
//! ends the wait as a lost connection, and a late answer folds nowhere.

use std::time::{Duration, Instant};

use contract::events::{CommandAccepted, CommandRejected, InputItem, TurnStarted};
use contract::shapes::{ContentPart, Origin, Point};
use contract::{Envelope, Seq, SessionId};
use serde_json::Value;

use super::{App, Effect, history_args, mint, session_command};
use crate::link::history_answer;
use crate::swapped::{Frame, Ink, List};

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

/// One turn's row: its prompt and every step boundary it opens into.
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

/// The rewind view's state: the folded turns, the chain pager, the
/// selection, and what the view is waiting on.
#[derive(Debug, Default)]
pub(crate) struct Rewind {
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
    /// The selected turn's key: its session and start seq.
    selected: Option<(SessionId, u64)>,
    /// Why the view shows no rows, or why a request failed.
    error: Option<String>,
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

impl Rewind {
    /// An empty fold for `serving`.
    fn open(serving: SessionId) -> Self {
        Self {
            serving: Some(serving),
            ..Self::default()
        }
    }

    /// Puts `older` before the held lines. Refuses a line carrying no
    /// seq, an answer that leaves a gap within the segment (its last
    /// seq is not one below the first held line), or a new segment
    /// across the seam that the held run does not name, that names
    /// another session, or that reaches above the named point. Cut-off
    /// turns past each successor's point never fold, because the walk
    /// never asks past them and refuses them here.
    fn prepend(&mut self, older: Vec<Envelope>) -> Result<(), ()> {
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
    fn chain_done(&self) -> bool {
        self.ended || (self.start && (self.crossed || self.forked_from().is_none()))
    }

    /// The args of the next read: `to_seq` just below the first held
    /// line, or the parent the held run continues from when it reached
    /// its segment's first line. Past the seam every window names its
    /// segment. `None` with nothing held.
    fn next_args(&self) -> Option<Value> {
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
    /// tool call before each boundary answered. The selection keeps its
    /// turn's key, else takes the default: the start of the latest turn
    /// that is not a summary's. Returns the selected row, if any.
    fn refold(&mut self) -> Option<usize> {
        let mut turns: Vec<TurnRow> = Vec::new();
        let mut current: Option<Building> = None;
        let mut started: Vec<(SessionId, Option<contract::ActionId>, u64)> = Vec::new();
        let mut completed: Vec<(SessionId, Option<contract::ActionId>, u64)> = Vec::new();
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
                "tool_call_started" => {
                    started.push((line.session_id.clone(), line.action_id.clone(), seq));
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
        // The selection keeps its turn's key across pages; a missing
        // key takes the default.
        let at = self
            .selected
            .clone()
            .and_then(|key| {
                self.turns
                    .iter()
                    .position(|turn| turn.session == key.0 && turn.start == key.1)
            })
            .or_else(|| self.turns.iter().rposition(|turn| !turn.summary));
        self.selected = at.and_then(|at| {
            self.turns
                .get(at)
                .map(|turn| (turn.session.clone(), turn.start))
        });
        at
    }

    /// A late answer for a dropped wait folds nowhere: only the pending
    /// id folds, and only into the open rewind view.
    fn pending_is(&self, id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
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
        bounds.push(Boundary {
            session: building.session.clone(),
            seq: end,
            kind: BoundKind::End,
            summary: String::new(),
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

impl App {
    /// Opens the rewind view on the attached session and sends its
    /// first `history`: the walk pages older segments as their answers
    /// land, at most one request on the wire. The view opens before
    /// this runs.
    pub(in crate::app) fn open_rewind(&mut self) -> Effect {
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        self.rewind = Rewind::open(session.clone());
        let id = mint();
        let line = session_command(
            &id,
            "history",
            &session,
            Some(history_args(&session, &session, None, None)),
        );
        self.rewind.pending = Some(Pending {
            id: id.clone(),
            sent_at: self.motion().now_instant(),
        });
        Effect::Send(vec![line.to_string()])
    }

    /// Folds a `command_accepted` into the open rewind view when it
    /// answers the pending `history`: a late answer, or another view's,
    /// matches no pending id and folds nowhere. Answers the next page
    /// while the chain runs on, so one request stays on the wire.
    pub(crate) fn rewind_answered(
        &mut self,
        envelope: &Envelope,
        accepted: &CommandAccepted,
    ) -> Vec<String> {
        if !self.rewind.pending_is(&accepted.command_id.0) {
            return Vec::new();
        }
        self.rewind.pending = None;
        match history_answer(envelope) {
            Err(message) => {
                self.rewind.ended = true;
                self.rewind.error = Some(message);
            }
            Ok(older) => {
                if self.rewind.prepend(older).is_err() {
                    self.rewind.ended = true;
                } else {
                    self.rewind.error = None;
                }
                let at = self.rewind.refold();
                let rows = self.rewind.turns.len();
                self.rewind_list_to(at.unwrap_or(0), rows);
            }
        }
        if self.rewind.chain_done() {
            return Vec::new();
        }
        let Some(args) = self.rewind.next_args() else {
            return Vec::new();
        };
        let Some(session) = self.session().cloned() else {
            return Vec::new();
        };
        let id = mint();
        let line = session_command(&id, "history", &session, Some(args));
        self.rewind.pending = Some(Pending {
            id: id.clone(),
            sent_at: self.motion().now_instant(),
        });
        vec![line.to_string()]
    }

    /// A `command_rejected` for the pending `history` ends the walk with
    /// the rejection's text; any other id reaches no rewind state.
    pub(in crate::app) fn rewind_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        if !self.rewind.pending_is(&id.0) {
            return;
        }
        self.rewind.pending = None;
        self.rewind.ended = true;
        self.rewind.error = Some(rejected.message.clone());
    }

    /// The connection ended: the pending `history` drops, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_lost(&mut self) {
        self.rewind.pending = None;
    }

    /// The stream reopened: a `history` wait ends at once, and its late
    /// answer folds nowhere.
    pub(in crate::app) fn rewind_reopened(&mut self) {
        self.rewind.pending = None;
    }

    /// The pending `history`'s deadline: what the frame wakes for.
    pub(in crate::app) fn rewind_wake(&self) -> Option<Instant> {
        let sent_at = self.rewind.pending.as_ref()?.sent_at?;
        sent_at.checked_add(HISTORY_WAIT)
    }

    /// Whether the pending `history` wait passed its bound: at or past
    /// ten seconds on checked arithmetic, failing closed when the bound
    /// is unrepresentable.
    pub(crate) fn rewind_due(&mut self, now: Instant) -> bool {
        let Some(pending) = &self.rewind.pending else {
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

    /// One turn's step boundaries, oldest first, as holding sessions
    /// and seqs. Tests only: production reads the fold through the
    /// frame.
    #[cfg(test)]
    pub(crate) fn rewind_turn_bounds(&self, at: usize) -> Vec<(String, u64)> {
        self.rewind
            .turns
            .get(at)
            .map(|turn| {
                turn.bounds
                    .iter()
                    .map(|bound| (bound.session.0.clone(), bound.seq))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The view's frame: one row per turn, the error or the empty note
/// below them.
pub(crate) fn frame(state: &Rewind, list: List) -> Frame {
    let rows = state
        .turns
        .iter()
        .map(|turn| {
            let label = if turn.summary {
                "rewind summary".to_owned()
            } else {
                turn.prompt.clone()
            };
            vec![(format!("before: {label}"), None, Ink::Plain)]
        })
        .collect();
    let mut below = Vec::new();
    if let Some(error) = &state.error {
        below.push(error.clone());
    } else if state.turns.is_empty() {
        below.push("No turns to rewind.".to_owned());
    }
    Frame {
        title: "Rewind".to_owned(),
        command: "/rewind".to_owned(),
        rows,
        list,
        below,
        field: None,
        footer: "↑↓ move · Esc close".to_owned(),
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
