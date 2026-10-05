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
use crate::home::read;
use crate::{Source, keys, path};

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
/// picker). The value's type, and the type of every known key it holds, is
/// checked against "Keys"; nothing else is.
pub fn set_global(home: &Path, key: &str, value: Value) -> Result<(), ConfigError> {
    let file = home.join("config.json");
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    let mut candidate = Value::Object(Map::new());
    path::set(&mut candidate, &segments, value.clone());
    if let Value::Object(map) = candidate {
        keys::check(map, &Source::Global(file.clone()), &mut Vec::new())?;
    }
    update(&file, &segments, value)
}

/// Deletes an extension's settings in the global and every per-project layer
/// (`fiber extension remove`).
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
    // `<extension>` is slugged as for `extensions/` (docs/state.md).
    dir.join("config")
        .join(format!("{}.json", extension.replace('/', "-")))
}

/// Appends `line` to a line-based file under its lock, creating the
/// directory: reads the file, adds the line, and renames a temporary file
/// over it, so a reader sees the old file or the new one, never half
/// (`docs/state.md`, "Concurrent access").
pub(crate) fn append_line(file: &Path, line: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let _lock = locked(file)?;
    let mut current = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io(source)),
    };
    if !current.is_empty() && !current.ends_with(b"\n") {
        current.push(b'\n');
    }
    current.extend_from_slice(line.as_bytes());
    current.push(b'\n');
    write_atomic(file, &current, 0o666)
}

/// Reads `file` under its lock, sets one key and writes the whole file back,
/// keys sorted with a 2-space indent.
pub(crate) fn update(file: &Path, key: &[String], value: Value) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let _lock = locked(file)?;
    let mut root = read(file)?.unwrap_or_else(|| Value::Object(Map::new()));
    path::set(&mut root, key, value);
    let mut text = serde_json::to_string_pretty(&root).map_err(|e| io(e.into()))?;
    text.push('\n');
    write_atomic(file, text.as_bytes(), 0o666)
}

/// Takes the lock for a whole-file write to `file`: creates the parent
/// directory, then holds `file.lock` until the caller renames over `file`
/// (`docs/state.md`, "Concurrent access").
pub(crate) fn locked(file: &Path) -> Result<File, ConfigError> {
    let lock = open_lock(file, 0o666)?;
    lock.lock().map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    Ok(lock)
}

/// Creates the parent directory and opens `file.lock` with `mode`, without
/// locking it, so a caller chooses to wait or to try.
pub(crate) fn open_lock(file: &Path, mode: u32) -> Result<File, ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let mut lock_name = file.as_os_str().to_owned();
    lock_name.push(".lock");
    make_parent(file)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(mode)
        .open(&lock_name)
        .map_err(io)
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
    let dir = file.parent().unwrap_or(Path::new("."));
    // Syncing the directory makes the rename itself survive a crash.
    let written = write_synced(&tmp, bytes, mode)
        .and_then(|()| fs::rename(&tmp, file))
        .and_then(|()| File::open(dir)?.sync_all());
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
