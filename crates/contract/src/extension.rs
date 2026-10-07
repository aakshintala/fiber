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
        name: &str,
        text: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, crate::inbox::Rejection>;
    /// Drops every later emission and delivery from the extensions; called by `Session::quiesce`.
    fn seal(&self);
    /// Part 4. Takes `reply` when it answers a pending `host.ask`; hands it back otherwise.
    fn reply(
        &self,
        reply: crate::commands::Reply,
        ack: crate::inbox::Ack,
    ) -> Option<(crate::commands::Reply, crate::inbox::Ack)>;
}
