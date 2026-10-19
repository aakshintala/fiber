//! The session's extensions as a door reaches them (`docs/extensions.md`,
//! "Commands and screens").

/// The session's extensions as a door reaches them (`docs/extensions.md`,
/// "Commands and screens").
pub trait ExtensionDoor: Send + Sync {
    /// Admits extension command `name` with `text` onto its extension's ordered stream and
    /// returns once it is queued; `run` has not started. Err is `unknown_command`.
    /// The call is held: `run` starts only once the returned release is called, which the
    /// door does after `command_accepted` is on the connection's queue.
    fn command(
        &self,
        id: &crate::CommandId,
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, crate::inbox::Rejection>;
    /// Drops every later emission and delivery from the extensions; called by `Session::quiesce`.
    fn seal(&self);
    /// Takes `reply` when it answers a pending `host.ask`; hands it back otherwise.
    fn reply(
        &self,
        reply: crate::commands::Reply,
        ack: crate::inbox::Ack,
    ) -> Option<(crate::commands::Reply, crate::inbox::Ack)> {
        Some((reply, ack))
    }
}

/// Sends one driver command from inside the session; `answer` is called once.
/// (`docs/extensions.md`, "Host calls": `host.drive`.)
pub trait Drive: Send + Sync {
    /// `extension` becomes `Origin::Extension` on any message the command carries.
    fn drive(
        &self,
        extension: &str,
        command: &str,
        args: serde_json::Map<String, serde_json::Value>,
        answer: crate::inbox::Ack,
    );
}

/// Starts an extension-owned delegate: the session owns the job, with the
/// model's delegate admission and limits. `main` implements it and injects
/// it into `extensions` for `host.delegate` (`docs/architecture.md`,
/// "The call rules").
pub trait HostDelegate: Send + Sync {
    /// Starts the delegate `extension` names in `arguments`, calling `bind`
    /// with the new id before any child spawns, under the registry's start
    /// gate. Blocking. Called with no hub lock held. Calls `bind` at most
    /// once. After an `Err` that followed `bind`, the caller unbinds.
    fn start(
        &self,
        extension: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        bind: &mut dyn FnMut(&crate::JobId),
    ) -> Result<crate::JobId, crate::shapes::Failure>;
    /// Stops every running extension-owned delegate whose extension is not
    /// in `loaded`, and returns how many were sent a stop.
    fn retain(&self, loaded: &[String]) -> usize;
    /// Stops those of `jobs` still running, and returns the count. Never
    /// blocks on a job's end.
    fn cancel(&self, jobs: &[crate::JobId]) -> usize;
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;
