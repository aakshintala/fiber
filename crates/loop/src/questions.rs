//! The step boundary after a step's calls completed: a call's
//! `control.questions` ends the turn (`docs/tools.md`, "What a result
//! carries"). The loop reads the field, never which tool set it.

use contract::TurnId;
use contract::events::{TurnCompleted, TurnOutcome};

use crate::{Error, Loop};

impl Loop {
    /// Collects the questions the step's completed calls set and, when any
    /// were asked, takes the queued subdirectory lines; runs the tools'
    /// handoff; writes the taken lines; returns the turn's end when
    /// questions were asked. Both are taken before the handoff, whose
    /// restart empties the step and replaces the instruction-file state.
    /// This ending builds no request, so the queued lines are written here,
    /// before `turn_completed`, rather than at a next step's start.
    pub(crate) fn after_calls(&mut self, turn: &TurnId) -> Result<Option<TurnCompleted>, Error> {
        let asked = self.handoff.carry.asked();
        if asked.is_empty() {
            self.handoff_from_tools(turn)?;
            return Ok(None);
        }
        let queued = self.changes.take_queued();
        self.handoff_from_tools(turn)?;
        for event in queued {
            self.append(&event, turn, None)?;
        }
        let mut completed = crate::ended(TurnOutcome::Completed, None);
        completed.questions = Some(asked);
        Ok(Some(completed))
    }
}
