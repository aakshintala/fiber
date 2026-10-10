//! Writing configuration (`docs/configuration.md`, "When Fiber writes"): take
//! the lock, read the file, change one key, and rename a temporary file over
//! it (`docs/state.md`, "Concurrent access").

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use contract::events::Notice;

use crate::error::ConfigError;
use crate::home::{ProjectKey, plain, read};
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

/// Which configuration file `set` writes (`docs/configuration.md`, "When
/// Fiber writes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `config.json` at the top of Fiber home.
    Global,
    /// `projects/<key>/config.json` in Fiber home.
    Project,
    /// The workspace's `.fiber/config.json`.
    Repository,
}

/// Sets one key in a layer's file (`fiber config set`). The value's type is
/// checked against "Keys", as [`set_global`] checks it, and a key the
/// layer may not hold is refused before anything is written: on any `Err`
/// no file is created or changed.
pub fn set(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    key: &str,
    value: Value,
) -> Result<(), ConfigError> {
    let (file, source) = layer_file(home, workspace, project, layer)?;
    let (segments, notices) = checked(key, value.clone(), &source)?;
    if !notices.is_empty() {
        return Err(ConfigError::Refused {
            key: key.into(),
            file,
            why: refused_why(&segments, &source),
        });
    }
    update(&file, &segments, value, false).map(|_| ())
}

/// One change to a list of names in one layer's file (`/tools`, `/skills`).
pub struct ListEdit<'a> {
    /// The list's dotted key.
    pub key: &'a str,
    /// The name to add or remove.
    pub name: &'a str,
    /// What to do to the list.
    pub change: ListChange,
    /// The list in force below this file, when its file sets none.
    pub inherited: Option<&'a [String]>,
}

/// What a [`ListEdit`] does to its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListChange {
    /// Adds the name, starting from the inherited list (or none) when the file holds no list.
    Add,
    /// Removes every copy of the name, starting likewise.
    Remove,
    /// Adds the name only when a list is in force (the file's or the
    /// inherited one) and lacks it: `tools.enabled` when a tool is switched
    /// on, decided under the file's lock.
    AddIfListed,
}

/// Applies `edits` to `layer`'s file under one lock and one write; `true`
/// when it wrote. A failure before the rename leaves the file unchanged; a
/// failure after it, syncing the directory, returns `Err` with the new
/// content already in place.
pub fn edit_list(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    edits: &[ListEdit<'_>],
) -> Result<bool, ConfigError> {
    if edits.is_empty() {
        return Ok(false);
    }
    let (file, source) = layer_file(home, workspace, project, layer)?;
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    let mut changed = false;
    for edit in edits {
        if apply_edit(&mut root, edit, &source, &file)? {
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    write_root(&file, &root)?;
    Ok(true)
}

/// Applies one edit to the file's in-memory root; whether its list changed.
/// Every edit is type-checked as `set` checks it, changed or not, so a key
/// the layer may not set is refused even when it would change nothing.
fn apply_edit(
    root: &mut Value,
    edit: &ListEdit<'_>,
    source: &Source,
    file: &Path,
) -> Result<bool, ConfigError> {
    let segments = path::parse(edit.key).ok_or_else(|| ConfigError::Override {
        arg: edit.key.into(),
    })?;
    let resolved = resolve_spelling(root, &segments);
    let shown = path::display(&resolved);
    // The file's list at the leaf, else the one the file inherits.
    let current: Option<Vec<String>> = match path::get(root, &resolved) {
        None => edit.inherited.map(|list| list.to_vec()),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Some(other) => {
            check_list(&shown, edit.key, &resolved, other.clone(), source, file)?;
            None
        }
    };
    let name = edit.name.to_owned();
    match edit.change {
        ListChange::Add => {
            let mut list = current.clone().unwrap_or_default();
            if list.iter().any(|n| n == edit.name) {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                return Ok(false);
            }
            list.push(name);
            check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
            path::set(root, &resolved, Value::Array(list_into(list)));
            Ok(true)
        }
        ListChange::Remove => {
            let mut list = current.clone().unwrap_or_default();
            if !list.iter().any(|n| n == edit.name) {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                return Ok(false);
            }
            list.retain(|n| n != edit.name);
            check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
            path::set(root, &resolved, Value::Array(list_into(list)));
            Ok(true)
        }
        ListChange::AddIfListed => match current {
            Some(mut list) if !list.iter().any(|n| n == edit.name) => {
                list.push(name);
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                path::set(root, &resolved, Value::Array(list_into(list)));
                Ok(true)
            }
            Some(list) => {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                Ok(false)
            }
            None => {
                check_list(
                    &shown,
                    edit.key,
                    &resolved,
                    Value::Array(Vec::new()),
                    source,
                    file,
                )?;
                Ok(false)
            }
        },
    }
}

/// The leaf list the file already holds under one spelling of an
/// extension's name keeps that spelling; a new list joins an existing
/// spelling of the same extension; else the key's own spelling stands. One
/// leaf is resolved at a time, so two lists may keep different spellings
/// without one key ever being set under both (`docs/configuration.md`,
/// "Keys").
fn resolve_spelling(root: &Value, segments: &[String]) -> Vec<String> {
    let [area, name, rest @ ..] = segments else {
        return segments.to_vec();
    };
    if area != "extensions" {
        return segments.to_vec();
    }
    let mut same: Vec<String> = root
        .get("extensions")
        .and_then(Value::as_object)
        .map(|extensions| {
            extensions
                .keys()
                .filter(|key| crate::names::full_name(key) == crate::names::full_name(name))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    same.sort();
    let holds = |key: &String| {
        let mut at = vec!["extensions".to_owned(), key.clone()];
        at.extend(rest.iter().cloned());
        path::get(root, &at).is_some()
    };
    let chosen = same.iter().find(|key| holds(key)).or_else(|| same.first());
    match chosen {
        Some(key) => {
            let mut resolved = vec!["extensions".to_owned(), key.clone()];
            resolved.extend(rest.iter().cloned());
            resolved
        }
        None => segments.to_vec(),
    }
}

/// Type-checks `value` for `shown` as `set` checks it: a notice is the same
/// refusal `set` returns.
fn check_list(
    shown: &str,
    key: &str,
    segments: &[String],
    value: Value,
    source: &Source,
    file: &Path,
) -> Result<(), ConfigError> {
    let (_, notices) = checked(shown, value, source)?;
    if notices.is_empty() {
        return Ok(());
    }
    Err(ConfigError::Refused {
        key: key.into(),
        file: file.to_path_buf(),
        why: refused_why(segments, source),
    })
}

fn strings_of(list: &[String]) -> Value {
    Value::Array(
        list.iter()
            .map(|name| Value::String(name.clone()))
            .collect(),
    )
}

fn list_into(list: Vec<String>) -> Vec<Value> {
    list.into_iter().map(Value::String).collect()
}

/// The file and source `set` and `edit_list` write through for `layer`.
fn layer_file(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
) -> Result<(PathBuf, Source), ConfigError> {
    let (file, source) = match layer {
        Layer::Global => {
            let file = home.join("config.json");
            let source = Source::Global(file.clone());
            (file, source)
        }
        Layer::Project => {
            let file = home
                .join("projects")
                .join(project.as_str())
                .join("config.json");
            let source = Source::Project(file.clone());
            (file, source)
        }
        Layer::Repository => {
            let file = workspace.join(".fiber/config.json");
            let source = Source::Repository(file.clone());
            (file, source)
        }
    };
    if matches!(layer, Layer::Repository) {
        // Someone else's text: a link, or anything but a directory and a
        // regular file, is refused before the lock is taken, as
        // `Config::load` refuses to read it.
        plain(&workspace.join(".fiber"), true)?;
        plain(&file, false)?;
    }
    Ok((file, source))
}

/// Why `set` refused `key`: the notice `keys::check` pushed, read back off
/// the table, since `keys` names no reason of its own. Only called with a
/// notice in hand, so a known key that is not repository-settable was
/// written to a repository, a global-only key was written to another layer,
/// a person-files key was written outside the person's own files, and any
/// other known key is repository-only.
fn refused_why(segments: &[String], source: &Source) -> &'static str {
    match keys::leaf(segments) {
        None => "this Fiber does not know it",
        Some(found) if !found.repo && matches!(source, Source::Repository(_)) => {
            "a repository may not set it"
        }
        Some(found) => match found.scope {
            keys::Scope::GlobalOnly => "only Fiber home's `config.json` may set it",
            keys::Scope::PersonFiles => {
                "only Fiber home's `config.json` or the project's `config.json` in Fiber home may set it"
            }
            keys::Scope::Any | keys::Scope::RepoOnly => "only a repository's own file may set it",
        },
    }
}

/// Parses `key` and checks one key's value against "Keys" for `source`,
/// returning the segments and the notices the check pushed. Shared by
/// [`set_global`], [`set_global_if_unset`], [`replace_global`] and [`set`]:
/// only `set` turns a notice into a refusal, so an unknown key is still
/// written elsewhere.
fn checked(
    key: &str,
    value: Value,
    source: &Source,
) -> Result<(Vec<String>, Vec<Notice>), ConfigError> {
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    let mut candidate = Value::Object(Map::new());
    path::set(&mut candidate, &segments, value);
    let mut notices = Vec::new();
    if let Value::Object(map) = candidate {
        keys::check(map, source, &mut notices)?;
    }
    Ok((segments, notices))
}

/// Sets one key in the global `config.json` (`fiber config set`, the model
/// picker). The value's type, and the type of every known key it holds, is
/// checked against "Keys"; nothing else is.
pub fn set_global(home: &Path, key: &str, value: Value) -> Result<(), ConfigError> {
    let file = home.join("config.json");
    let (segments, _) = checked(key, value.clone(), &Source::Global(file.clone()))?;
    update(&file, &segments, value, false).map(|_| ())
}

/// [`set_global`] when the global `config.json` holds no value at `key`,
/// checked and written under one lock (`fiber login` writing
/// `providers."<name>".credential` with a provider's first label). `true`
/// when it wrote. A project layer's value does not count: only the global
/// file is looked at.
pub fn set_global_if_unset(home: &Path, key: &str, value: Value) -> Result<bool, ConfigError> {
    let file = home.join("config.json");
    let (segments, _) = checked(key, value.clone(), &Source::Global(file.clone()))?;
    update(&file, &segments, value, true)
}

/// Sets one key in the global `config.json` to `value`, or removes it for
/// `None`, under one lock and one atomic write, returning the value it
/// replaced (`fiber hub install` writing `hub.port`). A value is type-checked
/// as [`set_global`] checks it. Removing a key the file does not hold
/// writes nothing. A failure before the rename leaves the file unchanged; a
/// failure after it, syncing the directory, returns `Err` with the new
/// content already in place.
pub fn replace_global(
    home: &Path,
    key: &str,
    value: Option<Value>,
) -> Result<Option<Value>, ConfigError> {
    let file = home.join("config.json");
    let segments = match &value {
        Some(value) => checked(key, value.clone(), &Source::Global(file.clone()))?.0,
        None => path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?,
    };
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    let previous = match value {
        Some(value) => {
            let previous = path::get(&root, &segments).cloned();
            path::set(&mut root, &segments, value);
            previous
        }
        None => match path::remove(&mut root, &segments) {
            Some(previous) => Some(previous),
            None => return Ok(None),
        },
    };
    write_root(&file, &root)?;
    Ok(previous)
}

/// The value at `key` in Fiber home's `config.json` alone: no other layer
/// and no default is consulted. `None` when the file or the key is absent.
pub fn get_global(home: &Path, key: &str) -> Result<Option<Value>, ConfigError> {
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    Ok(read(&home.join("config.json"))?.and_then(|root| path::get(&root, &segments).cloned()))
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

/// `config/<extension>.json` under a layer's directory, `<extension>`
/// named as in `extensions/` (`docs/state.md`, "What each part holds").
pub(crate) fn settings_file(dir: &Path, extension: &str) -> PathBuf {
    dir.join("config")
        .join(format!("{}.json", crate::names::dir_name(extension)))
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

/// Deletes physical line `line` of `file`, counted from 1, when it still
/// reads `text`: `true` when it removed. Every other byte is kept,
/// including blank lines, unknown keys, CRLF endings and a final line
/// with no newline. A line is cut as `str::lines` cuts it. A missing
/// file, a line number of 0 or past the end, or a line whose text
/// changed removes nothing and answers `false`, writing nothing; a
/// missing file gets no directory or lock file. The lock is held from
/// the read to the rename (`docs/state.md`, "Concurrent access").
pub(crate) fn remove_line(file: &Path, line: usize, text: &str) -> Result<bool, ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    if !plain(file, false)? {
        return Ok(false);
    }
    let _lock = locked(file)?;
    // A file deleted after the check above reports the missing read as an I/O error.
    let current = fs::read(file).map_err(io)?;
    let segments: Vec<&[u8]> = current.split_inclusive(|b| *b == b'\n').collect();
    let Some(segment) = line.checked_sub(1).and_then(|index| segments.get(index)) else {
        return Ok(false);
    };
    let mut held = *segment;
    let mut newline = false;
    if let Some(rest) = held.strip_suffix(b"\n") {
        held = rest;
        newline = true;
    }
    if newline {
        held = held.strip_suffix(b"\r").unwrap_or(held);
    }
    if held != text.as_bytes() {
        return Ok(false);
    }
    let mut rest = Vec::new();
    for (index, other) in segments.iter().enumerate() {
        if index + 1 != line {
            rest.extend_from_slice(other);
        }
    }
    write_atomic(file, &rest, 0o666)?;
    Ok(true)
}

/// Reads `file` under its lock, sets one key and writes the whole file back,
/// keys sorted with a 2-space indent. With `only_if_unset`, a file that
/// already holds a value at `key` is left alone. `true` when it wrote.
pub(crate) fn update(
    file: &Path,
    key: &[String],
    value: Value,
    only_if_unset: bool,
) -> Result<bool, ConfigError> {
    let _lock = locked(file)?;
    let mut root = read(file)?.unwrap_or_else(|| Value::Object(Map::new()));
    if only_if_unset && path::get(&root, key).is_some() {
        return Ok(false);
    }
    path::set(&mut root, key, value);
    write_root(file, &root)?;
    Ok(true)
}

/// Writes `root` over `file`, keys sorted with a 2-space indent, for a
/// caller holding the file's lock.
fn write_root(file: &Path, root: &Value) -> Result<(), ConfigError> {
    let mut text = serde_json::to_string_pretty(root).map_err(|e| ConfigError::Io {
        file: file.to_path_buf(),
        source: e.into(),
    })?;
    text.push('\n');
    write_atomic(file, text.as_bytes(), 0o666)
}

/// Callers blocked in [`locked`], raised before they wait, so a test can
/// observe that a second write is blocked rather than sleeping.
#[cfg(test)]
static WAITING: AtomicUsize = AtomicUsize::new(0);

/// How many callers are blocked in [`locked`].
///
/// Raised before the wait, so a test can observe that a second write is
/// blocked rather than sleeping.
#[cfg(test)]
fn waiting() -> usize {
    WAITING.load(Ordering::SeqCst)
}

/// Takes the lock for a whole-file write to `file`: creates the parent
/// directory, then holds `file.lock` until the caller renames over `file`
/// (`docs/state.md`, "Concurrent access").
pub(crate) fn locked(file: &Path) -> Result<File, ConfigError> {
    let lock = open_lock(file, 0o666)?;
    #[cfg(test)]
    WAITING.fetch_add(1, Ordering::SeqCst);
    let outcome = lock.lock().map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    });
    #[cfg(test)]
    WAITING.fetch_sub(1, Ordering::SeqCst);
    outcome?;
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

// Pause point between creating the temporary file and renaming it over the
// destination (`docs/testing.md`, "Waits and timeouts"): the
// credential-mode test installs a hook to hold the writer there, so the race
// between the temporary file being visible and the rename happens on every
// run instead of being waited for. Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static BEFORE_RENAME: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

/// Installs the pause-point hook run between creating the temporary file and
/// renaming it (`docs/testing.md`, "Waits and timeouts"), on the current
/// thread. The credential-mode test uses it to hold the writer with the
/// temporary file visible, so the race happens on every run.
#[cfg(test)]
pub(crate) fn before_rename(hook: impl Fn() + 'static) {
    BEFORE_RENAME.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

/// Where [`fail_at`] injects a write failure in `write_atomic`.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Before the rename: the file is left unchanged.
    BeforeRename,
    /// After the rename, syncing the directory: the new content is in place.
    AfterRename,
}

#[cfg(test)]
thread_local! {
    static FAIL_AT: std::cell::Cell<Option<Stage>> = const { std::cell::Cell::new(None) };
}

/// Injects an I/O failure at `stage` in `write_atomic` on this thread;
/// `None` clears it. Test-only; non-test builds never fail.
#[cfg(test)]
pub(crate) fn fail_at(stage: Option<Stage>) {
    FAIL_AT.with(|cell| cell.set(stage));
}

#[cfg(test)]
fn fail_stage() -> Option<Stage> {
    FAIL_AT.with(|cell| cell.get())
}

/// Writes `bytes` to a temporary file created with `mode` beside `file`,
/// syncs it, and renames it over `file`, so a reader sees the old file or the
/// new one, never half.
pub fn write_atomic(file: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
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
        .and_then(|()| {
            #[cfg(test)]
            BEFORE_RENAME.with(|cell| {
                if let Some(hook) = cell.borrow().as_ref() {
                    hook();
                }
            });
            #[cfg(test)]
            if fail_stage() == Some(Stage::BeforeRename) {
                return Err(std::io::Error::other("injected"));
            }
            fs::rename(&tmp, file)
        })
        .and_then(|()| {
            #[cfg(test)]
            if fail_stage() == Some(Stage::AfterRename) {
                return Err(std::io::Error::other("injected"));
            }
            File::open(dir)?.sync_all()
        });
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

#[cfg(test)]
#[path = "write_lock_tests.rs"]
mod lock_tests;

mod entries;

pub use entries::update_global_entries;

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use fakes::TempDir;
    use serde_json::json;

    use super::*;

    /// How long the test waits for a thread before failing.
    const DEADLINE: Duration = Duration::from_secs(10);

    fn wait_until(what: &str, pred: impl Fn() -> bool + Send + 'static) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            while !pred() {
                thread::yield_now();
            }
            done.send(()).unwrap();
        });
        assert!(
            finished.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for {what}"
        );
    }

    /// Runs `f`, which may block, on a worker and returns its result,
    /// failing the test after [`DEADLINE`] with `what` named.
    fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let _sent = done.send(f());
        });
        match finished.recv_timeout(DEADLINE) {
            Ok(value) => value,
            Err(_) => panic!("waited {DEADLINE:?} for {what}"),
        }
    }

    #[test]
    fn a_second_update_waits_for_the_files_lock_then_keeps_both_writes() {
        let dir = TempDir::new("fiber-write-lock");
        let file = dir.path().join("fiber-acme.json");
        let setup = file.clone();
        within("the first update", move || {
            update(&setup, &["a".to_owned()], Value::from(1), false)
        })
        .unwrap();
        let setup = file.clone();
        let held = within("the test to take the file's lock", move || locked(&setup)).unwrap();
        let (started, started_rx) = mpsc::channel();
        let (done, done_rx) = mpsc::channel();
        let worker_file = file.clone();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            update(&worker_file, &["b".to_owned()], Value::from(2), false).unwrap();
            done.send(()).unwrap();
        });
        assert!(
            started_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the second update to reach the file's lock"
        );
        wait_until("the second update to be waiting on the file's lock", || {
            waiting() == 1
        });
        assert!(
            done_rx.try_recv().is_err(),
            "the second update finished while the file was locked"
        );
        // Another session's write lands while the second update waits:
        // the lock serializes whole-file writes, it does not hide them.
        fs::write(&file, "{\"a\": 1, \"c\": 3}\n").unwrap();
        drop(held);
        assert!(
            done_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the second update to finish after the release"
        );
        worker.join().unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(written, json!({"a": 1, "b": 2, "c": 3}));
    }

    #[test]
    fn a_remove_waits_for_the_files_lock_then_keeps_the_other_write() {
        let dir = TempDir::new("fiber-write-lock");
        let file = dir.path().join("rules");
        fs::write(&file, "{\"gone\": 1}\n").unwrap();
        let setup = file.clone();
        let held = within("the test to take the file's lock", move || locked(&setup)).unwrap();
        let (started, started_rx) = mpsc::channel();
        let (done, done_rx) = mpsc::channel();
        let worker_file = file.clone();
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            let removed = remove_line(&worker_file, 1, "{\"gone\": 1}").unwrap();
            done.send(removed).unwrap();
        });
        assert!(
            started_rx.recv_timeout(DEADLINE).is_ok(),
            "waited {DEADLINE:?} for the remove to reach the file's lock"
        );
        wait_until("the remove to be waiting on the file's lock", || {
            waiting() == 1
        });
        assert!(
            done_rx.try_recv().is_err(),
            "the remove finished while the file was locked"
        );
        // Another session's line lands while the remove waits: the lock
        // serializes whole-file writes, it does not hide them.
        fs::write(&file, "{\"gone\": 1}\n{\"late\": 3}\n").unwrap();
        drop(held);
        let removed = match done_rx.recv_timeout(DEADLINE) {
            Ok(removed) => removed,
            Err(_) => panic!("waited {DEADLINE:?} for the remove to finish after the release"),
        };
        worker.join().unwrap();
        assert!(removed, "the held line is still there");
        assert_eq!(fs::read(&file).unwrap(), "{\"late\": 3}\n".as_bytes());
    }
}
