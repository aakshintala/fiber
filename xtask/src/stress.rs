//! The stress job's helpers (`docs/ci.md`, "On every pull request that changes code").

use crate::select::{Members, TestRef, changed_tests, filter, spec};

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

/// One failure line per retry-list line that names a touched test: the
/// line's head up to the first TAB is `<classname> <name>`, which matches
/// when the name is equal and the classname is the test's binary, or the
/// test's package when the test has no binary. A line without a space is
/// no test and is skipped.
pub(crate) fn touched_retries(list: &str, touched: &[TestRef]) -> Vec<String> {
    let mut failures = Vec::new();
    for line in list.lines() {
        let head = line.split('\t').next().unwrap_or_default();
        let Some((classname, name)) = head.split_once(' ') else {
            continue;
        };
        for test in touched {
            let scope = match &test.binary {
                Some(binary) => classname == binary,
                None => classname.split("::").next() == Some(test.package.as_str()),
            };
            if test.name == name && scope {
                failures.push(format!(
                    "touched-retries: {classname} {name} was added or changed by this pull request and passed only on retry"
                ));
            }
        }
    }
    failures
}

/// One sorted `test` line per distinct planned test.
fn plan_test_lines(touched: &[TestRef]) -> Vec<String> {
    let mut lines: Vec<String> = touched
        .iter()
        .map(|test| {
            let id = test.binary.as_deref().unwrap_or(&test.package);
            format!("test\t{id} {}", test.name)
        })
        .collect();
    lines.sort();
    lines.dedup();
    lines
}

/// The `--summary` block `stress-plan` appends, over the printed `test`
/// lines: one singular or plural heading, then those lines fenced.
fn plan_summary(count: u32, lines: &[String]) -> String {
    if lines.is_empty() {
        return "### Stress job: no test added or changed\n".to_owned();
    }
    let mut block = format!(
        "### Stress job: {} {}, {count} runs each\n```\n",
        lines.len(),
        if lines.len() == 1 { "test" } else { "tests" },
    );
    for line in lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str("```\n");
    block
}

/// The merge base with `base`, as `changed_files` computes it.
fn merge_base(base: &str) -> Result<String, String> {
    Ok(crate::git_lines(&["merge-base", base, "HEAD"])?
        .into_iter()
        .next()
        .ok_or("no merge base")?)
}

/// The workspace members with the test functions the working tree adds
/// or changes against `base`, found with the same selector as the stress
/// job.
fn touched_against(base: &str) -> Result<(Members, Vec<TestRef>), String> {
    let members = crate::workspace_members()?;
    let merge_base = merge_base(base)?;
    let diff = crate::output(
        "git",
        &[
            "diff",
            "-U0",
            "--no-renames",
            "--no-ext-diff",
            merge_base.as_str(),
        ],
    )?;
    let sources = crate::rust_files(&members)?;
    let touched = changed_tests(&diff, &members, &sources)?;
    Ok((members, touched))
}

pub(crate) fn plan_command(args: &[String]) -> Result<bool, String> {
    let base = crate::flag(args, "--base")?;
    let labels = crate::optional(args, "--labels").unwrap_or_default();
    let summary = crate::optional(args, "--summary");
    let (members, touched) = touched_against(&base)?;
    let count = stress_count(&labels);
    println!("run\t{}", if touched.is_empty() { "false" } else { "true" });
    println!("count\t{count}");
    println!("filter\t{}", filter(&touched));
    let mut packages: Vec<String> = touched.iter().map(|t| spec(&t.package, &members)).collect();
    packages.sort();
    packages.dedup();
    for package in &packages {
        println!("package\t{package}");
    }
    let lines = plan_test_lines(&touched);
    for line in &lines {
        println!("{line}");
    }
    if let Some(path) = summary {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write as _;
                file.write_all(plan_summary(count, &lines).as_bytes())
            })
            .map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(true)
}

pub(crate) fn touched_retries_command(args: &[String]) -> Result<bool, String> {
    let base = crate::flag(args, "--base")?;
    let list_path = crate::flag(args, "--list")?;
    let list = std::fs::read_to_string(&list_path).map_err(|e| format!("{list_path}: {e}"))?;
    let (_, touched) = touched_against(&base)?;
    let failures = touched_retries(&list, &touched);
    if failures.is_empty() {
        println!("touched-retries: ok");
        Ok(true)
    } else {
        for failure in &failures {
            println!("{failure}");
        }
        Ok(false)
    }
}

pub(crate) fn cost_command(args: &[String]) -> Result<bool, String> {
    let start: u64 = crate::flag(args, "--start")?
        .parse()
        .map_err(|e| format!("--start: {e}"))?;
    let end: u64 = crate::flag(args, "--end")?
        .parse()
        .map_err(|e| format!("--end: {e}"))?;
    let count: u32 = crate::flag(args, "--count")?
        .parse()
        .map_err(|e| format!("--count: {e}"))?;
    let tests: usize = crate::flag(args, "--tests")?
        .parse()
        .map_err(|e| format!("--tests: {e}"))?;
    let outcome = crate::flag(args, "--outcome")?;
    println!(
        "{}",
        cost_summary(&cost(start, end), count, tests, &outcome)
    );
    Ok(true)
}

#[cfg(test)]
#[path = "stress_tests.rs"]
mod tests;
