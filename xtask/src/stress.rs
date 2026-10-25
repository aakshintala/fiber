//! The stress job's helpers (`docs/ci.md`, "On every pull request that changes code").

/// Runs of one selected test in the stress job, unless the pull request fixes a flaky test.
pub(crate) const STRESS_COUNT: u32 = 30;
/// Runs of one selected test when the pull request fixes a flaky test.
pub(crate) const TEST_ONLY_COUNT: u32 = 100;
/// The linked-issue label that marks a pull request as fixing a flaky test.
pub(crate) const TEST_ONLY_LABEL: &str = "test-only";

/// How many times the stress job runs each selected test: the flaky-fix
/// count when a line of the linked issue's labels, trimmed, is exactly the
/// `test-only` label, else the pilot default.
pub(crate) fn stress_count(labels: &str) -> u32 {
    if labels.lines().any(|line| line.trim() == TEST_ONLY_LABEL) {
        TEST_ONLY_COUNT
    } else {
        STRESS_COUNT
    }
}

/// The stress job's measured time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cost {
    /// Seconds from the job's first step to its summary step.
    pub(crate) wall_seconds: u64,
    /// Whole GitHub-billed minutes for the wall time, rounded up.
    pub(crate) runner_minutes: u64,
}

/// The wall time from `start` to `end` and the whole minutes it bills: an
/// end before the start measures nothing, never underflows.
pub(crate) fn cost(start: u64, end: u64) -> Cost {
    let wall_seconds = end.saturating_sub(start);
    Cost {
        wall_seconds,
        runner_minutes: wall_seconds.div_ceil(60),
    }
}

/// The job-summary block the stress job appends, machine-readable on its
/// last line.
pub(crate) fn cost_summary(cost: &Cost, count: u32, tests: usize, outcome: &str) -> String {
    format!(
        "### Stress job cost\nwall time: {}m {}s ({} s)\nrunner minutes: {}\nstress-cost: wall_seconds={} runner_minutes={} count={count} tests={tests} outcome={outcome}",
        cost.wall_seconds / 60,
        cost.wall_seconds % 60,
        cost.wall_seconds,
        cost.runner_minutes,
        cost.wall_seconds,
        cost.runner_minutes,
    )
}

#[cfg(test)]
#[path = "stress_tests.rs"]
mod tests;
