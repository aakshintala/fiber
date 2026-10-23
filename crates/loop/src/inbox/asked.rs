//! An extension's ask lines, written at the drain that takes them
//! (`docs/extensions.md`, "Commands and screens"): `interaction_requested`
//! and `interaction_resolved` with no turn and no action, starting nothing
//! and joining nothing. A `Resolved`'s ack runs only once its line is in
//! the log, so a parked `host.ask` resumes after the drain.

use contract::events::{Event, InteractionRequested, InteractionResolved};
use contract::inbox::Ack;

use super::accept;
use crate::{Error, Loop};

impl Loop {
    /// Appends an extension's `host.ask` question, with no turn and no action.
    pub(crate) fn record_interaction(
        &mut self,
        requested: InteractionRequested,
    ) -> Result<(), Error> {
        let event = Event::InteractionRequested(requested);
        self.log.append(&event, None, None)?;
        self.summary.observe(&event, None);
        Ok(())
    }

    /// Appends an extension ask's answer, then accepts its ack.
    pub(crate) fn record_resolved(
        &mut self,
        resolved: InteractionResolved,
        ack: Ack,
    ) -> Result<(), Error> {
        let event = Event::InteractionResolved(resolved);
        self.log.append(&event, None, None)?;
        self.summary.observe(&event, None);
        accept(ack);
        Ok(())
    }
}
