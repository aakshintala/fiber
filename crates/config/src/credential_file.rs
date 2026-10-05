//! A stored OAuth credential (`docs/model-routing.md`, "Credentials"): the
//! file `credentials/<provider>/<label>` in Fiber home, mode 0600 in a 0700
//! directory. It is read and written only under a lock, so two sessions never
//! refresh one token twice (`docs/model-routing.md`, "Keys, tokens and
//! OAuth"; `docs/state.md`, "Concurrent access").

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ConfigError;
use crate::home::{one_file_name, plain, read};
use crate::write::{open_lock, write_atomic};

/// The lock file's mode: it holds nothing, but it is created beside a secret.
const LOCK_MODE: u32 = 0o600;

/// One stored OAuth credential. Two handles on one path exclude each other,
/// in this process or another.
#[derive(Debug, Clone)]
pub struct CredentialFile {
    path: PathBuf,
}

impl CredentialFile {
    /// `credentials/<provider>/<label>` in `home`. A name that is not one
    /// file name, a label ending in `.lock` or `.tmp` (the suffixes the lock
    /// and temporary files use, so `default.lock` would be `default`'s lock),
    /// and a `credentials/` or provider directory that is a
    /// symbolic link, which a tool call or a repository could plant to
    /// redirect secrets, are refused.
    pub fn new(home: &Path, provider: &str, label: &str) -> Result<Self, ConfigError> {
        for name in [provider, label] {
            if !one_file_name(name) {
                return Err(ConfigError::SecretName { name: name.into() });
            }
        }
        if label.ends_with(".lock") || label.ends_with(".tmp") {
            return Err(ConfigError::SecretName { name: label.into() });
        }
        let this = Self {
            path: home.join("credentials").join(provider).join(label),
        };
        check_directories(&this.path)?;
        Ok(this)
    }

    /// Takes the lock without waiting. `None` when another holder has it.
    /// Creates `credentials/` and the provider's directory mode 0700.
    pub fn try_lock(&self) -> Result<Option<CredentialLock>, ConfigError> {
        check_directories(&self.path)?;
        let file = open_lock(&self.path, LOCK_MODE)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(CredentialLock {
                _lock: file,
                path: self.path.clone(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(source)) => Err(ConfigError::Io {
                file: self.path.clone(),
                source,
            }),
        }
    }
}

/// The provider's directory, then `credentials/`, each a real directory or
/// absent.
fn check_directories(file: &Path) -> Result<(), ConfigError> {
    for dir in file.ancestors().skip(1).take(2) {
        plain(dir, true)?;
    }
    Ok(())
}

/// The held lock on a stored credential; dropping it releases the lock.
#[derive(Debug)]
pub struct CredentialLock {
    _lock: File,
    path: PathBuf,
}

impl CredentialLock {
    /// The stored object, or `None` when nothing is stored. A file that is a
    /// symbolic link is refused.
    pub fn read(&self) -> Result<Option<Value>, ConfigError> {
        check_directories(&self.path)?;
        if !plain(&self.path, false)? {
            return Ok(None);
        }
        read(&self.path)
    }

    /// Replaces the stored object: a temporary file created mode 0600 and
    /// renamed over the old one, so a reader sees the old file or the new,
    /// never half, and never a wider mode. A failure before the rename leaves
    /// the old file.
    pub fn write(&self, value: &Value) -> Result<(), ConfigError> {
        check_directories(&self.path)?;
        write_atomic(&self.path, value.to_string().as_bytes(), 0o600)
    }
}

#[cfg(test)]
#[path = "credential_file_tests.rs"]
mod tests;
