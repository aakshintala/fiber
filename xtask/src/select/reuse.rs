//! Whether a ready run reuses a same-head mutants pass (docs/ci.md,
//! "On every pull request that changes code").
//!
//! The runs file holds one page of the Actions runs list
//! (`workflow_runs[]` with `id`, `head_sha`, `status` and `pull_requests[]`
//! carrying `number`, `head.sha` and `base.sha`); the jobs file holds one
//! object mapping a run id to that run's jobs list (`jobs[]` with `name`
//! and `conclusion`). Only the runs' top-level `head_sha` pins the tested
//! code: a run's `pull_requests[].head.sha` tracks the pull request's live
//! head, so it is never compared.

use serde_json::Value;

use super::plan::mutant_shards;

/// The current run's side of the reuse predicate: the pull request under
/// test, its head and base shas, the raw mutant count the list step counted,
/// the current run id, and whether the current run is a draft.
pub(crate) struct Current {
    pub pr: u64,
    pub head: String,
    pub base: String,
    pub mutant_count: u64,
    pub run_id: u64,
    pub draft: bool,
}

/// The shard index of a `Mutants (shard N)` job name: `None` for anything
/// else, including the unexpanded matrix name of a run that never selected
/// mutants, so such a run contributes no shard.
fn shard_index(name: &str) -> Option<u64> {
    name.strip_prefix("Mutants (shard ")?
        .strip_suffix(')')
        .and_then(|index| index.parse().ok())
}

/// Whether one candidate run supplies the pass the current run reuses: a
/// different completed run on the same pull request, head and base, whose
/// every mutants shard succeeded and whose successful-shard count equals
/// the shard total the current mutant count derives.
fn run_matches(current: &Current, total: u64, run: &Value, jobs_by_run: &Value) -> bool {
    if run
        .get("id")
        .and_then(Value::as_u64)
        .is_none_or(|id| id == current.run_id)
    {
        return false;
    }
    if run.get("status").and_then(Value::as_str) != Some("completed") {
        return false;
    }
    if run.get("head_sha").and_then(Value::as_str) != Some(current.head.as_str()) {
        return false;
    }
    let same_pr_and_base = run
        .get("pull_requests")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter().any(|pull| {
                pull.get("number").and_then(Value::as_u64) == Some(current.pr)
                    && pull
                        .get("base")
                        .and_then(|base| base.get("sha"))
                        .and_then(Value::as_str)
                        == Some(current.base.as_str())
            })
        });
    if !same_pr_and_base {
        return false;
    }
    let Some(id) = run.get("id").and_then(Value::as_u64) else {
        return false;
    };
    let Some(jobs) = jobs_by_run.get(id.to_string()).and_then(Value::as_array) else {
        return false;
    };
    let shards: Vec<&Value> = jobs
        .iter()
        .filter(|job| {
            job.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| shard_index(name).is_some())
        })
        .collect();
    shards.len() as u64 == total
        && shards
            .iter()
            .all(|job| job.get("conclusion").and_then(Value::as_str) == Some("success"))
}

/// Whether the current run reuses a prior pass instead of starting shards.
/// A draft never reuses, even at a matching head; a zero mutant count never
/// reuses; any parse failure or missing field reruns the shards.
pub(crate) fn decide(current: &Current, runs_json: &str, jobs_json: &str) -> bool {
    if current.draft {
        return false;
    }
    let total = mutant_shards(current.mutant_count);
    if total == 0 {
        return false;
    }
    let runs: Value = serde_json::from_str(runs_json).unwrap_or(Value::Null);
    let jobs_by_run: Value = serde_json::from_str(jobs_json).unwrap_or(Value::Null);
    runs.get("workflow_runs")
        .and_then(Value::as_array)
        .is_some_and(|candidates| {
            candidates
                .iter()
                .any(|run| run_matches(current, total, run, &jobs_by_run))
        })
}

#[cfg(test)]
#[path = "reuse_tests.rs"]
mod tests;
