//! Whether a ready run reuses a same-head mutants pass (`docs/ci.md`,
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

/// The merge parent a `Mutants (shard N, base <sha>)` job name records, if
/// any: `None` for anything else, including the old base-less form and the
/// unexpanded matrix name of a run that never selected mutants, so such runs
/// contribute no shard. The index itself is never compared; equality with the
/// current base is checked by the caller, so only an exact match reuses.
fn is_mutants_shard(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Mutants (shard ")?;
    let (index, rest) = rest.split_once(", base ")?;
    let sha = rest.strip_suffix(')')?;
    index.parse::<u64>().ok()?;
    (!sha.is_empty()).then_some(sha)
}

/// The run the current run would reuse: the matched run's id plus the
/// matched pull-request entry's head sha, so the select log can name the
/// exact pass it reuses. A missing entry head sha fails closed like every
/// other missing field.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct MatchedReuse {
    pub run_id: u64,
    pub pr_head: CommitSha,
}

/// Whether one candidate run supplies the pass the current run reuses: a
/// different completed run on the same pull request and head whose every
/// mutants shard names the current merge parent and succeeded, and whose
/// successful-shard count equals the shard total the current mutant count
/// derives. The pull request entry's stored base is never compared: it can
/// equal the current base while the candidate merged onto older main.
/// Returns the matched run id with the matched entry's head sha.
fn match_in_run(
    current: &Current,
    total: u64,
    run: &Value,
    jobs_by_run: &Value,
) -> Option<MatchedReuse> {
    let id = run.get("id").and_then(Value::as_u64)?;
    if id == current.run_id.0 {
        return None;
    }
    if run.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    if run.get("head_sha").and_then(Value::as_str) != Some(current.head.0.as_str()) {
        return None;
    }
    let pr_head = run
        .get("pull_requests")
        .and_then(Value::as_array)
        .and_then(|refs| {
            refs.iter().find_map(|pull| {
                if pull.get("number").and_then(Value::as_u64) == Some(current.pr.0) {
                    pull.get("head")
                        .and_then(|head| head.get("sha"))
                        .and_then(Value::as_str)
                } else {
                    None
                }
            })
        })?;
    if pr_head.is_empty() {
        return None;
    }
    let jobs = jobs_by_run.get(id.to_string()).and_then(Value::as_array)?;
    let shards: Vec<&Value> = jobs
        .iter()
        .filter(|job| {
            job.get("name")
                .and_then(Value::as_str)
                .and_then(is_mutants_shard)
                .is_some()
        })
        .collect();
    if shards.len() as u64 == total
        && shards.iter().all(|job| {
            job.get("conclusion").and_then(Value::as_str) == Some("success")
                && job
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(is_mutants_shard)
                    == Some(current.base.0.as_str())
        })
    {
        Some(MatchedReuse {
            run_id: id,
            pr_head: CommitSha(pr_head.to_owned()),
        })
    } else {
        None
    }
}

/// The first candidate run the current run would reuse, if any: the same
/// guard clauses as the reuse decision, so an absent match always means the
/// decision is false and a logged match always names a real pass.
pub(crate) fn matched_reuse(
    current: &Current,
    runs_json: &str,
    jobs_json: &str,
) -> Option<MatchedReuse> {
    if current.draft {
        return None;
    }
    let total = mutant_shards(current.mutant_count);
    if total == 0 {
        return None;
    }
    let runs: Value = serde_json::from_str(runs_json).unwrap_or(Value::Null);
    let jobs_by_run: Value = serde_json::from_str(jobs_json).unwrap_or(Value::Null);
    runs.get("workflow_runs")
        .and_then(Value::as_array)
        .and_then(|candidates| {
            candidates
                .iter()
                .find_map(|run| match_in_run(current, total, run, &jobs_by_run))
        })
}

#[cfg(test)]
#[path = "reuse_tests.rs"]
mod tests;
