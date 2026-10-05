//! `read` of an image (`docs/tools.md`, "read"): one child process, `fiber
//! image`, processes the file once and writes the result to the session's
//! `artifacts/` (`docs/model-routing.md`, "Image limits"). The session runs no
//! image code (`docs/invocation.md`, "Processes").

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output as Collected, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

use contract::ErrorCode;
use contract::clock::Wake;
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};
use serde_json::Value;

use crate::files::{failed, text_output};

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
pub(crate) fn read(child: Option<&ImageChild>, path: &Path, cancel: &dyn Cancel) -> Output {
    let Some(child) = child else {
        return failed(
            ErrorCode::ToolError,
            "image reading is not configured.".to_owned(),
        );
    };
    // A new name for every read: an older log line's path never points at
    // new bytes.
    let stem = format!("i_{:016x}", RandomState::new().hash_one(()));
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

/// What the wait below hears about.
enum Event {
    Cancelled,
    PipeEnded,
}

/// Wakes the wait below on a cancel. A message sent before the wait starts
/// stays queued, so none is lost.
struct Signal(Sender<Event>);

impl Wake for Signal {
    fn wake(&self) {
        // The receiver lives as long as the wait; a send after it is gone
        // has nothing to wake.
        drop(self.0.send(Event::Cancelled));
    }
}

/// Reads a pipe to its end on its own thread, so neither pipe fills while
/// the other is read.
fn drain(
    mut pipe: impl Read + Send + 'static,
    ended: Sender<Event>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error ends the capture with what arrived.
        let _ended = pipe.read_to_end(&mut bytes);
        drop(ended.send(Event::PipeEnded));
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
    let (sender, receiver) = mpsc::channel();
    let wake: Arc<dyn Wake> = Arc::new(Signal(sender.clone()));
    cancel.subscribe(Arc::downgrade(&wake));
    let out = process
        .stdout
        .take()
        .map(|pipe| drain(pipe, sender.clone()));
    let err = process.stderr.take().map(|pipe| drain(pipe, sender));
    // Both pipes are piped above, so two drain threads run.
    let mut open = 2_u8;
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
        if open == 0 {
            // Both pipes closed: the child is exiting, and a pipe can close
            // a moment before the child is waitable, so block on it.
            break process.wait();
        }
        // Blocks until a cancel or a pipe end. `wake` holds a sender, so the
        // channel never disconnects while this loop runs.
        if let Ok(Event::PipeEnded) = receiver.recv() {
            open -= 1;
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
