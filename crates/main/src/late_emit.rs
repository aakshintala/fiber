//! An emitter whose target is set after it is handed out: a session's job
//! registry is built before its log exists, and job output reaches the log
//! once it does.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use contract::clock::Clock;

use contract::emit::Emit;
use contract::events::Event;

/// Drops events until [`LateEmit::set`], then forwards them.
#[derive(Default)]
pub(crate) struct LateEmit(OnceLock<Arc<dyn Emit>>);

impl LateEmit {
    /// Sets the target. A second call is ignored.
    pub(crate) fn set(&self, target: Arc<dyn Emit>) {
        let _first = self.0.set(target);
    }
}

impl Emit for LateEmit {
    fn emit(&self, event: &Event) {
        if let Some(target) = self.0.get() {
            target.emit(event);
        }
    }
}

/// A new session's one job registry, with its output files in the
/// session's `artifacts/` (which `Log::create` makes), and the emitter that
/// reaches the log once it exists. Nothing can start a job before then.
pub(crate) fn registry(dir: &Path, clock: &Arc<dyn Clock>) -> (Arc<LateEmit>, Arc<jobs::Registry>) {
    let emit = Arc::new(LateEmit::default());
    let registry = jobs::Registry::new(
        dir.join("artifacts"),
        Arc::clone(clock),
        Arc::clone(&emit) as _,
    );
    (emit, registry)
}

#[cfg(test)]
#[path = "late_emit_tests.rs"]
mod tests;
