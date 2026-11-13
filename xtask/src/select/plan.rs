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

/// `docs/ci.md`, "Selection": the crates whose change selects the
/// `session start` timing row on a pull request's base pass. Each member
/// names a crate the row's process calls: `main` builds the session
/// (`main/src/session_command.rs`), through `contract`, `support` and
/// `log` (`session_command.rs:9`, `shutdown.rs:30`,
/// `session_extensions.rs:9`), the `loop` (`session_extensions.rs:10`),
/// `doors` (`builtin.rs:126`), `provider` (`routing/src/connect.rs`),
/// `tools` (`builtin.rs:58`), `mcp` (`mcp_servers.rs:30`), `jobs`
/// (`delegates.rs:34`), `net` (through `provider`), `worktree`
/// (`doors/src/isolation.rs:61`), `extensions` (`session_command.rs:216`),
/// `config` (`routing/src/lua_providers.rs`) and `hub` (`session_command.rs:605`,
/// the `hub::RecentRow` it appends), and `routing`, which picks the
/// session's model (`docs/architecture.md`, module table, `routing` row;
/// `main` calls it at startup). Slash-bounded, so
/// `crates/loopback/…` does not match.
const SESSION_CRATES: [&str; 16] = [
    "crates/contract/",
    "crates/support/",
    "crates/log/",
    "crates/loop/",
    "crates/doors/",
    "crates/provider/",
    "crates/routing/",
    "crates/tools/",
    "crates/mcp/",
    "crates/jobs/",
    "crates/net/",
    "crates/worktree/",
    "crates/extensions/",
    "crates/config/",
    "crates/hub/",
    "crates/main/",
];

/// `docs/ci.md`, "Selection": the crates whose change selects the
/// `sessions list` timing row. The row runs `fiber sessions --json`:
/// `main` serves it (`main/src/main.rs:816`, `::cli::sessions_list`)
/// through `config` (`fiber_home_from_env`) and `cli`, `doors` (the hub
/// `cli` reaches, `cli/src/hub_status.rs:33`, `doors::hub`) and the `hub`
/// that serves (`main/src/hub_command.rs:55`; `hub/src/start.rs:17` and
/// `hub/src/feed.rs:137` call into `contract`, `support` and `log`),
/// and `worktree` (`cli::sessions_list::list` resolves the project
/// there, `cli/src/sessions_list.rs:137`).
const LIST_CRATES: [&str; 9] = [
    "crates/config/",
    "crates/cli/",
    "crates/doors/",
    "crates/hub/",
    "crates/contract/",
    "crates/support/",
    "crates/log/",
    "crates/worktree/",
    "crates/main/",
];

/// `docs/ci.md`, "Selection": the crates whose change selects the
/// terminal timing rows (`terminal first frame`, `terminal attach`,
/// `terminal wheel`). The rows run the `fiber` terminal, which is the
/// session-start process plus the terminal itself: `tui`
/// (`main/src/main.rs:753`, `tui::run`) and `cli`
/// (`main/src/configure.rs:34`, `::cli::project_of`).
const TERMINAL_CRATES: [&str; 18] = [
    "crates/contract/",
    "crates/support/",
    "crates/log/",
    "crates/loop/",
    "crates/doors/",
    "crates/provider/",
    "crates/routing/",
    "crates/tools/",
    "crates/mcp/",
    "crates/jobs/",
    "crates/net/",
    "crates/worktree/",
    "crates/extensions/",
    "crates/config/",
    "crates/hub/",
    "crates/main/",
    "crates/tui/",
    "crates/cli/",
];

/// `docs/ci.md`, "Selection": the crates whose change selects the
/// `paging` timing row. The row runs the `tui` crate's `paging` jig
/// (`tui/examples/paging.rs:40`, `tui::measure_open`), which calls into
/// `contract` (`paging.rs:25`), `support` (`tui/src/editor.rs:262`) and
/// `log` (`tui/src/format.rs:582`).
const PAGING_CRATES: [&str; 4] = [
    "crates/tui/",
    "crates/contract/",
    "crates/support/",
    "crates/log/",
];

/// `docs/ci.md`, "Selection": the crates whose change selects the
/// `sandbox setup` timing row. The row times `tools::prepare`
/// (`crates/tools/src/shell/sandbox/mod.rs`), the per-spawn policy build
/// every confined shell call makes (`docs/sandbox.md`, "How read denies
/// are built"). Slash-bounded like every other crate list here.
const SANDBOX_CRATES: [&str; 1] = ["crates/tools/"];

/// `docs/ci.md`, "Selection": each base timing row with the crates
/// whose change selects it, in workload order. The names are the
/// harness's own workload names, cited by
/// `crates/bench/examples/bench/main_tests.rs`.
const BASE_ROWS: [(&str, &[&str]); 7] = [
    ("session start", &SESSION_CRATES),
    ("sessions list", &LIST_CRATES),
    ("terminal first frame", &TERMINAL_CRATES),
    ("terminal attach", &TERMINAL_CRATES),
    ("terminal wheel", &TERMINAL_CRATES),
    ("paging", &PAGING_CRATES),
    ("sandbox setup", &SANDBOX_CRATES),
];

/// Whether `path` is a docs file for the base-row selection: `is_docs_file`
/// says so, and it is not under `crates/`. Every `.md` under `crates/` is
/// code: compiled-in Markdown lives in `crates/contract/prompt/`,
/// `crates/loop/prompt/`, `crates/tools/prompt/` and `crates/tools/tests/pages/`.
fn is_base_docs_file(path: &str) -> bool {
    super::is_docs_file(path) && !path.starts_with("crates/")
}

/// The base timing rows no path selects, in table order: on a pull
/// request the base pass runs only the selected rows
/// (`docs/ci.md`, "Selection"). A docs file selects nothing. Any other
/// path selects every row whose crate list holds its `crates/<name>/`
/// directory; a path no row's list covers selects every row, so
/// `crates/bench`, `xtask/src/bench*`, `scripts/*`, `.github/*` and the
/// manifests cost the whole base pass. A non-PR event or an empty path
/// list skips nothing and runs every row, as does a missing value
/// downstream: the harness runs every row on an empty `--skip-rows`.
/// `release_paths` is the diff's paths for the release rule, both ends of
/// a rename, so a rename counts both ends.
pub(crate) fn base_rows_skipped(event: &str, release_paths: &[String]) -> Vec<&'static str> {
    if event != "pull_request" || release_paths.is_empty() {
        return Vec::new();
    }
    let mut selected = [false; BASE_ROWS.len()];
    for path in release_paths {
        if is_base_docs_file(path) {
            continue;
        }
        let mut narrowed = false;
        for ((_, crates), chosen) in BASE_ROWS.iter().zip(selected.iter_mut()) {
            if crates.iter().any(|dir| path.starts_with(dir)) {
                *chosen = true;
                narrowed = true;
            }
        }
        if !narrowed {
            selected = [true; BASE_ROWS.len()];
        }
    }
    BASE_ROWS
        .iter()
        .zip(selected)
        .filter(|(_, chosen)| !chosen)
        .map(|((name, _), _)| *name)
        .collect()
}

/// Which CI jobs run, by the job ids in `.github/workflows/ci.yml`, and how
/// many mutant shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) jobs: BTreeMap<&'static str, bool>,
    pub(crate) shards: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathState {
    Present(Option<Vec<u8>>),
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedPath {
    pub(crate) path: String,
    pub(crate) state: PathState,
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

/// `mode` is the diff's selection mode, `packages` the selected packages,
/// `event` the GitHub event name, `draft` whether the pull request is a
/// draft, `bug` whether an issue the pull request resolves is labelled
/// `bug`, `mutants` whether the pull request runs its mutants and
/// `mutant_count` how many mutants its diff holds.
pub(crate) fn plan(
    mode: &str,
    packages: &[String],
    event: &str,
    draft: bool,
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
    // A draft selects the same jobs as a ready pull request (`docs/ci.md`,
    // "The merge gate"), so the selection ignores it.
    let _ = draft;
    let jobs = BTreeMap::from([
        ("clippy_macos", false),
        ("scripts_self_test_macos", false),
        ("lint", !pr || code),
        // The backstop on `main` compiles the whole workspace on every push.
        // Linux x86_64 runs the selected tests on every event; `test` runs
        // the other platforms on the backstop alone.
        ("test_linux", test),
        // Only the backstop runs it now: a pull request runs the Linux leg
        // alone (`docs/ci.md`).
        ("test", !pr && test),
        ("mutants", shards > 0),
        ("bug_red", pr && code && bug),
        // The post-merge backstop stresses what the push touched; pull
        // requests never run it (`docs/ci.md`).
        ("stress", !pr && test),
        // The backstop measures the release build on every push; pull
        // requests never run it (`docs/ci.md`).
        ("release", !pr),
    ]);
    Plan { jobs, shards }
}

/// Suppress only the bug-red legs for a bug pull request with the documented
/// budget-breach explanation and both measurements in its body.
pub(crate) fn plan_with_budget_breach_body(mut plan: Plan, bug: bool, body: &str) -> Plan {
    if bug && budget_breach_section(body) {
        plan.jobs.insert("bug_red", false);
    }
    plan
}

/// Whether the body has a complete budget-breach section outside fenced code.
fn budget_breach_section(body: &str) -> bool {
    let mut fence: Option<(u8, usize)> = None;
    let mut in_section = false;
    let mut base = false;
    let mut head = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        let bytes = trimmed.as_bytes();
        if let Some(&marker) = bytes.first()
            && (marker == b'`' || marker == b'~')
        {
            let width = bytes.iter().take_while(|byte| **byte == marker).count();
            if width >= 3 {
                match fence {
                    None => fence = Some((marker, width)),
                    Some((open, open_width)) if marker == open && width >= open_width => {
                        fence = None
                    }
                    _ => {}
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if in_section {
                break;
            }
            in_section = heading.trim() == "Budget breach";
            continue;
        }
        if in_section {
            base |= trimmed.starts_with("Base:");
            head |= trimmed.starts_with("Head:");
        }
    }
    in_section && base && head
}

/// Add the pull-request-only macOS selections from the head paths and their
/// current contents, while leaving release-path selection separate.
pub(crate) fn plan_with_changed_paths(
    mut plan: Plan,
    pull_request: bool,
    changed_paths: &[ChangedPath],
) -> Plan {
    let clippy_macos = pull_request && changed_paths.iter().any(|entry| {
        matches!(&entry.state, PathState::Present(Some(contents)) if contents.windows(b"target_os".len()).any(|window| window == b"target_os"))
    });
    let scripts_self_test_macos = pull_request
        && changed_paths.iter().any(|entry| {
            entry.path.starts_with("scripts/") && matches!(entry.state, PathState::Present(_))
        });
    plan.jobs.insert("clippy_macos", clippy_macos);
    plan.jobs
        .insert("scripts_self_test_macos", scripts_self_test_macos);
    plan
}

/// What the rerun at the base commit says about one test that failed at
/// the head: whether it passed, failed, or never ran there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseOutcome {
    Passed,
    Failed,
    /// The base test list names the id but the rerun never executed it,
    /// so the base proves nothing about it.
    Unrunnable,
}

/// The base side of the head-vs-base break rule: either the base listing
/// and rerun completed with one outcome per head-failing test, or the base
/// could not be tested at all. An untestable base fails closed: every head
/// failure fails the pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BaseResults {
    Listed(BTreeMap<String, BaseOutcome>),
    Untestable,
}

/// The head-vs-base disposition of the head failures: the ids the pull
/// request newly broke fail the job; the ids present at base but unrunnable
/// there are dropped and named, as the stress plan drops and names the tests
/// the runner left out (`docs/ci.md`, "On every pull request that changes
/// code").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewBreaks {
    pub(crate) broken: Vec<String>,
    pub(crate) dropped: Vec<String>,
}

/// The new-breaks disposition of `head_failures` against `base`: a test
/// that fails at the head and passes at the base commit is newly broken,
/// and so is one the base list does not name, which the pull request added;
/// a test that also fails at the base is not. A base pass reached on retry
/// still counts as a pass: the id failed at the head and passed at the
/// base, so the head broke it.
pub(crate) fn new_breaks(head_failures: &[String], base: &BaseResults) -> NewBreaks {
    let BaseResults::Listed(outcomes) = base else {
        return NewBreaks {
            broken: head_failures.to_vec(),
            dropped: Vec::new(),
        };
    };
    let mut broken = Vec::new();
    let mut dropped = Vec::new();
    for id in head_failures {
        match outcomes.get(id) {
            None | Some(BaseOutcome::Passed) => broken.push(id.clone()),
            Some(BaseOutcome::Failed) => {}
            Some(BaseOutcome::Unrunnable) => dropped.push(id.clone()),
        }
    }
    NewBreaks { broken, dropped }
}

/// The exact nextest ids that failed at the head, one per line: each line is
/// the binary id, a space, the test name, the same id the stress plan's
/// `test` lines and the flake lists use. A line that is not one is an error
/// naming it.
pub(crate) fn parse_head_failures(text: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(binary), Some(name)) = (fields.next(), fields.next()) else {
            return Err(format!("head failures line {}: {line}", index + 1));
        };
        if fields.next().is_some() {
            return Err(format!("head failures line {}: {line}", index + 1));
        }
        ids.push(format!("{binary} {name}"));
    }
    Ok(ids)
}

/// The base side's per-test outcomes from the rerun's three id-per-line
/// lists: the base test list, the base failures and the base executions.
/// Each list holds exact nextest ids as `parse_head_failures` lines hold
/// them, empty lines skipped. An id the base list does not name gets no
/// line: the pull request added the test, and the verdict reads the absence
/// as PR-added (`new_breaks`). A listed id the rerun failed is `Failed`; a
/// listed id it executed without failing is `Passed`, so a base pass on
/// retry still counts as a pass and a retry never masks a head break; a
/// listed id it never executed is `Unrunnable` (`docs/ci.md`, "On every
/// pull request that changes code").
pub(crate) fn classify_base(
    head_failures: &[String],
    base_list: &str,
    base_failed: &str,
    base_executed: &str,
) -> BTreeMap<String, BaseOutcome> {
    fn ids(text: &str) -> std::collections::HashSet<&str> {
        text.lines().filter(|line| !line.is_empty()).collect()
    }
    let listed = ids(base_list);
    let failed = ids(base_failed);
    let executed = ids(base_executed);
    let mut outcomes = BTreeMap::new();
    for id in head_failures {
        if !listed.contains(id.as_str()) {
            continue;
        }
        let outcome = if failed.contains(id.as_str()) {
            BaseOutcome::Failed
        } else if executed.contains(id.as_str()) {
            BaseOutcome::Passed
        } else {
            BaseOutcome::Unrunnable
        };
        outcomes.insert(id.clone(), outcome);
    }
    outcomes
}

/// Failures, one line each; empty when `CI` passes. `results` maps each job
/// `CI` needs to its result, and `jobs` says which the selection chose.
/// `head_failures` holds the exact nextest ids that failed at the head and
/// `base` what the rerun at the base commit says about them; the Linux leg
/// consults the new-breaks disposition, so a failed leg fails only on the
/// tests the head newly broke. `touched_failed` says the leg's
/// `touched-retries` step failed: a touched test that passes only on retry
/// blocks the merge (`docs/testing/running.md`, "Flaky tests"), and break
/// data never excuses it. Without head failures the leg fails as any
/// other selected job does.
pub(crate) fn verdict(
    results: &BTreeMap<String, String>,
    jobs: &BTreeMap<String, bool>,
    head_failures: &[String],
    base: &BaseResults,
    touched_failed: bool,
) -> Vec<String> {
    let select = results.get("select").map_or("missing", String::as_str);
    if select != "success" {
        return vec![format!("select: {select}, so the selection failed")];
    }
    let mut failures = Vec::new();
    for (name, result) in results.iter().filter(|(name, _)| *name != "select") {
        if name == "test_linux" && result != "success" && !head_failures.is_empty() {
            // Not selected, or unknown to the selection, falls through to
            // the arms below, which report that; break data never excuses
            // a leg the selection did not choose.
            if jobs.get(name) == Some(&true) {
                for id in &new_breaks(head_failures, base).broken {
                    failures.push(format!("test_linux: newly broken: {id}"));
                }
                if touched_failed {
                    failures.push("test_linux: touched test passed only on retry".to_owned());
                }
                continue;
            }
        }
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
