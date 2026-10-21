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

/// The `run` table for a completion with a held finish: `job_id` and
/// `status` always, with the finish's `text`, `artifact`, `questions`,
/// `usage` and `worktree` under their `docs/events.md` key names, each
/// only when the finish carries it (`usage` totals are non-optional, so
/// always). Nothing else: hook bookkeeping such as `changed_by` stays
/// out. A `failed` or `cancelled` end with no held finish carries only
/// `job_id` and `status`.
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
                "usage": serde_json::to_value(&finished.usage).unwrap_or(Value::Null),
            });
            if let Some(map) = arg.as_object_mut() {
                if let Some(artifact) = &finished.artifact {
                    map.insert("artifact".to_owned(), Value::String(artifact.clone()));
                }
                if let Some(questions) = &finished.questions {
                    map.insert(
                        "questions".to_owned(),
                        serde_json::to_value(questions).unwrap_or(Value::Null),
                    );
                }
                if let Some(worktree) = &finished.worktree {
                    map.insert(
                        "worktree".to_owned(),
                        serde_json::to_value(worktree).unwrap_or(Value::Null),
                    );
                }
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
