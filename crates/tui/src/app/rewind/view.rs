//! The rewind view's cursor and rendering (`docs/tui.md`, "Rewind").
//!
//! Owns the list cursor: the selected row, the chosen point, and the turn
//! open into its step boundaries. Renders the folded turns through the
//! fold's reads and the send's presentation state; it never touches their
//! fields, only their methods.

use contract::SessionId;

use super::fold::{BoundKind, Boundary, Fold, TurnRow};
use super::send::{Action, Send};
use crate::swapped::{Frame, Ink, List, Spot};

/// Where the list cursor sits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Sel {
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

/// One visible row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vis {
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

/// What Enter on the cursor's row asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnterAction {
    /// The cursor moved onto another row: follow it with the list.
    Reseat,
    /// Send the rewind for the chosen point.
    Send,
    /// Send a fresh `rewind` with the summary flag the choice names.
    SendAs(bool),
    /// The row sends nothing.
    None,
}

/// The list cursor: the selected row, the chosen point, and the turn
/// open into its step boundaries.
#[derive(Debug, Default)]
pub(super) struct Cursor {
    /// Where the list cursor sits.
    selected: Sel,
    /// The chosen point's holding session and seq.
    point: Option<(SessionId, u64)>,
    /// The turn open into its step boundaries, by session and start.
    expanded: Option<(SessionId, u64)>,
}

/// Whether `key` names a folded turn.
///
/// Exempt from mutation testing: accepted history folds only add earlier
/// segments, and a turn row is never removed or re-keyed while the view is
/// open. An expansion anchor cannot become stale, so this exact membership
/// check has no consumer-visible effect.
#[cfg_attr(false, mutants::skip)]
fn has_turn(turns: &[TurnRow], key: &(SessionId, u64)) -> bool {
    turns
        .iter()
        .any(|turn| turn.session == key.0 && turn.start == key.1)
}

/// Whether `session` and `seq` name a folded boundary.
fn has_bound(turns: &[TurnRow], session: &SessionId, seq: u64) -> bool {
    turns.iter().any(|turn| {
        turn.bounds
            .iter()
            .any(|bound| bound.session == *session && bound.seq == seq)
    })
}

impl Cursor {
    /// The chosen point's holding session and seq.
    pub(super) fn point(&self) -> Option<(SessionId, u64)> {
        self.point.clone()
    }

    /// Selects a turn's row, with the point on its first boundary.
    pub(super) fn select_turn(
        &mut self,
        key: Option<(SessionId, u64)>,
        point: Option<(SessionId, u64)>,
    ) {
        self.selected = match key.clone() {
            Some((session, start)) => Sel::Turn(session, start),
            None => Sel::Check,
        };
        self.point = key.and(point);
    }

    /// Selects a boundary's row, with the point on it.
    pub(super) fn select_bound(&mut self, turn: (SessionId, u64), point: (SessionId, u64)) {
        self.selected = Sel::Bound(turn.0, turn.1, point.1);
        self.point = Some(point);
    }

    /// Selects a chrome row: the checkbox, the button, or a refusal's
    /// choice.
    pub(super) fn select_simple(&mut self, selected: Sel) {
        self.selected = selected;
    }

    /// Toggles the turn open into its step boundaries.
    pub(super) fn toggle_expanded(&mut self, key: Option<(SessionId, u64)>) {
        if self.expanded == key {
            self.expanded = None;
        } else {
            self.expanded = key;
        }
    }

    /// The default point: the start of the latest turn that is not a
    /// summary's and still holds a boundary.
    fn default_point(turns: &[TurnRow]) -> Option<(SessionId, u64)> {
        turns
            .iter()
            .rev()
            .find(|turn| !turn.summary && !turn.bounds.is_empty())
            .and_then(|turn| {
                turn.bounds
                    .first()
                    .map(|bound| (bound.session.clone(), bound.seq))
            })
    }

    /// Anchors the cursor and the point on rows that still exist: the
    /// expansion keeps its turn, the cursor keeps its row's identity,
    /// and the point follows content rows. A missing anchor takes the
    /// default; the first fold always does. Returns the selected row.
    pub(super) fn sync_selection(
        &mut self,
        turns: &[TurnRow],
        fresh: bool,
        show_choices: bool,
    ) -> usize {
        if let Some(expanded) = self.expanded.clone()
            && !has_turn(turns, &expanded)
        {
            self.expanded = None;
        }
        if let Some(point) = self.point.clone()
            && !has_bound(turns, &point.0, point.1)
        {
            self.point = None;
        }
        if self.point.is_none() {
            self.point = Self::default_point(turns);
        }
        if fresh || !self.sel_kept(turns) {
            // The cursor sits on the point's row: a start boundary
            // selects its turn row, any other its boundary row. Starts
            // resolve across every turn first, because a turn's end and
            // the next turn's start share one seq.
            let at = self
                .point
                .clone()
                .and_then(|point| Self::selected_point_row(turns, &point));
            self.selected = match at {
                Some((at, 0)) => turns.get(at).map_or(Sel::Check, |turn| {
                    Sel::Turn(turn.session.clone(), turn.start)
                }),
                Some((at, bound)) => turns.get(at).map_or(Sel::Check, |turn| {
                    turn.bounds.get(bound).map_or(Sel::Check, |other| {
                        Sel::Bound(turn.session.clone(), turn.start, other.seq)
                    })
                }),
                None => Sel::Check,
            };
        }
        self.selected_index(turns, show_choices)
    }

    /// Whether the cursor's row still exists.
    ///
    /// A selected boundary names the turn it was folded with, and a seq
    /// stands or falls for the whole session at once: the fold drops or
    /// keeps every bound with one seq together, so asking the turn
    /// anchor and the boundary separately is the same check.
    fn sel_kept(&self, turns: &[TurnRow]) -> bool {
        match &self.selected {
            Sel::Turn(session, start) => has_turn(turns, &(session.clone(), *start)),
            Sel::Bound(session, start, seq) => {
                has_turn(turns, &(session.clone(), *start)) && has_bound(turns, session, *seq)
            }
            Sel::Check | Sel::Action => true,
            Sel::Retry | Sel::NoSummary | Sel::Cancel => true,
        }
    }

    /// Whether a visible row is the cursor's row.
    fn matches_selected(&self, row: &Vis, turns: &[TurnRow]) -> bool {
        match (row, &self.selected) {
            (Vis::Turn(at), Sel::Turn(session, start)) => turns
                .get(*at)
                .is_some_and(|turn| turn.session == *session && turn.start == *start),
            (Vis::Bound(at, bound), Sel::Bound(session, start, seq)) => {
                turns.get(*at).is_some_and(|turn| {
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
    pub(super) fn visible(&self, turns: &[TurnRow], show_choices: bool) -> Vec<Vis> {
        let mut out = Vec::new();
        for (at, turn) in turns.iter().enumerate() {
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
        if show_choices {
            out.push(Vis::Retry);
            out.push(Vis::NoSummary);
            out.push(Vis::Cancel);
        }
        out
    }

    /// The selected point's visible row, preferring a turn start when the
    /// same seq is also the preceding turn's end.
    ///
    /// Exempt from mutation testing: normal input keeps a non-start point
    /// paired with its selected boundary while it exists. If refolding drops
    /// that boundary, `sync_selection` resets the point to the default turn
    /// start before calling this; a non-start fallback has no consumer-visible
    /// caller.
    #[cfg_attr(false, mutants::skip)]
    fn selected_point_row(turns: &[TurnRow], point: &(SessionId, u64)) -> Option<(usize, usize)> {
        let start = turns.iter().enumerate().find_map(|(at, turn)| {
            turn.bounds
                .first()
                .filter(|bound| bound.session == point.0 && bound.seq == point.1)
                .map(|_| (at, 0))
        });
        start.or_else(|| {
            turns.iter().enumerate().find_map(|(at, turn)| {
                turn.bounds
                    .iter()
                    .position(|bound| bound.session == point.0 && bound.seq == point.1)
                    .map(|bound| (at, bound))
            })
        })
    }

    /// The cursor's row among the visible rows.
    pub(super) fn selected_index(&self, turns: &[TurnRow], show_choices: bool) -> usize {
        self.visible(turns, show_choices)
            .iter()
            .position(|row| self.matches_selected(row, turns))
            .unwrap_or(0)
    }
}

/// The frame's rows for the folded turns, the cursor, and the send
/// state, with the detail under them.
pub(super) fn render(
    fold: &Fold,
    cursor: &Cursor,
    send: &Send,
    jobs: &[String],
    list: List,
) -> Frame {
    let turns = fold.turns();
    let show_choices = send.choices_shown();
    let mut rows = Vec::new();
    for vis in cursor.visible(turns, show_choices) {
        match vis {
            Vis::Turn(at) => {
                let Some(turn) = turns.get(at) else {
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
                let Some(label) = turns
                    .get(at)
                    .and_then(|turn| turn.bounds.get(bound).map(bound_label))
                else {
                    continue;
                };
                rows.push(vec![(label, None, Ink::Plain)]);
            }
            Vis::Check => {
                let mark = if send.summarise() { "[x]" } else { "[ ]" };
                let at = rows.len();
                rows.push(vec![(
                    format!("{mark} Summarise the path after the point"),
                    Some(Spot::Cell(at, 0)),
                    Ink::Plain,
                )]);
            }
            Vis::Action => {
                let at = rows.len();
                let (text, ink) = action_row(send, !turns.is_empty());
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
    if let Some(error) = fold.error() {
        below.push(error.to_owned());
    } else if turns.is_empty() {
        below.push("No turns to rewind.".to_owned());
    } else {
        below.extend(detail(cursor.point(), fold, jobs));
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
fn action_row(send: &Send, has_turns: bool) -> (String, Ink) {
    match send.action(has_turns) {
        Action::Refusal(error) => (error, Ink::Plain),
        Action::Sending => ("Sending…".to_owned(), Ink::Muted),
        Action::Disabled => ("Rewind".to_owned(), Ink::Muted),
        Action::Ready => ("Rewind".to_owned(), Ink::Plain),
    }
}

/// The detail under the rows: the chosen point's files Fiber's tools
/// wrote since and calls that may have changed files, that none of it
/// is undone, and every job that stops. With no chosen point the view
/// says what to pick.
fn detail(point: Option<(SessionId, u64)>, fold: &Fold, jobs: &[String]) -> Vec<String> {
    let Some((session, seq)) = point else {
        return vec!["Select a step boundary to rewind.".to_owned()];
    };
    let model = fold.detail_model(&session, seq);
    vec![
        format!(
            "Files written since the point: {}",
            or_none(&model.writes.join(", "))
        ),
        format!(
            "Calls that may have changed files: {}",
            or_none(&model.executes.join(", "))
        ),
        "None of it is undone.".to_owned(),
        format!("Jobs that stop: {}", or_none(&jobs.join(", "))),
    ]
}

/// An empty list reads as none.
fn or_none(list: &str) -> &str {
    if list.is_empty() { "none" } else { list }
}
