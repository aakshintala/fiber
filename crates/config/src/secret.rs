//! Secrets (`docs/configuration.md`, "Secrets"): one file per name in
//! `credentials/` in Fiber home, mode 0600 in a 0700 directory, and a
//! provider's stored credentials, one file per label in
//! `credentials/<name>/`. Configuration itself never holds one; it holds only
//! where a provider's credential comes from.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::credential_file::{credential_path, is_lock_or_tmp};
use crate::error::ConfigError;
use crate::home::{one_file_name, plain};
use crate::write::write_atomic;

/// Where a provider's credential comes from, as
/// `providers."<name>".credentials."<label>"` sets it. Only the global and per-project layers may set it.
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
/// name, and a `credentials/` that is a symbolic link, which a tool call or a
/// repository could plant to redirect secrets.
fn secret_path(home: &Path, name: &str) -> Result<PathBuf, ConfigError> {
    if !one_file_name(name) {
        return Err(ConfigError::SecretName { name: name.into() });
    }
    let dir = home.join("credentials");
    plain(&dir, true)?;
    Ok(dir.join(name))
}

/// Reads `credentials/<name>` (`host.secret(name)`). `None` when there is no
/// such file; a symbolic link there is refused.
pub fn read_secret(home: &Path, name: &str) -> Result<Option<Secret>, ConfigError> {
    let path = secret_path(home, name)?;
    if !plain(&path, false)? {
        return Ok(None);
    }
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(Secret(value))),
        Err(source) => Err(ConfigError::Io { file: path, source }),
    }
}

/// Stores `credentials/<name>` (`fiber login <name>`), mode 0600, creating
/// `credentials/` mode 0700.
pub fn store_secret(home: &Path, name: &str, secret: &Secret) -> Result<(), ConfigError> {
    let path = secret_path(home, name)?;
    write_atomic(&path, secret.0.as_bytes(), 0o600)
}

/// Reads `credentials/<name>/<label>`. `None` when there is no such file; a
/// symbolic link there, or a `credentials/<name>` that is a file, is refused.
pub fn read_credential(
    home: &Path,
    name: &str,
    label: &str,
) -> Result<Option<Secret>, ConfigError> {
    let path = credential_path(home, name, label)?;
    if !plain(&path, false)? {
        return Ok(None);
    }
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(Secret(value))),
        Err(source) => Err(ConfigError::Io { file: path, source }),
    }
}

/// Stores `credentials/<name>/<label>` (`fiber login`), mode 0600, creating
/// `credentials/` and `credentials/<name>/` mode 0700.
pub fn store_credential(
    home: &Path,
    name: &str,
    label: &str,
    secret: &Secret,
) -> Result<(), ConfigError> {
    let path = credential_path(home, name, label)?;
    write_atomic(&path, secret.0.as_bytes(), 0o600)
}

/// Deletes `credentials/<name>/<label>` (`fiber logout`), and the label's lock
/// file and the provider's directory when nothing else is in it. `true` when
/// a credential was removed. The same name, label and symbolic link checks as
/// [`store_credential`] apply.
pub fn delete_credential(home: &Path, name: &str, label: &str) -> Result<bool, ConfigError> {
    let path = credential_path(home, name, label)?;
    if !plain(&path, false)? {
        return Ok(false);
    }
    fs::remove_file(&path).map_err(|source| ConfigError::Io {
        file: path.clone(),
        source,
    })?;
    let mut lock = path.clone().into_os_string();
    lock.push(".lock");
    // The lock file is only a lock; a failure to remove it leaves the
    // directory, which a later login reuses.
    fs::remove_file(&lock).unwrap_or(());
    // Still holds another label: `remove_dir` refuses a directory with
    // anything in it.
    if let Some(dir) = path.parent() {
        fs::remove_dir(dir).unwrap_or(());
    }
    Ok(true)
}

/// The labels stored for `name`, sorted: the regular files in
/// `credentials/<name>/`, without lock and temporary files. Empty when the
/// directory is absent or `credentials/<name>` is a file or a link.
pub fn credential_labels(home: &Path, name: &str) -> Result<Vec<String>, ConfigError> {
    if !one_file_name(name) {
        return Err(ConfigError::SecretName { name: name.into() });
    }
    let dir = home.join("credentials").join(name);
    plain(&home.join("credentials"), true)?;
    // A file or a symbolic link here lists nothing; reading a label reports it.
    if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&dir).map_err(|source| ConfigError::Io {
        file: dir.clone(),
        source,
    })?;
    let mut labels = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Io {
            file: dir.clone(),
            source,
        })?;
        let regular = entry.file_type().is_ok_and(|t| t.is_file());
        if let (true, Some(label)) = (regular, entry.file_name().to_str())
            && !is_lock_or_tmp(label)
        {
            labels.push(label.to_owned());
        }
    }
    labels.sort();
    Ok(labels)
}
