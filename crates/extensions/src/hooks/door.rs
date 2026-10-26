//! The session's extensions as a door reaches them
//! (`docs/extensions.md`, "Commands and screens").

use super::{SessionExtensions, notice};
use contract::ErrorCode;
use support::lock;

impl contract::extension::ExtensionDoor for SessionExtensions {
    fn command(
        &self,
        id: &contract::CommandId,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        let admitted = self.commands.admit_with(id, name, text)?;
        let wait = admitted.wait().clone();
        let emit = admitted.emit_target();
        let extension = admitted.extension().to_owned();
        let command = name.to_owned();
        // One waiter thread per admitted command waits for the call's end;
        // an `Err` gives a `notice` naming the extension through the
        // late-bound emitter.
        std::thread::Builder::new()
            .name(format!("command {name}"))
            .spawn(move || {
                if let Err(e) = wait.wait() {
                    emit.emit(contract::events::Event::Notice(notice(
                        ErrorCode::ExtensionFailed,
                        format!("Command `{command}` failed: {e}"),
                        Some(&extension),
                    )));
                }
            })
            .map_err(|e| contract::inbox::Rejection {
                code: ErrorCode::IoFailed,
                message: format!("cannot start a thread: {e}"),
            })?;
        let release = admitted.wait().clone();
        Ok(Box::new(move || release.release()))
    }

    fn seal(&self) {
        lock(&self.emitter).take();
        #[cfg(test)]
        self.at_late(super::LatePoint::Cleared);
        for extension in self.attached_vms() {
            extension.seal();
        }
    }

    fn reply(
        &self,
        reply: contract::commands::Reply,
        ack: contract::inbox::Ack,
    ) -> Option<(contract::commands::Reply, contract::inbox::Ack)> {
        // In load order: the owner holding the request takes it. Process
        // owners hold no requests yet, so the reply passes them by.
        self.runtime.answer(reply, ack)
    }
}

impl SessionExtensions {
    /// Every session VM, including one started later, logs in through `browser`
    /// while the live `attended` reader returns true.
    pub fn browse_with(
        &self,
        browser: std::sync::Arc<dyn crate::Browser>,
        attended: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        self.browser.set(browser, attended);
    }

    /// Sets whether a client can answer an extension's `host.ask`.
    pub fn answerable(&self, yes: bool) {
        for extension in &self.lua {
            extension.set_answerable(yes);
        }
    }
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
