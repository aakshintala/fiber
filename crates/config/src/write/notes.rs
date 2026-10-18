//! Reading and saving the person's reviewer notes (`docs/permissions.md`,
//! "What the person tells it"): each person layer's own `reviewer.context`
//! with its version, and a locked read-compare-write of the layers a save
//! sends (`docs/state.md`, "Concurrent access").

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::home::{ProjectKey, read_bytes};
use crate::layout::{global_config_file, project_config_file};

use super::atomic::locked;
use super::write_root;

/// One sent layer: the version its read saw, and the text to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesWrite {
    /// The version [`reviewer_notes`](crate::reviewer_notes) read.
    pub version: String,
    /// The `reviewer.context` to store.
    pub text: String,
}

/// What a notes save did.
#[derive(Debug)]
pub enum NotesSaved {
    /// A sent version no longer matched: nothing was written.
    Stale,
    /// Every sent layer's outcome, global first.
    Written(Vec<FileSaved>),
}

/// One sent file's outcome: three states, so "not saved, no error" cannot
/// be built.
#[derive(Debug)]
pub struct FileSaved {
    /// The layer's file.
    pub file: PathBuf,
    /// Whether it was written, and any error.
    pub outcome: FileOutcome,
}

/// Whether one file was written.
#[derive(Debug)]
pub enum FileOutcome {
    /// The file holds the new notes.
    Saved,
    /// The rename landed but syncing the directory failed, so the new
    /// notes are in place with an error.
    SavedUnsynced(ConfigError),
    /// The file is byte for byte as before, with why.
    NotSaved(ConfigError),
}

/// The version of a notes file's bytes: 16 lowercase hex digits, or
/// `absent` when the file does not exist. The same function versions a
/// read and compares a save, so any changed byte is a stale save.
pub fn version(bytes: Option<&[u8]>) -> String {
    match bytes {
        None => "absent".to_owned(),
        Some(bytes) => {
            let mut hashed = DefaultHasher::new();
            bytes.hash(&mut hashed);
            format!("{:016x}", hashed.finish())
        }
    }
}

/// Saves the sent layers' `reviewer.context`, global first then project.
/// Each sent layer is re-read under its lock and refused, or answered
/// `Stale`, before either file is written: a mismatch on one layer writes
/// neither. A missing version, a changed byte, a created file and a
/// deleted file are all mismatches. One file's failure never stops the
/// other file: each entry names whether it was saved, with any error. A
/// layer not sent is not read or written and has no entry.
pub fn save_reviewer_notes(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    global: Option<NotesWrite>,
    project_layer: Option<NotesWrite>,
) -> Result<NotesSaved, ConfigError> {
    // The repository layer is never read or written.
    let _ = workspace;
    if global.is_none() && project_layer.is_none() {
        return Ok(NotesSaved::Written(Vec::new()));
    }
    let global_file = global_config_file(home);
    let project_file = project_config_file(home, project);
    // The global lock first, held to the end; every other writer holds
    // one lock at a time, so no second lock waits on one held with ours.
    let _global = match &global {
        Some(_) => Some(locked(&global_file)?),
        None => None,
    };
    // Before taking the second lock: one directory means the two writes
    // would land in the same file, so refuse instead of writing twice.
    // A second lock on the same file would wait on the first forever.
    if global.is_some() && project_layer.is_some() && same_directory(&global_file, &project_file)? {
        return Err(ConfigError::Io {
            file: project_file,
            source: io::Error::other("the global and project notes are the same file"),
        });
    }
    let _project = match &project_layer {
        Some(_) => Some(locked(&project_file)?),
        None => None,
    };
    // Read, check and compare each sent layer before writing either, so
    // a refusal or a stale version leaves both files byte for byte.
    let mut pending = Vec::new();
    for (file, sent) in [(&global_file, &global), (&project_file, &project_layer)] {
        let Some(sent) = sent else {
            continue;
        };
        let (bytes, root) = read_root(file)?;
        checked_root(file, &root)?;
        if version(bytes.as_deref()) != sent.version {
            return Ok(NotesSaved::Stale);
        }
        pending.push((file.clone(), sent.text.clone(), root));
    }
    let mut files = Vec::new();
    for (file, text, root) in pending {
        let outcome = match write_root(&file, &with_context(&file, root, &text)?) {
            Ok(()) => FileOutcome::Saved,
            Err(error @ ConfigError::Unsynced { .. }) => FileOutcome::SavedUnsynced(error),
            Err(error) => FileOutcome::NotSaved(error),
        };
        files.push(FileSaved { file, outcome });
    }
    Ok(NotesSaved::Written(files))
}

/// A file's bytes with its parsed root: missing reads as an empty object,
/// so a created file is a version mismatch, never a parse error.
fn read_root(file: &Path) -> Result<(Option<Vec<u8>>, Value), ConfigError> {
    match read_bytes(file)? {
        None => Ok((None, Value::Object(Map::new()))),
        Some(bytes) => {
            let root: Value =
                serde_json::from_slice(&bytes).map_err(|error| ConfigError::Json {
                    file: file.to_path_buf(),
                    line: error.line(),
                    column: error.column(),
                })?;
            Ok((Some(bytes), root))
        }
    }
}

/// Refuses a root that is not an object, or whose `reviewer` is present
/// and not an object: the save edits `reviewer.context` in place, so
/// either shape would silently drop what the person wrote.
fn checked_root(file: &Path, root: &Value) -> Result<(), ConfigError> {
    let map = root
        .as_object()
        .ok_or_else(|| crate::top_level(&file.display().to_string()))?;
    if let Some(reviewer) = map.get("reviewer") {
        reviewer.as_object().ok_or_else(|| ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "reviewer".into(),
            expected: "an object".into(),
        })?;
    }
    Ok(())
}

/// `root` with `reviewer.context` set to `text`, every other key kept.
/// The save checked the shape first, so this cannot fail; a layer that
/// changed shape under the lock is refused instead of guessed at.
fn with_context(file: &Path, root: Value, text: &str) -> Result<Value, ConfigError> {
    let mut next = root;
    let reviewer = next.as_object_mut().and_then(|map| {
        map.entry("reviewer")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
    });
    match reviewer {
        Some(reviewer) => {
            reviewer.insert("context".to_owned(), Value::String(text.to_owned()));
            Ok(next)
        }
        None => Err(ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "reviewer".into(),
            expected: "an object".into(),
        }),
    }
}

/// Whether the two files' parent directories are one directory, by device
/// and inode on the real directories, never by comparing spellings: a
/// symlinked project directory still resolves to the same place.
fn same_directory(global: &Path, project: &Path) -> Result<bool, ConfigError> {
    let io = |file: PathBuf, source: io::Error| ConfigError::Io { file, source };
    let global_parent = global.parent().unwrap_or_else(|| Path::new("."));
    let project_parent = project.parent().unwrap_or_else(|| Path::new("."));
    let global_meta =
        fs::metadata(global_parent).map_err(|source| io(global_parent.to_path_buf(), source))?;
    let project_meta = match fs::metadata(project_parent) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io(project_parent.to_path_buf(), source)),
    };
    Ok(global_meta.dev() == project_meta.dev() && global_meta.ino() == project_meta.ino())
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
