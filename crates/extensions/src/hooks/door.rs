//! The session's extensions as a door reaches them
//! (`docs/extensions.md`, "Commands and screens").

use super::{SessionExtensions, notice};
use contract::ErrorCode;

impl contract::extension::ExtensionDoor for SessionExtensions {
    fn command(
        &self,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        let admitted = self.commands.admit_with(name, text)?;
        let queued = std::sync::Arc::clone(admitted.queued());
        let lua = std::sync::Arc::clone(admitted.lua());
        let extension = admitted.extension().to_owned();
        let command = name.to_owned();
        // One waiter thread per admitted command waits for the call's end;
        // an `Err` gives a `notice` naming the extension through the
        // extension's late-bound emitter.
        std::thread::Builder::new()
            .name(format!("command {name}"))
            .spawn(move || {
                if let Err(e) = queued.wait() {
                    lua.emit(contract::events::Event::Notice(notice(
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
        let release_queued = std::sync::Arc::clone(admitted.queued());
        Ok(Box::new(move || release_queued.release()))
    }

    fn seal(&self) {
        for extension in &self.lua {
            extension.seal();
        }
    }

    fn reply(
        &self,
        mut reply: contract::commands::Reply,
        mut ack: contract::inbox::Ack,
    ) -> Option<(contract::commands::Reply, contract::inbox::Ack)> {
        // In load order: the extension holding the request takes it.
        for extension in &self.lua {
            (reply, ack) = extension.answer(reply, ack)?;
        }
        Some((reply, ack))
    }
}

impl SessionExtensions {
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
