//! The `delegate_finished` a resumed parent settles from the delegate's
//! own log (`docs/delegates.md`, "Lifetime": "it writes the
//! `delegate_finished` from the delegate's own log"): the moved arm of the
//! live `outcome()` fold, called by `outcome()` and by loop's orphan
//! settle, the one `DelegateFinished` builder either reaches.

use super::{DelegateFinished, FiberExited};
use crate::JobId;

impl FiberExited {
    /// The finish the exit line carries: the final message's text, the
    /// questions and usage copied through, no worktree and no artifact.
    /// The live drive bounds the text and assigns a worktree after the
    /// fold; a resumed settle bounds it at its own write and keeps no
    /// worktree.
    pub fn delegate_finished(&self, job_id: &JobId) -> DelegateFinished {
        DelegateFinished {
            job_id: job_id.clone(),
            text: self
                .final_message
                .as_ref()
                .map(|message| message.text.clone())
                .unwrap_or_default(),
            artifact: None,
            questions: self.questions.clone(),
            usage: self.usage.clone(),
            worktree: None,

            changed_by: None,
        }
    }
}

#[cfg(test)]
#[path = "delegate_finished_tests.rs"]
mod tests;
