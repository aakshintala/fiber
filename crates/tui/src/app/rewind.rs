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

use contract::commands::RewindArgs;
use contract::events::{
    CommandAccepted, CommandRejected, CommandResult, InputItem, ToolCallRequested, ToolCallStarted,
    TurnStarted,
};
use contract::shapes::{ContentPart, Effect as Declared, Origin, Point};
use contract::{ActionId, CommandId, Envelope, ErrorCode, Seq, SessionId};
use serde_json::Value;

use super::{App, Effect, history_args, mint, session_command};
use crate::home::Level;
use crate::keys::Key;
use crate::link::history_answer;
use crate::swapped::{Frame, Ink, List, Spot};

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

/// Where the list cursor sits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Sel {
    /// On a turn's row: its session and start seq.
    Turn(SessionId, u64),
    /// On a boundary's row: its turn's session and start seq, and the
    /// boundary's seq.
    Bound(SessionId, u64, u64),
    /// On the summary checkbox row.
    #[default]
    Check,
    /// On the Rewind row.
    Action,
    /// On the retry row a `summary_failed` refusal offers.
    Retry,
    /// On the rewind-without-summary row it offers.
    NoSummary,
    /// On the cancel row it offers.
    Cancel,
}

/// A `rewind` request waiting for its answer.
#[derive(Debug)]
struct SendPending {
    /// The command id the answer names.
    id: String,
}

/// One tool call taking shape while its lines fold: what it declared.
#[derive(Debug)]
struct CallRec {
    session: SessionId,
    seq: u64,
    action: Option<ActionId>,
    effects: Vec<Declared>,
    paths: Vec<String>,
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
    /// The tool calls' declared effects, oldest first.
    calls: Vec<CallRec>,
    /// The tool calls' names by session and action id.
    names: Vec<(SessionId, Option<ActionId>, String)>,
    /// Where the list cursor sits.
    selected: Sel,
    /// The chosen point's holding session and seq.
    point: Option<(SessionId, u64)>,
    /// The turn open into its step boundaries, by session and start.
    expanded: Option<(SessionId, u64)>,
    /// Whether the rewind summarises the path after the point.
    summarise: bool,
    /// The `rewind` request on the wire, if any.
    send: Option<SendPending>,
    /// Why the last `rewind` failed, standing in place of the button.
    send_error: Option<String>,
    /// The standing error is a `summary_failed`: its three choices draw
    /// under the Rewind row.
    summary_failed: bool,
    /// Why the view shows no rows, or why a request failed.
    error: Option<String>,
}

/// One visible row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vis {
    /// A turn's row.
    Turn(usize),
    /// A step boundary's row: the turn and the boundary.
    Bound(usize, usize),
    /// The summary checkbox row.
    Check,
    /// The Rewind row.
    Action,
    /// The retry row a `summary_failed` refusal offers.
    Retry,
    /// The rewind-without-summary row it offers.
    NoSummary,
    /// The cancel row it offers.
    Cancel,
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
    /// tool call before each boundary answered. The cursor keeps its
    /// row's identity across pages; the first fold takes the default:
    /// the start of the latest turn that is not a summary's. Returns
    /// the selected row.
    fn refold(&mut self) -> usize {
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
                    if let Some(requested) = super::read!(line, ToolCallRequested)
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
                    if let Some(call) = super::read!(line, ToolCallStarted) {
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
        self.sync_selection(fresh)
    }

    /// The default point: the start of the latest turn that is not a
    /// summary's and still holds a boundary.
    fn default_point(&self) -> Option<(SessionId, u64)> {
        self.turns
            .iter()
            .rev()
            .find(|turn| !turn.summary && !turn.bounds.is_empty())
            .and_then(|turn| {
                turn.bounds
                    .first()
                    .map(|bound| (bound.session.clone(), bound.seq))
            })
    }

    /// Whether `key` names a folded turn.
    fn has_turn(&self, key: &(SessionId, u64)) -> bool {
        self.turns
            .iter()
            .any(|turn| turn.session == key.0 && turn.start == key.1)
    }

    /// Whether `session` and `seq` name a folded boundary.
    fn has_bound(&self, session: &SessionId, seq: u64) -> bool {
        self.turns.iter().any(|turn| {
            turn.bounds
                .iter()
                .any(|bound| bound.session == *session && bound.seq == seq)
        })
    }

    /// Anchors the cursor and the point on rows that still exist: the
    /// expansion keeps its turn, the cursor keeps its row's identity,
    /// and the point follows content rows. A missing anchor takes the
    /// default; the first fold always does. Returns the selected row.
    fn sync_selection(&mut self, fresh: bool) -> usize {
        if let Some(expanded) = self.expanded.clone()
            && !self.has_turn(&expanded)
        {
            self.expanded = None;
        }
        if let Some(point) = self.point.clone()
            && !self.has_bound(&point.0, point.1)
        {
            self.point = None;
        }
        if self.point.is_none() {
            self.point = self.default_point();
        }
        if fresh || !self.sel_kept() {
            // The cursor sits on the point's row: a start boundary
            // selects its turn row, any other its boundary row. Starts
            // resolve across every turn first, because a turn's end and
            // the next turn's start share one seq.
            let at = self.point.clone().and_then(|point| {
                let start = self.turns.iter().enumerate().find_map(|(at, turn)| {
                    turn.bounds
                        .first()
                        .filter(|bound| bound.session == point.0 && bound.seq == point.1)
                        .map(|_| (at, 0))
                });
                start.or_else(|| {
                    self.turns.iter().enumerate().find_map(|(at, turn)| {
                        turn.bounds
                            .iter()
                            .position(|bound| bound.session == point.0 && bound.seq == point.1)
                            .map(|bound| (at, bound))
                    })
                })
            });
            self.selected = match at {
                Some((at, 0)) => self.turns.get(at).map_or(Sel::Check, |turn| {
                    Sel::Turn(turn.session.clone(), turn.start)
                }),
                Some((at, bound)) => self.turns.get(at).map_or(Sel::Check, |turn| {
                    turn.bounds.get(bound).map_or(Sel::Check, |other| {
                        Sel::Bound(turn.session.clone(), turn.start, other.seq)
                    })
                }),
                None => Sel::Check,
            };
        }
        self.visible()
            .iter()
            .position(|row| self.matches_selected(row))
            .unwrap_or(0)
    }

    /// Whether the cursor's row still exists.
    fn sel_kept(&self) -> bool {
        match &self.selected {
            Sel::Turn(session, start) => self.has_turn(&(session.clone(), *start)),
            Sel::Bound(session, start, seq) => self.turns.iter().any(|turn| {
                turn.session == *session
                    && turn.start == *start
                    && turn.bounds.iter().any(|bound| bound.seq == *seq)
            }),
            Sel::Check | Sel::Action => true,
            Sel::Retry | Sel::NoSummary | Sel::Cancel => true,
        }
    }

    /// Whether a visible row is the cursor's row.
    fn matches_selected(&self, row: &Vis) -> bool {
        match (row, &self.selected) {
            (Vis::Turn(at), Sel::Turn(session, start)) => self
                .turns
                .get(*at)
                .is_some_and(|turn| turn.session == *session && turn.start == *start),
            (Vis::Bound(at, bound), Sel::Bound(session, start, seq)) => {
                self.turns.get(*at).is_some_and(|turn| {
                    turn.session == *session
                        && turn.start == *start
                        && turn
                            .bounds
                            .get(*bound)
                            .is_some_and(|other| other.seq == *seq)
                })
            }
            (Vis::Check, Sel::Check) | (Vis::Action, Sel::Action) => true,
            (Vis::Retry, Sel::Retry)
            | (Vis::NoSummary, Sel::NoSummary)
            | (Vis::Cancel, Sel::Cancel) => true,
            _ => false,
        }
    }

    /// Every visible row: each turn, the open turn's boundaries under
    /// it, then the checkbox and the Rewind row, then a
    /// `summary_failed` refusal's three choices.
    fn visible(&self) -> Vec<Vis> {
        let mut out = Vec::new();
        for (at, turn) in self.turns.iter().enumerate() {
            out.push(Vis::Turn(at));
            if self
                .expanded
                .as_ref()
                .is_some_and(|key| *key == (turn.session.clone(), turn.start))
            {
                for bound in 0..turn.bounds.len() {
                    out.push(Vis::Bound(at, bound));
                }
            }
        }
        out.push(Vis::Check);
        out.push(Vis::Action);
        if self.summary_failed {
            out.push(Vis::Retry);
            out.push(Vis::NoSummary);
            out.push(Vis::Cancel);
        }
        out
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
                let rows = self.rewind.visible().len();
                self.rewind_list_to(at, rows);
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

    /// A `command_rejected` for a pending request settles it with the
    /// rejection's text: the walk ends for `history`, the button shows
    /// it for `rewind`, with its three choices for a `summary_failed`.
    /// Any other id reaches no rewind state.
    pub(in crate::app) fn rewind_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        if self.rewind.pending_is(&id.0) {
            self.rewind.pending = None;
            self.rewind.ended = true;
            self.rewind.error = Some(rejected.message.clone());
        } else if self
            .rewind
            .send
            .as_ref()
            .is_some_and(|send| send.id == id.0)
        {
            self.rewind.send = None;
            self.rewind.send_error = Some(rejected.message.clone());
            self.rewind.summary_failed = rejected.code == ErrorCode::SummaryFailed;
        }
    }

    /// A hub-relayed `command_rejected` for the sent `rewind` settles
    /// it with the start's code: the terminal never moves. True when
    /// the id was the send's; any other id reaches no rewind state.
    pub(in crate::app) fn rewind_hub_refused(
        &mut self,
        id: &str,
        code: &str,
        message: &str,
    ) -> bool {
        if !self.rewind.send.as_ref().is_some_and(|send| send.id == id) {
            return false;
        }
        // The hub carries the code as text; an unknown one still
        // settles, read as itself.
        let code = serde_json::from_value(Value::String(code.to_owned()))
            .unwrap_or_else(|_| ErrorCode::Other(code.to_owned()));
        self.rewind_refused(&CommandRejected {
            command_id: Some(CommandId(id.to_owned())),
            code,
            message: message.to_owned(),
        });
        true
    }

    /// A `command_accepted` answering the sent `rewind` with its new
    /// session subscribes full to it, asks its commands, attaches, and
    /// closes the view. The reset attachment settles the send. Any
    /// other id, or another command's shape, falls through to the
    /// history path.
    pub(in crate::app) fn rewind_send_accepted(
        &mut self,
        accepted: &CommandAccepted,
    ) -> Option<Vec<String>> {
        let send = self.rewind.send.as_ref()?;
        if send.id != accepted.command_id.0 {
            return None;
        }
        let Some(CommandResult::Rewind { new_session_id }) = accepted.result.as_ref() else {
            return None;
        };
        let new = new_session_id.clone();
        // A new attachment asks the new session's commands, as a clear
        // `start` accept does for its session.
        let lines = vec![self.subscribe(&new, Level::Full), self.ask_commands(&new)];
        self.attach(new);
        self.close_session_view();
        Some(lines)
    }

    /// Clears the standing refusal with its choices, if any.
    fn clear_send_error(&mut self) {
        self.rewind.send_error = None;
        self.rewind.summary_failed = false;
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

    /// The view's rows and its height: what the cursor moves over.
    fn rewind_metrics(&self) -> Option<(usize, usize)> {
        let frame = self.session_view_screen(0)?;
        Some((
            frame.rows.len(),
            crate::swapped::rows_height(&frame, self.conversation_height()),
        ))
    }

    /// Every job that stops with the rewind: the running jobs beside
    /// the running delegates, a delegate listed like other jobs.
    pub(in crate::app) fn rewind_stopping(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .panel_state
            .running_jobs()
            .into_iter()
            .map(|(_, description)| description.to_owned())
            .collect();
        out.extend(
            self.panel_state
                .running_delegates()
                .into_iter()
                .map(|(_, description)| description.to_owned()),
        );
        out
    }

    /// Handles the open rewind view's keys: moving, opening turns,
    /// sending from a boundary or the Rewind row, toggling the summary
    /// checkbox, and Esc, which closes the view when no `rewind` is
    /// pending and is ignored while one is.
    pub(in crate::app) fn rewind_key(&mut self, key: &Key) -> Option<Effect> {
        if *key == Key::Esc {
            if self.rewind.send.is_some() {
                return Some(Effect::None);
            }
            self.close_session_view();
            return Some(Effect::None);
        }
        if *key == Key::Enter {
            return Some(self.rewind_enter());
        }
        if *key == Key::Char(' ') {
            self.rewind.summarise = !self.rewind.summarise;
            return Some(Effect::None);
        }
        let Some((rows, height)) = self.rewind_metrics() else {
            return Some(Effect::None);
        };
        let (before, after) = match self.rewind_list_mut() {
            Some(list) => {
                let before = list.selected();
                list.key(key, rows, height);
                (before, list.selected())
            }
            None => return Some(Effect::None),
        };
        if after != before {
            self.rewind_follow_selection(after);
        }
        Some(Effect::None)
    }

    /// Handles the open rewind view's clicks: closing, selecting,
    /// toggling the checkbox, and sending from the Rewind row.
    pub(in crate::app) fn rewind_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Close => {
                if self.rewind.send.is_some() {
                    return Effect::None;
                }
                self.close_session_view();
            }
            Spot::Row(at) => self.rewind_select_at(at),
            Spot::Cell(at, _) => match self.rewind.visible().get(at) {
                Some(Vis::Check) => self.rewind.summarise = !self.rewind.summarise,
                Some(Vis::Action) => return self.send_rewind(),
                Some(Vis::Retry) => return self.send_rewind_as(true),
                Some(Vis::NoSummary) => return self.send_rewind_as(false),
                Some(Vis::Cancel) => self.clear_send_error(),
                _ => self.rewind_select_at(at),
            },
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_) => {}
        }
        Effect::None
    }

    /// Enter on the cursor's row: opens or closes a turn, toggles the
    /// checkbox, or sends the rewind from the Rewind row.
    fn rewind_enter(&mut self) -> Effect {
        let visible = self.rewind.visible();
        let at = self
            .rewind_list_mut()
            .map(|list| list.selected())
            .unwrap_or(0);
        match visible.get(at) {
            Some(Vis::Turn(ti)) => {
                let key = self
                    .rewind
                    .turns
                    .get(*ti)
                    .map(|turn| (turn.session.clone(), turn.start));
                if self.rewind.expanded == key {
                    self.rewind.expanded = None;
                } else {
                    self.rewind.expanded = key;
                }
                self.reseat();
            }
            Some(Vis::Bound(ti, bound)) => {
                // Enter on a boundary row sends from that point at
                // once: reaching the Rewind row crosses content rows,
                // which would move the point.
                let point = self.rewind.turns.get(*ti).and_then(|turn| {
                    turn.bounds
                        .get(*bound)
                        .map(|other| (other.session.clone(), other.seq))
                });
                if let Some(point) = point {
                    self.rewind.point = Some(point);
                }
                return self.send_rewind();
            }
            Some(Vis::Check) => {
                self.rewind.summarise = !self.rewind.summarise;
            }
            Some(Vis::Action) => return self.send_rewind(),
            // A `summary_failed` refusal's choices: retry with the
            // summary on, rewind with it off, or back to the list.
            Some(Vis::Retry) => return self.send_rewind_as(true),
            Some(Vis::NoSummary) => return self.send_rewind_as(false),
            Some(Vis::Cancel) => self.clear_send_error(),
            None => {}
        }
        Effect::None
    }

    /// Moves the cursor onto a visible row and follows content rows
    /// with the point.
    fn rewind_select_at(&mut self, at: usize) {
        let visible = self.rewind.visible();
        let Some(row) = visible.get(at) else {
            return;
        };
        match row {
            Vis::Turn(ti) => {
                let key = self
                    .rewind
                    .turns
                    .get(*ti)
                    .map(|turn| (turn.session.clone(), turn.start));
                self.rewind.selected = match key.clone() {
                    Some((session, start)) => Sel::Turn(session, start),
                    None => Sel::Check,
                };
                self.rewind.point = key.and_then(|_| {
                    self.rewind.turns.get(*ti).and_then(|turn| {
                        turn.bounds
                            .first()
                            .map(|bound| (bound.session.clone(), bound.seq))
                    })
                });
                self.reseat_at(at);
            }
            Vis::Bound(ti, bound) => {
                let found = self.rewind.turns.get(*ti).and_then(|turn| {
                    turn.bounds.get(*bound).map(|other| {
                        (
                            (turn.session.clone(), turn.start),
                            (other.session.clone(), other.seq),
                        )
                    })
                });
                if let Some(((session, start), point)) = found {
                    self.rewind.selected = Sel::Bound(session, start, point.1);
                    self.rewind.point = Some(point);
                }
                self.reseat_at(at);
            }
            Vis::Check => {
                self.rewind.selected = Sel::Check;
                self.reseat_at(at);
            }
            Vis::Action => {
                self.rewind.selected = Sel::Action;
                self.reseat_at(at);
            }
            Vis::Retry => {
                self.rewind.selected = Sel::Retry;
                self.reseat_at(at);
            }
            Vis::NoSummary => {
                self.rewind.selected = Sel::NoSummary;
                self.reseat_at(at);
            }
            Vis::Cancel => {
                self.rewind.selected = Sel::Cancel;
                self.reseat_at(at);
            }
        }
    }

    /// Follows a cursor move onto content rows with the point.
    fn rewind_follow_selection(&mut self, at: usize) {
        let visible = self.rewind.visible();
        match visible.get(at) {
            Some(Vis::Turn(ti)) => {
                if let Some(turn) = self.rewind.turns.get(*ti) {
                    self.rewind.selected = Sel::Turn(turn.session.clone(), turn.start);
                    self.rewind.point = turn
                        .bounds
                        .first()
                        .map(|bound| (bound.session.clone(), bound.seq));
                }
            }
            Some(Vis::Bound(ti, bound)) => {
                let found = self.rewind.turns.get(*ti).and_then(|turn| {
                    turn.bounds.get(*bound).map(|other| {
                        (
                            (turn.session.clone(), turn.start),
                            (other.session.clone(), other.seq),
                        )
                    })
                });
                if let Some(((session, start), point)) = found {
                    self.rewind.selected = Sel::Bound(session, start, point.1);
                    self.rewind.point = Some(point);
                }
            }
            Some(Vis::Check) => self.rewind.selected = Sel::Check,
            Some(Vis::Action) => self.rewind.selected = Sel::Action,
            Some(Vis::Retry) => self.rewind.selected = Sel::Retry,
            Some(Vis::NoSummary) => self.rewind.selected = Sel::NoSummary,
            Some(Vis::Cancel) => self.rewind.selected = Sel::Cancel,
            None => {}
        }
    }

    /// Moves the cursor back onto the selection after the rows change.
    fn reseat(&mut self) {
        let at = self.rewind.sync_selection(false);
        let rows = self.rewind.visible().len();
        self.rewind_list_to(at, rows);
    }

    /// Moves the cursor onto a row the click chose.
    fn reseat_at(&mut self, at: usize) {
        let rows = self.rewind.visible().len();
        self.rewind_list_to(at.min(rows.saturating_sub(1)), rows);
    }

    /// Sends the rewind for the chosen point: `from_session_id` names
    /// the holding session only past the attached one, `summarise`
    /// rides the checkbox. One request at a time, and none with no
    /// point, no session, a wait on the wire, or a refusal standing.
    fn send_rewind(&mut self) -> Effect {
        // No point, no session, a wait on the wire, or a refusal
        // standing: with no turns there is no point either, so the
        // point check disables the empty view.
        if self.rewind.send.is_some() || self.rewind.send_error.is_some() {
            return Effect::None;
        }
        let Some((session, seq)) = self.rewind.point.clone() else {
            return Effect::None;
        };
        let Some(serving) = self.session().cloned() else {
            return Effect::None;
        };
        let from = if session == serving {
            None
        } else {
            Some(session)
        };
        let args = serde_json::to_value(RewindArgs {
            from_session_id: from,
            seq: Some(Seq(seq)),
            summarise: self.rewind.summarise,
        })
        .unwrap_or(Value::Null);
        let id = mint();
        let line = session_command(&id, "rewind", &serving, Some(args));
        self.rewind.send = Some(SendPending { id: id.clone() });
        Effect::Send(vec![line.to_string()])
    }

    /// Sends a fresh `rewind` for the chosen point with the summary
    /// flag the choice names, clearing the standing refusal first: a
    /// retry keeps the summary on, a rewind without summary turns it
    /// off, whatever the checkbox says.
    fn send_rewind_as(&mut self, summarise: bool) -> Effect {
        if self.rewind.send.is_some() {
            return Effect::None;
        }
        self.clear_send_error();
        let saved = std::mem::replace(&mut self.rewind.summarise, summarise);
        let out = self.send_rewind();
        self.rewind.summarise = saved;
        out
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

/// The view's frame: each turn, the open turn's step boundaries under
/// it, then the summary checkbox and the Rewind row. The detail under
/// the rows names the chosen point's files and calls, that none of it
/// is undone, and every job that stops.
pub(crate) fn frame(state: &Rewind, jobs: &[String], list: List) -> Frame {
    let mut rows = Vec::new();
    for vis in state.visible() {
        match vis {
            Vis::Turn(at) => {
                let Some(turn) = state.turns.get(at) else {
                    continue;
                };
                let label = if turn.summary {
                    "rewind summary".to_owned()
                } else {
                    turn.prompt.clone()
                };
                rows.push(vec![(format!("before: {label}"), None, Ink::Plain)]);
            }
            Vis::Bound(at, bound) => {
                let Some(label) = state
                    .turns
                    .get(at)
                    .and_then(|turn| turn.bounds.get(bound).map(bound_label))
                else {
                    continue;
                };
                rows.push(vec![(label, None, Ink::Plain)]);
            }
            Vis::Check => {
                let mark = if state.summarise { "[x]" } else { "[ ]" };
                let at = rows.len();
                rows.push(vec![(
                    format!("{mark} Summarise the path after the point"),
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )]);
            }
            Vis::Action => {
                let at = rows.len();
                let (text, ink) = action_row(state);
                rows.push(vec![(text, Some(Spot::Cell(at, 0)), ink)]);
            }
            Vis::Retry => {
                let at = rows.len();
                rows.push(vec![(
                    "Retry with summary".to_owned(),
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )]);
            }
            Vis::NoSummary => {
                let at = rows.len();
                rows.push(vec![(
                    "Rewind without summary".to_owned(),
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )]);
            }
            Vis::Cancel => {
                let at = rows.len();
                rows.push(vec![(
                    "Cancel".to_owned(),
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )]);
            }
        }
    }
    let mut below = Vec::new();
    if let Some(error) = &state.error {
        below.push(error.clone());
    } else if state.turns.is_empty() {
        below.push("No turns to rewind.".to_owned());
    } else {
        below.extend(detail(state, jobs));
    }
    Frame {
        title: "Rewind".to_owned(),
        command: "/rewind".to_owned(),
        rows,
        list,
        below,
        field: None,
        footer: "↑↓ move · Enter open/send · Space summarise · Esc close".to_owned(),
    }
}

/// One step boundary's row: where it sits and, for a step, what the
/// step said first.
fn bound_label(bound: &Boundary) -> String {
    match &bound.kind {
        BoundKind::Start => "  start of turn".to_owned(),
        BoundKind::AfterInput => "  after input".to_owned(),
        BoundKind::AfterStep { step } => {
            let first = bound.summary.split('\n').next().unwrap_or_default().trim();
            if first.is_empty() {
                format!("  after step {step}")
            } else {
                format!("  after step {step} · {first}")
            }
        }
        BoundKind::End => {
            let first = bound.summary.split('\n').next().unwrap_or_default().trim();
            if first.is_empty() {
                "  end of turn".to_owned()
            } else {
                format!("  end of turn · {first}")
            }
        }
    }
}

/// The Rewind row: the button, its busy state, its disabled state, or
/// the last refusal's text standing in the button's place.
fn action_row(state: &Rewind) -> (String, Ink) {
    if let Some(error) = &state.send_error {
        (error.clone(), Ink::Plain)
    } else if state.send.is_some() {
        ("Sending…".to_owned(), Ink::Muted)
    } else if state.turns.is_empty() {
        ("Rewind".to_owned(), Ink::Muted)
    } else {
        ("Rewind".to_owned(), Ink::Plain)
    }
}

/// The detail under the rows: the chosen point's files Fiber's tools
/// wrote since and calls that may have changed files, that none of it
/// is undone, and every job that stops. With no chosen point the view
/// says what to pick.
fn detail(state: &Rewind, jobs: &[String]) -> Vec<String> {
    let Some((session, seq)) = state.point.clone() else {
        return vec!["Select a step boundary to rewind.".to_owned()];
    };
    let after: Vec<&Envelope> = {
        let mut out = Vec::new();
        let mut past = false;
        for line in &state.lines {
            if !past {
                if line.session_id != session {
                    continue;
                }
                if line.seq.is_none_or(|held| held.0 <= seq) {
                    continue;
                }
                past = true;
            }
            out.push(line);
        }
        out
    };
    let mut writes = Vec::new();
    let mut executes = Vec::new();
    for line in after {
        let Some(call) = state.calls.iter().find(|call| {
            call.session == line.session_id && Some(call.seq) == line.seq.map(|held| held.0)
        }) else {
            continue;
        };
        if call.effects.contains(&Declared::Writes) {
            for path in &call.paths {
                if !writes.contains(path) {
                    writes.push(path.clone());
                }
            }
        }
        if call.effects.contains(&Declared::Executes) {
            let name = state
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
    vec![
        format!(
            "Files written since the point: {}",
            or_none(&writes.join(", "))
        ),
        format!(
            "Calls that may have changed files: {}",
            or_none(&executes.join(", "))
        ),
        "None of it is undone.".to_owned(),
        format!("Jobs that stop: {}", or_none(&jobs.join(", "))),
    ]
}

/// An empty list reads as none.
fn or_none(list: &str) -> &str {
    if list.is_empty() { "none" } else { list }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
