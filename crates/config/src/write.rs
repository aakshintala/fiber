//! Writing configuration (`docs/configuration.md`, "When Fiber writes"): take
//! the lock, read the file, change one key, and rename a temporary file over
//! it (`docs/state.md`, "Concurrent access").

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::{keys, path, slug};

/// Which extension settings file `host.config.set` writes: the same words
/// `host.data_dir` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `config/<extension>.json` in Fiber home.
    Machine,
    /// `projects/<key>/config/<extension>.json` in Fiber home.
    Project,
}

/// Sets one key in the global `config.json` (`fiber config set`, the model
/// picker). The value's type is checked against "Keys" when the key is known;
/// nothing else is.
pub fn set_global(home: &Path, key: &str, value: Value) -> Result<(), ConfigError> {
    let file = home.join("config.json");
    let segments = parse_key(key)?;
    if let Some(known) = keys::leaf(&segments)
        && !known.kind.accepts(&value)
    {
        return Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: path::display(&segments),
            expected: known.kind.expected(),
        });
    }
    update(&file, &segments, value)
}

/// Sets one key in an extension's settings file (`host.config.set`).
pub fn set_extension_setting(
    home: &Path,
    project: &str,
    extension: &str,
    scope: Scope,
    key: &str,
    value: Value,
) -> Result<(), ConfigError> {
    let dir = match scope {
        Scope::Machine => home.to_path_buf(),
        Scope::Project => home.join("projects").join(project),
    };
    update(&settings_file(&dir, extension), &parse_key(key)?, value)
}

/// Deletes an extension's settings in the global and every per-project layer
/// (`fiber remove`).
pub fn remove_extension_settings(home: &Path, extension: &str) -> Result<(), ConfigError> {
    let mut dirs = vec![home.to_path_buf()];
    match fs::read_dir(home.join("projects")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| ConfigError::Io {
                    file: home.join("projects"),
                    source,
                })?;
                dirs.push(entry.path());
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ConfigError::Io {
                file: home.join("projects"),
                source,
            });
        }
    }
    for dir in dirs {
        let file = settings_file(&dir, extension);
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => return Err(ConfigError::Io { file, source }),
        }
    }
    Ok(())
}

/// `config/<slug>.json` under a layer's directory.
pub(crate) fn settings_file(dir: &Path, extension: &str) -> PathBuf {
    dir.join("config").join(format!("{}.json", slug(extension)))
}

fn parse_key(key: &str) -> Result<Vec<String>, ConfigError> {
    path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })
}

/// Reads `file` under its lock, sets one key and writes the whole file back,
/// keys sorted with a 2-space indent.
fn update(file: &Path, key: &[String], value: Value) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let mut lock_name = file.as_os_str().to_owned();
    lock_name.push(".lock");
    make_parent(file)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_name)
        .map_err(io)?;
    lock.lock().map_err(io)?;
    let mut root = match fs::read(file) {
        Ok(bytes) => crate::parse(file, &bytes)?,
        Err(e) if e.kind() == ErrorKind::NotFound => Value::Object(Map::new()),
        Err(source) => return Err(io(source)),
    };
    path::set(&mut root, key, value);
    let mut text = serde_json::to_string_pretty(&root).map_err(|e| io(e.into()))?;
    text.push('\n');
    write_atomic(file, text.as_bytes(), 0o666)
}

fn make_parent(file: &Path) -> Result<(), ConfigError> {
    let Some(dir) = file.parent() else {
        return Ok(());
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|source| ConfigError::Io {
            file: dir.to_path_buf(),
            source,
        })
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to a temporary file created with `mode` beside `file`,
/// syncs it, and renames it over `file`, so a reader sees the old file or the
/// new one, never half.
pub(crate) fn write_atomic(file: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    make_parent(file)?;
    let mut tmp_name = file.as_os_str().to_owned();
    tmp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp_name);
    let written = write_synced(&tmp, bytes, mode).and_then(|()| fs::rename(&tmp, file));
    if written.is_err() {
        fs::remove_file(&tmp).unwrap_or(());
    }
    written.map_err(io)
}

fn write_synced(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut out: File = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)?;
    out.write_all(bytes)?;
    out.sync_all()
}
