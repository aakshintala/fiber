//! Which CI jobs run, and the verdict behind the `CI` check.

use std::collections::BTreeMap;

/// The package whose tests are the binary-level tests (`docs/ci.md`,
/// "Selection"): the `fiber` binary.
pub(super) const BINARY_TESTS: &str = "main";
/// `docs/ci.md`: the most mutants one shard tests, measured from CI runs.
const MUTANTS_PER_SHARD: u64 = 15;
/// `docs/ci.md`: the most shards one run starts.
const MAX_MUTANT_SHARDS: u64 = 16;
/// `docs/ci.md`: a shard's time limit with up to one shard's worth of
/// mutants, in minutes.
const SHARD_TIMEOUT_BASE_MINUTES: u64 = 40;
/// `docs/ci.md`: the most time one shard gets, in minutes: GitHub stops a
/// job after 6 hours, so a larger bound buys nothing.
const SHARD_TIMEOUT_MAX_MINUTES: u64 = 360;

/// `docs/ci.md`, "Selection": prefixes of release-code paths. Slash-bounded,
/// so `crates/benchmark/…` does not match; `xtask/src/bench` is the doc's
/// literal `bench*` prefix (`bench.rs`, `bench_tests.rs`, `bench/…`).
const RELEASE_CODE_PREFIXES: [&str; 2] = ["crates/bench/", "xtask/src/bench"];
/// `docs/ci.md`, "Selection": exact release-code files.
const RELEASE_CODE_FILES: [&str; 2] = ["scripts/release-size", "scripts/stored-results"];

/// Whether `path` is code the release job runs (`docs/ci.md`, "Selection"):
/// under a release-code prefix or exactly a release-code file, and never a
/// docs file, so a Markdown file under release-code paths is not release code.
fn is_release_code(path: &str) -> bool {
    !super::is_docs_file(path)
        && (RELEASE_CODE_PREFIXES.iter().any(|p| path.starts_with(p))
            || RELEASE_CODE_FILES.contains(&path))
}

/// `docs/ci.md`, "Selection": prefixes whose diffs also run the
/// 10-busy-turns workload in `release`. The loop holds the session's
/// state, so a diff elsewhere rarely moves its row. Slash-bounded, so
/// `crates/benchmark/…` does not match; `xtask/src/bench` is the doc's
/// literal `bench*` prefix (`bench.rs`, `bench_tests.rs`, `bench/…`).
const TEN_TURNS_PREFIXES: [&str; 3] = ["crates/loop/", "crates/bench/", "xtask/src/bench"];

/// Whether `path` is code the 10-busy-turns workload measures
/// (`docs/ci.md`, "Selection"): under a ten-turns prefix, and never a
/// docs file, so a Markdown file under those paths does not select it.
fn is_ten_turns_path(path: &str) -> bool {
    !super::is_docs_file(path) && TEN_TURNS_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// Whether the run measures the 10-busy-turns workload: every push to
/// `main` does, and a pull request does when its diff touches the code
/// the workload measures (`docs/ci.md`, "Selection"). `release_paths`
/// is the diff's paths for the release rule, both ends of a rename,
/// unquoted.
pub(crate) fn ten_turns_runs(event: &str, release_paths: &[String]) -> bool {
    event != "pull_request" || release_paths.iter().any(|p| is_ten_turns_path(p))
}

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

/// One shard's time limit for `count` mutants, in minutes: 40 minutes per
/// 15 of the largest round-robin share, rounded up, never under 40 and
/// never over 360. The multiplication saturates, so no `u64` count overflows.
pub(crate) fn shard_timeout_minutes(count: u64) -> u64 {
    let share = count.div_ceil(mutant_shards(count).max(1));
    SHARD_TIMEOUT_BASE_MINUTES
        .saturating_mul(share)
        .div_ceil(MUTANTS_PER_SHARD)
        .clamp(SHARD_TIMEOUT_BASE_MINUTES, SHARD_TIMEOUT_MAX_MINUTES)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one per input the Plan step reads"
)]
/// `release_paths` is the diff's paths for the release rule, both ends of a
/// rename, unquoted; read only by `release`.
pub(crate) fn plan(
    mode: &str,
    packages: &[String],
    event: &str,
    draft: bool,
    bug: bool,
    mutants: bool,
    mutant_count: u64,
    release_paths: &[String],
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
        // Linux x86_64 alone, so `mutants` can wait for it; `test` runs the
        // other platforms.
        ("test_linux", test),
        // A draft skips it (`docs/ci.md`).
        ("test", test && !draft),
        ("mutants", shards > 0),
        ("bug_red", pr && code && bug),
        // Runs with the binary-level tests, on every push, or when the diff
        // touches release code (`docs/ci.md`, "Selection").
        (
            "release",
            !pr || packages.iter().any(|p| p == BINARY_TESTS)
                || release_paths.iter().any(|p| is_release_code(p)),
        ),
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
