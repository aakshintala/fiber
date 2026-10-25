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
/// `docs/ci.md`, "Selection": exact release-code files. The performance
/// table is release code: the release job judges the run against it.
const RELEASE_CODE_FILES: [&str; 3] = [
    "scripts/release-size",
    "scripts/stored-results",
    "docs/performance.md",
];

/// Whether `path` is code the release job runs (`docs/ci.md`, "Selection"):
/// exactly a release-code file, or under a release-code prefix and never a
/// docs file, so a Markdown file under release-code paths is not release
/// code. The exact-file list comes first, so the performance table is
/// release code even though it is a docs file.
fn is_release_code(path: &str) -> bool {
    RELEASE_CODE_FILES.contains(&path)
        || (!super::is_docs_file(path) && RELEASE_CODE_PREFIXES.iter().any(|p| path.starts_with(p)))
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
/// `hub/src/feed.rs:137` call into `contract`, `support` and `log`).
const LIST_CRATES: [&str; 8] = [
    "crates/config/",
    "crates/cli/",
    "crates/doors/",
    "crates/hub/",
    "crates/contract/",
    "crates/support/",
    "crates/log/",
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

/// `docs/ci.md`, "Selection": each base timing row with the crates
/// whose change selects it, in workload order. The names are the
/// harness's own workload names, cited by
/// `crates/bench/examples/bench/main_tests.rs`.
const BASE_ROWS: [(&str, &[&str]); 6] = [
    ("session start", &SESSION_CRATES),
    ("sessions list", &LIST_CRATES),
    ("terminal first frame", &TERMINAL_CRATES),
    ("terminal attach", &TERMINAL_CRATES),
    ("terminal wheel", &TERMINAL_CRATES),
    ("paging", &PAGING_CRATES),
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
        // A draft skips it (`docs/ci.md`).
        ("stress", pr && test && !draft),
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
