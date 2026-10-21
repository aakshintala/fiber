//! The hub's captured environment for one placeholder lookup
//! (`docs/invocation.md`, "A session's environment"): the values of the
//! placeholder variable names the installed providers declare, read from
//! the running hub, so the list and the session agree. No hub answers and
//! the caller falls back to the process environment, as before.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use contract::clock::Clock;

/// How long the hub query waits for its answer: the connect deadline plus
/// the login-shell capture's 10-second limit, since a hub in its first
/// seconds answers only after its first capture (`docs/invocation.md`,
/// "A session's environment"). `cli` declares it because `cli` may not
/// depend on `hub`.
pub(crate) const HUB_ENV_WITHIN: Duration =
    Duration::from_secs(doors::hub::CONNECT_DEADLINE.as_secs() + 10);

/// The hub's captured values for the requested placeholder variable
/// names, or `None` when no hub answers: absent, refusing or silent. The
/// caller falls back to the process environment on `None`, so today's
/// offline behaviour is unchanged.
pub(crate) fn hub_env(
    home: &Path,
    names: &[String],
    clock: &dyn Clock,
) -> Option<BTreeMap<String, String>> {
    hub_env_with(home, names, clock, &mut || {}, &mut || {})
}

/// [`hub_env`] with hooks for tests: `before_read` runs just before each
/// answer read, and `before_wait` after each wait's deadline check, just
/// before [`Clock::wait_until`].
fn hub_env_with(
    _home: &Path,
    _names: &[String],
    _clock: &dyn Clock,
    _before_read: &mut dyn FnMut(),
    _before_wait: &mut dyn FnMut(),
) -> Option<BTreeMap<String, String>> {
    None
}

/// One placeholder value for `fill_placeholders`: the hub's answer when
/// one arrived, else the process environment. A name the hub answered
/// without stays a gap even when the process environment holds it, so a
/// stale client export cannot revive a value the hub never saw.
pub(crate) fn lookup(
    answered: &Option<BTreeMap<String, String>>,
    process: &dyn Fn(&str) -> Option<String>,
    name: &str,
) -> Option<String> {
    match answered {
        Some(values) => values.get(name).cloned(),
        None => process(name),
    }
}

#[cfg(test)]
#[path = "hub_env_tests.rs"]
mod tests;
