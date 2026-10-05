//! `read` of an image (`docs/tools.md`, "read"): one child process, `fiber
//! image`, processes the file once and writes the result to the session's
//! `artifacts/` (`docs/model-routing.md`, "Image limits"). The session runs no
//! image code (`docs/invocation.md`, "Processes").

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::Output;
use serde_json::Value;

use crate::files::failed;

/// How much of the child's standard error a failure message keeps.
const MESSAGE_CAP: usize = 2048;

/// The wiring of one session's image child.
pub(crate) struct ImageChild {
    fiber: PathBuf,
    artifacts: PathBuf,
}

impl ImageChild {
    pub(crate) fn new(fiber: PathBuf, artifacts: PathBuf) -> Self {
        Self { fiber, artifacts }
    }
}

/// What the child printed on success.
struct Stored {
    file: String,
    mime_type: String,
    width: u32,
    height: u32,
}

/// Runs the child on `path`, which `read` has already resolved and classified
/// as a regular file with an image's first bytes.
pub(crate) fn read(child: Option<&ImageChild>, path: &Path) -> Output {
    let Some(child) = child else {
        return failed(
            ErrorCode::ToolError,
            "image reading is not configured.".to_owned(),
        );
    };
    // A new name for every read: an older log line's path never points at
    // new bytes.
    let stem = format!("i_{:016x}", RandomState::new().hash_one(()));
    let output = match Command::new(&child.fiber)
        .arg("image")
        .arg(path)
        .arg(&child.artifacts)
        .arg(&stem)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return failed(
                ErrorCode::ToolError,
                format!("the image child could not start: {error}."),
            );
        }
    };
    let message = capped(&output.stderr);
    match output.status.code() {
        Some(0) => match parse(&output.stdout) {
            Ok(stored) => rendered(&stored),
            Err(why) => failed(ErrorCode::ToolError, format!("the image child {why}.")),
        },
        Some(1) => failed(
            ErrorCode::UnsupportedFile,
            format!("`{}` cannot be read as an image: {message}", path.display()),
        ),
        Some(code) => failed(
            ErrorCode::ToolError,
            format!("the image child exited with status {code}: {message}"),
        ),
        None => failed(
            ErrorCode::ToolError,
            format!("the image child was killed by a signal: {message}"),
        ),
    }
}

/// The first [`MESSAGE_CAP`] bytes of `stderr`, cut at a character boundary,
/// without the trailing newline.
fn capped(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim_end();
    text.get(..text.floor_char_boundary(MESSAGE_CAP))
        .unwrap_or_default()
        .to_owned()
}

/// The one JSON line the child prints, or why it is not.
fn parse(stdout: &[u8]) -> Result<Stored, &'static str> {
    let text = std::str::from_utf8(stdout).map_err(|_| "printed text that is not UTF-8")?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains('\n') {
        return Err("printed more than one line");
    }
    let value: Value = serde_json::from_str(line).map_err(|_| "printed a line that is not JSON")?;
    let string = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    match (
        string("file"),
        string("mime_type"),
        number("width"),
        number("height"),
    ) {
        (Some(file), Some(mime_type), Some(width), Some(height)) => Ok(Stored {
            file,
            mime_type,
            width,
            height,
        }),
        _ => Err("printed a line without file, mime_type, width and height"),
    }
}

fn rendered(stored: &Stored) -> Output {
    let Stored {
        file,
        mime_type,
        width,
        height,
    } = stored;
    Output {
        content: vec![
            ContentPart::Text {
                text: format!("Image: {width}x{height} {mime_type}.\n"),
            },
            ContentPart::Image {
                path: format!("artifacts/{file}"),
                mime_type: mime_type.clone(),
                width: *width,
                height: *height,
            },
        ],
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
