//! An emitter of ephemeral events (`docs/architecture.md`, "Streaming").
//! Any thread may hold one. Durable events stay the loop's to write, so
//! [`Emit::emit`] refuses them. A running tool call holds one that writes
//! under its own call (`docs/tools.md`, "Progress").

use crate::events::Event;

/// Sends ephemeral events to whoever is watching. A durable event is not
/// written: `seq` stays minted in one place.
pub trait Emit: Send + Sync {
    /// Emits `event` when it is ephemeral. A durable event is ignored, and
    /// nothing is written for it.
    fn emit(&self, event: &Event);

    /// Asked once, just before a tool spawns its command: `false` means do
    /// not start it. The tool runs every check that can refuse the call
    /// first, so a `false` here is the last thing before the spawn. The
    /// default starts at once.
    fn starting(&self) -> bool {
        true
    }

    /// Whether this emitter wants the call's output paced as `job_delta`
    /// is (`docs/tools.md`, "Progress"): the first change after idle at
    /// once, held changes collapsed into one per interval. The default
    /// keeps the tool's own unpaced deltas.
    fn paced(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::Emit;

    struct Default;

    impl Emit for Default {
        fn emit(&self, _event: &crate::events::Event) {}
    }

    /// The defaults a new emitter gets: starting at once, with the tool's
    /// own unpaced deltas. Flipping either breaks the call it guards.
    #[test]
    fn the_default_emitter_starts_at_once_and_keeps_unpaced_deltas() {
        let default = Default;
        assert!(default.starting(), "the default starts at once");
        assert!(!default.paced(), "the default keeps unpaced deltas");
    }
}
