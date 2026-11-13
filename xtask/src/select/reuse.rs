//! Whether a ready run reuses a same-head mutants pass (`docs/ci.md`,
//! "On every pull request that changes code").

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::plan::mutant_shards;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PullRequestNumber(pub u64);
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RunId(pub u64);
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct CommitSha(pub String);
pub(crate) struct Current {
    pub pr: PullRequestNumber,
    pub head: CommitSha,
    pub base: CommitSha,
    pub mutant_count: u64,
    pub run_id: RunId,
    pub draft: bool,
}
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct MatchedReuse {
    pub run_id: u64,
    pub pr_head: CommitSha,
}

fn is_mutants_shard(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Mutants (shard ")?;
    let (index, rest) = rest.split_once(", base ")?;
    let sha = rest.strip_suffix(')')?;
    index.parse::<u64>().ok()?;
    (!sha.is_empty()).then_some(sha)
}

fn match_in_run(
    current: &Current,
    total: u64,
    run: &Value,
    jobs_by_run: &Value,
) -> Option<MatchedReuse> {
    let id = run.get("id").and_then(Value::as_u64)?;
    if id == current.run_id.0
        || run.get("status").and_then(Value::as_str) != Some("completed")
        || run.get("head_sha").and_then(Value::as_str) != Some(current.head.0.as_str())
    {
        return None;
    }
    let pr_head = run
        .get("pull_requests")?
        .as_array()?
        .iter()
        .find_map(|pull| {
            (pull.get("number").and_then(Value::as_u64) == Some(current.pr.0))
                .then(|| pull.get("head")?.get("sha")?.as_str())
                .flatten()
        })?;
    if pr_head.is_empty() {
        return None;
    }
    let jobs = jobs_by_run.get(id.to_string())?.as_array()?;
    let shards: Vec<_> = jobs
        .iter()
        .filter(|job| {
            job.get("name")
                .and_then(Value::as_str)
                .and_then(is_mutants_shard)
                .is_some()
        })
        .collect();
    (shards.len() as u64 == total
        && shards.iter().all(|job| {
            job.get("conclusion").and_then(Value::as_str) == Some("success")
                && job
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(is_mutants_shard)
                    == Some(current.base.0.as_str())
        }))
    .then(|| MatchedReuse {
        run_id: id,
        pr_head: CommitSha(pr_head.to_owned()),
    })
}

pub(crate) fn reuse_candidate_ids(runs_json: &str) -> Option<Vec<u64>> {
    let runs: Value = serde_json::from_str(runs_json).ok()?;
    let candidates = runs.get("workflow_runs")?.as_array()?;
    let mut ids = Vec::new();
    for run in candidates {
        if let Some(id) = run.get("id").and_then(Value::as_u64)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    Some(ids)
}

#[cfg(test)]
pub(crate) fn matched_reuse(
    current: &Current,
    runs_json: &str,
    jobs_json: &str,
) -> Option<MatchedReuse> {
    if current.draft || mutant_shards(current.mutant_count) == 0 {
        return None;
    }
    let runs: Value = serde_json::from_str(runs_json).ok()?;
    let jobs: Value = serde_json::from_str(jobs_json).ok()?;
    runs.get("workflow_runs")?
        .as_array()?
        .iter()
        .find_map(|run| match_in_run(current, mutant_shards(current.mutant_count), run, &jobs))
}

pub(crate) fn matched_reuse_files(
    current: &Current,
    runs_json: &str,
    jobs_dir: &Path,
) -> Option<MatchedReuse> {
    if current.draft || mutant_shards(current.mutant_count) == 0 {
        return None;
    }
    let ids = reuse_candidate_ids(runs_json)?;
    let runs: Value = serde_json::from_str(runs_json).ok()?;
    let candidates = runs.get("workflow_runs")?.as_array()?;
    let mut jobs_by_run = Map::new();
    for id in ids {
        let raw = fs::read_to_string(jobs_dir.join(format!("{id}.json"))).ok()?;
        let pages: Vec<Value> = serde_json::Deserializer::from_str(&raw)
            .into_iter::<Value>()
            .collect::<Result<_, _>>()
            .ok()?;
        if pages.is_empty() {
            return None;
        }
        let mut jobs = Vec::new();
        for page in pages {
            jobs.extend(page.get("jobs")?.as_array()?.iter().cloned());
        }
        jobs_by_run.insert(id.to_string(), Value::Array(jobs));
    }
    let jobs_by_run = Value::Object(jobs_by_run);
    candidates.iter().find_map(|run| {
        match_in_run(
            current,
            mutant_shards(current.mutant_count),
            run,
            &jobs_by_run,
        )
    })
}

#[cfg(test)]
#[path = "reuse_tests.rs"]
mod tests;
