//! What a session message may still enter the inbox (`docs/tools.md`,
//! "Messaging other sessions"): the door admits it under the same lock the
//! loop's last idle check closes under, so a message is either accepted
//! while the loop will still run it, or refused `closing`. The inbox lock
//! is taken inside the admission lock; no path takes them in the other
//! order.

use contract::inbox::SessionMessage;

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

    /// Sends `message` to the loop's inbox under the admission lock:
    /// `Ok` once it is in the inbox, `Closed` once admission closed,
    /// `Ended` once the inbox is gone.
    pub(crate) fn deliver_message(&self, message: SessionMessage) -> Result<(), Shut> {
        enum Sent {
            Delivered,
            Gone,
        }
        let sent = self.admission.admit(|| {
            // Noted while the lock is held, so a race test can hold a
            // `close` on another connection here.
            #[cfg(test)]
            self.note(super::tests::Probe::MessageLocked);
            let inbox = super::lock(&self.inbox);
            match inbox.as_ref() {
                None => Sent::Gone,
                Some(inbox) => match inbox.send(contract::inbox::Delivery::Message(message)) {
                    Ok(()) => Sent::Delivered,
                    Err(_) => Sent::Gone,
                },
            }
        });
        match sent {
            None => Err(Shut::Closed),
            Some(Sent::Gone) => Err(Shut::Ended),
            Some(Sent::Delivered) => Ok(()),
        }
    }
}
