//! Reading and saving the person's reviewer notes (`docs/permissions.md`,
//! "What the person tells it"): each person layer's own `reviewer.context`
//! with its version, the confirmed `shell.read_only` entries beside it, and
//! a locked read-compare-write of the layers a save sends (`docs/state.md`,
//! "Concurrent access").

use std::collections::BTreeSet;
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
    /// The read-only entries to merge into `shell.read_only`: an empty
    /// list means no entry changes.
    pub read_only: Vec<ReadOnlyFlags>,
}

/// One confirmed read-only entry: the command path and the flag names it
/// may take and stay read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyFlags {
    /// The command path, such as `"acme-cli query"`.
    pub command: String,
    /// The flag names the entry allows.
    pub flags: Vec<String>,
}

/// Which layer of the reviewer notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryLayer {
    /// Fiber home's `config.json`.
    Global,
    /// The project's own `config.json`.
    Project,
}

/// One read-only entry the save would add to a layer: the layer, its file,
/// and the entry. Entries identical to the layer's own table converge and
/// are omitted, so every item here is an addition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPreview {
    /// The layer the entry would land in.
    pub layer: EntryLayer,
    /// The layer's file.
    pub file: PathBuf,
    /// The command path, such as `"acme-cli query"`.
    pub command: String,
    /// The flag names the entry allows.
    pub flags: Vec<String>,
}

/// Why a read-only entry is refused: every variant fails the save with
/// the existing `config_invalid` code, naming the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyEntryProblem {
    /// The command names no words.
    Empty,
    /// The built-in list already holds the command.
    Builtin,
    /// A flag is write-shaped.
    UnsafeFlag,
    /// A command word holds a `/`.
    SlashPath,
    /// The command names a single word with no subcommand.
    OneWord,
    /// A flag carries a `=value`.
    ValuedFlag,
    /// The command equals, contains or falls under a configured key.
    Overlap,
    /// The command repeats in one layer.
    Duplicate,
    /// The layer's `shell.read_only` cannot be read.
    Malformed,
}

/// Whether `command` with `flags` may be saved as a read-only entry: the
/// built-in list, write-shaped flags and slash paths come from the
/// contract data tables; one-word paths, valued flags, overlap and layer
/// shape are checked by the save around this. The loop's ambiguity guard
/// stays in the loop: this never parses a command line.
pub fn validate_read_only_entry(
    command: &str,
    flags: &[String],
) -> Result<(), ReadOnlyEntryProblem> {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.is_empty() {
        return Err(ReadOnlyEntryProblem::Empty);
    }
    if builtin_held(command) {
        return Err(ReadOnlyEntryProblem::Builtin);
    }
    if flags.iter().any(|flag| {
        let name = flag.split('=').next().unwrap_or(flag);
        contract::notes::WRITE_SHAPED_FLAGS.contains(&name)
    }) {
        return Err(ReadOnlyEntryProblem::UnsafeFlag);
    }
    if words.iter().any(|word| word.contains('/')) {
        return Err(ReadOnlyEntryProblem::SlashPath);
    }
    if words.len() < 2 {
        return Err(ReadOnlyEntryProblem::OneWord);
    }
    if flags.iter().any(|flag| flag.contains('=')) {
        return Err(ReadOnlyEntryProblem::ValuedFlag);
    }
    Ok(())
}

/// Whether the built-in list owns `command`: a table entry at or above
/// it, word for word. The table holds the built-in names with their
/// slash-qualified extensions, so no path is stripped here.
fn builtin_held(command: &str) -> bool {
    contract::notes::BUILTIN_READ_ONLY_NAMES
        .iter()
        .any(|held| command == *held || command.starts_with(&format!("{held} ")))
}

/// Whether two command paths overlap: they are equal, or one extends the
/// other word for word. Shared with the read-only gather, which suppresses
/// a candidate under any configured key by this same predicate.
pub fn overlaps(first: &str, second: &str) -> bool {
    first == second
        || first.starts_with(&format!("{second} "))
        || second.starts_with(&format!("{first} "))
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

/// Saves the sent layers' `reviewer.context` with their confirmed
/// `shell.read_only` entries, global first then project. Each sent layer
/// is re-read under its lock and refused, or answered `Stale`, before
/// either file is written: a mismatch on one layer writes neither. A
/// missing version, a changed byte, a created file and a deleted file are
/// all mismatches. One file's failure never stops the other file: each
/// entry names whether it was saved, with any error. A layer not sent is
/// not read or written and has no entry.
///
/// Entries merge into each layer's `shell.read_only` beside the notes, in
/// the one locked write under the layer's unchanged whole-file version, so
/// both land or neither does. Every entry is validated before any layer
/// is written, against both layers' configured keys and the run's other
/// kept entries: a refused entry names itself with the existing
/// `config_invalid` code and writes neither file. A layer whose file
/// holds `shell.read_only` in no readable shape fails an entry merge the
/// same way. A layer whose merged bytes equal its read bytes reports
/// `Saved` without rewriting, so a retry converges instead of erroring.
pub fn save_reviewer_notes(
    home: &Path,
    project: &ProjectKey,
    global: Option<NotesWrite>,
    project_layer: Option<NotesWrite>,
) -> Result<NotesSaved, ConfigError> {
    // The repository layer is never read or written.
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
        pending.push((file.clone(), sent, bytes, root));
    }
    // Entries merge only when the run sends some: a notes-only save reads
    // no configured keys and refuses nothing new.
    if pending
        .iter()
        .any(|(_, sent, _, _)| !sent.read_only.is_empty())
    {
        check_entries(&global_file, &project_file, &pending)?;
    }
    let mut files = Vec::new();
    for (file, sent, bytes, root) in pending {
        let merged = with_notes_and_entries(&file, root, &sent.text, &sent.read_only)?;
        // The file already holds the notes and entries: converge instead
        // of rewriting.
        if Some(pretty_bytes(&file, &merged)?.as_slice()) == bytes.as_deref() {
            files.push(FileSaved {
                file,
                outcome: FileOutcome::Saved,
            });
            continue;
        }
        let outcome = match write_root(&file, &merged) {
            Ok(()) => FileOutcome::Saved,
            Err(error @ ConfigError::Unsynced { .. }) => FileOutcome::SavedUnsynced(error),
            Err(error) => FileOutcome::NotSaved(error),
        };
        files.push(FileSaved { file, outcome });
    }
    Ok(NotesSaved::Written(files))
}

/// The refusal for `entry`: the existing `config_invalid` code naming
/// `shell.read_only."<command>"`.
fn entry_error(file: &Path, entry: &ReadOnlyFlags, problem: ReadOnlyEntryProblem) -> ConfigError {
    let expected = match problem {
        ReadOnlyEntryProblem::Empty => "a non-empty command",
        ReadOnlyEntryProblem::Builtin => "a command the built-in list does not hold",
        ReadOnlyEntryProblem::UnsafeFlag => "flags without write-shaped names",
        ReadOnlyEntryProblem::SlashPath => "a command path without a `/`",
        ReadOnlyEntryProblem::OneWord => "a command with a subcommand path",
        ReadOnlyEntryProblem::ValuedFlag => "flags without values",
        ReadOnlyEntryProblem::Overlap => "a command outside the configured entries",
        ReadOnlyEntryProblem::Duplicate => "one entry per command",
        ReadOnlyEntryProblem::Malformed => "a readable `shell.read_only` object",
    };
    ConfigError::WrongType {
        source_name: file.display().to_string(),
        key: format!("shell.read_only.\"{}\"", entry.command),
        expected: expected.into(),
    }
}

/// Every entry in `sent` validated and de-duplicated, in order: the shape
/// and duplicate refusals the save raises before it writes either layer.
/// The save's check and its preview share this, so the two cannot disagree
/// on what a sent entry is.
fn checked_sent_entries(file: &Path, sent: &[ReadOnlyFlags]) -> Result<(), ConfigError> {
    for entry in sent {
        validate_read_only_entry(&entry.command, &entry.flags)
            .map_err(|problem| entry_error(file, entry, problem))?;
    }
    let mut seen = BTreeSet::new();
    for entry in sent {
        if !seen.insert(entry.command.clone()) {
            return Err(entry_error(file, entry, ReadOnlyEntryProblem::Duplicate));
        }
    }
    Ok(())
}

/// `sent` merged over the layer's current entries: every sent command takes
/// its flags, so an identical entry converges and any other sent entry is
/// added. The save's writer and its preview merge through this one
/// function, so the two cannot disagree on what an entry adds.
fn merged_entries(current: &HashMapFlags, sent: &[ReadOnlyFlags]) -> HashMapFlags {
    let mut next = current.clone();
    for entry in sent {
        next.insert(entry.command.clone(), entry.flags.clone());
    }
    next
}

/// The entries `sent` would add over the layer's current table: entries
/// identical to what the layer already holds converge and are omitted. A
/// changed entry — one overlapping any configured key or an earlier kept
/// entry — is the save's `Overlap` refusal. Every sent entry, identical or
/// added, joins `kept` for the next layer's check, exactly as the save
/// checks the run's entries global first. The save's check and its preview
/// share this, so the two cannot disagree on what a sent entry adds.
fn checked_additions(
    file: &Path,
    sent: &[ReadOnlyFlags],
    current: Option<&HashMapFlags>,
    configured: &[&HashMapFlags],
    kept: &mut Vec<ReadOnlyFlags>,
) -> Result<Vec<ReadOnlyFlags>, ConfigError> {
    let empty = HashMapFlags::new();
    let current = current.unwrap_or(&empty);
    let merged = merged_entries(current, sent);
    let mut added = Vec::new();
    for entry in sent {
        let changed = merged.get(&entry.command) != current.get(&entry.command);
        if changed
            && (configured
                .iter()
                .flat_map(|table| table.keys())
                .any(|key| overlaps(key, &entry.command))
                || kept
                    .iter()
                    .any(|kept| overlaps(&kept.command, &entry.command)))
        {
            return Err(entry_error(file, entry, ReadOnlyEntryProblem::Overlap));
        }
        kept.push(entry.clone());
        if changed {
            added.push(entry.clone());
        }
    }
    Ok(added)
}

/// Refuses the run's entries before any layer is written: every entry's
/// own shape, a repeated command in one layer, an unreadable
/// `shell.read_only` in either layer, and an overlap with either layer's
/// configured keys or the run's other kept entries. A refusal names its
/// entry with the existing `config_invalid` code.
fn check_entries(
    global_file: &Path,
    project_file: &Path,
    pending: &[(PathBuf, &NotesWrite, Option<Vec<u8>>, Value)],
) -> Result<(), ConfigError> {
    for (file, sent, _, _) in pending {
        checked_sent_entries(file, &sent.read_only)?;
    }
    // Both layers' configured keys, read before any layer is written. A
    // sent layer's keys come from its locked read; a layer that is not
    // sent is read unlocked for its keys alone. A missing file covers
    // nothing, while a file with no readable `shell.read_only` fails the
    // merge naming the run's first entry.
    let first = pending
        .iter()
        .find_map(|(file, sent, _, _)| sent.read_only.first().map(|entry| (file, entry)));
    let Some((first_file, first_entry)) = first else {
        return Ok(());
    };
    let malformed = || entry_error(first_file, first_entry, ReadOnlyEntryProblem::Malformed);
    // Each layer's configured commands with their flags: sent layers from
    // their locked reads, the other layer read now. An entry identical to
    // what its own layer already holds converges instead of overlapping.
    let mut layers: Vec<(PathBuf, HashMapFlags)> = Vec::new();
    for file in [global_file, project_file] {
        let pending_root = pending
            .iter()
            .find(|(pending_file, _, _, _)| pending_file == file)
            .map(|(_, _, _, root)| root);
        let table = match pending_root {
            Some(root) => existing_keys(root).map_err(|_| malformed())?,
            None => unread_keys(file).map_err(|_| malformed())?,
        };
        layers.push((file.to_path_buf(), table));
    }
    // Every kept entry against both layers' keys and the run's earlier
    // kept entries, global first: a layer flip or an in-run overlap
    // refuses with the entry named, and an unchanged entry converges
    // below.
    let configured: Vec<&HashMapFlags> = layers.iter().map(|(_, table)| table).collect();
    let mut kept: Vec<ReadOnlyFlags> = Vec::new();
    for (file, sent, _, _) in pending {
        let same = layers
            .iter()
            .find(|(layer, _)| layer == file)
            .map(|(_, table)| table);
        checked_additions(file, &sent.read_only, same, &configured, &mut kept)?;
    }
    Ok(())
}

/// A layer's configured commands with each entry's flags.
type HashMapFlags = std::collections::HashMap<String, Vec<String>>;

/// The configured `shell.read_only` entries in `root`, read as the
/// nested objects dotted paths name: absent `shell` or `read_only` means
/// no entries. Present means an object mapping command paths to entry
/// objects with string-array flags; anything else is malformed.
fn existing_keys(root: &Value) -> Result<HashMapFlags, ReadOnlyEntryProblem> {
    let map = root.as_object().ok_or(ReadOnlyEntryProblem::Malformed)?;
    let Some(shell) = map.get("shell") else {
        return Ok(HashMapFlags::new());
    };
    let shell = shell.as_object().ok_or(ReadOnlyEntryProblem::Malformed)?;
    let Some(node) = shell.get("read_only") else {
        return Ok(HashMapFlags::new());
    };
    let entries = node.as_object().ok_or(ReadOnlyEntryProblem::Malformed)?;
    let mut table = HashMapFlags::new();
    for (command, value) in entries {
        let flags = value
            .get("flags")
            .and_then(Value::as_array)
            .ok_or(ReadOnlyEntryProblem::Malformed)?;
        let mut taken = Vec::new();
        for flag in flags {
            taken.push(
                flag.as_str()
                    .ok_or(ReadOnlyEntryProblem::Malformed)?
                    .to_owned(),
            );
        }
        table.insert(command.clone(), taken);
    }
    Ok(table)
}

/// The configured `shell.read_only` entries in `file`, read unlocked: a
/// missing file covers nothing. Anything present but unreadable as a
/// configured map is malformed.
fn unread_keys(file: &Path) -> Result<HashMapFlags, ReadOnlyEntryProblem> {
    let Some(bytes) = read_bytes(file).map_err(|_| ReadOnlyEntryProblem::Malformed)? else {
        return Ok(HashMapFlags::new());
    };
    let root: Value =
        serde_json::from_slice(&bytes).map_err(|_| ReadOnlyEntryProblem::Malformed)?;
    existing_keys(&root)
}

/// The configured `shell.read_only` command paths in both of the person's
/// layers, global first: a missing file covers nothing, while a present
/// file with no readable `shell.read_only` is malformed. The read-only
/// gather reads its suppression keys through this, so only `config` reads
/// configuration files (docs/architecture.md, "Boundaries between modules").
pub fn read_only_keys(
    home: &Path,
    project: &ProjectKey,
) -> Result<Vec<String>, ReadOnlyEntryProblem> {
    let mut keys = Vec::new();
    for file in [global_config_file(home), project_config_file(home, project)] {
        keys.extend(unread_keys(&file)?.keys().cloned());
    }
    Ok(keys)
}

/// `root` with `reviewer.context` set to `text` and `entries` merged into
/// `shell.read_only`, every other key kept. The context is left alone when
/// the layer holds none and the text is empty, so an entries-only save
/// adds no empty context field. The save checked every shape first, so
/// this cannot fail; a layer that changed shape under the lock is refused
/// instead of guessed at.
fn with_notes_and_entries(
    file: &Path,
    root: Value,
    text: &str,
    entries: &[ReadOnlyFlags],
) -> Result<Value, ConfigError> {
    let mut next = root;
    let map = next
        .as_object_mut()
        .ok_or_else(|| crate::top_level(&file.display().to_string()))?;
    if map
        .get("reviewer")
        .and_then(|reviewer| reviewer.get("context"))
        .is_some()
        || !text.is_empty()
    {
        let reviewer = map
            .entry("reviewer")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| ConfigError::WrongType {
                source_name: file.display().to_string(),
                key: "reviewer".into(),
                expected: "an object".into(),
            })?;
        reviewer.insert("context".to_owned(), Value::String(text.to_owned()));
    }
    if !entries.is_empty() {
        let shell = map
            .entry("shell")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| ConfigError::WrongType {
                source_name: file.display().to_string(),
                key: "shell".into(),
                expected: "an object".into(),
            })?;
        let table = shell
            .entry("read_only")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| ConfigError::WrongType {
                source_name: file.display().to_string(),
                key: "shell.read_only".into(),
                expected: "an object".into(),
            })?;
        let current = entry_table(table).ok_or_else(|| ConfigError::WrongType {
            source_name: file.display().to_string(),
            key: "shell.read_only".into(),
            expected: "an object".into(),
        })?;
        // Only changed commands are written back, so entries the save did
        // not touch keep their bytes: an identical entry converges instead
        // of rewriting. The merged table decides the flags; iterating the
        // sent entries keeps the write order deterministic.
        let merged = merged_entries(&current, entries);
        for entry in entries {
            if merged.get(&entry.command) != current.get(&entry.command) {
                table.insert(entry.command.clone(), entry_object(&entry.flags));
            }
        }
    }
    Ok(next)
}

/// The flags each configured command holds in the layer's live table.
/// `None` for a table the save's check would have refused: every flags
/// value is a string array past it.
fn entry_table(table: &Map<String, Value>) -> Option<HashMapFlags> {
    let mut current = HashMapFlags::new();
    for (command, value) in table {
        let flags = value.get("flags")?.as_array()?;
        let mut taken = Vec::new();
        for flag in flags {
            taken.push(flag.as_str()?.to_owned());
        }
        current.insert(command.clone(), taken);
    }
    Some(current)
}

/// One entry as its `{"flags": [...]}` object.
fn entry_object(flags: &[String]) -> Value {
    Value::Object(Map::from_iter([(
        "flags".to_owned(),
        Value::Array(
            flags
                .iter()
                .map(|flag| Value::String(flag.clone()))
                .collect(),
        ),
    )]))
}

/// The read-only additions the save would merge for `global` and
/// `project_layer`, global first: identical entries converge and are
/// omitted, and a changed entry fails with the writer's `Overlap` refusal.
/// Each layer reads like the save reads it — a sent layer through its
/// root, an unsent one through its keys alone — so every failure names the
/// same entry the save would name. The save re-validates under its locks,
/// so the preview is advisory and the save stays authoritative.
pub fn preview_read_only_changes(
    home: &Path,
    project: &ProjectKey,
    global: &[ReadOnlyFlags],
    project_layer: &[ReadOnlyFlags],
) -> Result<Vec<EntryPreview>, ConfigError> {
    let global_file = global_config_file(home);
    let project_file = project_config_file(home, project);
    let sent = [
        (&global_file, global, EntryLayer::Global),
        (&project_file, project_layer, EntryLayer::Project),
    ];
    // Malformed tables fail naming the run's first entry, as the save's
    // check does; with no sent entry there is nothing to add.
    let Some((first_file, first_entry)) = sent
        .iter()
        .find_map(|(file, entries, _)| entries.first().map(|entry| (*file, entry)))
    else {
        return Ok(Vec::new());
    };
    let malformed = || entry_error(first_file, first_entry, ReadOnlyEntryProblem::Malformed);
    let mut tables: Vec<HashMapFlags> = Vec::new();
    for (file, entries, _) in &sent {
        let table = if entries.is_empty() {
            unread_keys(file).map_err(|_| malformed())?
        } else {
            let (_, root) = read_root(file)?;
            checked_root(file, &root)?;
            existing_keys(&root).map_err(|_| malformed())?
        };
        tables.push(table);
    }
    let configured: Vec<&HashMapFlags> = tables.iter().collect();
    let mut kept: Vec<ReadOnlyFlags> = Vec::new();
    let mut out = Vec::new();
    for ((file, entries, layer), current) in sent.iter().zip(tables.iter()) {
        if entries.is_empty() {
            continue;
        }
        checked_sent_entries(file, entries)?;
        let added = checked_additions(file, entries, Some(current), &configured, &mut kept)?;
        for entry in &added {
            out.push(EntryPreview {
                layer: *layer,
                file: (*file).clone(),
                command: entry.command.clone(),
                flags: entry.flags.clone(),
            });
        }
    }
    Ok(out)
}

/// The bytes `write_root` would write for `root`: the convergence check
/// compares these with the read bytes.
fn pretty_bytes(file: &Path, root: &Value) -> Result<Vec<u8>, ConfigError> {
    let mut text = serde_json::to_string_pretty(root).map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source: source.into(),
    })?;
    text.push('\n');
    Ok(text.into_bytes())
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
