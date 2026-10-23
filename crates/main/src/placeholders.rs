//! The placeholder lookup one session fills its hosts from
//! (`docs/invocation.md`, "A session's environment"): built once beside
//! the startup snapshot, before `add_session`, and shared across discovery,
//! filling and the background refresh.

use std::path::Path;

use contract::clock::Clock;
use extensions::Providers;

#[cfg(test)]
#[path = "placeholders_tests.rs"]
mod tests;

/// The lookup `routing::lua_providers::add_lua` fills placeholders from.
/// A hub-spawned session carries the hub's marker
/// (`hub::HUB_SESSION_MARKER`, set beside the startup snapshot): the
/// process environment is that fixed snapshot, so placeholders resolve
/// from it with no hub query at all — a `fiber hub refresh` between
/// spawn and here must not move them under the session
/// (`docs/invocation.md`, "A session's environment"). Anything else is an
/// in-process `fiber ask`, which queries the hub so the session agrees
/// with `fiber models` (issue #2909). Decided when called, at the point
/// `add_lua` decided before: after extensions load, before `add_session`;
/// a hub-spawned session never queries the hub.
#[allow(
    clippy::type_complexity,
    reason = "the `Lookup` alias is cut; the inline type is the whole interface"
)]
pub(crate) fn lookup(
    home: &Path,
    providers: &Providers,
    clock: &dyn Clock,
) -> Box<dyn Fn(&str) -> Option<String>> {
    if std::env::var_os(hub::HUB_SESSION_MARKER).is_some() {
        Box::new(|name: &str| std::env::var(name).ok())
    } else {
        Box::new(::cli::placeholder_lookup(home, providers, clock))
    }
}
