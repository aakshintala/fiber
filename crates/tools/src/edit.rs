//! The `edit` tool (`docs/tools.md`, "File tools").

mod matching;

use std::path::Path;
use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::FileChange;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};
use similar::TextDiff;

use matching::{Applied, Block, MatchError, Report};

use crate::files::{
    InspectError, Inspected, Shared, declare, effects_error, failed, hash_bytes, inspect,
    path_text, resolve, resolved, string_argument, text_output, unsupported_message,
};
use crate::write::line_changes;

/// Replaces stretches of a text file. Every block matches the file as it was
/// before the call, and the file is written once or not at all.
pub struct Edit {
    shared: Arc<Shared>,
}

impl Edit {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl Tool for Edit {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".to_owned(),
            description: "Replaces one or more stretches of a text file. Each old_text must \
                 occur exactly once. The file is written once, or not at all."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file. A relative path is resolved against the workspace."
                    },
                    "edits": {
                        "type": "array",
                        "description": "The blocks to apply, each matched against the file as it is now.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {
                                    "type": "string",
                                    "description": "The text to replace. It must occur exactly once."
                                },
                                "new_text": {
                                    "type": "string",
                                    "description": "The text to put in its place."
                                }
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let raw = string_argument(arguments, "path", "Give the file path as `path`.")
            .map_err(EffectsError::Arguments)?;
        let _blocks = blocks(arguments).map_err(EffectsError::Arguments)?;
        let resolved = resolve(self.shared.workspace(), &raw).map_err(effects_error)?;
        self.shared.note_judged(&raw, &resolved);
        Ok(declare(Effect::Writes, false, &resolved))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return text_output("Cancelled before it started.\n".to_owned());
        }
        let raw = match string_argument(arguments, "path", "Give the file path as `path`.") {
            Ok(raw) => raw,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let blocks = match blocks(arguments) {
            Ok(blocks) => blocks,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let key = match resolved(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(output) => return output,
        };
        let locks = self.shared.locks();
        let _guard = locks.lock(&key);
        let path = match resolved(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(output) => return output,
        };
        if path != key
            || self
                .shared
                .judged(&raw)
                .is_some_and(|judged| judged != path)
        {
            return failed(
                ErrorCode::PathChanged,
                format!(
                    "`{raw}` changed between the permission check and the write. Nothing was written."
                ),
            );
        }
        let text = match inspect(&path) {
            Ok(Inspected::Text { text }) => text,
            Ok(Inspected::Image { kind, size, .. }) => {
                return failed(
                    ErrorCode::UnsupportedFile,
                    unsupported_message(&path, kind, size, "Edit changes text files only."),
                );
            }
            Ok(Inspected::Unsupported { kind, size, hint }) => {
                return failed(
                    ErrorCode::UnsupportedFile,
                    unsupported_message(&path, &kind, size, hint),
                );
            }
            Err(InspectError::NotFound) => {
                return failed(
                    ErrorCode::NotFound,
                    format!("`{}` does not exist.", path.display()),
                );
            }
            Err(InspectError::Tool(message)) => return failed(ErrorCode::ToolError, message),
        };
        let applied = match matching::apply(&text, &blocks) {
            Ok(applied) => applied,
            Err(error) => return match_failed(error),
        };
        if let Err(err) = crate::files::land::land(&path, &applied.bytes) {
            return failed(
                ErrorCode::ToolError,
                format!("`{}` could not be written: {err}.", path.display()),
            );
        }
        self.shared.set_seen(&path, hash_bytes(&applied.bytes));
        written(&path, &text, &applied)
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("edit")
    }
}

fn blocks(arguments: &Map<String, Value>) -> Result<Vec<Block>, String> {
    let Some(value) = arguments.get("edits") else {
        return Err("Give the edits as `edits`.".to_owned());
    };
    let Some(list) = value.as_array() else {
        return Err("`edits` must be a list of blocks.".to_owned());
    };
    if list.is_empty() {
        return Err("`edits` must contain at least one block.".to_owned());
    }
    let mut blocks = Vec::with_capacity(list.len());
    for (index, block) in list.iter().enumerate() {
        let Some(object) = block.as_object() else {
            return Err(format!("`edits[{index}]` must be an object."));
        };
        let old_text = match string_argument(object, "old_text", "Give `old_text`.") {
            Ok(old_text) => old_text,
            Err(message) => return Err(format!("edits[{index}]: {message}")),
        };
        let new_text = match string_argument(object, "new_text", "Give `new_text`.") {
            Ok(new_text) => new_text,
            Err(message) => return Err(format!("edits[{index}]: {message}")),
        };
        blocks.push(Block { old_text, new_text });
    }
    Ok(blocks)
}

fn match_failed(error: MatchError) -> Output {
    let code = match &error {
        MatchError::NoMatch { .. } => ErrorCode::NoMatch,
        MatchError::Ambiguous { .. } => ErrorCode::AmbiguousMatch,
        MatchError::Invalid(_) => ErrorCode::InvalidArguments,
        MatchError::Boundary => ErrorCode::ToolError,
    };
    failed(code, error.message())
}

fn written(path: &Path, old: &str, applied: &Applied) -> Output {
    let shown = path_text(path);
    let mut lines = Vec::with_capacity(applied.reports.len() + 1);
    for report in &applied.reports {
        lines.push(report_line(report));
    }
    let size = u64::try_from(applied.bytes.len()).unwrap_or(u64::MAX);
    lines.push(format!("Wrote {shown}: {size} bytes."));
    let diff = unified(&shown, old, &String::from_utf8_lossy(&applied.bytes));
    let (added, removed) = line_changes(old.as_bytes(), &applied.bytes);
    Output {
        content: vec![ContentPart::Text {
            text: lines.join("\n"),
        }],
        details: Some(json!({ "diff": diff })),
        changes: Some(vec![FileChange {
            path: shown,
            added,
            removed,
        }]),
        ..Output::default()
    }
}

fn report_line(report: &Report) -> String {
    let occupied = match report.new_span {
        Some((start, end)) => format!("lines {start}-{end}"),
        None => "no lines".to_owned(),
    };
    let mut line = format!(
        "edits[{}]: replaced lines {}-{} with {occupied}.",
        report.index, report.old_start, report.old_end
    );
    if report.normalised {
        line.push_str(" Matched after normalising quotes, dashes and spaces.");
    }
    line
}

fn unified(path: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(path, path)
        .to_string()
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
