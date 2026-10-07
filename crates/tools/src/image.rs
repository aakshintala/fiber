//! `read` of an image (`docs/tools.md`, "read"): one child process, `fiber
//! image`, processes the file once and writes the result to the session's
//! `artifacts/` (`docs/model-routing.md`, "Image limits"). The session runs no
//! image code (`docs/invocation.md`, "Processes").

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::Read;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output as Collected, Stdio};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::images::{ImageError, Images};
use contract::provider::ImageRef;
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};
use serde_json::Value;

use crate::files::{failed, text_output};

/// How much of the child's standard error a failure message keeps.
const MESSAGE_CAP: usize = 2048;

/// The wiring of one session's image child.
pub struct ImageChild {
    fiber: PathBuf,
    artifacts: PathBuf,
}

impl ImageChild {
    /// Runs `fiber image` with `fiber`, the running binary, writing into
    /// `artifacts`, the session's `artifacts/` directory.
    pub fn new(fiber: PathBuf, artifacts: PathBuf) -> Self {
        Self { fiber, artifacts }
    }
}

impl Images for ImageChild {
    fn process(&self, bytes: &[u8], cancel: &dyn Cancel) -> Result<ImageRef, ImageError> {
        if cancel.is_cancelled() {
            return Err(ImageError::Cancelled);
        }
        let stem = fresh_stem();
        let spawned = Command::new(&self.fiber)
            .arg("image")
            .arg("/dev/stdin")
            .arg(&self.artifacts)
            .arg(&stem)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut process = match spawned {
            Ok(process) => process,
            Err(error) => {
                return Err(ImageError::Failed(format!(
                    "the image child could not start: {error}."
                )));
            }
        };
        let out = process.stdout.take().map(drain);
        let err = process.stderr.take().map(drain);
        let stdin = process.stdin.take();
        // The writer runs on a scoped thread borrowing `bytes`, so no copy;
        // the drains are already running, so neither pipe fills while the
        // other is read. The calling thread runs the same cancel-and-wait
        // loop as `read`, so a cancel while the write is blocked still
        // kills and reaps the child, then joins the writer.
        let (status, write) = thread::scope(|scope| {
            let writer = scope.spawn(move || {
                let Some(mut stdin) = stdin else {
                    return Ok(());
                };
                stdin.write_all(bytes)
            });
            let status = loop {
                if cancel.is_cancelled() {
                    // `Child::kill` signals this one pid: the child is not
                    // a group leader, so no group signal is involved.
                    // Already exited, or killed here: either way it is
                    // reaped. The writer's next write then fails with
                    // EPIPE, and the scope joins it.
                    drop(process.kill());
                    drop(process.wait());
                    break None;
                }
                match process.try_wait() {
                    Ok(Some(status)) => break Some(Ok(status)),
                    Ok(None) => {
                        // The wait is for a child's exit or a cancel, which an
                        // injected clock sees neither of.
                        #[allow(
                            clippy::disallowed_methods,
                            reason = "nothing a clock can signal: the wait is for a child process's exit"
                        )]
                        thread::sleep(POLL);
                    }
                    Err(error) => break Some(Err(error)),
                }
            };
            let write = match writer.join() {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::other("the image writer panicked")),
            };
            (status, write)
        });
        let Some(status) = status else {
            return Err(ImageError::Cancelled);
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                return Err(ImageError::Failed(format!(
                    "the image child could not start: {error}."
                )));
            }
        };
        let join = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
            handle
                .and_then(|handle| handle.join().ok())
                .unwrap_or_default()
        };
        let collected = Collected {
            status,
            stdout: join(out),
            stderr: join(err),
        };
        map_process(&collected, &stem, write)
    }
}

/// Maps the child's exit and output to the processed reference. A write
/// error after the child exited reports the exit first: an exit the
/// mapping reports wins over the pipe.
fn map_process(
    output: &Collected,
    stem: &str,
    write: std::io::Result<()>,
) -> Result<ImageRef, ImageError> {
    let message = capped(&output.stderr);
    match output.status.code() {
        Some(0) => {
            if let Err(error) = write {
                return Err(ImageError::Failed(format!(
                    "the image child did not take the image: {error}."
                )));
            }
            match parse(&output.stdout, stem) {
                Ok(stored) => Ok(image_ref(&stored)),
                Err(why) => Err(ImageError::Failed(format!("the image child {why}."))),
            }
        }
        Some(1) => Err(ImageError::Unreadable(message)),
        Some(code) => Err(ImageError::Failed(format!(
            "the image child exited with status {code}: {message}"
        ))),
        None => Err(ImageError::Failed(format!(
            "the image child was killed by a signal: {message}"
        ))),
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
pub(crate) fn read(child: Option<&ImageChild>, path: &Path, cancel: &dyn Cancel) -> Output {
    let Some(child) = child else {
        return failed(
            ErrorCode::ToolError,
            "image reading is not configured.".to_owned(),
        );
    };
    // A new name for every read: an older log line's path never points at
    // new bytes.
    let stem = fresh_stem();
    let Some(output) = run_child(child, path, &stem, cancel) else {
        return text_output("Cancelled and stopped.\n".to_owned());
    };
    let output = match output {
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
        Some(0) => match parse(&output.stdout, &stem) {
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

/// A fresh stem per call: an older log line's path never points at new
/// bytes.
fn fresh_stem() -> String {
    format!("i_{:016x}", RandomState::new().hash_one(()))
}

/// Whether `file` is the name asked for: `<stem>.<ext>`, where `<ext>` is
/// one or more ASCII letters or digits. That shape has no separator, no
/// `..` and no second dot.
fn valid_file(stem: &str, file: &str) -> bool {
    let Some(rest) = file.strip_prefix(stem) else {
        return false;
    };
    let Some(ext) = rest.strip_prefix('.') else {
        return false;
    };
    !ext.is_empty() && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// How often the wait looks for a cancel and for the child's exit. Picked,
/// not measured: a cancel stops the child within one interval.
const POLL: Duration = Duration::from_millis(50);

/// Reads a pipe to its end on its own thread, so neither pipe fills while
/// the other is read.
fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error ends the capture with what arrived.
        let _ended = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Runs the child to its end. `None` when the call was cancelled: the child
/// is then killed and reaped, and its output is dropped. The child is the
/// only process, so killing it needs no group.
fn run_child(
    child: &ImageChild,
    path: &Path,
    stem: &str,
    cancel: &dyn Cancel,
) -> Option<std::io::Result<Collected>> {
    let spawned = Command::new(&child.fiber)
        .arg("image")
        .arg(path)
        .arg(&child.artifacts)
        .arg(stem)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut process = match spawned {
        Ok(process) => process,
        Err(error) => return Some(Err(error)),
    };
    let out = process.stdout.take().map(drain);
    let err = process.stderr.take().map(drain);
    let status = loop {
        if cancel.is_cancelled() {
            // `Child::kill` signals this one pid: the child is not a group
            // leader, so no group signal is involved. Already exited, or
            // killed here: either way it is reaped. The pipe threads are left
            // to end with the pipes.
            drop(process.kill());
            // A `wait` failure is dropped: the output is dropped too, and
            // the error has no caller to act on it.
            drop(process.wait());
            return None;
        }
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                // The wait is for a child's exit or a cancel, which an
                // injected clock sees neither of.
                #[allow(
                    clippy::disallowed_methods,
                    reason = "nothing a clock can signal: the wait is for a child process's exit"
                )]
                thread::sleep(POLL);
            }
            Err(error) => break Err(error),
        }
    };
    let join = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
        handle
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    };
    Some(status.map(|status| Collected {
        status,
        stdout: join(out),
        stderr: join(err),
    }))
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

/// The one JSON line the child prints, or why it is not. The named file
/// must be the one asked for.
fn parse(stdout: &[u8], stem: &str) -> Result<Stored, &'static str> {
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
        (Some(file), Some(mime_type), Some(width), Some(height)) => {
            if !valid_file(stem, &file) {
                return Err("named a file other than the one asked for");
            }
            Ok(Stored {
                file,
                mime_type,
                width,
                height,
            })
        }
        _ => Err("printed a line without file, mime_type, width and height"),
    }
}

fn image_ref(stored: &Stored) -> ImageRef {
    ImageRef {
        path: format!("artifacts/{}", stored.file),
        mime_type: stored.mime_type.clone(),
        width: stored.width,
        height: stored.height,
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
