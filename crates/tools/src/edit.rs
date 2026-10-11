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
use serde::Deserialize;
use serde_json::{Map, Value, json};
use similar::TextDiff;

use matching::{Applied, Block, MatchError, Report};

use crate::files::{
    InspectError, Inspected, Shared, declare, effects_error, hash_bytes, inspect,
    path_text, resolve, resolved, unsupported_message,
};
use crate::tool_util::failed;
use crate::write::line_changes;

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    edits: Vec<Block>,
}

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
        let args: Args =
            crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        blocks(&args.edits).map_err(EffectsError::Arguments)?;
        let resolved =
            resolve(self.shared.workspace(), &args.path).map_err(effects_error)?;
        self.shared.note_judged(&args.path, &resolved);
        Ok(declare(Effect::Writes, false, &resolved))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return crate::tool_util::cancelled_before();
        }
        let args: Args = match crate::tool_util::arguments(arguments) {
            Ok(args) => args,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let raw = args.path;
        let all = args.edits;
        let blocks = match blocks(&all) {
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
        let text = match inspect(&path) {
            Ok(Inspected::Pdf { size, .. }) | Ok(Inspected::PdfOverCap { size }) => {
                return failed(
                    ErrorCode::UnsupportedFile,
                    unsupported_message(&path, "a PDF", size, "Edit changes text files only."),
                );
            }
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
        let applied = match matching::apply(&text, blocks) {
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

fn blocks(edits: &[Block]) -> Result<&[Block], String> {
    if edits.is_empty() {
        return Err("`edits` must contain at least one block.".to_owned());
    }
    Ok(edits)
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
