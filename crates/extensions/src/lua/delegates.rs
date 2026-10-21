//! One ordered-stream `run` per extension-owned delegate end
//! (`docs/extensions.md`, "Host calls"): the loop writes the delegate's
//! `delegate_finished` just before its `job_completed`, and the session's
//! event fan-out offers both lines to the hub. The hub holds the finish,
//! keyed by job id, until its completion arrives; the completion then pushes
//! exactly one stream item carrying the `run` table, and the binding goes
//! with it, so a second completion for the same id pushes nothing. A finish
//! for a job no extension binds is not held: model-owned delegates pass
//! through.

use contract::events::{DelegateFinished, JobCompleted, Outcome};
use serde_json::{Value, json};

/// The `run` table for `completed`: `job_id` and `status` always, with the
/// held finish's bounded final text and `artifact` when one was held. A
/// `failed` or `cancelled` end with no finish carries no finish fields.
pub(super) fn run_arg(completed: &JobCompleted, finished: Option<&DelegateFinished>) -> Value {
    let status = match completed.status {
        Outcome::Completed => "completed",
        Outcome::Failed => "failed",
        Outcome::Cancelled => "cancelled",
    };
    match finished {
        Some(finished) => {
            let mut arg = json!({
                "job_id": finished.job_id.0,
                "status": status,
                "text": finished.text,
            });
            if let Some(artifact) = &finished.artifact
                && let Some(map) = arg.as_object_mut()
            {
                map.insert("artifact".to_owned(), Value::String(artifact.clone()));
            }
            arg
        }
        None => json!({
            "job_id": completed.job_id.0,
            "status": status,
        }),
    }
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
