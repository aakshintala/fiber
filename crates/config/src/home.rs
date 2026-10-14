//! Where configuration lives (`docs/state.md`): Fiber home and its `FIBER_HOME`
//! override, a project's key, and reading a file that may be untrusted.

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ConfigError;

/// Fiber home: `FIBER_HOME` when set, which must be an absolute path, or
/// `.fiber` in the home directory. A missing directory is created, mode 0700
/// (`docs/state.md`, "Override").
pub fn fiber_home(
    fiber_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let dir = fiber_home_path(fiber_home, home)?;
    create_fiber_home(&dir)?;
    Ok(dir)
}

/// Where Fiber home is, as [`fiber_home`] resolves it, without creating
/// anything.
pub fn fiber_home_path(
    fiber_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    match fiber_home {
        Some(value) if value.is_empty() => Err(ConfigError::FiberHome(
            "FIBER_HOME is empty; set it to an absolute path or unset it.",
        )),
        Some(value) => {
            let dir = PathBuf::from(value);
            if dir.is_relative() {
                return Err(ConfigError::FiberHome(
                    "FIBER_HOME must be an absolute path.",
                ));
            }
            Ok(dir)
        }
        None => match home.map(PathBuf::from) {
            Some(home) if home.is_absolute() => Ok(home.join(".fiber")),
            Some(_) | None => Err(ConfigError::FiberHome(
                "HOME is not an absolute path, so Fiber home is unknown; set FIBER_HOME.",
            )),
        },
    }
}

/// Creates Fiber home at `dir` and any missing parent, mode 0700.
pub fn create_fiber_home(dir: &Path) -> Result<(), ConfigError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|source| ConfigError::Io {
            file: dir.to_path_buf(),
            source,
        })
}

/// [`fiber_home`] from the process's `FIBER_HOME` and `HOME`.
pub fn fiber_home_from_env() -> Result<PathBuf, ConfigError> {
    fiber_home(std::env::var_os("FIBER_HOME"), std::env::var_os("HOME"))
}

/// [`fiber_home_path`] from the process's `FIBER_HOME` and `HOME`.
pub fn fiber_home_path_from_env() -> Result<PathBuf, ConfigError> {
    fiber_home_path(std::env::var_os("FIBER_HOME"), std::env::var_os("HOME"))
}

/// A project's key, naming `projects/<key>/` in Fiber home: the slug of the
/// project's identity path, every `/` made `-` (`docs/state.md`, "Projects"),
/// so it is always one file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectKey(String);

impl ProjectKey {
    /// Refuses a key that is not one file name, such as an absolute path or
    /// `..`, which would name a directory outside `projects/`.
    pub fn new(key: impl Into<String>) -> Result<Self, ConfigError> {
        let key = key.into();
        if one_file_name(&key) {
            Ok(Self(key))
        } else {
            Err(ConfigError::ProjectKey { key })
        }
    }

    /// The key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether `name` names one entry in a directory and nothing above it.
pub(crate) fn one_file_name(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']))
}

/// Refuses a symbolic link, or anything but a directory (`dir`) or a regular
/// file, at `path`. `false` when nothing is there.
pub(crate) fn plain(path: &Path, dir: bool) -> Result<bool, ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if (dir && meta.is_dir()) || (!dir && meta.is_file()) => Ok(true),
        Ok(_) => Err(ConfigError::NotPlain {
            file: path.to_path_buf(),
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ConfigError::Io {
            file: path.to_path_buf(),
            source,
        }),
    }
}

/// A file's bytes, or `None` when it does not exist.
pub(crate) fn read_bytes(file: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            file: file.to_path_buf(),
            source,
        }),
    }
}

/// A file's JSON, or `None` when it does not exist.
pub(crate) fn read(file: &Path) -> Result<Option<Value>, ConfigError> {
    read_bytes(file)?
        .map(|bytes| parse(file, &bytes))
        .transpose()
}

/// Strict JSON: no comments, no trailing commas.
pub(crate) fn parse(file: &Path, bytes: &[u8]) -> Result<Value, ConfigError> {
    serde_json::from_slice(bytes).map_err(|e| ConfigError::Json {
        file: file.to_path_buf(),
        line: e.line(),
        column: e.column(),
    })
}
