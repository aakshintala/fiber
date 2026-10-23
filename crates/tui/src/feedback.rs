//! The feedback report writer `main` injects: the TUI names only the
//! contract seam, never `cli`. The write runs off the loop thread, as a
//! clipboard image read does, and its answer lands as a notice.

use std::sync::mpsc::Sender;
use std::thread;

use contract::SessionId;

use crate::{FeedbackDone, Input, WriteFeedback};

/// The terminal cannot write feedback without an injected writer: the
/// jigs never set one.
pub(crate) const NO_WRITER: &str = "Feedback is not available here.";

/// The loop is gone: the worker has no channel to answer on.
pub(crate) const NO_CHANNEL: &str = "Feedback is not available: the terminal is closing.";

/// Runs the writer off the loop thread, for the session on screen when
/// there is one, and posts its answer: the path with the issue link, or
/// why the report could not be written. Without
/// a writer or a channel, answers nothing and returns the notice the
/// loop lands instead, as a clipboard read with no reader does.
pub(crate) fn start(
    writer: Option<WriteFeedback>,
    out: Option<&Sender<Input>>,
    description: String,
    session: Option<SessionId>,
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
            let done: Result<FeedbackDone, String> = writer(&description, session.as_ref());
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
