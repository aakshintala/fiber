//! `/login` through `cli::login` (`docs/tui.md`, "Logging in"): the
//! installed providers by name, then the secrets installed extensions
//! declare, and one store call through the same steps as `fiber login`.

use std::path::Path;

use super::from_failure;
use tui::{ConfigureError, LoginKind, LoginTarget, Stored};

/// The installed providers as key rows, then the declared secrets.
pub(super) fn targets(home: &Path) -> Result<Vec<LoginTarget>, ConfigureError> {
    ::cli::login_targets(home)
        .map(|names| {
            names
                .into_iter()
                .map(|name| match name {
                    ::cli::LoginName::Provider(name) => LoginTarget {
                        name,
                        kind: LoginKind::Key,
                    },
                    ::cli::LoginName::Secret(name) => LoginTarget {
                        name,
                        kind: LoginKind::Secret,
                    },
                })
                .collect()
        })
        .map_err(from_failure)
}

/// Stores `key` for provider or secret `name`, under `label` for a
/// provider, through the same steps as `fiber login`.
pub(super) fn store(
    home: &Path,
    name: &str,
    label: Option<&str>,
    key: contract::Secret,
) -> Result<Stored, ConfigureError> {
    ::cli::login_store(home, name, label, key)
        .map(|stored| Stored {
            path: stored.path,
            replaced: stored.replaced,
        })
        .map_err(from_failure)
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
