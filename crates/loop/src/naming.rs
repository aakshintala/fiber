//! The session name's pin (`docs/tools.md`, "Naming the session"): a name
//! the person set with the `name` command pins the session against the
//! model's `name_session`, until the person clears it.

use contract::ErrorCode;
use contract::commands::Name;
use contract::events::{CallStatus, Control, Event, NamedBy, SessionNamed};
use contract::inbox::Ack;

use crate::inbox::accept;
use contract::shapes::Failure;

use crate::{Error, Loop};

/// What a pinned call tells the model.
pub(crate) const PINNED: &str = "the person named this session";

/// Whether the person's name pins the session: the latest `session_named`
/// has `by: person` with a non-null name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Naming {
    pinned: bool,
}

/// What a completed call carrying `control.name` does.
pub(crate) enum Named {
    /// No name to write: the call was not completed with a name.
    Nothing,
    /// Write `session_named` by the model before the completion.
    Write(SessionNamed),
    /// Fail the call `name_pinned` while the person's name pins it.
    Pinned(Failure),
}

/// The `session_named` the person's `name` command writes: only the
/// exact empty string clears, to `name: null`; every other text,
/// whitespace included, is kept exactly as sent.
pub(crate) fn by_person(text: String) -> SessionNamed {
    SessionNamed {
        name: (!text.is_empty()).then_some(text),
        by: NamedBy::Person,
    }
}

impl Loop {
    /// Writes the person's `name` command as `session_named` by `person`,
    /// with no turn and no action: the session's name is about the
    /// session, not a turn. The fold pins the name before the ack runs,
    /// so a `name_session` call judged after answers `name_pinned`. A
    /// failed append returns before folding or acknowledging.
    pub(crate) fn take_name(&mut self, name: Name, ack: Ack) -> Result<(), Error> {
        let event = Event::SessionNamed(by_person(name.text));
        self.log.append(&event, None, None)?;
        self.naming.fold(&event);
        accept(ack);
        Ok(())
    }
}

impl Naming {
    /// Folds one event: a `session_named` pins while the person's name
    /// holds, and unpins when cleared or replaced; any other event
    /// changes nothing.
    pub(crate) fn fold(&mut self, event: &Event) {
        if let Event::SessionNamed(named) = event {
            self.pinned = named.by == NamedBy::Person && named.name.is_some();
        }
    }

    /// Judges a call that ran: only a completed call carrying
    /// `control.name` names the session, or fails pinned while the
    /// person's name pins it.
    pub(crate) fn judge(&self, status: CallStatus, control: Option<&Control>) -> Named {
        let name = match (status, control) {
            (CallStatus::Completed, Some(control)) => match &control.name {
                Some(name) => name.clone(),
                None => return Named::Nothing,
            },
            _ => return Named::Nothing,
        };
        if self.pinned {
            Named::Pinned(Failure {
                code: ErrorCode::NamePinned,
                message: PINNED.to_owned(),
                retry_after_ms: None,
                provider: None,
            })
        } else {
            Named::Write(SessionNamed {
                name: Some(name),
                by: NamedBy::Model,
            })
        }
    }
}

#[cfg(test)]
#[path = "naming_tests.rs"]
mod tests;
