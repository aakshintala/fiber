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

/// How the session's repository approvals fetch dependencies: the github
/// `Origin`, this binary's version, on `clock`. No repository set is
/// loaded here; that is Part 2.
pub(crate) fn fetch(clock: &Arc<dyn contract::clock::Clock>) -> extensions::Fetch {
    extensions::Fetch::new(
        extensions::Origin::github(),
        env!("CARGO_PKG_VERSION"),
        Arc::clone(clock),
    )
}

/// Attaches loaded extensions, including empty hook chains whose state
/// still needs to reach the loop's drains.
pub(crate) fn hooked(looped: Loop, extensions: &Arc<SessionExtensions>) -> Loop {
    if !extensions.has_extensions() {
        return looped;
    }
    let hooks: Arc<dyn Hooks> = Arc::clone(extensions) as Arc<dyn Hooks>;
    looped.hooks(hooks)
}
