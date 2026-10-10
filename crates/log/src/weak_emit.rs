//! An [`Emit`] over a weak handle to the [`Log`]: the extensions never keep
//! the log, and its session lock, alive past `Session::close`.

use std::sync::Weak;

use contract::emit::Emit;
use contract::events::Event;

use super::write::Log;

/// An ephemeral emitter that holds the log weakly, so the extensions never
/// keep the log (and its session lock) alive after `Session::close`. While
/// the log lives it emits through it; after the last `Arc<Log>` drops it
/// does nothing.
pub struct WeakEmit {
    log: Weak<Log>,
}

impl WeakEmit {
    /// Holds `log` weakly.
    pub fn new(log: &std::sync::Arc<Log>) -> Self {
        Self {
            log: std::sync::Arc::downgrade(log),
        }
    }
}

impl Emit for WeakEmit {
    fn emit(&self, event: &Event) {
        if let Some(log) = self.log.upgrade() {
            log.emit(event);
        }
    }
}
