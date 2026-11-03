//! The stress job's helpers (`docs/ci.md`, "On every pull request that changes code").

use crate::rules::RustFile;
use crate::select::{
    Members, TestRef, filter, has_test_functions, issue_test_names, spec, split_named_tests,
};
use crate::test_index::TestIndex;
use proc_macro2::{Delimiter, TokenStream, TokenTree};

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

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for sequence_character in chars.by_ref() {
                if ('@'..='~').contains(&sequence_character) {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn iteration_summary(line: &str) -> Option<(u32, u32, usize)> {
    let line = line.trim_start().strip_prefix("Stress test ")?;
    let (_, line) = line.split_once(" iteration ")?;
    let (iteration, summary) = line.split_once(": ")?;
    let (iteration, total) = iteration.split_once('/')?;
    let iteration = iteration.parse().ok()?;
    let total = total.parse().ok()?;
    let (executed, run) = summary.split_once(' ')?;
    let executed = executed.parse().ok()?;
    let run = run
        .strip_prefix("test run:")
        .or_else(|| run.strip_prefix("tests run:"))?;
    if !run.starts_with(' ') {
        return None;
    }
    Some((iteration, total, executed))
}

fn stress_run_summary(line: &str) -> Option<(u32, u32, u32)> {
    let line = line.trim_start().strip_prefix("Summary ")?;
    let (_, line) = line.split_once("] ")?;
    let (iterations, passed) = line.split_once(" stress run iterations: ")?;
    let (completed, total) = iterations.split_once('/')?;
    let (passed, status) = passed.split_once(' ')?;
    if status != "passed" {
        return None;
    }
    Some((
        completed.parse().ok()?,
        total.parse().ok()?,
        passed.parse().ok()?,
    ))
}

fn check_stress_output(log: &str, count: u32, tests: usize) -> Result<(), String> {
    let log = strip_ansi(log);
    let mut iteration_count = 0;
    let mut run_summaries = Vec::new();
    for line in log.lines() {
        if let Some((iteration, total, executed)) = iteration_summary(line) {
            iteration_count += 1;
            if total != count {
                return Err(format!(
                    "iteration {iteration}/{total} does not match the planned count {count}"
                ));
            }
            if executed != tests {
                return Err(format!(
                    "iteration {iteration}/{total} ran {executed} tests; plan names {tests}"
                ));
            }
        }
        if let Some(summary) = stress_run_summary(line) {
            run_summaries.push(summary);
        }
    }
    if iteration_count != count {
        return Err(format!(
            "expected {count} iteration summaries, found {iteration_count}"
        ));
    }
    let [(completed, total, passed)] = run_summaries.as_slice() else {
        return Err(format!(
            "expected one stress run summary, found {}",
            run_summaries.len()
        ));
    };
    let (completed, total, passed) = (*completed, *total, *passed);
    if completed != count || total != count || passed != count {
        return Err(format!(
            "stress run summary says {completed}/{total} iterations and {passed} passed; expected {count}/{count} and {count} passed"
        ));
    }
    Ok(())
}

pub(crate) fn stress_check_command(args: &[String]) -> Result<bool, String> {
    let path = crate::flag(args, "--log")?;
    let count: u32 = crate::flag(args, "--count")?
        .parse()
        .map_err(|error| format!("--count: {error}"))?;
    let tests: usize = crate::flag(args, "--tests")?
        .parse()
        .map_err(|error| format!("--tests: {error}"))?;
    let log = std::fs::read_to_string(&path).map_err(|error| format!("{path}: {error}"))?;
    check_stress_output(&log, count, tests).map_err(|error| format!("stress output: {error}"))?;
    println!("stress-check: ok ({count} iterations, {tests} tests each)");
    Ok(true)
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

/// Whether the file's top-level inner attributes include `#![cfg(...)]`.
fn has_cfg_inner_attr(source: &str) -> Result<bool, String> {
    let tokens: TokenStream = source.parse().map_err(|_| "cannot parse".to_owned())?;
    let mut tokens = tokens.into_iter();
    while let Some(TokenTree::Punct(hash)) = tokens.next() {
        if hash.as_char() != '#' {
            continue;
        }
        let Some(TokenTree::Punct(bang)) = tokens.next() else {
            continue;
        };
        if bang.as_char() != '!' {
            continue;
        }
        let Some(TokenTree::Group(attribute)) = tokens.next() else {
            continue;
        };
        if attribute.delimiter() != Delimiter::Bracket {
            continue;
        }
        let mut attribute = attribute.stream().into_iter();
        if matches!(attribute.next(), Some(TokenTree::Ident(name)) if name == "cfg")
            && matches!(attribute.next(), Some(TokenTree::Group(args)) if args.delimiter() == Delimiter::Parenthesis)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A test binary has no listed tests for a changed file that still holds a
/// test function. Files with no functions, cfg-gated files, and files the
/// runner did not compile need no selector-miss error.
fn empty_test_file_error(
    files: &[String],
    sources: &[RustFile],
    members: &Members,
    index: &TestIndex,
) -> Option<String> {
    files.iter().find_map(|path| {
        let package = crate::select::owner(path, members)?;
        let member = members.get(package)?;
        if member.unrun_tests.iter().any(|unrun| unrun == path) {
            return None;
        }
        let source = sources.iter().find(|source| source.path == *path)?;
        if has_cfg_inner_attr(&source.source).unwrap_or(false) {
            return None;
        }
        let binaries = index.binaries_for(path);
        (!binaries.is_empty()
            && binaries.iter().all(|binary| binary.tests.is_empty())
            && matches!(has_test_functions(&source.source), Ok(true)))
        .then(|| format!("{path}: changed test file selected no tests"))
    })
}

fn touched_against(base: &str, profile: &str) -> Result<(Members, Vec<TestRef>), String> {
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
    let paths = crate::git_lines(&["diff", "--name-only", "--no-renames", merge_base.as_str()])?;
    let sources = crate::rust_files(&members)?;
    let test_files = crate::select::test_files(&paths, &members, &sources)?;
    let index = TestIndex::build(
        std::path::Path::new("."),
        &test_files,
        &members,
        Some(profile),
    )?;
    let selected = crate::select::changed_tests_by_file(&diff, &members, &sources, &index)?;
    if let Some(error) = empty_test_file_error(&test_files, &sources, &members, &index) {
        return Err(error);
    }
    let mut touched = Vec::new();
    for (_, tests) in selected {
        for test in tests {
            if !touched.contains(&test) {
                touched.push(test);
            }
        }
    }
    Ok((members, touched))
}

pub(crate) fn stress_config(filter: &str) -> String {
    let escaped = filter.replace('\\', "\\\\").replace('"', "\\\"");
    format!("[profile.stress]\ndefault-filter = \"{escaped}\"\n")
}

/// Which half of a split plan one `stress-plan` call prints: the tests the
/// linked flake issue names, or every other selected test. Without `--only`
/// the call prints today's single group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Only {
    Named,
    Rest,
}

/// The `--only` flag: which half of the split to plan, or no half when the
/// call plans today's single group.
fn parse_only(value: Option<String>) -> Result<Option<Only>, String> {
    match value.as_deref() {
        None => Ok(None),
        Some("named") => Ok(Some(Only::Named)),
        Some("rest") => Ok(Some(Only::Rest)),
        Some(other) => Err(format!("--only {other:?} must be named or rest")),
    }
}

/// Runs for one planned group: the named tests keep the flaky-fix count
/// from the labels, every other selected test runs the pilot default.
fn group_count(only: Option<Only>, labels: &str) -> u32 {
    match only {
        Some(Only::Rest) => STRESS_COUNT,
        None | Some(Only::Named) => stress_count(labels),
    }
}

/// The `--summary` block one `--only` call appends: one heading per group
/// with its count, over that group's fenced `test` lines. An empty group
/// still names itself, with zero tests, so the run step can skip it.
fn group_summary(count: u32, lines: &[String], only: Only) -> String {
    let group = match only {
        Only::Named => " (named by the linked flake issue)",
        Only::Rest => " (rest)",
    };
    let mut block = format!(
        "### Stress job: {} {}, {count} runs each{group}\n```\n",
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

/// Names from the issue text that match no touched test, in file order:
/// they run nowhere, and the rest call names them.
fn unmatched_names(names: &[String], touched: &[TestRef]) -> Vec<String> {
    names
        .iter()
        .filter(|name| !touched.iter().any(|test| test.name == name.as_str()))
        .cloned()
        .collect()
}

/// The normalised names in `--issue-text` FILE: the linked issue's body
/// then each comment body, as `issue_test_names` extracts them. Nothing
/// else in this command parses FILE: no trimming, no case folding, no
/// prefix stripping here, because normalisation already happened inside
/// `issue_test_names`.
fn names_from_issue_text(path: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
    Ok(issue_test_names(&text))
}

pub(crate) fn plan_command(args: &[String]) -> Result<bool, String> {
    let base = crate::flag(args, "--base")?;
    let labels = crate::optional(args, "--labels").unwrap_or_default();
    let summary = crate::optional(args, "--summary");
    let filter_file = crate::optional(args, "--filter-file");
    let names = match crate::optional(args, "--issue-text") {
        Some(path) => names_from_issue_text(&path)?,
        None => Vec::new(),
    };
    let only = parse_only(crate::optional(args, "--only"))?;
    let (members, touched) = touched_against(&base, "stress")?;
    let (group, unmatched) = match only {
        None => (touched, Vec::new()),
        Some(Only::Named) => {
            let (named, _) = split_named_tests(&touched, &names);
            (named, Vec::new())
        }
        Some(Only::Rest) => {
            let (_, rest) = split_named_tests(&touched, &names);
            (rest, unmatched_names(&names, &touched))
        }
    };
    let count = group_count(only, &labels);
    let filter = filter(&group);
    if let Some(path) = filter_file {
        std::fs::write(&path, stress_config(&filter))
            .map_err(|error| format!("{path}: {error}"))?;
    }
    println!("run\t{}", if group.is_empty() { "false" } else { "true" });
    println!("count\t{count}");
    println!("filter\t{filter}");
    let mut packages: Vec<String> = group.iter().map(|t| spec(&t.package, &members)).collect();
    packages.sort();
    packages.dedup();
    for package in &packages {
        println!("package\t{package}");
    }
    let lines = plan_test_lines(&group);
    for line in &lines {
        println!("{line}");
    }
    if let Some(path) = summary {
        let block = match only {
            None => plan_summary(count, &lines),
            Some(group) => group_summary(count, &lines, group),
        };
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write as _;
                file.write_all(block.as_bytes())
            })
            .map_err(|e| format!("{path}: {e}"))?;
    }
    for name in &unmatched {
        println!("Unmatched: {name}");
    }
    Ok(true)
}

pub(crate) fn touched_retries_command(args: &[String]) -> Result<bool, String> {
    let base = crate::flag(args, "--base")?;
    let list_path = crate::flag(args, "--list")?;
    let list = std::fs::read_to_string(&list_path).map_err(|e| format!("{list_path}: {e}"))?;
    let (_, touched) = touched_against(&base, "ci")?;
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
