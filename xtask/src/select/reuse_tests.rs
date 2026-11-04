//! Tests for the same-head reuse predicate: each side of every boundary.

use serde_json::{Value, json};

use super::{CommitSha, Current, PullRequestNumber, RunId, decide, matched_run_id};

const PR: u64 = 3899;
const HEAD: &str = "abc123";
const BASE: &str = "def456";
const RUN_ID: u64 = 100;
/// 40 mutants derive 3 shards: one per 15, rounded up
/// (docs/ci.md, "On every pull request that changes code").
const MUTANT_COUNT: u64 = 40;
const SHARD_TOTAL: usize = 3;

fn current() -> Current {
    Current {
        pr: PullRequestNumber(PR),
        head: CommitSha(HEAD.to_owned()),
        base: CommitSha(BASE.to_owned()),
        mutant_count: MUTANT_COUNT,
        run_id: RunId(RUN_ID),
        draft: false,
    }
}

/// One candidate run: the top-level `head_sha` pins the tested code, while
/// the pull request entry's head tracks live state and is never compared.
// debt: candidate duplicates xtask/tests/cli.rs reuse_candidate (same candidate-run JSON shape),
// ceiling: two builders, upgrade trigger: a third candidate-run fixture or a fence that allows
// a shared xtask fixture module merges both into one builder.
fn candidate(
    id: u64,
    head_sha: &str,
    pr_head_sha: &str,
    pr_number: u64,
    base_sha: Option<&str>,
    status: &str,
) -> Value {
    json!({
        "id": id,
        "head_sha": head_sha,
        "status": status,
        "pull_requests": [{
            "number": pr_number,
            "head": { "sha": pr_head_sha },
            "base": base_sha.map(|sha| json!({ "sha": sha })).unwrap_or(Value::Null),
        }],
    })
}

fn matching_candidate() -> Value {
    candidate(7, HEAD, HEAD, PR, Some(BASE), "completed")
}

// debt: shard duplicates the shard-job object built inside xtask/tests/cli.rs reuse_shards
// (same "Mutants (shard {index})" JSON), ceiling: two builders, upgrade trigger: a third
// shard-job fixture or a fence that allows a shared xtask fixture module merges both into one builder.
fn shard(index: usize, conclusion: &str) -> Value {
    json!({ "name": format!("Mutants (shard {index})"), "conclusion": conclusion })
}

fn successful_shards(total: usize) -> Value {
    Value::Array((0..total).map(|index| shard(index, "success")).collect())
}

fn decide_with(current: &Current, runs: &Value, jobs: &Value) -> bool {
    decide(current, &runs.to_string(), &jobs.to_string())
}

fn matched_with(current: &Current, runs: &Value, jobs: &Value) -> Option<u64> {
    matched_run_id(current, &runs.to_string(), &jobs.to_string())
}

fn jobs_for(id: u64, jobs: Value) -> Value {
    json!({ id.to_string(): jobs })
}

#[test]
fn full_match_reuses() {
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn draft_current_run_reruns() {
    let rerun = Current {
        draft: true,
        ..current()
    };
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(
        &rerun,
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn head_mismatch_reruns() {
    let runs =
        json!({ "workflow_runs": [candidate(7, "other", "other", PR, Some(BASE), "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn base_mismatch_reruns() {
    let runs =
        json!({ "workflow_runs": [candidate(7, HEAD, HEAD, PR, Some("older"), "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn pr_mismatch_reruns() {
    let runs =
        json!({ "workflow_runs": [candidate(7, HEAD, HEAD, PR + 1, Some(BASE), "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn pr_entry_head_match_with_top_level_mismatch_reruns() {
    // The entry tracks live state: it can name the current head while the
    // run tested another commit.
    let runs = json!({ "workflow_runs": [candidate(7, "tested-elsewhere", HEAD, PR, Some(BASE), "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn top_level_match_with_divergent_pr_entry_head_reuses() {
    // The entry moved on after the run; the top-level sha still pins what
    // the run tested.
    let runs =
        json!({ "workflow_runs": [candidate(7, HEAD, "moved-on", PR, Some(BASE), "completed")] });
    assert!(decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn failed_shard_reruns() {
    let mut shards = successful_shards(SHARD_TOTAL);
    shards[1] = shard(1, "failure");
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(&current(), &runs, &jobs_for(7, shards)));
}

#[test]
fn cancelled_shard_reruns() {
    let mut shards = successful_shards(SHARD_TOTAL);
    shards[2] = shard(2, "cancelled");
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(&current(), &runs, &jobs_for(7, shards)));
}

#[test]
fn unselected_mutants_reruns() {
    // A run that never selected mutants carries only the unexpanded matrix
    // name, which contributes no shard.
    let jobs = Value::Array(vec![
        json!({ "name": "Mutants (shard ${{ matrix.shard }})", "conclusion": "skipped" }),
    ]);
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(&current(), &runs, &jobs_for(7, jobs)));
}

#[test]
fn partial_shards_rerun() {
    // One fewer shard than the derived total reruns
    // (docs/ci.md, "On every pull request that changes code").
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL - 1))
    ));
}

#[test]
fn count_off_by_one_reruns() {
    // One more shard than the derived total reruns
    // (docs/ci.md, "On every pull request that changes code").
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL + 1))
    ));
}

#[test]
fn zero_mutants_never_reuses() {
    let none = Current {
        mutant_count: 0,
        ..current()
    };
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(
        &none,
        &runs,
        &jobs_for(7, Value::Array(vec![]))
    ));
}

#[test]
fn self_run_id_never_matches() {
    let runs =
        json!({ "workflow_runs": [candidate(RUN_ID, HEAD, HEAD, PR, Some(BASE), "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(RUN_ID, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn unfinished_candidate_reruns() {
    let runs =
        json!({ "workflow_runs": [candidate(7, HEAD, HEAD, PR, Some(BASE), "in_progress")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn missing_base_field_reruns() {
    let runs = json!({ "workflow_runs": [candidate(7, HEAD, HEAD, PR, None, "completed")] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn empty_pull_requests_reruns() {
    let mut run = matching_candidate();
    run["pull_requests"] = Value::Array(vec![]);
    let runs = json!({ "workflow_runs": [run] });
    assert!(!decide_with(
        &current(),
        &runs,
        &jobs_for(7, successful_shards(SHARD_TOTAL))
    ));
}

#[test]
fn missing_jobs_entry_reruns() {
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(
        &current(),
        &runs,
        &json!({ "8": successful_shards(SHARD_TOTAL) })
    ));
}

#[test]
fn malformed_json_reruns() {
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide(
        &current(),
        "not json",
        &jobs_for(7, successful_shards(SHARD_TOTAL)).to_string()
    ));
    assert!(!decide(&current(), &runs.to_string(), "not json"));
    assert!(!decide(&current(), "[1, 2]", &json!({}).to_string()));
}

#[test]
fn run_without_shard_jobs_reruns() {
    let jobs = Value::Array(vec![
        json!({ "name": "Selection", "conclusion": "success" }),
    ]);
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(&current(), &runs, &jobs_for(7, jobs)));
}

#[test]
fn non_shard_jobs_never_count_toward_the_total() {
    // As many successful non-shard jobs as the derived total still reruns:
    // only `Mutants (shard N)` jobs count
    // (docs/ci.md, "On every pull request that changes code").
    let jobs = Value::Array(
        (0..SHARD_TOTAL)
            .map(|index| json!({ "name": format!("Lint (leg {index})"), "conclusion": "success" }))
            .collect(),
    );
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert!(!decide_with(&current(), &runs, &jobs_for(7, jobs)));
}

#[test]
fn later_matching_candidate_reuses() {
    let runs = json!({ "workflow_runs": [
        candidate(5, "other", "other", PR, Some(BASE), "completed"),
        matching_candidate(),
    ]});
    let jobs = json!({ "5": successful_shards(SHARD_TOTAL), "7": successful_shards(SHARD_TOTAL) });
    assert!(decide_with(&current(), &runs, &jobs));
}

#[test]
fn matched_run_id_names_the_match() {
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert_eq!(
        matched_with(
            &current(),
            &runs,
            &jobs_for(7, successful_shards(SHARD_TOTAL))
        ),
        Some(7)
    );
}

#[test]
fn matched_run_id_names_the_first_match() {
    let runs = json!({ "workflow_runs": [
        candidate(9, HEAD, HEAD, PR, Some(BASE), "completed"),
        matching_candidate(),
    ]});
    let jobs = json!({ "9": successful_shards(SHARD_TOTAL), "7": successful_shards(SHARD_TOTAL) });
    assert_eq!(matched_with(&current(), &runs, &jobs), Some(9));
}

#[test]
fn matched_run_id_is_absent_without_a_match() {
    let runs =
        json!({ "workflow_runs": [candidate(7, "other", "other", PR, Some(BASE), "completed")] });
    assert_eq!(
        matched_with(
            &current(),
            &runs,
            &jobs_for(7, successful_shards(SHARD_TOTAL))
        ),
        None
    );
}

#[test]
fn matched_run_id_is_absent_for_a_draft_current_run() {
    let draft = Current {
        draft: true,
        ..current()
    };
    let runs = json!({ "workflow_runs": [matching_candidate()] });
    assert_eq!(
        matched_with(&draft, &runs, &jobs_for(7, successful_shards(SHARD_TOTAL))),
        None
    );
}
