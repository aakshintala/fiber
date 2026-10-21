//! One ordered-stream `run` per extension-owned delegate end
//! (`docs/extensions.md`, "Host calls"): the loop writes the delegate's
//! `delegate_finished` just before its `job_completed`, and the session's
//! event fan-out offers both lines here. The finish is held, keyed by job
//! id, until its completion arrives; the completion then pushes exactly one
//! stream item carrying the `run` table, and the binding goes with it, so a
//! second completion for the same id pushes nothing. A finish for a job no
//! extension binds is not held: model-owned delegates pass through.

use std::collections::HashMap;
use std::time::Duration;

use contract::JobId;
use contract::events::{DelegateFinished, JobCompleted, Outcome};
use serde_json::{Value, json};

use super::Target;

/// The stream item a delegate end pushes: its target and its `run` argument.
pub(super) struct StreamItem {
    pub(super) target: Target,
    pub(super) arg: Value,
}

/// Records `finished` for its job: nothing is pushed until its
/// `job_completed` arrives. A later finish for the same job replaces the
/// earlier one; each job's finish is written once, so this never happens.
pub(super) fn hold(held: &mut HashMap<JobId, DelegateFinished>, finished: DelegateFinished) {
    held.insert(finished.job_id.clone(), finished);
}

/// Builds the one stream item for `completed`: the binding's handle and
/// timeout ride along in the target, and any held finish shapes the `run`
/// table. The caller consumes the binding with the item, so a second
/// completion for the same id pushes nothing.
pub(super) fn push_on_job_completed(
    handle: u64,
    timeout: Duration,
    finished: Option<&DelegateFinished>,
    extension: &str,
    completed: &JobCompleted,
) -> StreamItem {
    StreamItem {
        target: Target::Delegate {
            extension: extension.to_owned(),
            job_id: completed.job_id.clone(),
            handle,
            timeout,
        },
        arg: run_arg(completed, finished),
    }
}

/// The `run` table for `completed`: `job_id` and `status` always, with the
/// held finish's bounded final text and `artifact` when one was held. A
/// `failed` or `cancelled` end with no finish carries no finish fields.
fn run_arg(completed: &JobCompleted, finished: Option<&DelegateFinished>) -> Value {
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

