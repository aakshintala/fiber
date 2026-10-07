//! The credential label a session uses and the key under it
//! (`docs/model-routing.md`, "Which credential a session uses"): the label a
//! resumed session was using, else `providers."<name>".credential`, else
//! `default`.

use config::{Config, ProviderData, Secret};
use contract::shapes::Failure;

use crate::failed;

/// The session's label and its key. `recorded` is the label the resumed
/// session's log last recorded; it beats the configured one. A label that
/// names no credential fails with `credential_missing`.
pub(crate) fn session_credential(
    config: &Config,
    provider: &ProviderData,
    recorded: Option<&str>,
) -> Result<(String, Secret), Failure> {
    let label = recorded.map_or_else(|| config.credential_label(provider), str::to_owned);
    let key = config
        .credential(provider, &label)
        .map_err(|e| failed(e.code(), e))?;
    Ok((label, key))
}

/// A switch's read of `provider`'s key under `label`, running a `command`
/// source through `run`: the key and the canonical path of a `file` source
/// it read. A source that cannot be read fails with its own code, such as
/// `credential_missing` (`docs/model-routing.md`, "When a credential is
/// missing or fails").
pub(crate) fn switch_credential(
    config: &Config,
    provider: &ProviderData,
    label: &str,
    run: config::Runner<'_>,
) -> Result<config::Read, Failure> {
    config
        .credential_with(provider, label, run)
        .map_err(|e| failed(e.code(), e))
}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
