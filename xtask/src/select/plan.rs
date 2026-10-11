//! Which CI jobs run, and the verdict behind the `CI` check.

use std::collections::BTreeMap;

use super::BINARY_TESTS;

/// `docs/ci.md`: the most mutants one shard tests, measured from CI runs.
const MUTANTS_PER_SHARD: u64 = 15;
/// `docs/ci.md`: the most shards one run starts.
const MAX_MUTANT_SHARDS: u64 = 16;
/// `docs/ci.md`: a shard's time limit with up to one shard's worth of
/// mutants, in minutes.
const SHARD_TIMEOUT_BASE_MINUTES: u64 = 20;
/// `docs/ci.md`: the most time one shard gets, in minutes: GitHub stops a
/// job after 6 hours, so a larger bound buys nothing.
const SHARD_TIMEOUT_MAX_MINUTES: u64 = 360;

/// Which CI jobs run, by the job ids in `.github/workflows/ci.yml`, and how
/// many mutant shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) jobs: BTreeMap<&'static str, bool>,
    pub(crate) shards: u64,
}

/// Shards for `count` mutants: none without mutants, else one per
/// `MUTANTS_PER_SHARD`, rounded up, at most `MAX_MUTANT_SHARDS`.
pub(crate) fn mutant_shards(count: u64) -> u64 {
    count.div_ceil(MUTANTS_PER_SHARD).min(MAX_MUTANT_SHARDS)
}

/// One shard's time limit for `count` mutants, in minutes: 20 minutes per
/// 15 of the largest round-robin share, rounded up, never under 20 and
/// never over 360. The multiplication saturates, so no `u64` count overflows.
pub(crate) fn shard_timeout_minutes(count: u64) -> u64 {
    let share = count.div_ceil(mutant_shards(count).max(1));
    SHARD_TIMEOUT_BASE_MINUTES
        .saturating_mul(share)
        .div_ceil(MUTANTS_PER_SHARD)
        .clamp(SHARD_TIMEOUT_BASE_MINUTES, SHARD_TIMEOUT_MAX_MINUTES)
}

pub(crate) fn plan(
    mode: &str,
    packages: &[String],
    event: &str,
    bug: bool,
    mutants: bool,
    mutant_count: u64,
) -> Plan {
    let pr = event == "pull_request";
    let code = mode != "docs";
    let test = !pr || !packages.is_empty();
    // The backstop runs no mutants: each pull request tested its own diff.
    // A run that selects no tests runs no mutants either.
    let shards = if pr && mutants && test {
        mutant_shards(mutant_count)
    } else {
        0
    };
    let jobs = BTreeMap::from([
        ("lint", !pr || code),
        // The backstop on `main` compiles the whole workspace on every push.
        ("test", test),
        ("mutants", shards > 0),
        ("bug_red", pr && code && bug),
        // Runs with the binary-level tests, and on every push (`docs/ci.md`, "Selection").
        ("release", !pr || packages.iter().any(|p| p == BINARY_TESTS)),
    ]);
    Plan { jobs, shards }
}

/// Failures, one line each; empty when `CI` passes. `results` maps each job
/// `CI` needs to its result, and `jobs` says which the selection chose.
pub(crate) fn verdict(
    results: &BTreeMap<String, String>,
    jobs: &BTreeMap<String, bool>,
) -> Vec<String> {
    let select = results.get("select").map_or("missing", String::as_str);
    if select != "success" {
        return vec![format!("select: {select}, so the selection failed")];
    }
    let mut failures = Vec::new();
    for (name, result) in results.iter().filter(|(name, _)| *name != "select") {
        match jobs.get(name) {
            None => failures.push(format!("{name}: not in the selection")),
            Some(true) if result != "success" => {
                failures.push(format!("{name}: selected, but {result}"))
            }
            Some(false) if result != "skipped" => {
                failures.push(format!("{name}: not selected, but {result}"))
            }
            Some(_) => {}
        }
    }
    failures
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
