//! Secrets (`docs/configuration.md`, "Secrets"): one file per name in
//! `credentials/` in Fiber home, mode 0600 in a 0700 directory, and a
//! provider's stored credentials, one file per label in
//! `credentials/<name>/`. Configuration itself never holds one; it holds only
//! where a provider's credential comes from.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use contract::Secret;
use serde::Deserialize;

use crate::credential_file::{CredentialFile, CredentialLock, credential_path, is_lock_or_tmp};
use crate::error::ConfigError;
use crate::home::{one_file_name, plain};
use crate::write::write_atomic;

/// Where a provider's credential comes from, as
/// `providers."<name>".credentials."<label>"` sets it. Only the global and per-project layers may set it.
/// A `command` source carries its resolved `timeout`: the source's own, or
/// 30 s. Nothing holding a source is serialised, so there is no `Serialize`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawSource")]
pub enum CredentialSource {
    /// An environment variable holding the key.
    Env(String),
    /// A file holding the key.
    File(PathBuf),
    /// A program and its arguments that print the key, and how long a run
    /// of them may last.
    Command {
        /// A program and its arguments that print the key.
        argv: Vec<String>,
        /// How long a run of them may last: the source's `timeout`, or 30 s.
        timeout: Duration,
    },
}

/// The default `timeout` of a `command` source: 30 s.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// One credential source as written: exactly one of `env`, `file` or
/// `command`, and `timeout` only beside `command`, a string `parse_duration`
/// accepts.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    /// An environment variable holding the key; the only field when set.
    env: Option<String>,
    /// A file holding the key; the only field when set.
    file: Option<PathBuf>,
    /// A program and its arguments that print the key.
    command: Option<Vec<String>>,
    /// How long a run of `command` may last; only beside `command`.
    timeout: Option<String>,
}

// The refusal names the shape, never the value: arguments may hold a key.
impl TryFrom<RawSource> for CredentialSource {
    type Error = String;

    fn try_from(raw: RawSource) -> Result<Self, Self::Error> {
        let RawSource {
            env,
            file,
            command,
            timeout,
        } = raw;
        if timeout.is_some() && command.is_none() {
            return Err("`timeout` is only allowed beside `command`".to_owned());
        }
        match (env, file, command) {
            (Some(name), None, None) => Ok(Self::Env(name)),
            (None, Some(path), None) => Ok(Self::File(path)),
            (None, None, Some(argv)) => Ok(Self::Command {
                argv,
                timeout: match timeout {
                    None => COMMAND_TIMEOUT,
                    Some(text) => crate::keys::parse_duration(&text).ok_or_else(|| {
                        "a command's `timeout` must be a duration such as `\"2m\"`".to_owned()
                    })?,
                },
            }),
            _ => Err("expected exactly one of `env`, `file` or `command`".to_owned()),
        }
    }
}

impl CredentialSource {
    /// What `fiber logout` and `/settings` show for a source: its kind,
    /// and a command by its program alone, since its arguments may hold a
    /// key.
    pub fn describe(&self) -> String {
        match self {
            Self::Env(name) => format!("the environment variable {name}"),
            Self::File(path) => format!("the file {}", path.display()),
            Self::Command { argv, .. } => match argv.first() {
                Some(program) => format!("the command {program}"),
                None => "an empty command".to_owned(),
            },
        }
    }
}

// A command's arguments can hold a key, so only the program prints
// (`docs/code-quality.md`, "Errors").
impl fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(name) => f.debug_tuple("Env").field(name).finish(),
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Command { argv, .. } => {
                let program = argv.first().map(String::as_str);
                let redacted = argv.iter().skip(1).map(|_| "redacted");
                let argv: Vec<&str> = program.into_iter().chain(redacted).collect();
                f.debug_tuple("Command").field(&argv).finish()
            }
        }
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

fn read_at(path: &Path) -> Result<Option<Secret>, ConfigError> {
    if !plain(path, false)? {
        return Ok(None);
    }
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(Secret::new(value))),
        Err(source) => Err(ConfigError::Io {
            file: path.to_path_buf(),
            source,
        }),
    }
}

fn store_at(path: &Path, secret: &Secret) -> Result<(), ConfigError> {
    write_atomic(path, secret.expose().as_bytes(), 0o600)
}

/// Reads `credentials/<name>` (`host.secret(name)`). `None` when there is no
/// such file; a symbolic link there is refused.
pub fn read_secret(home: &Path, name: &str) -> Result<Option<Secret>, ConfigError> {
    let path = secret_path(home, name)?;
    read_at(&path)
}

/// Stores `credentials/<name>` (`fiber login <name>`), mode 0600, creating
/// `credentials/` mode 0700.
pub fn store_secret(home: &Path, name: &str, secret: &Secret) -> Result<(), ConfigError> {
    let path = secret_path(home, name)?;
    store_at(&path, secret)
}

/// Reads `credentials/<name>/<label>`. `None` when there is no such file; a
/// symbolic link there, or a `credentials/<name>` that is a file, is refused.
pub fn read_credential(
    home: &Path,
    name: &str,
    label: &str,
) -> Result<Option<Secret>, ConfigError> {
    let path = credential_path(home, name, label)?;
    read_at(&path)
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
    store_at(&path, secret)
}

/// Deletes `credentials/<name>/<label>` (`fiber logout`) under the label's
/// [`CredentialFile`] lock, so it never races a login. `true` when a
/// credential was removed. A lock held elsewhere is an error. The label's
/// `.lock` file and the provider's directory stay: the lock file's identity
/// must not change while anyone may hold it. The same name, label and
/// symbolic link checks as [`store_credential`] apply.
pub fn delete_credential(home: &Path, name: &str, label: &str) -> Result<bool, ConfigError> {
    let path = credential_path(home, name, label)?;
    if !plain(&path, false)? {
        return Ok(false);
    }
    let Some(lock) = CredentialFile::new(home, name, label)?.try_lock()? else {
        return Err(ConfigError::Io {
            file: path,
            source: io::Error::new(
                io::ErrorKind::WouldBlock,
                "another login or logout holds its lock",
            ),
        });
    };
    delete_credential_held(home, name, label, &lock)
}

/// [`delete_credential`] for a caller that already holds the label's lock,
/// such as a login undoing its own store.
pub fn delete_credential_held(
    home: &Path,
    name: &str,
    label: &str,
    _lock: &CredentialLock,
) -> Result<bool, ConfigError> {
    let path = credential_path(home, name, label)?;
    if !plain(&path, false)? {
        return Ok(false);
    }
    fs::remove_file(&path).map_err(|source| ConfigError::Io { file: path, source })?;
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
