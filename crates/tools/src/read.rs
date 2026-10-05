//! The `read` tool (`docs/tools.md`, "File tools").

use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::Effect;
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::files::{
    InspectError, Inspected, ResolveError, Shared, declare, effects_error, failed, hash_bytes,
    inspect, resolve, string_argument, text_output, unsupported_message,
};

/// The tool's own cut (`docs/tools.md`, "Bounded results"). The loop's bound
/// sits above this, so a result is not cut twice and no artifact is written.
const CAP: usize = 16_384;

/// Reads a text file.
pub struct Read {
    shared: Arc<Shared>,
}

impl Read {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl Tool for Read {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_owned(),
            description: "Reads a text file. The text is the file's own, with no line numbers and \
                 no byte order mark. `offset` is the first line, counted from 1. `limit` is how \
                 many lines. A directory is not listed; use the shell."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file. A relative path is resolved against the workspace."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "The first line to return, counted from 1. The default is 1."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "How many lines to return. The default is the rest of the file."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            deferred: false,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let raw = string_argument(arguments, "path", "Give the file path as `path`.")
            .map_err(EffectsError::Arguments)?;
        let resolved = resolve(self.shared.workspace(), &raw).map_err(effects_error)?;
        self.shared.note_judged(&raw, &resolved);
        Ok(declare(Effect::Reads, true, &resolved))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return text_output("Cancelled before it started.\n".to_owned());
        }
        let raw = match string_argument(arguments, "path", "Give the file path as `path`.") {
            Ok(raw) => raw,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let offset = match line_argument(arguments, "offset", 1) {
            Ok(offset) => offset,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let limit = match line_argument(arguments, "limit", usize::MAX) {
            Ok(limit) => limit,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let path = match resolve(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(ResolveError::Arguments(message)) => {
                return failed(ErrorCode::InvalidArguments, message);
            }
            Err(ResolveError::Tool(message)) => return failed(ErrorCode::ToolError, message),
        };
        if self
            .shared
            .judged(&raw)
            .is_some_and(|judged| judged != path)
        {
            return failed(
                ErrorCode::PathChanged,
                format!(
                    "`{raw}` changed between the permission check and the read. Nothing was read."
                ),
            );
        }
        // debt: the whole file is read into memory, a measured file that does not fit
        let text = match inspect(&path) {
            Ok(Inspected::Text { text }) => text,
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
        let body = text.strip_prefix('\u{feff}').unwrap_or(text.as_str());
        match slice_text(body, offset, limit) {
            Ok(shown) => {
                self.shared.set_seen(&path, hash_bytes(text.as_bytes()));
                text_output(shown)
            }
            Err(message) => failed(ErrorCode::InvalidArguments, message),
        }
    }

    fn bound(&self) -> Bound {
        Bound {
            start: 32_768,
            end: 0,
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("read")
    }
}

/// `offset` and `limit`. Absent means `default`. Below 1, or not an integer,
/// is `invalid_arguments`.
fn line_argument(
    arguments: &Map<String, Value>,
    key: &str,
    default: usize,
) -> Result<usize, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(default);
    };
    let Some(number) = value.as_number().filter(|number| number.is_i64()) else {
        return Err(format!("`{key}` must be an integer."));
    };
    let Some(value) = number.as_i64() else {
        return Err(format!("`{key}` must be an integer."));
    };
    if value < 1 {
        return Err(format!("`{key}` must be 1 or greater."));
    }
    usize::try_from(value).map_err(|_| format!("`{key}` is too large."))
}

fn slice_text(text: &str, offset: usize, limit: usize) -> Result<String, String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    if offset > total && !(total == 0 && offset == 1) {
        return Err(format!(
            "`offset` is past the end of the file, which has {total} lines."
        ));
    }
    if total == 0 {
        return Ok(String::new());
    }
    let start = offset - 1;
    let end = start.saturating_add(limit).min(total);
    let mut shown = String::new();
    let mut used = 0usize;
    let mut count = 0usize;
    let mut cut_at = None;
    for line in lines.iter().take(end).skip(start) {
        let len = line.len();
        if used.saturating_add(len) > CAP && count > 0 {
            break;
        }
        if len > CAP && count == 0 {
            let prefix = line
                .get(..line.floor_char_boundary(CAP))
                .unwrap_or_default();
            cut_at = Some(prefix.len());
            shown.push_str(prefix);
            count = 1;
            break;
        }
        shown.push_str(line);
        used = used.saturating_add(len);
        count += 1;
    }
    let last = offset + count - 1;
    if let Some(bytes) = cut_at {
        shown.push_str(&format!(
            "[Line {offset} is longer than {CAP} bytes; showing its first {bytes} bytes. \
             Read the rest by byte range through the shell, such as cut -c or dd. \
             The file has {total} lines.]"
        ));
    } else if last < total {
        shown.push_str(&format!(
            "[Showing lines {offset}-{last} of {total}. Continue with offset={}.]",
            last + 1
        ));
    }
    Ok(shown)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
