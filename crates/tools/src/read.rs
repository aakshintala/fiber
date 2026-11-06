//! The `read` tool (`docs/tools.md`, "File tools").

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::{FileError, InspectError, ReadError};
use crate::files::{
    Inspected, Shared, attach_seen, declare, effects_error, hash_bytes, inspect, resolve,
};
use crate::pdf::{PageRange, page_range};
use crate::tool_util::{errored, text_output};

/// The tool's own cut (`docs/tools.md`, "Bounded results"): the default cap.
/// The loop's bound sits above this, so a result is not cut twice and no
/// artifact is written.
const CAP: usize = 16_384;

/// How far the loop's bound for a `read` sits above its own cut, so the loop
/// never cuts a `read`'s own result and a cut `read` writes no artifact.
const NOTICE_ROOM: usize = 16_384;

/// At most this many sibling names a missing-path message lists; the rest
/// collapse into a count (`docs/tools.md`, "read").
const MAX_LOOKALIKES: usize = 10;

/// Reads a text file or an image.
pub struct Read {
    shared: Arc<Shared>,
    cap: usize,
}

impl Read {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared, cap: CAP }
    }
}

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    offset: Option<i64>,
    limit: Option<i64>,
    pages: Option<String>,
}

impl Tool for Read {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_owned(),
            description:
                "Reads a text file, or a PNG, JPEG, GIF or WebP image, which comes back as an \
                 image, or a PDF, which comes back as a PDF. A PDF of more than 10 pages needs \
                 `pages`, at most 20 pages a call. The text is the file's own, with no line \
                 numbers and no byte order mark. `offset` is the first line, counted from 1. \
                 `limit` is how many lines. A directory is not listed; use the shell."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file. A relative path is resolved against the workspace."
                    },
                    "offset": {
                        "type": ["integer", "null"],
                        "description": "The first line to return, counted from 1. The default is 1."
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "description": "How many lines to return. The default is the rest of the file."
                    },
                    "pages": {
                        "type": ["string", "null"],
                        "description": "For a PDF only: a page or a range such as `3` or `1-5`, counted from 1."
                    }
                },
                "required": ["path", "offset", "limit", "pages"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let args: Args = crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        let resolved = resolve(self.shared.workspace(), &args.path).map_err(effects_error)?;
        self.shared.note_judged(&args.path, &resolved);
        Ok(declare(Effect::Reads, true, &resolved))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return crate::tool_util::cancelled_before();
        }
        let args: Args = match crate::tool_util::parsed(arguments) {
            Ok(args) => args,
            Err(error) => return errored(ReadError::from(error)),
        };
        let raw = args.path;
        let offset = match line_argument(args.offset, "offset", 1) {
            Ok(offset) => offset,
            Err(error) => return errored(error),
        };
        let limit = match line_argument(args.limit, "limit", usize::MAX) {
            Ok(limit) => limit,
            Err(error) => return errored(error),
        };
        let pages = match page_range(args.pages.as_deref()) {
            Ok(pages) => pages,
            Err(error) => return crate::tool_util::errored(ReadError::Pdf(error)),
        };
        let path = match resolve(self.shared.workspace(), &raw) {
            Ok(path) => path,
            Err(error) => return errored(ReadError::from(FileError::from(error))),
        };
        let judged = self.shared.judged(&raw);
        if let Err(error) = crate::tool_util::recheck(
            &raw,
            &path,
            None,
            judged.as_deref(),
            crate::tool_util::Act::Read,
        ) {
            return crate::tool_util::errored(error);
        }
        // debt: the whole file is read into memory, a measured file that does not fit
        let text = match inspect(&path) {
            Ok(Inspected::Text { text }) => {
                if let Err(error) = reject_pages(&path, pages) {
                    return errored(error);
                }
                text
            }
            // `offset` and `limit` do not apply to an image.
            Ok(Inspected::Image { hash, bytes, .. }) => {
                if let Err(error) = reject_pages(&path, pages) {
                    return errored(error);
                }
                let mut output = crate::image::read(self.shared.images(), &path, &bytes, cancel);
                // Seen for a later `write`: the bytes this read took in. A
                // failed or cancelled read returned no image, so saw nothing.
                if output
                    .content
                    .iter()
                    .any(|part| matches!(part, ContentPart::Image { .. }))
                {
                    self.shared.set_seen(&path, hash);
                    output.details = attach_seen(output.details, &path, hash);
                }
                return output;
            }
            // `offset` and `limit` do not apply to a PDF.
            Ok(Inspected::Pdf { hash, .. }) => {
                let mut output = crate::pdf::read(self.shared.images(), &path, pages, cancel);
                // Seen for a later `write`: the bytes this read took in. A
                // failed or cancelled read returned no PDF, so saw nothing.
                if output
                    .content
                    .iter()
                    .any(|part| matches!(part, ContentPart::Pdf(_)))
                {
                    self.shared.set_seen(&path, hash);
                    output.details = attach_seen(output.details, &path, hash);
                }
                return output;
            }
            // A PDF over the cap never reaches the child. `pages` does not
            // apply: the file cannot be read at all, grammar errors aside
            // (parsed before inspection).
            Ok(Inspected::PdfOverCap { size }) => {
                return errored(ReadError::from(FileError::PdfOverCap {
                    path: path.to_path_buf(),
                    size,
                }));
            }
            Ok(Inspected::Unsupported { kind, size, hint }) => {
                if let Err(error) = reject_pages(&path, pages) {
                    return errored(error);
                }
                return errored(ReadError::from(FileError::Unsupported {
                    path: path.to_path_buf(),
                    kind,
                    size,
                    hint,
                }));
            }
            Err(error) => return errored(missing(error)),
        };
        let body = text.strip_prefix('\u{feff}').unwrap_or(text.as_str());
        match slice_text(body, offset, limit, self.cap) {
            Ok(shown) => {
                let hash = hash_bytes(text.as_bytes());
                self.shared.set_seen(&path, hash);
                let mut output = text_output(shown);
                output.details = attach_seen(Some(json!({"start_line": offset})), &path, hash);
                output
            }
            Err(error) => errored(error),
        }
    }

    fn bound(&self) -> Bound {
        Bound {
            start: self.cap.saturating_add(NOTICE_ROOM),
            end: 0,
        }
    }

    fn with_cap(&self, cap: usize) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            shared: Arc::clone(&self.shared),
            cap,
        }))
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("read")
    }
}

/// A failed inspection's error. A missing path names a same-directory entry
/// whose name matches apart from Unicode spaces (`docs/tools.md`, "read");
/// every other failure keeps its own error. Nothing is ever opened here.
fn missing(error: InspectError) -> ReadError {
    let InspectError::NotFound { path } = error else {
        return ReadError::from(FileError::from(error));
    };
    let Some(parent) = path.parent().map(Path::to_path_buf) else {
        return not_found(path);
    };
    let Some(wanted) = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
    else {
        return not_found(path);
    };
    let (shown, omitted) = space_lookalikes(&parent, &wanted);
    if shown.is_empty() {
        return not_found(path);
    }
    ReadError::SpaceLookalike {
        requested: path,
        shown,
        omitted,
    }
}

/// The plain missing-path error, unchanged by the lookalike scan.
fn not_found(path: PathBuf) -> ReadError {
    ReadError::from(FileError::from(InspectError::NotFound { path }))
}

/// Every whitespace character folded to one plain space for sibling-name
/// comparison (`docs/tools.md`, "read"). All else compares as itself, so a
/// zero-width space never matches.
fn fold_spaces(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect()
}

/// Basenames in `dir` matching `wanted` apart from Unicode spaces: at most
/// the 10 smallest sorted matches plus the count of the rest. Names that do
/// not decode never match. Any failure reading the directory discards the
/// partials and reports no match, so the caller falls back to its plain
/// `not_found`.
fn space_lookalikes(dir: &Path, wanted: &str) -> (Vec<PathBuf>, usize) {
    let folded = fold_spaces(wanted);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return (Vec::new(), 0),
    };
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut total = 0usize;
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(_) => return (Vec::new(), 0),
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if fold_spaces(name) != folded {
            continue;
        }
        total += 1;
        let position = kept
            .binary_search(&path)
            .unwrap_or_else(|position| position);
        if position < MAX_LOOKALIKES {
            kept.insert(position, path);
            kept.truncate(MAX_LOOKALIKES);
        }
    }
    let omitted = total - kept.len();
    (kept, omitted)
}

/// Fails the call when `pages` names a file that is not a PDF
/// (`docs/tools.md`, "read"). `not_found` and path errors come first,
/// because inspection runs first.
fn reject_pages(path: &Path, pages: Option<PageRange>) -> Result<(), ReadError> {
    match pages {
        Some(_) => Err(ReadError::PagesOnNonPdf {
            path: path.to_path_buf(),
        }),
        None => Ok(()),
    }
}

/// `offset` and `limit`. Absent means `default`. Below 1 is
/// `invalid_arguments`.
fn line_argument(
    value: Option<i64>,
    key: &'static str,
    default: usize,
) -> Result<usize, ReadError> {
    let Some(value) = value else {
        return Ok(default);
    };
    if value < 1 {
        return Err(ReadError::LineBelowOne { key });
    }
    usize::try_from(value).map_err(|_| ReadError::LineTooLarge { key })
}

fn slice_text(text: &str, offset: usize, limit: usize, cap: usize) -> Result<String, ReadError> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    if offset > total && !(total == 0 && offset == 1) {
        return Err(ReadError::OffsetPastEnd { total });
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
        if used.saturating_add(len) > cap && count > 0 {
            break;
        }
        if len > cap && count == 0 {
            let prefix = line
                .get(..line.floor_char_boundary(cap))
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
            "[Line {offset} is longer than {cap} bytes; showing its first {bytes} bytes. \
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
