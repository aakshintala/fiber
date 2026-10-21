//! The feedback report writer `main` injects: the TUI names only the
//! contract seam, never `cli`. The write runs off the loop thread, as a
//! clipboard image read does, and its answer lands as a notice.

use std::sync::mpsc::Sender;
use std::thread;

use crate::{FeedbackDone, Input, WriteFeedback};

/// The terminal cannot write feedback without an injected writer: the
/// jigs never set one.
pub(crate) const NO_WRITER: &str = "Feedback is not available here.";

/// The loop is gone: the worker has no channel to answer on.
pub(crate) const NO_CHANNEL: &str = "Feedback is not available: the terminal is closing.";

/// The injected feedback writer `main` supplies; empty in the jigs,
/// which write nothing.
#[derive(Clone, Default)]
pub(crate) struct Feedback {
    writer: Option<WriteFeedback>,
}

impl Feedback {
    /// A holder for `writer`, or an empty one without it.
    pub(crate) fn new(writer: Option<WriteFeedback>) -> Self {
        Self { writer }
    }

    /// The injected writer, if `main` supplied one.
    pub(crate) fn writer(&self) -> Option<WriteFeedback> {
        self.writer.clone()
    }
}

/// Runs the writer off the loop thread and posts its answer: the path
/// with the issue link, or why the report could not be written. Without
/// a writer or a channel, answers nothing and returns the notice the
/// loop lands instead, as a clipboard read with no reader does.
pub(crate) fn start(
    writer: Option<WriteFeedback>,
    out: Option<&Sender<Input>>,
    description: String,
) -> Option<String> {
    let Some(writer) = writer else {
        return Some(NO_WRITER.to_owned());
    };
    let Some(out) = out else {
        return Some(NO_CHANNEL.to_owned());
    };
    let out = out.clone();
    match thread::Builder::new()
        .name("tui-feedback".to_owned())
        .spawn(move || {
            let done: Result<FeedbackDone, String> = writer(&description);
            match out.send(Input::Feedback(done)) {
                Ok(()) | Err(_) => {}
            }
        }) {
        Ok(_) => None,
        Err(error) => Some(format!("Feedback could not start: {error}")),
    }
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;
