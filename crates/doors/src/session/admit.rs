//! What a session message may still enter the inbox (`docs/tools.md`,
//! "Messaging other sessions"): the door admits it under the same lock the
//! loop's last idle check closes under, so a message is either accepted
//! while the loop will still run it, or refused `closing`. The inbox lock
//! is taken inside the admission lock; no path takes them in the other
//! order.

use contract::inbox::{Ack, SessionMessage};

use super::Gate;

/// Why `deliver_message` refused a session message.
pub(crate) enum Shut {
    /// Admission already closed: `close`, `fiber ask`, a shutdown, a
    /// rewind, or the loop's idle exit.
    Closed,
    /// The inbox is gone: the session ended before answering.
    Ended,
}

impl Gate {
    /// Closes admission: no session message enters after this. Idempotent.
    pub(crate) fn mark_closed(&self) {
        // Closed under the lock, so a message admitted before this stays
        // ahead of every `Close`. Under test the close is noted while the
        // lock is held, so a race test can order a message against it.
        let _closed = self.admission.close_unless(|| {
            #[cfg(test)]
            self.note(super::tests::Probe::CloseLocked);
            None::<()>
        });
    }

    /// Sends `message` to the loop's inbox under the admission lock,
    /// handing the loop its ack: `Ok` once it is in the inbox, the ack
    /// back with `Closed` once admission closed or with `Ended` once the
    /// inbox is gone, so the caller answers through it.
    pub(crate) fn deliver_message(
        &self,
        message: SessionMessage,
        ack: Ack,
    ) -> Result<(), (Shut, Ack)> {
        // Held outside the lock's closure: a closed admission never runs
        // it, so the ack comes back uncalled.
        let mut ack = Some(ack);
        let sent = self.admission.admit(|| {
            // Noted while the lock is held, so a race test can hold a
            // `close` on another connection here.
            #[cfg(test)]
            self.note(super::tests::Probe::MessageLocked);
            let Some(ack) = ack.take() else {
                unreachable!("admission runs its closure once");
            };
            let inbox = super::lock(&self.inbox);
            match inbox.as_ref() {
                None => Err(ack),
                Some(inbox) => match inbox.send(contract::inbox::Delivery::Message(message, ack)) {
                    Ok(()) => Ok(()),
                    Err(failed) => {
                        let contract::inbox::Delivery::Message(_, ack) = failed.0 else {
                            unreachable!("the send returns the message just sent");
                        };
                        Err(ack)
                    }
                },
            }
        });
        match sent {
            None => {
                let Some(ack) = ack.take() else {
                    unreachable!("a closed admission runs nothing");
                };
                Err((Shut::Closed, ack))
            }
            Some(Ok(())) => Ok(()),
            Some(Err(ack)) => Err((Shut::Ended, ack)),
        }
    }
}
