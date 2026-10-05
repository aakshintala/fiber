//! The session's extensions, loaded once its configuration is read
//! (`docs/extensions.md`, "Registering"): `extensions_loaded` after
//! `fiber_started`, and the hooks on the loop when any registered one.

use std::sync::Arc;

use contract::hook::Hooks;
use extensions::SessionExtensions;
use log::Log;
use r#loop::Loop;

/// Writes `extensions_loaded` and the notices loading raised.
pub(crate) fn written(log: &Log, extensions: &SessionExtensions) -> Result<(), r#loop::Error> {
    r#loop::extensions_loaded(log, extensions.loaded(), extensions.notices())
}

/// `looped` asking the session's hooks, when any extension registered one;
/// otherwise `looped` as it was.
pub(crate) fn hooked(looped: Loop, extensions: &Arc<SessionExtensions>) -> Loop {
    if !extensions.has_hooks() {
        return looped;
    }
    let hooks: Arc<dyn Hooks> = Arc::clone(extensions) as Arc<dyn Hooks>;
    looped.hooks(hooks)
}
