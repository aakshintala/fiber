//! A tool group: everything between two pieces of assistant text, by step,
//! with its thinking blocks, calls, failed model calls and the calls still
//! streaming (`docs/tui.md`, "Tool groups and the ledger"). `format.rs`
//! draws it.

use contract::events::{CallStatus, FileChange};
use serde_json::Value;

use super::target_id;
use crate::app::Target;

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
    pub(crate) open: bool,
    /// A `permission_requested` for it is open.
    pub(crate) asking: bool,
    /// It had `tool_call_started`.
    pub(crate) started: bool,
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
            Target::Login | Target::Note(_) | Target::Orphans(_) | Target::Copy { .. } => None,
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
