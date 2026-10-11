//! The session name's pin (`docs/tools.md`, "Naming the session"): a name
//! the person set with the `name` command pins the session against the
//! model's `name_session`, until the person clears it.

use contract::ErrorCode;
use contract::events::{CallStatus, Control, Event, NamedBy, SessionNamed};
use contract::shapes::Failure;

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
