//! Secrets (`docs/configuration.md`, "Secrets"): one file per name in
//! `credentials/` in Fiber home, mode 0600 in a 0700 directory. Configuration
//! itself never holds one; it holds only where a provider's credential comes
//! from.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::write::write_atomic;

/// Where a provider's credential comes from, as `providers."<name>".credential`
/// sets it. Only the global and per-project layers may set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSource {
    /// An environment variable holding the key.
    Env(String),
    /// A file holding the key.
    File(PathBuf),
    /// A program and its arguments that print the key.
    Command(Vec<String>),
}

/// A secret's value. It never prints: `Debug` shows only that it is a secret,
/// and there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value, such as one `fiber login` was given.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value itself, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

/// `credentials/<name>` in Fiber home, refusing a name that is not one file
/// name.
fn secret_path(home: &Path, name: &str) -> Result<PathBuf, ConfigError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(ConfigError::SecretName { name: name.into() });
    }
    Ok(home.join("credentials").join(name))
}

/// Reads `credentials/<name>` (`host.secret(name)`). `None` when there is no
/// such file.
pub fn read_secret(home: &Path, name: &str) -> Result<Option<Secret>, ConfigError> {
    let path = secret_path(home, name)?;
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(Secret(value))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io { file: path, source }),
    }
}

/// Stores `credentials/<name>` (`fiber login <name>`), mode 0600, creating
/// `credentials/` mode 0700.
pub fn store_secret(home: &Path, name: &str, secret: &Secret) -> Result<(), ConfigError> {
    let path = secret_path(home, name)?;
    write_atomic(&path, secret.0.as_bytes(), 0o600)
}
