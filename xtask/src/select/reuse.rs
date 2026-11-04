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

/// A pull request number. Never a run id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PullRequestNumber(pub u64);

/// An Actions run id. Never a pull request number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RunId(pub u64);

/// A commit sha: a head sha or a base sha.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct CommitSha(pub String);

/// The current run's side of the reuse predicate: the pull request under
/// test, its head and base shas, the raw mutant count the list step counted,
/// the current run id, and whether the current run is a draft.
pub(crate) struct Current {
    pub pr: PullRequestNumber,
    pub head: CommitSha,
    pub base: CommitSha,
    pub mutant_count: u64,
    pub run_id: RunId,
    pub draft: bool,
}

/// Whether a job name is one of a run's `Mutants (shard N)` jobs: false
/// for anything else, including the unexpanded matrix name of a run that
/// never selected mutants, so such a run contributes no shard. The index
/// itself is never compared, so this is a boolean rather than the parsed
/// index.
fn is_mutants_shard(name: &str) -> bool {
    name.strip_prefix("Mutants (shard ")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|index| index.parse::<u64>().is_ok())
}

/// Whether one candidate run supplies the pass the current run reuses: a
/// different completed run on the same pull request, head and base, whose
/// every mutants shard succeeded and whose successful-shard count equals
/// the shard total the current mutant count derives.
fn run_matches(current: &Current, total: u64, run: &Value, jobs_by_run: &Value) -> bool {
    if run
        .get("id")
        .and_then(Value::as_u64)
        .is_none_or(|id| id == current.run_id.0)
    {
        return false;
    }
    if run.get("status").and_then(Value::as_str) != Some("completed") {
        return false;
    }
    if run.get("head_sha").and_then(Value::as_str) != Some(current.head.0.as_str()) {
        return false;
    }
    let same_pr_and_base = run
        .get("pull_requests")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter().any(|pull| {
                pull.get("number").and_then(Value::as_u64) == Some(current.pr.0)
                    && pull
                        .get("base")
                        .and_then(|base| base.get("sha"))
                        .and_then(Value::as_str)
                        == Some(current.base.0.as_str())
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
                .is_some_and(is_mutants_shard)
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
