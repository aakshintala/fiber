//! Fiber's CI and gate helpers, run as `cargo xtask <command>` from the
//! repository root. It implements the selection and checks in
//! `docs/ci.md`; `scripts/check` and the workflows call it. It is
//! development tooling: no Fiber crate depends on it and it never ships.
//!
//! Commands:
//!
//! - `select --base REV`: what a diff from the merge base with REV runs,
//!   uncommitted and untracked files included, as `key=value` lines;
//!   `packages` are bare names (for tools that only match
//!   workspace members, such as `cargo fmt -p`), `package_specs` and
//!   `library_specs` are `name@version` (for `cargo -p`, which resolves a
//!   bare name against the whole dependency graph and can be ambiguous), and
//!   `extension_packages` are first-party package directories with cases
//! - `plan --base REV --mode M --packages "A B" --event E --draft true|false --bug true|false --mutants true|false --mutant-count N`:
//!   which CI jobs run, the mutant shards and each shard's time limit,
//!   as `key=value` lines
//! - `verdict`: reads `NEEDS` and `JOBS` from the environment and passes only
//!   if every selected job passed and every other job was skipped. With the
//!   `HEAD_FAILURES` file plus the `BASE_LIST`, `BASE_FAILED` and
//!   `BASE_EXECUTED` files set, a failed `test_linux` leg fails only on the
//!   tests the head newly broke against the base commit
//!   (`docs/ci.md`, "On every pull request that changes code").
//! - `mutants-reuse --pr N --head H --base B --mutant-count C --run-id R --draft true|false --runs FILE --jobs FILE`:
//!   prints `reuse=true` plus the matched run id and PR-entry head sha when
//!   an earlier completed run on the same pull request, head and base passed
//!   every mutants shard, else `reuse=false`
//! - `ticket`: the resolved issue the pull request body on stdin prints: the
//!   only one, or with several the first one labelled `bug`, else the first
//! - `bug-filter FILE...`: the nextest filter and packages for the test files among FILE,
//!   the `unrun` ones in `test = false` targets, and a `planned` line per test
//!   function in those files, as tab-separated lines
//! - `narrow-plan --list FILE --filter EXPR`: reads `planned` lines on stdin and the
//!   `cargo nextest list --message-format json` in FILE, drops tests the list did
//!   not match, and prints `dropped`, `run` and `filter` lines
//! - `stress-plan --base REV [--labels L] [--summary FILE] [--filter-file PATH] [--issue-text FILE] [--only named|rest]`:
//!   the test functions the working tree adds or changes against the merge base with REV that this runner
//!   compiles, as tab-separated lines, naming each dropped test, appending the
//!   job-summary block to FILE when given; with `--only`, plans only the named
//!   tests or only the rest, read from the issue text in FILE
//! - `touched-retries --base REV --list FILE`: whether any test the working tree adds or
//!   changes passed only on retry, reading the retry list in FILE; exit 1 when one did
//! - `stress-check --log FILE --count N --tests T`: checks the nextest stress output
//!   has N iteration summaries, each with T executed tests, and a complete run summary
//! - `stress-cost --start S --end E --count N --tests T --outcome O`: the stress job's
//!   wall time and runner minutes, as the job-summary block
//! - `docs-only FILE...`: whether every file is a docs file, as `docs-only: yes` or
//!   `docs-only: no`
//! - `bench-report --head FILE [--base FILE --base-commit SHA] [--stored-base FILE --stored-commit SHA]
//!   --doc docs/performance.md --event pull_request|push --comment FILE [--remeasure FILE]
//!   [--head-extra FILE --base-extra FILE]`: judges the benchmark result
//!   files against the budget table and writes the pull request comment; exit 1
//!   when a budget, a head self-check or the table mapping fails. `--remeasure`
//!   takes the path the skip list for the remeasure run is written to, when a
//!   gated timing row breaches on a pull request; `--head-extra` and
//!   `--base-extra` judge again with the remeasure run's samples merged in
//! - `models-dev [--from FILE]`: regenerates the first-party model lists
//!   from models.dev, or from FILE instead of fetching
//!   (`docs/model-routing.md`)
//! - `line-cap`, `unsafe-table`, `signal-sites`, `support-tests`, `visibility`, `compiled-in`, `dependency-list`, `image-isolation`, `tui-isolation`, `crate-edges`, `preserve-order`, `check-docs`, `ci-needs`, `kinds-ratchet`: the checks
//! - `doc-citations --print`: prints the allow table for code citations
//!   (`xtask/doc-citations.allow`); `check-docs` fails what the table
//!   does not list
//! - `flake-report-events`: reads one JSON object on stdin carrying the
//!   flake titles and the complete open and closed exact-title issue
//!   lookups with their completeness, and prints the unique closed issue
//!   numbers whose event lists the workflow must fetch, one per line;
//!   titles decided without events request nothing
//!   (`docs/testing/running.md`, "Flaky tests")
//! - `flake-report [--tested-commit SHA]`: reads one JSON object on stdin
//!   carrying the flake titles, the complete open and closed exact-title
//!   issue lookups with their completeness and the requested issues'
//!   event lists, and prints one JSON object per line:
//!   `{"action": ..., "issue": ..., "source": ..., "title": ...}` for
//!   every unique title with no open match (`reopen`, `file-new`,
//!   `stale`, `unknown`, with the failed lookup `source` on unknown);
//!   a complete open match prints nothing
//!   (`docs/testing/running.md`, "Flaky tests"). With `--tested-commit`
//!   it probes ancestry itself with `git merge-base --is-ancestor`, but
//!   only for each title's latest closed event's non-null commit; without
//!   it every fix probes unknown
//! - `break-glass-audit [--since] [--now EPOCH]`: reads one JSON object on
//!   stdin carrying the lookups of the last 48 hours of `main` commits, each
//!   commit's pull requests and each pull request head's `CI` check runs,
//!   and prints one line per merge whose head had no green `CI`, then the
//!   counts; it fails on any such merge whose pull request body does not
//!   start with `Break-glass:` and on a commit with no pull request, and an
//!   unreadable lookup is an error. `--since` prints the window's cutoff and
//!   reads no stdin (`docs/ci.md`, "The break-glass audit")
//! - `flake-classify --flaky-dir DIR --event NAME --tested-commit SHA
//!   [--base SHA] [--head SHA] [--compare-files FILE] [--pr-files FILE]`:
//!   reads the `flaky-tests-<runner>.txt` files under DIR and prints one
//!   JSON object per unique title, in first-seen order: `lookup` when the
//!   failure goes on to the issue lookup, `stale-base` when main changed
//!   the failing file since base, `pr-owned` when the pull request changed
//!   it (`docs/testing/running.md`, "Flaky tests")
//! - `session-paths`, `fiber-home-paths`, `loop-tool-names`: architecture rules 3, 4 and 5
//! - `must-succeed`: no test file holds a wait bound of 1 s up to `fakes::MUST_SUCCEED_WITHIN`, except the kept bounds in `xtask/src/must_succeed.rs`
//! - `support-tests`: no file under a crate's `tests/support/` that more than one test binary compiles holds a `#[test]`; the helpers' tests live in the one binary that declares them alone (`docs/code-quality.md`, "Size")
//! - `nightly-pair (--is-nightly|--prev-excluding|--parse-version|--check-extensions) <bare-version>|--count-nightly`:
//!   the shared nightly-tag predicate, the previous-nightly lookup over the
//!   tag list on stdin, and the `fiber --version`, extension-list and
//!   nightly-ref parses over supplied lines (`docs/ci.md`, "Releases")
//! - `nightly-fixture --repo <dir> --keep <bare-version>...`: prunes the
//!   clone to the bare keeps only (`docs/ci.md`, "Releases")
//! - `threads <cpus> <load>`: the nextest `--test-threads` count for the
//!   host's load, printed as one integer
//! - `budget-bounds --base REV`: whether any line changed against the merge
//!   base with REV raises `WAITS`, `BUDGET` or `CLEANUP`, or adds a
//!   `Deadline::after` of 60 s or more in a test — except the known-short
//!   bounds (`WAIT`, `DEADLINE`, `MUST_SUCCEED_WITHIN`, `Duration::ZERO`) and direct literals below 60 s — without `// ruling: #N`
//! - `screen-ratchet`: reads the inline per-file maxima from stdin as one
//!   `<path> <maximum>` per line, and fails when a file under
//!   `crates/main/tests/` outside `support/` holds more unmarked `.screen()`
//!   call lines than its maximum (0 when absent); a `mouse <path> <maximum>`
//!   line caps that file's mouse-event lines outside the frame-locked click
//!   helper the same way (`docs/testing/screens.md`, "Screens")
//! - `census-ratchet [--print]`: reads the inline per-file maxima from stdin
//!   as one `<path> <maximum>` per line, and fails when a production file
//!   under `crates/` holds more string-typed closed-set sites than its
//!   maximum (0 when absent); with `--print` it prints the current per-file
//!   counts instead and reads no stdin
//! - `stale-wait-ratchet`: reads the inline per-file maxima from stdin as one
//!   `<path> <maximum>` per line, and fails when a file under
//!   `crates/main/tests/` outside `support/` holds more stale-wait
//!   occurrences (subscribe-then-action-then-unbounded-wait on a replayable
//!   session, plus `recv_reply`, `fn subscribe(`, `fn subscribe_full(`)
//!   than its maximum (0 when absent)
//! - `site --out <dir>`: builds the user docs site (docs/ci.md, "The user docs site")
//! - `logo-mask --font <path> [--out <path>]`: regenerates the logo's alpha
//!   mask from JetBrains Mono ExtraBold; the font is downloaded by whoever
//!   regenerates and never committed

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool whose output is its interface"
)]

mod bench;
mod break_glass_audit;
mod budget_bounds;
mod census_ratchet;
mod ci_needs;
mod doc_citations;
mod docs;
mod flake_report;
mod logo;
mod models_dev;
mod models_dev_table;
mod must_succeed;
mod nightly_fixture;
mod nightly_pair;
mod rules;
mod screen_ratchet;
mod select;
mod site;
mod stale_wait_ratchet;
mod stress;
mod test_index;
mod threads;
mod ticket;
mod visibility;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde_json::Value;

use crate::rules::RustFile;
use crate::select::{Member, Members};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

/// Runs one command. Ok(false) means a check failed.
fn run(args: &[String]) -> Result<bool, String> {
    let (command, rest) = args
        .split_first()
        .ok_or("usage: cargo xtask <command>; see xtask/src/main.rs")?;
    match command.as_str() {
        "select" => {
            let members = workspace_members()?;
            let selection = select::classify(&changed_files(&flag(rest, "--base")?)?, &members);
            let package_specs: Vec<String> = selection
                .packages()
                .iter()
                .map(|p| select::spec(p, &members))
                .collect();
            let library_specs: Vec<String> = selection
                .packages()
                .iter()
                .filter(|p| members.get(*p).is_some_and(|m| m.library))
                .map(|p| select::spec(p, &members))
                .collect();
            println!("mode={}", selection.mode());
            println!("packages={}", selection.packages().join(" "));
            println!("package_specs={}", package_specs.join(" "));
            println!("library_specs={}", library_specs.join(" "));
            if let Some(extension_packages) =
                select::extension_packages(&selection, Path::new("."))?
            {
                println!("extension_packages={}", extension_packages.join(" "));
            }
            Ok(true)
        }
        "plan" => {
            let packages: Vec<String> = flag(rest, "--packages")?
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let draft = flag(rest, "--draft")? == "true";
            let bug = flag(rest, "--bug")? == "true";
            let mutants = flag(rest, "--mutants")? == "true";
            let mutant_count: u64 = flag(rest, "--mutant-count")?
                .parse()
                .map_err(|e| format!("--mutant-count: {e}"))?;
            let release_paths = release_paths(&flag(rest, "--base")?)?;
            let plan = select::plan(
                &flag(rest, "--mode")?,
                &packages,
                &flag(rest, "--event")?,
                draft,
                bug,
                mutants,
                mutant_count,
            );
            let shards: Vec<u64> = (0..plan.shards).collect();
            println!(
                "jobs={}",
                Value::from(serde_json::Map::from_iter(
                    plan.jobs
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
                ))
            );
            println!("shards={}", Value::from(shards));
            println!("shard_total={}", plan.shards);
            println!(
                "shard_timeout={}",
                select::shard_timeout_minutes(mutant_count)
            );
            println!(
                "ten_turns={}",
                if select::ten_turns_runs(&flag(rest, "--event")?, &release_paths) {
                    "true"
                } else {
                    "false"
                }
            );
            println!(
                "base_skip={}",
                select::base_rows_skipped(&flag(rest, "--event")?, &release_paths).join(",")
            );
            Ok(true)
        }
        "mutants-reuse" => {
            let current = reuse_current(rest)?;
            let runs = read(&flag(rest, "--runs")?)?;
            let jobs = read(&flag(rest, "--jobs")?)?;
            match select::matched_reuse(&current, &runs, &jobs) {
                Some(matched) => {
                    println!("reuse=true");
                    println!("matched_run_id={}", matched.run_id);
                    println!("matched_pr_head={}", matched.pr_head.0);
                }
                None => println!("reuse=false"),
            }
            Ok(true)
        }
        "verdict" => {
            let jobs: BTreeMap<String, bool> = env_object("JOBS")?
                .into_iter()
                .map(|(k, v)| (k, v.as_bool() == Some(true)))
                .collect();
            let results: BTreeMap<String, String> = env_object("NEEDS")?
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        v.get("result")
                            .and_then(Value::as_str)
                            .unwrap_or("missing")
                            .to_owned(),
                    )
                })
                .collect();
            // The head-vs-base break data the Linux leg uploads on a pull
            // request whose head run failed (`docs/ci.md`, "On every pull
            // request that changes code"): the head's failed ids plus the
            // base test list, failures and executions, classified in tested
            // code. Absent everywhere else, when the verdict falls back to
            // the job results alone; a failed leg with head failures but no
            // usable base fails closed.
            let head_failures = match data_file("HEAD_FAILURES")? {
                None => Vec::new(),
                Some(text) => select::parse_head_failures(&text)?,
            };
            let base = match (
                data_file("BASE_LIST")?,
                data_file("BASE_FAILED")?,
                data_file("BASE_EXECUTED")?,
            ) {
                (Some(list), Some(failed), Some(executed)) => select::BaseResults::Listed(
                    select::classify_base(&head_failures, &list, &failed, &executed),
                ),
                _ => select::BaseResults::Untestable,
            };
            if !head_failures.is_empty() {
                let breaks = select::new_breaks(&head_failures, &base);
                let base_failed = match &base {
                    select::BaseResults::Listed(outcomes) => head_failures
                        .iter()
                        .filter(|id| outcomes.get(*id) == Some(&select::BaseOutcome::Failed))
                        .count(),
                    select::BaseResults::Untestable => 0,
                };
                println!(
                    "new-breaks: {} failing at head, {base_failed} also failing at base, {} newly broken",
                    head_failures.len(),
                    breaks.broken.len()
                );
                for id in &breaks.dropped {
                    println!("new-breaks dropped (unrunnable at base): {id}");
                }
            }
            // Whether the Linux leg's `touched-retries` step failed: a
            // touched test that passes only on retry blocks the merge
            // (`docs/testing/running.md`, "Flaky tests"), and break data
            // never excuses it. Unset when the leg never ran, so the
            // verdict falls back to the job results alone.
            let touched_failed = std::env::var("TOUCHED_FAILED").as_deref() == Ok("true");
            let failures = select::verdict(&results, &jobs, &head_failures, &base, touched_failed);
            report(
                "verdict",
                &failures,
                "every selected job passed and every other job was skipped",
            )
        }
        "ticket" => {
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("stdin: {e}"))?;
            let numbers = ticket::ticket(&body);
            let chosen = match numbers.as_slice() {
                [] => None,
                [only] => Some(only.clone()),
                [first, ..] => {
                    let mut chosen = first.clone();
                    for number in &numbers {
                        if labelled_bug(number)? {
                            chosen = number.clone();
                            break;
                        }
                    }
                    Some(chosen)
                }
            };
            if let Some(number) = chosen {
                println!("{number}");
            }
            Ok(true)
        }
        "bug-filter" => {
            let members = workspace_members()?;
            let sources = rust_files(&members)?;
            let test_files = select::test_files(rest, &members, &sources)?;
            let index = test_index::TestIndex::build(Path::new("."), &test_files, &members, None)?;
            let (filter, packages) = select::test_filter(rest, &members, &sources, &index)?;
            println!("filter\t{filter}");
            for package in packages {
                println!("package\t{package}");
            }
            for path in select::unrun_test_files(rest, &members) {
                println!("unrun\t{path}");
            }
            for test in select::tests_in_files(rest, &members, &sources, &index)? {
                println!("{}", select::planned_line(&test));
            }
            Ok(true)
        }
        "narrow-plan" => {
            let list_path = flag(rest, "--list")?;
            let expression = flag(rest, "--filter")?;
            let json = read(&list_path)?;
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|error| format!("stdin: {error}"))?;
            let plan = select::narrow_plan(&expression, &body, &json)?;
            for line in &plan.dropped {
                println!("dropped\t{line}");
            }
            println!("run\t{}", if plan.run { "true" } else { "false" });
            println!("filter\t{}", plan.filter);
            Ok(true)
        }
        "stress-plan" => stress::plan_command(rest),
        "stress-check" => stress::stress_check_command(rest),
        "touched-retries" => stress::touched_retries_command(rest),
        "stress-cost" => stress::cost_command(rest),
        "docs-only" => {
            let yes = select::docs_only(rest);
            println!("docs-only: {}", if yes { "yes" } else { "no" });
            Ok(yes)
        }
        "models-dev" => {
            let from = optional(rest, "--from");
            models_dev::run(Path::new("."), from.as_deref().map(Path::new)).map(|()| true)
        }
        "line-cap" => {
            let over = rules::over_cap(&rust_files(&workspace_members()?)?);
            for line in &over {
                println!("line-cap: {line}");
            }
            if over.is_empty() {
                println!("line-cap: ok");
            }
            Ok(true)
        }
        "unsafe-table" => {
            let files = rust_files(&workspace_members()?)?;
            report(
                "unsafe-table",
                &rules::unsafe_mismatches(&files, &read("docs/code-quality.md")?)?,
                "ok",
            )
        }
        "signal-sites" => report(
            "signal-sites",
            &rules::signal_sites(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "support-tests" => report(
            "support-tests",
            &rules::support_tests(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "visibility" => {
            let members = workspace_members()?;
            let baseline = visibility::load_baseline();
            let files = rust_files(&members)?;
            report(
                "visibility",
                &visibility::check_with_baseline(&files, &baseline),
                "ok",
            )
        }
        "session-paths" => report(
            "session-paths",
            &rules::session_paths(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "fiber-home-paths" => report(
            "fiber-home-paths",
            &rules::fiber_home_paths(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "loop-tool-names" => report(
            "loop-tool-names",
            &rules::loop_tool_names(&rust_files(&workspace_members()?)?),
            "ok",
        ),
        "must-succeed" => report(
            "must-succeed",
            &must_succeed::must_succeed(
                &rust_files(&workspace_members()?)?,
                must_succeed::ALLOWANCES,
            ),
            "ok",
        ),
        "nightly-pair" => nightly_pair::run(rest),
        "nightly-fixture" => nightly_fixture::run(rest),
        "threads" => {
            let (cpus, load) = match rest {
                [cpus, load] => (cpus, load),
                _ => return Err("usage: cargo xtask threads <cpus> <load>".to_owned()),
            };
            let cpus: f64 = cpus.parse().map_err(|e| format!("cpus: {e}"))?;
            let load: f64 = load.parse().map_err(|e| format!("load: {e}"))?;
            println!("{}", threads::threads(cpus, load));
            Ok(true)
        }
        "budget-bounds" => {
            let base = flag(rest, "--base")?;
            report("budget-bounds", &budget_bounds::budget_bounds(&base)?, "ok")
        }
        "screen-ratchet" => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| format!("stdin: {e}"))?;
            let maxima = screen_ratchet::parse_screen_maxima(&input)?;
            report(
                "screen-ratchet",
                &screen_ratchet::check(&rust_files(&workspace_members()?)?, &maxima),
                "ok",
            )
        }
        "census-ratchet" => {
            let files = rust_files(&workspace_members()?)?;
            if rest.iter().any(|arg| arg.as_str() == "--print") {
                print!("{}", census_ratchet::table(&files));
                Ok(true)
            } else {
                let mut input = String::new();
                std::io::stdin()
                    .read_to_string(&mut input)
                    .map_err(|e| format!("stdin: {e}"))?;
                let maxima = screen_ratchet::parse_maxima(&input)?;
                report(
                    "census-ratchet",
                    &census_ratchet::check(&files, &maxima),
                    "ok",
                )
            }
        }
        "stale-wait-ratchet" => {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| format!("stdin: {e}"))?;
            let maxima = screen_ratchet::parse_maxima(&input)?;
            report(
                "stale-wait-ratchet",
                &stale_wait_ratchet::check(&rust_files(&workspace_members()?)?, &maxima),
                "ok",
            )
        }
        "site" => site::run(rest),
        "compiled-in" => {
            let members = workspace_members()?;
            let files = rust_files(&members)?;
            let mut failures = select::compiled_in_mismatches(&files, &members)?;
            failures.extend(select::package_reader_mismatches(&files, &members)?);
            failures.extend(select::runtime_read_mismatches(&files, &members)?);
            report("compiled-in", &failures, "ok")
        }
        "dependency-list" => {
            let failures = rules::unlisted(&cargo_dependencies()?, &read("docs/dependencies.md")?)?;
            report("dependency-list", &failures, "ok")
        }
        "image-isolation" => isolation(
            "image-isolation",
            &rules::IMAGE,
            "no crate but picture and main links image code",
        ),
        "tui-isolation" => isolation(
            "tui-isolation",
            &rules::TUI,
            "no crate but tui and main links ratatui or crossterm",
        ),
        "crate-edges" => {
            let failures = rules::crate_edges(
                &normal_edges()?,
                rules::CALLS,
                &read("docs/architecture.md")?,
            )?;
            report(
                "crate-edges",
                &failures,
                "every first-party dependency is one the call rules grant",
            )
        }
        "preserve-order" => {
            // What ships in a Fiber binary, over every target and feature:
            // Cargo unifies features across every crate linked into one
            // binary, so one normal dependency enabling preserve_order
            // anywhere poisons it everywhere. A build script is resolved
            // and compiled separately, so its serde_json never serialises
            // a request: `-e features,no-build` leaves build edges out of
            // the tree.
            // research/tui-prototype is not a workspace member, so its own
            // preserve_order never reaches this tree.
            let tree = output(
                "cargo",
                &[
                    "tree",
                    "--workspace",
                    "--target",
                    "all",
                    "--all-features",
                    "-e",
                    "features,no-build",
                    "--prefix",
                    "depth",
                    "-i",
                    "serde_json",
                ],
            )?;
            report(
                "preserve-order",
                &rules::preserve_order(&tree),
                "serde_json's preserve_order feature is never enabled",
            )
        }
        "check-docs" => {
            let root = Path::new(".");
            let mut failures = Vec::new();
            for path in docs::checked_files(root)? {
                failures.extend(docs::check_file(root, &path)?);
            }
            failures.extend(doc_citations::check(root)?);
            let members = workspace_members()?;
            failures.extend(visibility::check_with_baseline(
                &rust_files(&members)?,
                &visibility::load_baseline(),
            ));
            report("check-docs", &failures, "ok")
        }
        "doc-citations" => {
            if rest == ["--print"] {
                print!("{}", doc_citations::table(Path::new("."))?);
                Ok(true)
            } else {
                Err("usage: cargo xtask doc-citations --print".to_owned())
            }
        }
        "kinds-ratchet" => {
            let max: usize = flag(rest, "--max")?
                .parse()
                .map_err(|e| format!("--max: {e}"))?;
            let support_max: usize = flag(rest, "--support-max")?
                .parse()
                .map_err(|e| format!("--support-max: {e}"))?;
            report(
                "kinds-ratchet",
                &rules::kinds_ratchet(&rust_files(&workspace_members()?)?, max, support_max),
                "ok",
            )
        }
        "ci-needs" => report(
            "ci-needs",
            &ci_needs::check(&read(ci_needs::WORKFLOW)?, &read(ci_needs::CI_DOC)?)?,
            "every job but ci and the report jobs is in the ci job's needs",
        ),
        "flake-report-events" => {
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("stdin: {e}"))?;
            let input: Value =
                serde_json::from_str(&body).map_err(|e| format!("flake-report-events: {e}"))?;
            for number in flake_report::event_requests(&input)? {
                println!("{}", number.0);
            }
            Ok(true)
        }
        "flake-report" => {
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("stdin: {e}"))?;
            let input: Value =
                serde_json::from_str(&body).map_err(|e| format!("flake-report: {e}"))?;
            let tested = optional(rest, "--tested-commit");
            for decision in flake_report::decide_with(&input, &|fix| {
                tested
                    .as_deref()
                    .and_then(|commit| ancestor_of(fix, commit))
            })? {
                println!("{}", decision.line());
            }
            Ok(true)
        }
        "break-glass-audit" => {
            let args = break_glass_audit::parse_args(rest)?;
            let now = match args.now {
                Some(now) => now,
                None => epoch_now()?,
            };
            if args.since {
                println!("{}", break_glass_audit::window_since(now));
                return Ok(true);
            }
            let annotate = std::env::var_os("GITHUB_ACTIONS").is_some();
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("stdin: {e}"))?;
            let report = break_glass_audit::parse_input(&body)
                .and_then(|input| break_glass_audit::audit(&input, now))
                .inspect_err(|message| {
                    if annotate {
                        println!("::error::{message}");
                    }
                })?;
            for line in report.render() {
                println!("{line}");
            }
            if annotate {
                for line in report.failing() {
                    println!("::error::{line}");
                }
            }
            Ok(report.passed())
        }
        "flake-classify" => {
            let dir = flag(rest, "--flaky-dir")?;
            let event = flag(rest, "--event")?;
            let tested = flag(rest, "--tested-commit")?;
            let base = optional(rest, "--base").unwrap_or_default();
            let head = optional(rest, "--head").unwrap_or_default();
            let compare_files = match optional(rest, "--compare-files") {
                Some(path) => read(&path)?,
                None => String::new(),
            };
            let pr_files = match optional(rest, "--pr-files") {
                Some(path) => read(&path)?,
                None => String::new(),
            };
            let lists = flake_report::read_lists(Path::new(&dir))?;
            let listed: Vec<flake_report::Listed> = lists
                .iter()
                .map(|(runner, text)| flake_report::Listed { runner, text })
                .collect();
            let run = flake_report::Run {
                pull_request: event == "pull_request",
                tested_commit: &tested,
                base: &base,
                head: &head,
                compare_files: &compare_files,
                pr_files: &pr_files,
            };
            for classified in flake_report::classify(&listed, &run) {
                println!("{}", classified.line());
            }
            Ok(true)
        }
        "flake-titles" => {
            for title in flake_report::flake_titles(rest)? {
                println!("{title}");
            }
            Ok(true)
        }
        "logo-mask" => logo::run(rest),
        "bench-report" => {
            let event = bench::Event::parse(&flag(rest, "--event")?)?;
            let comment_path = flag(rest, "--comment")?;
            let doc = read(&flag(rest, "--doc")?)?;
            let head = read(&flag(rest, "--head")?)?;
            // An unreadable base is reported in the comment, not as a usage
            // error: the base only feeds advisory timings.
            let base = optional(rest, "--base").map(|path| read(&path));
            let base_commit = optional(rest, "--base-commit").unwrap_or_default();
            // An unreadable stored file is reported in the comment, not as
            // a usage error: memory and exact rows then fall back to the table.
            let stored_commit = optional(rest, "--stored-commit").unwrap_or_default();
            let stored = optional(rest, "--stored-base").map(|path| {
                let commit = stored_commit.clone();
                match read(&path) {
                    Ok(text) => bench::Stored {
                        commit,
                        text: Ok(text),
                    },
                    Err(e) => bench::Stored {
                        commit,
                        text: Err(e),
                    },
                }
            });
            // The remeasure run's samples, merged into the breached rows and
            // judged again: both or neither, since one side alone cannot
            // move the median both sides are judged on.
            let extra = match (
                optional(rest, "--head-extra"),
                optional(rest, "--base-extra"),
            ) {
                (Some(head_path), Some(base_path)) => {
                    let head_extra = read(&head_path)?;
                    Some((head_extra, Some(read(&base_path))))
                }
                (None, None) => None,
                _ => {
                    return Err("--head-extra and --base-extra must be given together".to_owned());
                }
            };
            // `report` is `report_with_extra` without extras, so one call
            // covers both arms: `None` judges the first pass alone.
            let out =
                bench::report_with_extra(&doc, &head, base, stored, event, &base_commit, extra);
            std::fs::write(&comment_path, &out.comment)
                .map_err(|e| format!("{comment_path}: {e}"))?;
            // The skip list for the remeasure run: only when a gated timing
            // row breached on a pull request, so the file's absence or
            // emptiness means no second harness run.
            if event == bench::Event::PullRequest
                && let (Some(path), Some(skip)) =
                    (optional(rest, "--remeasure"), &out.remeasure_skip)
            {
                std::fs::write(path, format!("{skip}\n"))
                    .map_err(|e| format!("--remeasure: {e}"))?;
            }
            report(
                "bench-report",
                &out.failures,
                "every exact and memory budget holds",
            )
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// Checks every workspace member's normal dependency tree against `rule`.
fn isolation(name: &str, rule: &rules::Isolation, ok: &str) -> Result<bool, String> {
    let members = workspace_members()?;
    let mut trees = Vec::new();
    for member in members.keys() {
        let tree = output(
            "cargo",
            &[
                "tree",
                "-p",
                &select::spec(member, &members),
                "-e",
                "normal",
                "--prefix",
                "none",
            ],
        )?;
        trees.push((member.clone(), tree));
    }
    report(name, &rules::leaks(&trees, rule), ok)
}

fn report(name: &str, failures: &[String], ok: &str) -> Result<bool, String> {
    for failure in failures {
        println!("{name}: {failure}");
    }
    if failures.is_empty() {
        println!("{name}: {ok}");
    }
    Ok(failures.is_empty())
}

/// The system clock in whole seconds since the epoch: `break-glass-audit`
/// reads it only as the CLI default when `--now` is omitted; the scheduled job
/// always passes `--now`.
// Exempt: the only caller is the CLI default when `--now` is omitted, which
// `scheduled.yml` never does; a test would read the real clock, which
// docs/testing.md forbids, so no test can tell `Ok(0)` or `Ok(1)` apart.
#[cfg_attr(false, mutants::skip)]
#[allow(
    clippy::disallowed_methods,
    reason = "the audit window is relative to the real day the job runs; tests pass --now"
)]
fn epoch_now() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|e| format!("clock: {e}"))
}

fn flag(args: &[String], name: &str) -> Result<String, String> {
    optional(args, name).ok_or(format!("{name} is required"))
}

fn optional(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// The JSON object in environment variable `name`; empty when unset.
fn env_object(name: &str) -> Result<serde_json::Map<String, Value>, String> {
    let raw = std::env::var(name).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    if let Value::Object(map) = serde_json::from_str(&raw).map_err(|e| format!("{name}: {e}"))? {
        Ok(map)
    } else {
        Err(format!("{name} is not a JSON object"))
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

/// The reuse decision's scalar flags for `mutants-reuse`.
fn reuse_current(args: &[String]) -> Result<select::ReuseCurrent, String> {
    Ok(select::ReuseCurrent {
        pr: select::ReusePullRequestNumber(
            flag(args, "--pr")?
                .parse()
                .map_err(|e| format!("--pr: {e}"))?,
        ),
        head: select::ReuseCommitSha(flag(args, "--head")?),
        base: select::ReuseCommitSha(flag(args, "--base")?),
        mutant_count: flag(args, "--mutant-count")?
            .parse()
            .map_err(|e| format!("--mutant-count: {e}"))?,
        run_id: select::ReuseRunId(
            flag(args, "--run-id")?
                .parse()
                .map_err(|e| format!("--run-id: {e}"))?,
        ),
        draft: flag(args, "--draft")? == "true",
    })
}

/// The contents of the file the `name` environment variable points at: `None`
/// when the variable is unset or empty, so the same invocation runs with and
/// without the break data. Other reads fail the command: unusable break data
/// fails closed.
fn data_file(name: &str) -> Result<Option<String>, String> {
    let path = std::env::var(name).unwrap_or_default();
    if path.trim().is_empty() {
        return Ok(None);
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{name} {path}: {error}")),
    }
}

/// Whether `fix` is an ancestor of `tested`: `None` when the object is
/// missing or Git errors, which the decision reports as unknown.
fn ancestor_of(fix: &str, tested: &str) -> Option<bool> {
    let exists = Command::new("git")
        .args(["cat-file", "-e", fix])
        .status()
        .ok()?
        .success();
    if !exists {
        return None;
    }
    Some(
        Command::new("git")
            .args(["merge-base", "--is-ancestor", fix, tested])
            .status()
            .ok()?
            .success(),
    )
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(Path::new("."))
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{program}: {e}"))
}

fn labelled_bug(number: &str) -> Result<bool, String> {
    let out = output(
        "gh",
        &[
            "issue",
            "view",
            number,
            "--json",
            "labels",
            "-q",
            ".labels[].name",
        ],
    )?;
    Ok(out.lines().any(|line| line == "bug"))
}

fn git_lines(args: &[&str]) -> Result<Vec<String>, String> {
    Ok(output("git", args)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Files that differ from the merge base with `base`, uncommitted and
/// untracked changes included.
fn changed_files(base: &str) -> Result<Vec<String>, String> {
    let merge_base = git_lines(&["merge-base", base, "HEAD"])?
        .into_iter()
        .next()
        .ok_or("no merge base")?;
    let mut files: BTreeSet<String> = git_lines(&["diff", "--name-only", &merge_base])?
        .into_iter()
        .collect();
    files.extend(git_lines(&["ls-files", "--others", "--exclude-standard"])?);
    Ok(files.into_iter().collect())
}

/// Paths for the release rule from the merge base with `base`: committed
/// changes with renames reported as both endpoints, plus untracked files.
/// Both reads are NUL-delimited, so non-ASCII paths arrive literally,
/// never git-quoted; a non-UTF-8 path fails with an error, never a skip.
fn release_paths(base: &str) -> Result<Vec<String>, String> {
    let merge_base = git_lines(&["merge-base", base, "HEAD"])?
        .into_iter()
        .next()
        .ok_or("no merge base")?;
    let mut files: BTreeSet<String> = BTreeSet::new();
    files.extend(split_nul(&output(
        "git",
        &["diff", "--name-only", "--no-renames", "-z", &merge_base],
    )?));
    files.extend(split_nul(&output(
        "git",
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?));
    Ok(files.into_iter().collect())
}

/// The NUL-delimited paths in `out`, dropping the empty trailing field.
fn split_nul(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn packages() -> Result<(String, Vec<Value>), String> {
    let meta: Value = serde_json::from_str(&output(
        "cargo",
        &["metadata", "--format-version", "1", "--no-deps"],
    )?)
    .map_err(|e| format!("cargo metadata: {e}"))?;
    let root = meta
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or("cargo metadata: no workspace_root")?;
    let packages = meta
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata: no packages")?;
    Ok((root.to_owned(), packages.clone()))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A path under `root`, relative to it, with `/` separators.
fn relative(path: &Path, root: &str) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn workspace_members() -> Result<Members, String> {
    let (root, packages) = packages()?;
    Ok(members_from(&root, &packages))
}

/// The workspace members `cargo metadata` describes: its workspace root and
/// the `packages` it lists. Pure, so a fixture can stand in for the command.
fn members_from(root: &str, packages: &[Value]) -> Members {
    let dir_of = |package: &Value| {
        relative(
            Path::new(text(package, "manifest_path"))
                .parent()
                .unwrap_or(Path::new("")),
            root,
        )
    };
    let by_dir: BTreeMap<String, &str> = packages
        .iter()
        .map(|p| (dir_of(p), text(p, "name")))
        .collect();
    packages
        .iter()
        .map(|package| {
            let mut deps = Vec::new();
            let mut normal_deps = Vec::new();
            for dependency in array(package, "dependencies") {
                let Some(path) = dependency.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let Some(name) = by_dir.get(&relative(Path::new(path), root)) else {
                    continue;
                };
                deps.push((*name).to_owned());
                // The normal-edge subset mirrors `normal_edges`: a missing
                // or null `kind` is a normal edge, anything else is not.
                if !dependency.get("kind").is_some_and(|kind| !kind.is_null()) {
                    normal_deps.push((*name).to_owned());
                }
            }
            let library = array(package, "targets")
                .iter()
                .any(|t| array(t, "kind").iter().any(|k| k.as_str() == Some("lib")));
            let unrun_tests = array(package, "targets")
                .iter()
                .filter(|t| array(t, "kind").iter().any(|k| k.as_str() == Some("test")))
                .filter(|t| t.get("test").and_then(Value::as_bool) == Some(false))
                .map(|t| relative(Path::new(text(t, "src_path")), root))
                .collect();
            let member = Member {
                dir: dir_of(package),
                version: text(package, "version").to_owned(),
                deps,
                normal_deps,
                library,
                unrun_tests,
            };
            (text(package, "name").to_owned(), member)
        })
        .collect()
}

/// (crate, dependency) for every dependency a workspace Cargo.toml names,
/// other than workspace members.
fn cargo_dependencies() -> Result<BTreeSet<(String, String)>, String> {
    let (_, packages) = packages()?;
    let names: BTreeSet<&str> = packages.iter().map(|p| text(p, "name")).collect();
    Ok(packages
        .iter()
        .flat_map(|p| {
            array(p, "dependencies")
                .iter()
                .map(move |d| (text(p, "name"), text(d, "name")))
        })
        .filter(|(_, dep)| !names.contains(dep))
        .map(|(krate, dep)| (krate.to_owned(), dep.to_owned()))
        .collect())
}

fn normal_edges() -> Result<BTreeSet<(String, String)>, String> {
    let (root, packages) = packages()?;
    let dir_of = |package: &Value| {
        relative(
            Path::new(text(package, "manifest_path"))
                .parent()
                .unwrap_or(Path::new("")),
            &root,
        )
    };
    let by_dir: BTreeMap<String, &str> = packages
        .iter()
        .map(|package| (dir_of(package), text(package, "name")))
        .collect();
    let mut edges = BTreeSet::new();
    for package in &packages {
        for dependency in array(package, "dependencies") {
            if dependency.get("kind").is_some_and(|kind| !kind.is_null()) {
                continue;
            }
            let Some(path) = dependency.get("path").and_then(Value::as_str) else {
                continue;
            };
            let dir = relative(Path::new(path), &root);
            if let Some(member) = by_dir.get(&dir) {
                edges.insert((text(package, "name").to_owned(), (*member).to_owned()));
            }
        }
    }
    Ok(edges)
}

/// Every Rust file in a member, tracked or not, skipping ignored ones,
/// listed and read relative to the working directory like every other check.
fn rust_files(members: &Members) -> Result<Vec<RustFile>, String> {
    let mut args = vec![
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend(members.values().map(|m| m.dir.as_str()));
    let mut files = Vec::new();
    for path in git_lines(&args)?.into_iter().collect::<BTreeSet<_>>() {
        let Some(krate) = select::owner(&path, members) else {
            continue;
        };
        let Some(member) = members.get(krate) else {
            continue;
        };
        let disk = Path::new(&path);
        if !path.ends_with(".rs") || !disk.exists() {
            continue;
        }
        let rel = path
            .get(member.dir.len() + 1..)
            .unwrap_or_default()
            .to_owned();
        let source = std::fs::read_to_string(disk).map_err(|e| format!("{path}: {e}"))?;
        files.push(RustFile {
            krate: krate.to_owned(),
            path,
            rel,
            source,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod check_resources_tests;
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
