//! The credential label a session uses and the key under it
//! (`docs/model-routing.md`, "Which credential a session uses"): the label a
//! resumed session was using, else `providers."<name>".credential`, else
//! `default`. The order applies to a provider that takes a credential; a
//! scripted provider has none (`crate::scripted`).

use config::{Config, ProviderData, Secret};
use contract::shapes::Failure;

use crate::failed;

/// The labels a starting session may take (`docs/model-routing.md`,
/// "Which credential a session uses"): `asked`, `--credential` on a resume,
/// and `recorded`, the label the resumed or rewound log last recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Labels<'a> {
    pub(crate) asked: Option<&'a str>,
    pub(crate) recorded: Option<&'a str>,
}

impl<'a> Labels<'a> {
    pub(crate) fn new(asked: Option<&'a str>, recorded: Option<&'a str>) -> Self {
        Self { asked, recorded }
    }

    /// The label the session reads its key under: `asked`, else `recorded`,
    /// else the configured one. A scripted provider rejects `asked`
    /// (`scripted::label`) and ignores `recorded`; its returned label is never
    /// recorded (`scripted::credential`).
    pub(crate) fn label(self, config: &Config, provider: &ProviderData) -> Result<String, Failure> {
        crate::scripted::label(provider, self.asked)?;
        Ok(self
            .asked
            .or(self.recorded)
            .map(str::to_owned)
            .unwrap_or_else(|| config.credential_label(provider)))
    }
}

/// A recorded-only `Labels`: a rewind's or a new session's.
impl<'a> From<Option<&'a str>> for Labels<'a> {
    fn from(recorded: Option<&'a str>) -> Self {
        Self {
            asked: None,
            recorded,
        }
    }
}

/// `credential_missing` for `label` on `provider`, listing `labels`
/// (`Config::listed`, "none" when empty).
pub(crate) fn no_label(provider: &str, label: &str, labels: &[String]) -> Failure {
    failed(
        contract::ErrorCode::CredentialMissing,
        format!(
            "`{provider}` has no credential label `{label}`. The labels for `{provider}` are: {}",
            Config::listed(labels)
        ),
    )
}

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
