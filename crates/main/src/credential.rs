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

/// The reviewer's key: under the session's label when the reviewer is the
/// session's own provider, else under its own configured label.
pub(crate) fn reviewer_credential(
    config: &Config,
    reviewer: &ProviderData,
    session: &ProviderData,
    session_label: &str,
) -> Result<Secret, Failure> {
    let label = if reviewer.name == session.name {
        session_label.to_owned()
    } else {
        config.credential_label(reviewer)
    };
    config
        .credential(reviewer, &label)
        .map_err(|e| failed(e.code(), e))
}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
