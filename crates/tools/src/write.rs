//! The `write` tool (`docs/tools.md`, "File tools").

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::FileChange;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};
use similar::{ChangeTag, TextDiff};

use crate::files::land::{land, shape_replacement};
use crate::files::{
    Shared, declare, effects_error, hash_bytes, kind_of, path_text, resolve, resolved,
    string_argument, unsupported_message,
};
use crate::tool_util::failed;

/// Creates or replaces a text file.
pub struct Write {
    shared: Arc<Shared>,
}

impl Write {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl Tool for Write {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".to_owned(),
            description: "Creates a file, and any missing parent directories, or replaces one. \
                 Replacing keeps the file's line endings and byte order mark, and is refused \
                 until this session has seen the file's current bytes."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file. A relative path is resolved against the workspace."
                    },
                    "content": {
                        "type": "string",
                        "description": "The bytes to store. A new file is stored as given."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let raw = string_argument(arguments, "path", "Give the file path as `path`.")
            .map_err(EffectsError::Arguments)?;
        let _content = string_argument(arguments, "content", "Give the file content as `content`.")
            .map_err(EffectsError::Arguments)?;
        let resolved = resolve(self.shared.workspace(), &raw).map_err(effects_error)?;
        self.shared.note_judged(&raw, &resolved);
        Ok(declare(Effect::Writes, reversible(&resolved), &resolved))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return crate::tool_util::cancelled_before();
        }
        let raw = match string_argument(arguments, "path", "Give the file path as `path`.") {
            Ok(raw) => raw,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let content =
            match string_argument(arguments, "content", "Give the file content as `content`.") {
                Ok(content) => content,
                Err(message) => return failed(ErrorCode::InvalidArguments, message),
            };
        let key = match resolved(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(output) => return output,
        };
        // Taken before the wait, so two replaces that both saw the same read
        // cannot both land: the second finds the first's bytes.
        let captured = self.shared.seen(&key);
        let locks = self.shared.locks();
        let _guard = locks.lock(&key);
        let path = match resolved(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(output) => return output,
        };
        let judged = self.shared.judged(&raw);
        if let Err(output) = crate::tool_util::recheck(
            &raw,
            &path,
            Some(&key),
            judged.as_deref(),
            crate::tool_util::Act::Write,
        ) {
            return output;
        }
        let existing = match existing(&path) {
            Ok(existing) => existing,
            Err((code, message)) => return failed(code, message),
        };
        let (created, old, written) = match existing {
            None => (true, None, content.into_bytes()),
            Some(old) => {
                if let Some(output) = stale(&path, captured, &old) {
                    return output;
                }
                let written = shape_replacement(&old, &content);
                (false, Some(old), written)
            }
        };
        if let Err(err) = land(&path, &written) {
            return failed(
                ErrorCode::ToolError,
                format!("`{}` could not be written: {err}.", path.display()),
            );
        }
        self.shared.set_seen(&path, hash_bytes(&written));
        stored(&path, created, old.as_deref(), &written)
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("write")
    }
}

/// A path that is not there yet is a reversible create. Anything we cannot
/// stat is declared irreversible, so a replace is never offered as reversible.
fn reversible(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Ok(_) | Err(_) => false,
    }
}

fn existing(path: &Path) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err((
                ErrorCode::ToolError,
                format!("`{}` could not be written: {err}.", path.display()),
            ));
        }
    };
    if let Some((kind, hint)) = kind_of(meta.file_type()) {
        return Err((
            ErrorCode::UnsupportedFile,
            unsupported_message(path, kind, meta.len(), hint),
        ));
    }
    // A PDF over the read cap is never readable, so it is never seen and
    // never writable: refuse it before its whole bytes are loaded below
    // (`docs/tools.md`, "read").
    if crate::pdf::pdf_over_cap(meta.len()) {
        match crate::files::pdf_magic(path) {
            Ok(true) => {
                return Err((
                    ErrorCode::UnsupportedFile,
                    crate::pdf::over_cap_message(path, meta.len()),
                ));
            }
            Ok(false) => {}
            Err(error) => {
                return Err((
                    ErrorCode::ToolError,
                    format!("`{}` could not be written: {error}.", path.display()),
                ));
            }
        }
    }
    read_present(path)
}

// Stat already classified this as a regular file. NotFound means it disappeared
// before the read, which is the same as creating it.
fn read_present(path: &Path) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err((
            ErrorCode::ToolError,
            format!("`{}` could not be written: {err}.", path.display()),
        )),
    }
}

fn stale(path: &Path, captured: Option<u64>, bytes: &[u8]) -> Option<Output> {
    let message = match captured {
        None => format!(
            "`{}` was not read in this context. Read it first.",
            path.display()
        ),
        Some(seen) if seen != hash_bytes(bytes) => format!(
            "`{}` changed since it was read. Read it first.",
            path.display()
        ),
        Some(_) => return None,
    };
    Some(failed(ErrorCode::StaleFile, message))
}

fn stored(path: &Path, created: bool, old: Option<&[u8]>, written: &[u8]) -> Output {
    let lines = line_count(written);
    let size = u64::try_from(written.len()).unwrap_or(u64::MAX);
    let verb = if created { "Created" } else { "Replaced" };
    let text = format!("{verb} {}: {size} bytes, {lines} lines.", path_text(path));
    let (added, removed) = match old {
        None => (lines, 0),
        Some(old) => line_changes(old, written),
    };
    Output {
        content: vec![ContentPart::Text { text }],
        changes: Some(vec![FileChange {
            path: path_text(path),
            added,
            removed,
        }]),
        ..Output::default()
    }
}

pub(crate) fn line_count(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let extra = usize::from(bytes.last() != Some(&b'\n'));
    u64::try_from(newlines.saturating_add(extra)).unwrap_or(u64::MAX)
}

pub(crate) fn line_changes(old: &[u8], new: &[u8]) -> (u64, u64) {
    let (Ok(old), Ok(new)) = (std::str::from_utf8(old), std::str::from_utf8(new)) else {
        return (line_count(new), line_count(old));
    };
    diff_lines(old, new)
}

fn diff_lines(old: &str, new: &str) -> (u64, u64) {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0u64;
    let mut removed = 0u64;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
