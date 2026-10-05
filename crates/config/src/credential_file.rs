//! A stored OAuth credential (`docs/model-routing.md`, "Credentials"): the
//! file `credentials/<provider>/<label>` in Fiber home, mode 0600 in a 0700
//! directory. It is read and written only under a lock, so two sessions never
//! refresh one token twice (`docs/model-routing.md`, "Keys, tokens and
//! OAuth"; `docs/state.md`, "Concurrent access").

use std::fs::{self, File, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ConfigError;
use crate::home::{one_file_name, plain};
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
    /// file name, and a `credentials/` or provider directory that is a
    /// symbolic link, which a tool call or a repository could plant to
    /// redirect secrets, are refused.
    pub fn new(home: &Path, provider: &str, label: &str) -> Result<Self, ConfigError> {
        for name in [provider, label] {
            if !one_file_name(name) {
                return Err(ConfigError::SecretName { name: name.into() });
            }
        }
        let this = Self {
            path: home.join("credentials").join(provider).join(label),
        };
        this.check_directories()?;
        Ok(this)
    }

    /// Takes the lock without waiting. `None` when another holder has it.
    /// Creates `credentials/` and the provider's directory mode 0700.
    pub fn try_lock(&self) -> Result<Option<CredentialLock>, ConfigError> {
        self.check_directories()?;
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

    fn check_directories(&self) -> Result<(), ConfigError> {
        let mut dir = self.path.parent();
        // The provider's directory, then `credentials/`.
        for _ in 0..2 {
            let Some(here) = dir else { break };
            plain(here, true)?;
            dir = here.parent();
        }
        Ok(())
    }
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
        if !plain(&self.path, false)? {
            return Ok(None);
        }
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    file: self.path.clone(),
                    source,
                });
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| ConfigError::Json {
                file: self.path.clone(),
                line: e.line(),
                column: e.column(),
            })
    }

    /// Replaces the stored object: a temporary file created mode 0600 and
    /// renamed over the old one, so a reader sees the old file or the new,
    /// never half, and never a wider mode. A failure before the rename leaves
    /// the old file.
    pub fn write(&self, value: &Value) -> Result<(), ConfigError> {
        let bytes = serde_json::to_vec(value).map_err(|e| ConfigError::Io {
            file: self.path.clone(),
            source: e.into(),
        })?;
        write_atomic(&self.path, &bytes, 0o600)
    }
}

#[cfg(test)]
#[path = "credential_file_tests.rs"]
mod tests;
