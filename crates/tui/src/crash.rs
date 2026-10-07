//! Lines that can come outside any turn, and what the conversation shows
//! after a crash (`docs/tui.md`, "Errors and retries", "After a crash").
//!
//! An aside goes into the open turn's card where it came; with no turn
//! open it stands after the turns there were when it came. A resumed
//! process finding a turn with no `turn_completed` closes it as cut short,
//! unless the process before it exited suspended on a request, when the
//! turn resumes. A child of `turn`, so it closes and places in a card.

use std::collections::HashMap;

use contract::events::{FiberExited, FiberStarted, JobCompleted, JobStarted, McpServerFailed};
use contract::{Envelope, ErrorCode};
use ratatui::text::Line;

use super::{Ending, Entry, Fold, Turn, open};
use crate::app::{Target, read};
use crate::format;
use crate::turn::Row;

/// What the fold knows of the process and its jobs.
#[derive(Debug, Clone, Default)]
pub(crate) struct Crash {
    /// The last `fiber_exited` carried `suspended_on`.
    suspended: bool,
    /// Descriptions for jobs that may still complete, by job id.
    jobs: HashMap<String, String>,
    /// The orphaned-jobs line since the latest `fiber_started`.
    orphans: Option<usize>,
}

impl Fold {
    /// The continuation state a page seed keeps: scalars, and descriptions
    /// for jobs still open at the boundary. Rendered asides stay in resident
    /// pages and are folded again from that page's lines on reload.
    pub(crate) fn seed_continuation(
        &self,
    ) -> (usize, bool, Option<u64>, bool, HashMap<String, String>) {
        (
            self.next,
            self.ledgers,
            self.trigger_at,
            self.crash.suspended,
            self.crash.jobs.clone(),
        )
    }

    /// A page's starting fold from its seed's continuation state, with no
    /// rendered text: the reload folds the page's own lines into it.
    pub(crate) fn seeded(
        next: usize,
        ledgers: bool,
        trigger_at: Option<u64>,
        suspended: bool,
        jobs: HashMap<String, String>,
    ) -> Self {
        Self {
            next,
            ledgers,
            trigger_at,
            crash: Crash {
                suspended,
                jobs,
                ..Crash::default()
            },
            ..Self::default()
        }
    }
}

/// A line outside a turn's own items.
#[derive(Debug, Clone)]
pub(crate) enum Aside {
    /// One line, as drawn.
    Line(Line<'static>),
    /// The jobs a resumed process marked orphaned: each one's name and
    /// message.
    Orphans {
        id: usize,
        jobs: Vec<(String, String)>,
        open: bool,
    },
}

impl Aside {
    /// Its lines.
    pub(crate) fn rows(&self, out: &mut Vec<Row>) {
        match self {
            Self::Line(line) => out.push((line.clone(), None)),
            Self::Orphans { id, jobs, open } => {
                let names: Vec<&str> = jobs.iter().map(|(name, _)| name.as_str()).collect();
                let line = Line::raw(format!("Orphaned jobs: {}", names.join(", ")));
                out.push((line, Some(Target::Orphans(*id))));
                if *open {
                    for (name, message) in jobs {
                        out.push((format::dim(format!("  {name}: {message}")), None));
                    }
                }
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
            Self::Orphans { .. } | Self::Line(_) => false,
        }
    }

    /// Toggles what `target` opens, returning its new state when found.
    pub(crate) fn toggle(&mut self, target: Target) -> Option<bool> {
        match self {
            Self::Orphans { id, open, .. } if target == Target::Orphans(*id) => {
                *open = !*open;
                Some(*open)
            }
            Self::Orphans { .. } | Self::Line(_) => None,
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
            Entry::Reply { .. } | Entry::Steer(_) | Entry::Group(_) | Entry::Band(_) => None,
        });
    fold.asides
        .iter_mut()
        .map(|(_, aside)| aside)
        .chain(in_turns)
        .find_map(|aside| match aside {
            Aside::Orphans { id: has, jobs, .. } if *has == id => Some(jobs),
            Aside::Orphans { .. } | Aside::Line(_) => None,
        })
}

/// Folds `mcp_server_failed`, `fiber_exited`, `fiber_started`,
/// `job_started` and `job_completed`; false when no card changed.
pub(crate) fn fold(turns: &mut [Turn], fold: &mut Fold, envelope: &Envelope) -> bool {
    match envelope.kind.as_str() {
        "mcp_server_failed" => read!(envelope, McpServerFailed).is_some_and(|failed| {
            let line = Line::raw(format!("⚠ {}", failed.error.message));
            place(turns, fold, Aside::Line(line));
            true
        }),
        "fiber_exited" => {
            if let Some(exited) = read!(envelope, FiberExited) {
                fold.crash.suspended = exited.suspended_on.is_some();
            }
            false
        }
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
            let name = fold.crash.jobs.remove(&done.job_id.0);
            let Some(error) = done.error.filter(|error| error.code == ErrorCode::Orphaned) else {
                return false;
            };
            let job = (name.unwrap_or(done.job_id.0), error.message);
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

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
