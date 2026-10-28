//! Lines that can come outside any turn, and what the conversation shows
//! after a crash (`docs/tui.md`, "Errors and retries", "After a crash").
//!
//! An aside goes into the open turn's card where it came; with no turn
//! open it stands after the turns there were when it came. A resumed
//! process finding a turn with no `turn_completed` closes it as cut short,
//! unless the process before it exited suspended on a request, when the
//! turn resumes. A child of `turn`, so it closes and places in a card.

use std::collections::HashMap;

use contract::events::{
    FiberExited, FiberStarted, JobCompleted, JobStarted, McpServerFailed, Rewound,
};
use contract::{Envelope, ErrorCode};
use ratatui::text::Line;

use super::{Ending, Entry, Fold, Turn, open};
use crate::app::{Target, read};
use crate::format;
use crate::rows::Rows;

/// What the fold knows of the process and its jobs.
#[derive(Debug, Clone, Default)]
pub(crate) struct Crash {
    /// The last `fiber_exited` carried `suspended_on`.
    suspended: bool,
    /// Descriptions of jobs started on this page and still open, by job id.
    jobs: HashMap<String, String>,
    /// Descriptions of jobs started on earlier pages and still open.
    earlier: HashMap<String, String>,
    /// Descriptions of jobs that ended stopped or orphaned on this
    /// page: all its seed keeps of job text.
    carried: HashMap<String, String>,
    /// Jobs completed by the contiguous stop run immediately before a
    /// `rewound` line.
    rewind_stopped: HashMap<String, String>,
    /// The orphaned-jobs line since the latest `fiber_started`.
    orphans: Option<usize>,
}

impl Fold {
    /// The continuation state a page seed keeps. Rendered asides stay in
    /// resident pages and are folded again from that page's lines on reload.
    pub(crate) fn seed_continuation(&self) -> (usize, bool, Option<u64>, bool) {
        (
            self.next,
            self.ledgers,
            self.trigger_at,
            self.crash.suspended,
        )
    }

    /// A page's starting fold from its seed's continuation state, with no
    /// rendered text: the reload folds the page's own lines into it.
    /// `carried` names the page's stopped or orphaned completions, so
    /// its reload keeps their descriptions.
    pub(crate) fn seeded(
        next: usize,
        ledgers: bool,
        trigger_at: Option<u64>,
        suspended: bool,
        carried: HashMap<String, String>,
    ) -> Self {
        Self {
            next,
            ledgers,
            trigger_at,
            crash: Crash {
                suspended,
                earlier: carried,
                ..Crash::default()
            },
            ..Self::default()
        }
    }

    /// Takes the open jobs of the page `old` closes as this page's earlier
    /// ones, moving them so no closed page keeps their descriptions.
    pub(crate) fn carry_from(&mut self, old: &mut Self) {
        self.crash
            .earlier
            .extend(std::mem::take(&mut old.crash.jobs));
        self.crash
            .earlier
            .extend(std::mem::take(&mut old.crash.earlier));
    }

    /// Copies `open`'s open jobs as this candidate page's earlier ones,
    /// held only while the cut is pending.
    pub(crate) fn carry_copy(&mut self, open: &Self) {
        self.crash.earlier.extend(
            open.crash
                .jobs
                .iter()
                .chain(&open.crash.earlier)
                .map(|(id, name)| (id.clone(), name.clone())),
        );
    }

    /// Takes the descriptions of jobs that ended stopped or orphaned on
    /// this page.
    pub(crate) fn take_carried(&mut self) -> HashMap<String, String> {
        std::mem::take(&mut self.crash.carried)
    }
}

/// A line outside a turn's own items.
#[derive(Debug, Clone)]
pub(crate) enum Aside {
    /// One line, as drawn.
    Line(Line<'static>),
    /// A failed MCP server's message: "⚠ " in `attention`, then the
    /// message dim, wrapped under itself
    /// (`docs/tui.md`, "Errors and retries").
    Warning { message: String },
    /// The jobs a resumed process marked orphaned: each one's name and
    /// message.
    Orphans {
        id: usize,
        jobs: Vec<(String, String)>,
        open: bool,
    },
}

impl Aside {
    /// Its card entry: the line's joined span text, the warning's
    /// message, or the orphaned jobs with their open state. The row
    /// targets stay the terminal's.
    pub(crate) fn turn_input(&self) -> crate::ext::TurnAside<'_> {
        match self {
            Self::Line(line) => crate::ext::TurnAside::Line {
                text: line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect(),
            },
            Self::Warning { message } => crate::ext::TurnAside::Warning { message },
            Self::Orphans { jobs, open, .. } => crate::ext::TurnAside::Orphans {
                jobs: jobs
                    .iter()
                    .map(|(name, message)| crate::ext::TurnOrphan { name, message })
                    .collect(),
                open: *open,
            },
        }
    }

    /// Its lines at the card's inner width.
    pub(crate) fn rows(&self, inner: u16, out: &mut Rows) {
        match self {
            Self::Line(line) => out.push((line.clone(), None)),
            Self::Warning { message } => warning_rows(message, inner, out),
            Self::Orphans { id, jobs, open } => {
                let names: Vec<&str> = jobs.iter().map(|(name, _)| name.as_str()).collect();
                let line = Line::raw(format!("Orphaned jobs: {}", names.join(", ")));
                out.push((line, Some(Target::Orphans(*id))));
                if out.open_scope(Target::Orphans(*id), *open) {
                    for (name, message) in jobs {
                        out.push((format::dim(format!("  {name}: {message}")), None));
                    }
                }
                out.end_scope();
            }
        }
    }

    /// Sets what `target` opens to `open`; false when it is not this aside's.
    pub(crate) fn set_open(&mut self, target: &Target, open: bool) -> bool {
        match self {
            Self::Orphans { id, open: flag, .. } if target == &Target::Orphans(*id) => {
                *flag = open;
                true
            }
            Self::Orphans { .. } | Self::Line(_) | Self::Warning { .. } => false,
        }
    }

    /// Toggles what `target` opens, returning its new state when found.
    pub(crate) fn toggle(&mut self, target: Target) -> Option<bool> {
        match self {
            Self::Orphans { id, open, .. } if target == Target::Orphans(*id) => {
                *open = !*open;
                Some(*open)
            }
            Self::Orphans { .. } | Self::Line(_) | Self::Warning { .. } => None,
        }
    }
}

impl Turn {
    /// Closes the card as cut short at its last line. A call that started
    /// and never completed may have run.
    fn cut_short(&mut self) {
        self.ended = Some((Ending::CutShort, self.last));
        for group in &mut self.groups {
            group.cut = true;
        }
    }
}

/// Places `aside` in the open turn, or after the turns there are.
pub(super) fn place(turns: &mut [Turn], fold: &mut Fold, aside: Aside) {
    match open(turns) {
        Some(turn) => turn.entries.push(Entry::Aside(aside)),
        None => fold.asides.push((turns.len(), aside)),
    }
}

/// The jobs of orphaned-jobs line `id`, wherever it stands.
fn orphans<'a>(
    turns: &'a mut [Turn],
    fold: &'a mut Fold,
    id: usize,
) -> Option<&'a mut Vec<(String, String)>> {
    let in_turns = turns
        .iter_mut()
        .flat_map(|turn| turn.entries.iter_mut())
        .filter_map(|entry| match entry {
            Entry::Aside(aside) => Some(aside),
            Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Band(_)
            | Entry::Answers(_)
            | Entry::Message(_) => None,
        });
    fold.asides
        .iter_mut()
        .map(|(_, aside)| aside)
        .chain(in_turns)
        .find_map(|aside| match aside {
            Aside::Orphans { id: has, jobs, .. } if *has == id => Some(jobs),
            Aside::Orphans { .. } | Aside::Line(_) | Aside::Warning { .. } => None,
        })
}

/// Drops stop completions when a non-job event separates them from a
/// `rewound` line. The rewind stop wait writes only job records before it.
pub(crate) fn before_line(fold: &mut Fold, envelope: &Envelope) {
    if !envelope.is_durable() {
        return;
    }
    if !matches!(
        envelope.kind.as_str(),
        "job_started"
            | "job_completed"
            | "delegate_started"
            | "delegate_finished"
            | "extension_state_set"
            | "extension_state_unset"
            | "rewound"
    ) {
        fold.crash.rewind_stopped.clear();
    }
}

/// Folds `mcp_server_failed`, `fiber_exited`, `fiber_started`,
/// `job_started` and `job_completed`; false when no card changed.
pub(crate) fn fold(turns: &mut [Turn], fold: &mut Fold, envelope: &Envelope) -> bool {
    match envelope.kind.as_str() {
        "mcp_server_failed" => read!(envelope, McpServerFailed).is_some_and(|failed| {
            place(
                turns,
                fold,
                Aside::Warning {
                    message: failed.error.message,
                },
            );
            true
        }),
        "fiber_exited" => {
            if let Some(exited) = read!(envelope, FiberExited) {
                fold.crash.suspended = exited.suspended_on.is_some();
            }
            false
        }
        "rewound" => read!(envelope, Rewound).is_some_and(|rewound| {
            // The line names the new session and says what was not
            // restored and which jobs stopped (`docs/tui.md`, "Rewind"):
            // a rewind never restores files, and every job still running
            // stops with the old session.
            let rewind_stopped = std::mem::take(&mut fold.crash.rewind_stopped);
            let mut stopped: Vec<String> = fold
                .crash
                .jobs
                .values()
                .chain(fold.crash.earlier.values())
                .chain(rewind_stopped.values())
                .cloned()
                .collect();
            stopped.sort();
            stopped.dedup();
            let jobs = if stopped.is_empty() {
                "none".to_owned()
            } else {
                stopped.join(", ")
            };
            place(
                turns,
                fold,
                Aside::Line(Line::raw(format!(
                    "⟲ {} · nothing restored · jobs stopped: {jobs}",
                    rewound.new_session_id.0
                ))),
            );
            true
        }),
        "fiber_started" => read!(envelope, FiberStarted).is_some_and(|started| {
            let suspended = std::mem::take(&mut fold.crash.suspended);
            fold.crash.orphans = None;
            if !started.resumed || suspended {
                return false;
            }
            let Some(turn) = open(turns) else {
                return false;
            };
            turn.cut_short();
            place(turns, fold, Aside::Line(Line::raw("↺ resumed")));
            true
        }),
        "job_started" => {
            if let Some(job) = read!(envelope, JobStarted) {
                fold.crash.jobs.insert(job.job_id.0, job.description);
            }
            false
        }
        "job_completed" => {
            let Some(done) = read!(envelope, JobCompleted) else {
                return false;
            };
            let id = done.job_id.0;
            let here = fold.crash.jobs.remove(&id);
            let earlier = fold.crash.earlier.remove(&id);
            let name = here
                .or_else(|| earlier.clone())
                .unwrap_or_else(|| id.clone());
            let stopped = matches!(done.status, contract::events::Outcome::Cancelled)
                || matches!(done.status, contract::events::Outcome::Failed)
                    && done
                        .error
                        .as_ref()
                        .is_some_and(|error| error.code == ErrorCode::Indeterminate);
            if stopped {
                fold.crash.rewind_stopped.insert(id.clone(), name.clone());
            }
            let orphaned = done
                .error
                .as_ref()
                .is_some_and(|error| error.code == ErrorCode::Orphaned);
            // Every stopped or orphaned completion forwards its
            // description: the reload refolds this page's own lines,
            // whose completion consumes the entry again, so no other
            // page reads it.
            if stopped || orphaned {
                fold.crash.carried.insert(id, name.clone());
            }
            let Some(error) = done.error.filter(|error| error.code == ErrorCode::Orphaned) else {
                return false;
            };
            let job = (name, error.message);
            if let Some(jobs) = fold.crash.orphans.and_then(|id| orphans(turns, fold, id)) {
                jobs.push(job);
            } else {
                let id = fold.id();
                fold.crash.orphans = Some(id);
                let aside = Aside::Orphans {
                    id,
                    jobs: vec![job],
                    open: false,
                };
                place(turns, fold, aside);
            }
            true
        }
        _ => false,
    }
}

/// A warning's rows: "⚠ " in `attention`, then the message dim,
/// wrapped at `inner - 2` with every continuation starting under the
/// message's first column, so a copy joins the wrapped text.
fn warning_rows(message: &str, inner: u16, out: &mut Rows) {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    let width = usize::from(inner).saturating_sub(2).max(1);
    for (at, (row, join)) in format::wrap_joined(message, width).iter().enumerate() {
        let line = if at == 0 {
            Line::from(vec![
                Span::styled(
                    "⚠ ".to_owned(),
                    crate::theme::style(crate::theme::Role::Attention),
                ),
                Span::styled(row.clone(), Style::default().add_modifier(Modifier::DIM)),
            ])
        } else {
            Line::styled(
                format!("  {row}"),
                Style::default().add_modifier(Modifier::DIM),
            )
        };
        out.push_text(
            (line, None),
            crate::rows::RowText {
                join: *join,
                skip: 2,
                ..crate::rows::RowText::plain()
            },
        );
    }
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
