//! The running jobs' live output: each job's `job_delta` text feeds its own
//! bounded state from when the terminal attached, and the `tty` mark fold
//! chooses a grid or lines at its `job_started` (`docs/tui.md`, "Swapped
//! views": a job running under a pseudo-terminal has a live view of its
//! screen).

use contract::Envelope;
use serde_json::Value;

use super::super::App;
use crate::tty_screen::Output;

impl App {
    /// Folds one attached-session line into the jobs' live output: a
    /// `shell` call's `tty` marks its action id, its `job_started` takes
    /// a grid when marked and lines otherwise, each `job_delta` feeds
    /// that job's output in order, and `job_completed` drops it unless
    /// that job's view is open.
    pub(in crate::app) fn output_line(&mut self, envelope: &Envelope) {
        match envelope.kind.as_str() {
            "tool_call_requested" => {
                if let Some(requested) =
                    super::super::read!(envelope, contract::events::ToolCallRequested)
                    && requested.name == "shell"
                    && let Some(action) = envelope.action_id.clone()
                {
                    let tty = requested.repair.as_ref().map_or_else(
                        || tty_of(&requested.arguments),
                        |repair| map_tty(&repair.repaired),
                    );
                    if tty {
                        self.items.tty_marks.insert(action);
                    }
                }
            }
            "tool_call_started" => {
                if let Some(started) =
                    super::super::read!(envelope, contract::events::ToolCallStarted)
                    && let Some(action) = envelope.action_id.clone()
                    && let Some(arguments) = started.arguments.as_ref()
                    && map_tty(arguments)
                {
                    self.items.tty_marks.insert(action);
                }
            }
            "job_started" => {
                if let Some(started) = super::super::read!(envelope, contract::events::JobStarted) {
                    let tty = envelope
                        .action_id
                        .as_ref()
                        .is_some_and(|action| self.items.tty_marks.remove(action));
                    if let Some(record) = self.items.jobs.get_mut(&started.job_id) {
                        record.output = Some(if tty { Output::tty() } else { Output::plain() });
                    }
                }
            }
            "job_delta" => {
                if let Some(delta) = super::super::read!(envelope, contract::events::JobDelta)
                    && let Some(text) = delta.progress.text.as_deref()
                    && let Some(record) = self.items.jobs.get_mut(&delta.job_id)
                    && let Some(output) = record.output.as_mut()
                {
                    output.feed(text);
                }
            }
            "job_completed" => {
                if let Some(done) = super::super::read!(envelope, contract::events::JobCompleted) {
                    let open = self.items.open.as_ref().map(|open| open.job_id.clone());
                    if let Some(record) = self.items.jobs.get_mut(&done.job_id)
                        && open.as_ref() != Some(&done.job_id)
                    {
                        record.output = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// The open job's output rows in a `width` by `height` body; empty
    /// with no open job or no output yet.
    pub(crate) fn item_output_rows(&self, width: u16, height: u16) -> Vec<String> {
        let Some(open) = self.items.open.as_ref() else {
            return Vec::new();
        };
        self.items
            .jobs
            .get(&open.job_id)
            .and_then(|record| record.output.as_ref())
            .map_or_else(Vec::new, |output| output.rows(width, height))
    }
}

/// Whether `arguments` asks for a pseudo-terminal: its `tty` is true.
fn tty_of(arguments: &Value) -> bool {
    arguments
        .get("tty")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a repaired or rewritten argument map asks for a
/// pseudo-terminal: its `tty` is true.
fn map_tty(arguments: &serde_json::Map<String, Value>) -> bool {
    arguments
        .get("tty")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
