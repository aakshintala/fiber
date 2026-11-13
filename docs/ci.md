# CI

What runs in CI, on which runners, and what must pass before a pull request
merges and before a release ships. This is what is true now, not a plan. It
is settled by
[CI: what runs, where, and what gates a merge](https://github.com/aakshintala/fiber/issues/62);
that ticket's resolution holds the rationale and the rejected alternatives.

The checks themselves are set elsewhere: tests in `docs/testing.md`, lints
and file rules in `docs/code-quality.md`, crates and supply chain in
`docs/dependencies.md`, budgets in `docs/performance.md`, and the tool
definition size budget in `docs/tools.md`. What a release contains is
`docs/releasing.md`.

## Runners

CI is GitHub Actions on GitHub-hosted runners: `ubuntu-24.04` for Linux
x86_64, `ubuntu-24.04-arm` for Linux arm64, and a macOS arm64 runner. The
repository is public, so minutes cost nothing. The account is on GitHub Pro,
which runs at most 40 jobs at once and at most 5 macOS jobs at once, shared
by every repository on the account. A pull request runs Linux x86_64 jobs
(`lint` and `test_linux`, plus `bug_red_linux` when an issue its
body resolves carries the `bug` label) and macOS arm64 jobs
(`bug_red_macos` on the same bug pull requests, `clippy_macos`
when its diff touches a file containing `target_os`, and
`scripts_self_test_macos` when its diff touches `scripts/`; the latter two
run on the macOS arm64 runner ("On every pull request that changes code")).
Each macOS job holds one macOS slot against the 5-slot cap while it runs; no
Linux-side job changes. The scripts job is selected only for pull requests
with a path under `scripts/`, never on the backstop. Draft and ready pull requests select
the same jobs. The Linux arm64 and macOS arm64 test legs, `stress` and
`release` run on the backstop, which is a push to `main`. A bug pull request
uses a macOS job (`bug_red_macos`); so does any pull request whose diff
touches a file containing `target_os` (`clippy_macos`). The 40-job cap is the one CI reaches: from 2026-10-07 to
2026-10-09 every job that queued for more than 15 minutes waited while 38 or
more jobs ran, and the macOS cap held jobs back for 85 of 3,250 queued
minutes. A change that saves jobs, such as fewer mutants shards, shortens the
queue; one that saves only macOS jobs does not.

Nightly releases share the backstop pushes' concurrency group (`ci-main` in
both `.github/workflows/release.yml` and `.github/workflows/ci.yml`). A
nightly never overlaps a backstop run while either waits on the 5-slot macOS
cap. The group does not cover the whole cap. Pull-request macOS jobs
(`bug_red_macos` and `clippy_macos`), stable push releases (the `release-push` group), manually
dispatched workflows, and other repositories on the account still use macOS
slots beside a nightly.

Every runner has a C compiler, which `mlua` needs to build Lua
([ADR 0006](adr/0006-extension-runtime-lua.md)).

## The merge gate

One check is required to merge: `CI`. It passes only when every job the
selection chose succeeded and every job it did not choose was skipped. If
the selection itself fails, `CI` fails. On a draft pull request the verdict
job reports as `CI (draft)`, so the required `CI` check stays pending. A
draft selects the same jobs as a ready pull request, so marking it ready
starts nothing new: the Linux jobs already ran, and the `bug_red` legs run
on drafts too.
The owner can merge without a green `CI` only by a break-glass merge
(`docs/workflow.md`, "Break-glass merge").

Every job in `.github/workflows/ci.yml` but the verdict job is in the
verdict job's `needs`, except the jobs that report without gating it
directly: `backstop_report`, `bench_comment`, `cache_prune`, `flake_decide`,
`flake_report`, `bug_red_linux`, `bug_red_macos`, `journey`, `journey_report`.
A run can conclude `failure` while `CI` passes, when the job that failed is
one of these: the merge verdict is the `CI` check, never the run's
conclusion.
The bug-red legs report
nextest's status to the `bug_red` collector, which is in the verdict's
`needs`; their raw results are not selection entries. The docs check fails
when any other job is missing from `needs`, so a new job, such as a new
shard, cannot run without gating the merge.

The selection is one job that every other job waits for, so it does as
little as it can: it works out the selection and, on a pull request, runs
the docs check, below.

A branch does not have to be up to date with `main` to merge, and there is no
merge queue. The backstop on `main` catches two pull requests that each
passed alone but break together. A pull request runs `main`'s workflow
against its own head, so when a `main` commit changes `ci.yml` together with
the code it calls, a branch from before that commit fails on what the
workflow now passes, such as an unknown argument, until it merges `main`.
GitHub reads the workflow from `refs/pull/N/merge`, so a pull request that
edits `ci.yml` runs its own edit, and the pull request's own run proves that
edit.

A newer push to a pull request cancels that pull request's older run. A
label, a change from draft to ready or a reopen at the same head cancels
nothing: the run in flight finishes, then the new event's run, which waited,
runs the whole selection again at the same head. At most one run waits, and
a newer event replaces it before it starts. Rerunning everything keeps the
`CI` check, which is the latest run's, from hiding a failure in the earlier
run. A ready run still reuses a same-head mutants pass instead of rerunning
those shards ("On every pull request that changes code"). Pushes
to `main` share one group that cancels nothing: the running backstop finishes
and a newer push waits, replacing any older one that waits ("The backstop on
`main`").

## Selection

A pull request runs only what its diff can affect.

- A diff in which every file is Markdown, or is under `docs/` or
  `research/`, runs no job after the selection, whose docs check is the
  whole run. A file under `research/tui-prototype/` is an exception: it
  selects `lint`, which checks the prototype, and no tests. A file a crate
  compiles in is another exception, below. A file
  under `providers/` or `extensions/` is the last exception: it runs the
  package readers, below, even when it is Markdown.
- A diff that changes `Cargo.lock`, any `Cargo.toml`, `rust-toolchain.toml`,
  anything under `.github/` or `scripts/`, `clippy.toml`, `deny.toml`,
  `.cargo/config.toml` or `.config/nextest.toml` runs everything. These
  files are matched outside `research/`, which is not part of the
  workspace: a manifest there is a docs file.
- A file a crate compiles in runs that crate alone, not the crates that
  depend on it. This covers Markdown anywhere, and any file outside the
  crate. A change to `docs/events.md`, `docs/errors.md` or
  `docs/invocation.md` runs `contract`, whose tests check
  the code against them. A change to `docs/extensions.md` runs `extensions`,
  whose tests check its hook keys table against the code. A change to `docs/tui.md` runs `contract` and
  `tui`. A change to `docs/ci.md` or `docs/performance.md` runs `xtask`, whose tests pin the fast session_list ceiling to the 9V45 class data in `xtask/tests/class_data/`, recomputed from it by the `docs/performance.md` rule. A change to a built-in skill under `docs/skills/` runs `loop`, whose
  tests check it parses, and `main`, whose tests install it. The selector lists these files. The gate fails when
  the list and the source disagree, or when an include's argument is not a
  string literal. Tests read a repository file only by compiling it in,
  except a package reader's read of a first-party package through
  `CARGO_MANIFEST_DIR` (next item); the gate fails on any other run-time read.
  An `include_str!` path resolves from the source file that holds it.
- A diff under `providers/` or `extensions/` runs the binary-level tests
  and every crate whose tests read a first-party package. The selector lists
  the crates that read packages. The gate fails when the list and the source
  disagree.
- Any other diff runs the crates it touches and every crate that depends on
  them, read from the workspace's dependency graph
  ([ADR 0002](adr/0002-module-boundaries-are-crate-boundaries.md)).
  Binary-level tests depend on every crate that ships, so any change to one
  runs them. `xtask` ships in no binary, so a change to it alone runs its
  own tests. A changed file that is documentation, under `providers/` or
  `extensions/`, or a file a crate compiles in is not a candidate — docs
  files do not own a crate, and listed files run their crate without
  dependents. For each crate the diff touches, when every candidate file in
  that crate is a test file — under `tests/`, named `tests.rs` or ending
  `_tests.rs`, or under a `snapshots/` directory — the diff runs that crate
  without the crates that depend on it, except the crates that depend on it
  only through a development-dependency, which run one hop with no further
  fan-out; a crate with any other candidate file runs with its dependents,
  where development-edges propagate one hop and normal edges transitively.
  Run-everything paths and diffs touching the prototype directory keep their
  rules above and below.
- Every diff that runs the binary-level tests also runs `fiber extension
  test` for every first-party package that has cases, not only the changed
  ones ("Testing an extension"). A change to Fiber's own code can break a
  package that did not change. The release-profile job, `release`, never runs
  on a pull request; the backstop measures it on every push ("The backstop
  on `main`").
- No diff selects `release` on a pull request, whatever it touches: not the
  binary-level tests, not the code the release job runs or the table it
  judges against (`crates/bench`, `xtask/src/bench*`, `scripts/release-size`,
  `scripts/stored-results`, `docs/performance.md`). A broken harness or judge
  fails the backstop, which files an issue, instead of failing the pull
  request.
- `release` always runs the "Session, memory kept after 10 busy turns"
  workload (`docs/performance.md`): a push measures every workload. The loop
  holds the session's state, so a diff elsewhere rarely moves the row.
- The backstop measures every timing row on its release binary
  (`docs/performance.md`), whatever the diff touches; the selector still maps
  each diff to rows for the record, but no pull request runs the head or base
  passes. `session start` maps to `crates/contract`, `crates/support`,
  `crates/log`, `crates/loop`, `crates/doors`, `crates/provider`,
  `crates/routing`, `crates/tools`, `crates/mcp`, `crates/jobs`, `crates/net`,
  `crates/worktree`, `crates/extensions`, `crates/config`, `crates/hub` or
  `crates/main`; `sessions list` to `crates/config`, `crates/cli`,
  `crates/doors`, `crates/hub`, `crates/contract`, `crates/support`,
  `crates/log`, `crates/worktree` or `crates/main`; the terminal rows
  (`terminal first frame`, `terminal attach`, `terminal wheel`) to the
  session-start crates plus `crates/tui` or `crates/cli`; `paging` to
  `crates/tui`, `crates/contract`, `crates/support` or `crates/log`;
  `sandbox setup` to `crates/tools/`. Every other path maps to every row, and
  a `.md` file under `crates/` counts as code. A regression that reaches a
  row through a dependency the mapping misses surfaces at the backstop.

The selector has its own tests.

## On every pull request that changes code

On every pull request, ready or draft, one Linux x86_64 job (`test_linux`)
runs `scripts/check`, the gate CI applies (`docs/workflow.md`, "The gate").
A pull request fails only on the tests it newly broke against its base. The
Linux arm64 and macOS arm64 test legs (the `test` job), the release build
and size check, and the stress run move to the post-merge run on `main`
("The backstop on `main`"). `test_linux` runs on the backstop too.
Each job:

- first checks free space in the scratch directory and on the `target`
  filesystem (`scripts/check-resources preflight`), and exits 75 without
  building when either is too low (`docs/workflow.md`)
- exits 75 when a compile or link step is killed for lack of resources
  (`scripts/check-resources classify`)
- compiles the selected crates with the debug profile, every target,
  through clippy and nextest; the whole workspace is built by the backstop
  ("The backstop on `main`")
- runs clippy with the workspace lints across all targets, so code compiled
  only for one platform is linted on that platform
- runs the selected tests under nextest, and doc-tests with
  `cargo test --doc`
- scans for processes the tests leaked, failing with their PIDs and commands
  (`docs/testing.md`, "Running tests")
- reports how many tests ran

When the pull request's diff touches a file containing `target_os`, a second
job (`clippy_macos`) lints the xtask-selected crates on the macOS arm64
runner: the workspace lints across all targets with `-D warnings`, clippy
only, no tests. An empty selection logs and succeeds without running cargo.
The trigger reads the names-only diff at the head commit, so a pull request
that moved a gated file still fires it and a deleted file never does; it
fires on pull requests only, never on the backstop. Its known gap: a
macOS-only error in a file containing no `target_os` does not trigger the
job.

When a pull request's names-only diff contains a path under `scripts/`, a
`scripts_self_test_macos` job runs these self-tests on macOS arm64 by invoking
each script with the runner's system `/bin/bash`: `scripts/test-leak-scan`,
`scripts/test-look-diff`, `scripts/test-flaky-tests`,
`scripts/test-flake-report`, `scripts/test-retry-filter`,
`scripts/test-stress-plan`, `scripts/test-journey-report`,
`scripts/test-mutants-rotation`, `scripts/test-mutants-shard-finish`,
`scripts/test-bug-red` and `scripts/test-class-data`. They mirror the
self-test block in `scripts/check`; the job does not call `scripts/check`.
This includes the leak-scan self-test. It triggers only on pull requests,
never on the backstop; the backstop already runs the macOS test leg. The job
holds one macOS slot while it runs.

The debug profile sets `panic = "abort"`, as the release profile does, so
the `fiber` binary that binary-level tests start behaves like the one that
ships. Cargo ignores the setting when it builds the test harness.

When the Linux run fails, the job compares against the base commit before
the verdict runs. It writes the failed tests to a file as exact nextest ids,
one per line, then checks out the base commit in a scratch worktree and
reruns exactly those tests there, under the same retry profile the head ran
under. A base pass on retry still counts as a pass, so a retry never masks a
head break. The scratch checkout never touches the job's own checkout,
target directory or caches.

The verdict fails only the newly broken tests: an id that fails at the head
and passes at the base, or that the base list does not name because the pull
request added the test. An id that also fails at the base does not fail the
pull request, and an id the base lists but never runs is dropped and named. A
base that does not build, or whose test list fails, proves nothing old, so
every head failure fails the pull request. Both files travel to the verdict
as an artifact; a missing file fails closed there too.

### The one-week trial

For one week after this gate lands, count pull-request CI failures where
`scripts/check` passed on the same head, and record the count on
[#4272](https://github.com/aakshintala/fiber/issues/4272). Near zero means a
follow-up can drop pull-request CI and keep the post-merge run.

A failed binary-level test retries once. A pass on retry does not fail the
run, except for a test the pull request touched (below): CI opens a flake
issue for the test's file, or comments on the open one
(`docs/testing.md`, "Flaky tests"). An area binary in `crates/main/tests/<area>/`
is keyed by its first test-module segment, the file the test lives in, as in
`Flaky tests: main::reviewer_setup` for
`main::extension_host reviewer_setup::the_setup_saves`; a root-level test in
an area binary falls back to `Flaky tests: main::<area>`, while a unit-test
binary is keyed per file, as in `Flaky tests: extensions::host::exec`, and a
single-file integration binary keeps `Flaky tests: <binary>`. The retry is nextest's `ci` profile over
the `crates/main/tests` binaries, and two report jobs file the issue. The
read-only `flake_decide` job holds only `contents: read` and `issues: read`:
it downloads each selected test leg's exact `flaky-tests-<runner>.txt`
artifact, tolerating a missing artifact so the collate can report that leg as
partial when another selected leg has a readable list. It fails closed on a
cancelled or skipped selected leg before downloading anything (an empty list
with a true marker is a complete empty result): a leg whose tests failed but
whose list was written still feeds the collate, which fails closed only when
no readable list feeds it: no list at all, or an existing-but-empty list with
a `false` marker, which does not feed when no other readable list exists. The reporter files for the legs it
has. It then looks
up the complete open and closed issues, asks
`cargo xtask flake-report-events` which issue numbers need event fetches,
fetches only those endpoints, and runs
`cargo xtask flake-report --tested-commit` to decide one JSON-lines action
per unique title, probing ancestry itself only for each selected
latest fix. It then runs `cargo xtask flake-classify`, which reads the
runner lists and the two file lists git diffs from the merge base, and
writes one classification line per unique title: a stale base, a
file the pull request owns, or the issue lookup. It uploads one combined
`flake-report-data` artifact carrying the decision lines plus the
classification lines. The write-only
`flake_report` job holds `contents: read`, `pull-requests: write` and
`issues: write`, checks nothing out and runs no Fiber code: it downloads
only that exact combined artifact, with no Actions API call and no added
permission, guards on it, comments on the open issue, and otherwise
carries out the one decided action per title after validating the reopen
predicates, labelling a new issue `test-only`, and rechecking the open
titles immediately before the write. Unknown decisions name the failed
lookup so the summary does too. One report writes at a time:
`concurrency.group: flake-report` with `cancel-in-progress: false` and
`queue: max` serialises runs, and GitHub cancels a new report past its 100
pending reports; two runs racing to file the same title can still file
twice, and the recheck keeps that window small. Pinned actionlint 1.7.12
does not know the `queue` key, so the workflow step ignores only that one
unsupported-schema diagnostic. The workflow-level
concurrency is unchanged: a newer push still cancels an older pull request
run, which can lose its report, and a later test run reports the flake
again. No other test retries.

A test the pull request adds or changes runs 30 times under the `stress`
profile, which never retries. When the pull request fixes a flaky test,
each test the linked flake issue names runs 100 times, in every binary
that compiles it. One failure fails the run. The retry above applies only
to tests the pull request did not touch.

The `stress` job runs on the backstop on Linux x86_64 beside the
test jobs and builds from the same restored cache. The tests it runs are
the test functions the diff adds or changes, found by
`cargo xtask stress-plan`. An empty plan skips the run unless a changed
source contains a test function, at least one indexed test binary compiles
it, and every such binary lists no tests; this error also applies when the
diff changes only helper code. A test-name mismatch is not caught when a
binary lists other tests. A changed file under `tests/` with no test
function selects nothing and skips the run. A source with a top-level
`#![cfg(...)]` inner attribute is excluded from this error. A test
function in a target whose `Cargo.toml` declares `test = false` is not one:
only an explicit `--test <name>` builds such a target, so a plain filter
matches nothing in it, and a diff whose touched test functions are all in
such targets skips the run. The plan is then intersected with
`cargo nextest list` on the runner. The plan writes the filter to
`$RUNNER_TEMP/stress.toml`, which the run step passes by path with
`--tool-config-file`; the filter text does not travel in an environment
variable or command-line argument. Linux caps each argument and environment
string at 128 KiB (`MAX_ARG_STRLEN`). The run step annotates an
`Argument list too long` failure as `Stress run could not start`. The
always-run cost step annotates a failed run step that never wrote its start
marker as `Stress step did not start`; no test ran. Both are job failures; the annotation says only why no test result exists. A touched test the runner
does not compile, such as
`cfg(target_os = "macos")` on this Linux x86_64 job, is dropped from the
plan, named in the job output and not stressed. It still runs in the
macOS leg on the backstop. A plan left empty by that intersection skips
the run. A run whose
filter matches no test, or whose iteration runs a different number of tests
than the plan names, fails the job. The count is read from the linked issue's
labels, which decide whether the pull request fixes a flaky test. Which tests
run 100 times is decided by the names: each test the linked flake issue names
runs 100 times and every other selected test runs 30; when the issue names no
test, every selected test runs 30 times. Nothing caps the test set: the job stresses
every test the plan selects. The job is in `ci`'s `needs`; on the backstop
a failure files an issue instead of stopping a merge. It writes its wall
time, runner minutes and selected tests to its job summary. A touched
test still gets nextest's one retry but a pass on that retry fails the
test job (`cargo xtask touched-retries`).

Names are the test ids on the linked issue's marker lines, the body and the
comments together: the filer's "The test `...` passed after a retry" sentence
names one test per sighting, normalised past the binary prefix. The plan step
writes the body and each comment to a file and `stress-plan` reads the names
from it, matching each against the tests the plan selected; a name that
matches no selected test runs nowhere and is reported as unmatched in the
plan output. With names and a flaky-test fix the plan runs twice, `--only
named` and `--only rest`, writing one filter file per group; the run step
executes one nextest invocation per non-empty group, each with its own count
and its own check, skipping an empty group and naming it in the summary. A
heading per group carries its count, and the single aggregate cost step keeps
the job envelope.

The Linux x86_64 full-suite run uploads its JUnit XML as the `junit`
artifact, kept 7 days. It holds only XML this run wrote: the full-suite
`scripts/check` output after retry handling, uploaded before the focused
hub_service run overwrites `target/nextest/ci/junit.xml`.

On Linux x86_64 alone:

- `cargo fmt --check`
- a process signal appears only in the guarded helpers (`cargo xtask signal-sites`); the check scans Rust tokens, so comments never count, and its allowlist is a trust list: the files it names may send signals, every other file may not
- no test file holds a wait bound of 1 s up to `fakes::MUST_SUCCEED_WITHIN`, in a `const`, a `Deadline::after` or `.within`, a wait call such as `await_requests`, a `.min` cap or a socket timeout setter (`cargo xtask must-succeed`, `docs/testing.md`, "Waits and timeouts"); the check scans Rust tokens, so comments never count, and its allow-list names only each kept bound (a poll slice, a fake-clock grace or step, a bound the code under test reads, a "nothing arrives" check); it reports each site by file path, then line
- a raised `WAITS`, `BUDGET` or `CLEANUP` bound, and a `Deadline::after` in a test that is 60 s or more or that the check cannot read, fail (`cargo xtask budget-bounds --base "$base"`, `xtask/src/budget_bounds.rs`); only lines changed against the merge base are scanned, as Rust tokens, so comments never count and unchanged lines never fail. A bound the check cannot read — a shared name outside its known list, or a computed `Duration` — fails closed: the long names (`WAITS`, `BUDGET`, `CLEANUP`, `MAX`) fail while the short ones (`WAIT`, `DEADLINE`, `MUST_SUCCEED_WITHIN`, `ZERO`) pass unruled, and anything else fails as not shown below 60 s; the check reports `Deadline::after({spelling}) is 60 s or more`, or `Deadline::after({spelling}) (readable: WAIT, DEADLINE, MUST_SUCCEED_WITHIN, Duration::ZERO) is not shown below 60 s`. A site is cleared by `// ruling: #N` on the source line holding the `after` ident, as in `crates/main/tests/support/mod.rs` (`Deadline::after(fakes::MUST_SUCCEED_WITHIN * 2) // ruling: #4075`); the doubled bound is the relation (`docs/testing.md`, "Waits and timeouts") asks for
- `narrating-comments --base "$base"` (`xtask/src/narrating_comments.rs`); checks added comment lines in Rust, shell, Python, TOML and YAML sources, plus files under `scripts/`, using the stateless line rules in [`docs/code-quality/lints.md`](code-quality/lints.md)
- the compiled-in list matches the files crates compile in, Markdown
  anywhere or any file outside the crate, every include argument is a
  string literal, and no crate reads a repository file at run time but a
  package reader reading a first-party package ("Selection")
- no crate but `picture` and `main` has `image` or `fast_image_resize` in its
  normal dependency tree (`cargo xtask image-isolation`), so the session
  process links no image code
- no crate but `tui` and `main` has `ratatui` or `crossterm` in its normal
  dependency tree (`cargo xtask tui-isolation`), so no other crate depends on
  a crate admitted only for the terminal
- every direct normal dependency between workspace crates is granted by the
  quoted allow-list in `docs/architecture.md`, "The call rules"
  (`cargo xtask crate-edges`)
- no crate enables serde_json's `preserve_order` feature in any crate linked into a workspace binary or test, over every target and feature; build-dependency edges are excluded (`cargo xtask
  preserve-order`), so prompt bytes stay cacheable (`docs/prompt-cache.md`)
- string-typed closed-set sites stay capped per production file under `crates/` (`cargo xtask census-ratchet`): a file whose count rises above its inline maximum in `scripts/check` fails, a file missing from the list holds none, and lowering a count never fails; `cargo xtask census-ratchet --print` prints the current per-file counts
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` in
  `research/tui-prototype`, when the diff touches it. The prototype is not a
  workspace member, so the checks above never reach it; it has its own
  `clippy.toml`, which drops the clock bans because the prototype reads the
  wall clock. A diff that touches `research/tui-prototype/` selects `lint`,
  whether or not it touches anything else; any other `research/` path alone
  selects the docs job alone
- `shellcheck --shell=sh` on `scripts/install.sh` and its test stubs
- `shellcheck --shell=bash` on every other `scripts/` shell file: a file at
  any depth under `scripts/`, other than `scripts/install.sh`, whose first line
  is a shebang naming `bash`, directly or through `env` with its options and
  assignments, plus every `scripts/lib/*.sh`. `cargo xtask shellcheck-files`
  lists them from the tree when the check runs, so a new bash script is
  checked with nothing added here or to `scripts/check`, and the command
  fails when it finds none
- actionlint over every file in `.github/workflows/`, when the diff changes
  one. A workflow change runs the whole selection ("Selection"), so the job is
  always selected. No `taiki-e/install-action` tool installs actionlint, so the
  step downloads the pinned release, v1.7.12, and checks its SHA-256. It also runs
  shellcheck on each `run:` script. Any finding fails the job. Locally it is
  optional: `brew install actionlint`, then `actionlint` from the repository
  root.
- cargo-deny's licence, source and ban checks
- the built-in tool definitions within their byte budget, with each
  definition's size printed
- mutation testing: `cargo-mutants --in-diff`, split across runners. It runs
  on a ready pull request and on a draft pull request
  that carries the label `mutants`, never on a push to `main`; an unlabelled draft skips it, and adding
  the label starts a run. A ready run whose pull request number and head sha
  match an earlier completed run on the same head, and whose every mutants
  shard in that run names the current merge parent in its job name and
  succeeded with the same shard count, reuses the pass instead of starting
  shards; `cargo xtask mutants-reuse` decides from the current run's pull
  request number, head, base, and mutant count. Any lookup failure, head or
  pull request mismatch, a shard naming a different base, partial, cancelled,
  or failed shards, count mismatch, or self-match starts the shards. A reused
  run still lists the diff's mutants in the selection
  summary, and the label rule above is unchanged. Shards start once the Linux x86_64 test job, its own
  job `test_linux` (a job can wait for another job, not one platform's leg of
  it), passes on the head; the arm64 and macOS legs, the `test` job, run in
  parallel and still gate `CI`. Each shard runs `cargo mutants --in-place
  --baseline=skip --timeout 120`, since the tests it would rerun as a baseline
  just passed on this head. Shards build the test fakes `scripts/check` builds
  and fail if a scenario log shows a fake was missing. A systemd user scope
  caps the run at 12 GiB, leaving 4 GiB of the
  runner's 16 GiB for the host. `OOMPolicy=kill` kills the whole scope on an
  out-of-memory event, so the shard fails instead of counting a killed test as
  a caught mutant. If the run exits non-zero, the step prints the mutant in the
  newest scenario log under `mutants.out/log/`. Each shard prints its mutant
  list before the run and uploads `mutants.out` as a shard-named artifact, even
  if the run fails.
  Selection lists the diff's mutants (`cargo mutants --in-diff --list`) to its
  job summary on every code pull request event, draft or not, whether or not
  shards will run. A run whose Linux x86_64 tests fail starts none, and a run that selects no tests
  runs no mutants. The selection counts the diff's mutants
  (`cargo mutants --list`) and plans one shard per 15 of them, rounded up, at
  most 16; a diff with no mutants starts none. Recent runs (#1461, #1395)
  tested a mutant in 13 to 27 seconds on a runner and the unmutated baseline
  took 1 to 3.5 minutes, so 15 mutants keep a shard near 10 minutes; a
  `crates/main` mutant takes 50 to 74 seconds. Shards split the list
  round-robin, so every shard holds the same mix of crates and
  the count alone sets its size. Past 240 mutants the cap makes shards larger,
  and their time limit grows with them ("Job time limits"). Two pull requests at the cap
  leave 8 of the 40 jobs for everything else.
  Mutants run under nextest's `mutants` profile (`.cargo/mutants.toml`),
  which stops every running test at the first failure. A mutant that makes
  one test hang until its deadline is then caught by a faster test, not
  reported as a timeout.

  A separate workflow runs daily at 04:00 UTC against the scheduled commit,
  generating mutants across every workspace package. Its shards build the test
  fakes, run `cargo mutants --in-place`, and fail if a scenario log shows a fake
  was missing. It runs one full-workspace
  nextest baseline under the `ci` profile, with the same retry as the test job
  (a failed binary-level test retries once), then a list job counts the workspace
  mutants and picks one rotation day's 16 shards, and the shards run once the
  baseline passes; at most
  16 shards run at once, and a concurrency group prevents scheduled runs from
  overlapping. The rotation covers the whole list over `days = max(1,
  ceil(count / 4800))` days, so each day runs 16 shards of about 300 mutants:
  the day is the days since 1970-01-01 modulo the rotation length, and its
  shards are `--shard k/T` with round-robin sharding for the 16 values of `k`
  starting at 16 times the day, where `T` is 16 times the rotation length. Each
  shard's artifact name and job summary carry its `k`, `T` and rotation day.
  The 300-mutant target comes from run 39399082087, which listed about 28,900
  workspace mutants and tested 18 to over 30 seconds a mutant, so 602-mutant
  shards ran over the 300-minute step limit while 300 mutants keep a shard
  near 150 minutes.
  Both baseline and shard jobs install zsh and fish for the completion tests.
  Each shard always uploads `mutants.out`, including survivors and logs. A
  shard that finishes with only missed mutants is green: its job summary
  lists each missed mutant's file and line, and survivors are filed after
  each full rotation as per-crate "kill or exempt" tickets. The
  initial job limit is a generous 360 minutes because scheduled cold-run history
  is not available; each mutation step has a 300-minute limit. This report gates
  no pull request or merge.
- for a pull request where any issue its body resolves is labelled
  `bug`, the tests its first commit, the red commit, adds or changes run
  at the red commit on Linux x86_64 and macOS arm64. Each leg must build
  the red commit. The check passes only when at least one test fails on at
  least one leg; a test may pass or have no match on the other. Every test
  the pull request adds or changes must pass at its head on both platforms.
  The two red-test legs report nextest's status class to a `bug_red`
  collector job, which is in the verdict's `needs`, so the proof gates the
  merge through the `CI` check; `CI` stays the only required check
  (`docs/workflow.md`, "The pull request"). A pull request whose every
  changed file is a docs file, as "Selection" defines it, passes the check
  with a message saying so: the doc was wrong and the code was right, so no
  test can show the bug. Test files in a target whose `Cargo.toml` declares
  `test = false` (today `main_journeys`) are left out of the plan, because
  only an explicit `--test <name>` runs them. A pull request whose red
  commit's only test files are such files passes the check, with a message
  saying its only touched tests are journey tests, which pull-request CI
  does not run until #3899 step 6; the legs agree on that result or the
  check fails. A red commit that also holds another test file runs that
  file and needs it to fail, and any other empty plan fails. Each leg
  plans only the touched tests its platform compiles, and names the ones
  it drops. A plan empty on both legs after that drop fails. When the
  journey target joins pull-request CI (#3899, step 6), the plans emit
  `--test <name>` for it instead. The label is read when the run's selection job
  runs, so a relabel takes effect at the next push or a whole-run rerun;
  rerunning only the failed job keeps the old reading.
- each profiling feature built once, without running it:
  `cargo check -p main --bins --tests --features alloc-stats` and
  `cargo check -p main --bins --tests --features dhat-heap`, so neither
  stops compiling unnoticed. The job's `timeout-minutes` is 12 to leave
  room for the two uncached checks alongside `scripts/check`. The backstop
  on `main` runs the tests that exist only under those features, on Linux
  x86_64, Linux arm64 and macOS arm64: the `diag` unit tests
  (`cargo nextest run -p diag --lib --features alloc-stats`) and one exact
  test each inside the `timings` binary
  (`test(=alloc_stats::a_debug_tool_run_line_carries_alloc_fields)` under
  `alloc-stats`,
  `test(=dhat_heap::ask_writes_dhat_heap_json_at_exit)` under `dhat-heap`),
  never the whole suite under either feature.

No pull request runs `release`. The backstop measures the release build on
every push ("The backstop on `main`").

One more Linux x86_64 job builds the release profile for the target that
ships, `x86_64-unknown-linux-musl` (`docs/releasing.md`), at the pushed
commit (`scripts/release-size`). It checks that the stripped binary is
under 26 MiB and runs the benchmarks that judge each push, every row at 5
runs including the 10-busy-turns workload (`docs/performance.md`). A breach
fails the backstop, which files an issue instead of stopping a merge. A
commit that does not build fails the job the same way.

The release profile sets `lto = "fat"` and `codegen-units = 1`, which
shrinks the binary and lengthens the release build. Fiber's own crates, except
`tui` and `picture`, build at `opt-level = "s"`: they are the largest share of
the binary's code, and drawing and image resizing, which stay at the default
`opt-level = 3`, are the paths where the smaller code costs measurable time.
Third-party crates stay at 3 (#2042).

Nothing in CI writes a snapshot, calls a live provider or reaches the public
network (`docs/testing.md`).

## The docs check

`scripts/check-docs` checks `docs/`, `GLOSSARY.md`, `AGENTS.md` and
`README.md`, offline, and the CI gate wiring below. It fails when:

- a relative Markdown link, or its `#anchor`, does not resolve
- a section citation, such as `` (`docs/workflow.md`, "The gate") ``, names a
  heading the file does not have, including a citation broken across lines
- a citation in code under `crates/`, `xtask/` or `scripts/` gives a line number (`docs/<file>.md:<n>`, `docs/<file>.md line <n>`) instead of the file and a quoted heading, or quotes a heading its file lacks, or names the doc path without backticks, and `xtask/doc-citations.allow` does not list it
- a backticked path under `docs/`, `crates/`, `scripts/`, `research/` or
  `.github/` does not exist. A path with a placeholder in it, such as
  `docs/<area>.md`, is not checked.
- a job in `.github/workflows/ci.yml` other than the verdict job and the
  report jobs is missing from the verdict job's `needs`, or the report
  jobs named in "The merge gate" differ from the check's own list
  (`cargo xtask ci-needs`)
- a `pub(crate)` or `pub(super)` function, const or static no other file
  of its crate names (`cargo xtask visibility`)

`xtask/doc-citations.allow` lists, per file, each such citation still allowed and how many times. A citation beyond its count fails, and so does an entry whose citation the code no longer holds. The PR that adds a rule seeds the list with the citations already in the code, and after that the list only shrinks. `cargo xtask doc-citations --print` prints it. The check reads every text file under those three directories except the allow table itself, whose own entries would otherwise read as citations.

It runs in the selection job on every pull request, not only docs-only
ones, because the change that breaks a citation is usually a code change
that renames or deletes what a doc points at. External URLs are not checked: nothing in CI reaches the
public network.

The same job, on every pull request and every push to `main`, checks that
the `unsafe` table in `docs/code-quality.md` matches the code
(`cargo xtask unsafe-table`) and that every crate from outside the workspace
that a `Cargo.toml` names is listed in `docs/dependencies.md`
(`cargo xtask dependency-list`). Each
compares a doc with the source, so a change to either side can break it,
and a docs-only diff, which runs no other job, would otherwise skip it.

## Advisories

cargo-deny's advisory check blocks a pull request that changes `Cargo.lock`,
and blocks a release. It also runs daily on `main` and opens an issue, or
comments on the open one, when an advisory applies. A pull request that does
not change `Cargo.lock` is not failed by an advisory published after it was
opened.

## The break-glass audit

A break-glass merge (`docs/workflow.md`, "Break-glass merge") lands without a
green `CI`. The `break-glass audit` job in `scheduled.yml` is the detection
half: it finds merges to `main` that did so and checks that each one declared
it. It runs daily at 06:53Z and on `workflow_dispatch`.

The job fetches with `gh api` and hands the responses to
`cargo xtask break-glass-audit`, which makes every decision. It audits each
commit on `main` from the last 48 hours. The window is twice the cadence, so
one skipped or failed run leaves no gap; the overlap lists a merge twice,
which is harmless. Each commit maps to the merged pull request that carries
it, and that pull request's head must have a completed `CI` check run with
conclusion `success`. Only the exact name `CI` counts: a `CI (draft)` run does
not. When a head has several `CI` runs, the newest decides.

A merge with no green `CI` is listed on one line:
`merge <sha> head <sha> PR #<n> ticket <#n|-> <declared|undeclared>`. The
pull request body decides which: it declares break-glass only when it starts
with exactly `Break-glass:`, and the ticket is the first `#<number>` in it.
The job fails on an undeclared merge, and on a direct push to `main`, which
has no pull request and prints as `PR none`. A declared merge is listed and
passes. The last line counts what the run read:
`audited <n> merges, <k> without green CI, <u> undeclared`.

A result the job cannot read fails it: a `gh` error, a response that is not
JSON or lacks an expected field, or a check-run list shorter than its
`total_count`. A green run therefore means every merge in the window was read.
The declaration is the pull request body as it is at audit time and is
self-attested.

The job reads with the workflow token, `contents`, `pull-requests` and
`checks` at `read`, and uses no secret. It never reads the `main` ruleset, so
the owner's `bypass_actors` check after each break-glass merge
(`docs/workflow.md`, "Break-glass merge") is the other control, and nothing in
CI covers it. A red audit pauses no merges and is not part of any release
gate.

## The backstop on `main`

Each push to `main` queues a backstop run. It runs the lint, test_linux, test,
stress and release jobs the selection chooses from the diff
since the last `main` commit whose backstop passed. The tests run on all
three platforms, and it compiles
the whole workspace. That is the parent
commit unless a run was cancelled or failed. A conflict between two merged
pull requests shows in a crate that depends on what the later one changed,
and the selection includes that crate. The release job runs on every push
and stores that commit's stripped binary in the build cache and its
benchmark results, the full suite at 5 runs, including the 10-busy-turns workload, as the `benchmarks-<sha>`
artifact, both for a later pull request's base. A push runs every gating row, as does the release workflow: neither passes `--skip-rows`. The results file records the
stripped binary's size in bytes, so a size history is one artifact per commit.

One backstop runs and one waits. All pushes to `main` share a concurrency group
(`ci-main`) with non-rehearsal nightly releases, and the group never cancels
a running run, so the running backstop finishes, which lets `main` record
a pass during a stream of merges. GitHub keeps one pending run per group:
a newer push replaces a waiting backstop, and a nightly joining the group
evicts a waiting push. The newest backstop's selection covers every commit it
replaced, including commits evicted by a nightly, because it diffs from the
last passing commit. A replaced or evicted commit stores no release binary
until that next backstop runs; a pull request based on one compares against
the nearest ancestor that has one ("On every pull request that changes code").
When `main` is quiet, that next backstop does not run until the next push,
so an evicted commit has no backstop result and no stored release binary
until then. If nightly evictions keep happening, the remedy is to move the
05:00Z nightly cron, not to widen the fence.

The backstop runs no mutants. Each pull request already tests its own diff
("On every pull request that changes code"), and a second run on `main` would
repeat that cost.

When lint, the tests or the release job fail, the backstop opens an issue, or
comments on the open one. It
never blocks a merge.

The backstop is the only run that saves the build cache. Pull requests
restore it and never write it, so branches do not fill the repository's
10 GB cache. When the backstop saves a new generation of the build cache, a final job
(`scripts/cache-prune`, the only one with `actions: write`) deletes the older
ones, and it keeps a stored release binary only for a commit among the 10
nearest first-parent commits of `main`'s head or of an open pull request's
base, the commits the 9-ancestor lookup can reach. A failed delete warns and
never fails the backstop; the job is not in `ci`'s needs.
The test job saves its cache once the workspace build succeeded,
even when a later check fails, and saves nothing when the build fails.
Dev and test builds carry line tables only (`Cargo.toml`, `[profile.dev]`),
which keeps the cache small and the link fast; a backtrace still names file
and line. rust-cache's key leaves out `[profile]`, so a profile change also
changes the `shared-key:` the build cache steps carry (`ci-line-tables`; a
`key:` input is ignored beside it), or the old builds restore as an exact
match and every build recompiles the dependencies.

## The journey suite

After the backstop passes on `main`, and in the nightly before it publishes,
the journey suite drives that commit's release binary through the paths a
person walks (`docs/testing.md`, "Journeys"). A failure opens an issue, or
comments on the open one. The selector considers at most 20 prior runs, newest first. After two or more consecutive failures of the same
journey, the comment appends `Streak: <n> consecutive failures since
<first-sha>.`, counting this run and using the full SHA of the first failing
commit; incomplete history keeps the plain sighting. It never blocks a merge,
and it is not part of the gate. In the nightly, a failure reports and does
not stop the publish.

## Toolchain

`rust-toolchain.toml` pins the exact Rust version, 1.99.1. Local builds and
CI read the same file, and it is the only version Fiber supports
(`docs/dependencies.md`, "Toolchain").

A weekly scheduled job opens a pull request that bumps the pin when a newer
stable release exists. That pull request passes CI like any other. A lint
renamed in the new release fails it, because `unknown_lints` is denied
(`docs/code-quality.md`).

## The model catalog

A workflow runs `cargo xtask models-dev` every night at 03:00 UTC on `main`
and opens a pull request when a committed model list or
`providers/models-corrections.toml` changes (`docs/model-routing.md`,
"What a provider extension declares"). It fetches models.dev's public catalog and needs no secret. Its
pull request passes CI like any other. When an open pull request from an
earlier run exists, the workflow updates that branch instead of opening a
second one. This is the only CI job that fetches over the network outside a
build's dependencies.

## Releases

`.github/workflows/release.yml` runs on a push to `main` that changes the
fiber version in `crates/main/Cargo.toml`. A nightly schedule, or a manual
dispatch of the workflow from main, builds a pre-release once a day, only when
main changed since the last nightly: its skip-when-unchanged preflight
computes the next nightly version, skips when that version's tag already
exists (a scheduled nightly after a manual one the same day), and every later
nightly job runs only when it publishes, reusing the same gate. A nightly build first sets the fiber crate's version, in `crates/main/Cargo.toml` and `Cargo.lock` together, to the nightly version, so the binary it builds reports that version. A `preflight`
job compares the version at the push's base and head, and every release job runs only when
it changed: any other push runs no release. A version decrease, a missing
`CHANGELOG.md` section or an existing `v<version>` fails the run before
anything builds. Before the release workflow publishes anything, it:

- builds the release artifacts from the release commit, with `--locked`,
  one target per runner
- runs the full test suite, with no selection, on all three platforms, with
  binary-level tests run against those exact artifacts, including the built
  binary's fresh-home install of the built docs and extensions archives
- runs cargo-deny's advisory check
- measures the benchmarks on Linux arm64 and macOS arm64 and reports them,
  without gating (`docs/performance.md`)

The Release is created as a draft and published last, with the version's
`CHANGELOG.md` section as its notes; publishing is what creates
`v<version>`, so a failure leaves no tag and no Release.

A rehearsal dispatch (`gh workflow run release.yml --ref main -f rehearse=true`)
runs only the `rehearsal` job: every nightly and stable job skips. The job
proves the draft, upload and publish wiring under the real publish working
directory and environment, against a throwaway pre-release version it deletes,
with its tag, in an always-run cleanup step; it does not prove the archives,
which carry stand-in bytes while the release gate proves the real ones. Only
`main` can run it: the `release` environment deploys only from `main`.

A nightly shares the backstop pushes' concurrency group (`ci-main`). A
nightly and a backstop push serialize in either order: whichever run starts
first finishes before the other starts. The two never overlap while the
nightly runs. GitHub keeps one pending run per group, so a push landing while
a nightly waits replaces the queued nightly. A replaced run reads as
cancelled, never green, so the pause (`docs/workflow.md`, "Merging") does not
end falsely. The next scheduled nightly recovers. A rehearsal dispatch
(`-f rehearse=true`) keeps its own group and never joins the hold. Stable
push releases (the `release-push` group) still overlap nightlies.

### The nightly update check

The `nightly-update-check` job in `.github/workflows/release.yml` proves the
nightly a person on the previous nightly installs: it carries the same publish
gate as the nightly build, installs the previous nightly into a fresh home,
updates to the new nightly, and fails the run on the update's own failure.
The previous nightly comes from the job's own tags: the just-published version
picks its predecessor through `cargo xtask nightly-pair`, and an empty listing
fails the lookup instead of reading as a skip. Fewer than two nightlies fail
the same way: the first nightly has no predecessor to install. The deciding
logic lives in `xtask`, tested like any other code (`docs/testing.md`,
"Levels"): the strict nightly-tag predicate the pair lookup and the fixture
prune share, and the deterministic probe, which re-runs the update against a
nightly-only origin fixture holding only the two captured tags. Unlike the
journey suite (`docs/ci.md`, "The journey suite"), a failure files a
`Nightly update check failed` issue and fails the job. After the merge, the
owner runs the workflow once from `main`
(`gh workflow run release.yml -R aakshintala/fiber --ref main`); a branch
cannot dispatch it.

What the artifacts are, how a release is triggered and how it is signed is
`docs/releasing.md`.

## The user docs site

The site's address is `https://aakshintala.com/fiber/`: GitHub serves the
repository's Pages, with GitHub Actions as the source, under the account's
custom domain. The front page is a product page, built from
`research/site-prototype/`; the user guide sits beside it. The site carries
the rest of `docs/` too, for readers who follow a link into an area doc. Every
docs page renders in the front page's theme, with the same palette and faces.
The front page's "Docs" link opens the rendered guide. The build fails when a
front-page link points at `docs/` on GitHub, or names a page or anchor the
build didn't write.

The site follows releases, not merges. `/fiber/` shows the front page and
`docs/` at the newest stable release tag, and `/fiber/nightly/` shows `docs/`
at the newest nightly tag, under a banner naming the nightly. A deploy
replaces the whole site, so the release job and the nightly job each build
both trees from those two tags and deploy them together, after the
release is published. Until the first stable release, `/fiber/` shows the
front page and the nightly's docs. Nothing else hosts Fiber's documentation.

The command reference in `docs/user/` is generated from the same command
definitions as `fiber help` and shell completion. A test in the `fiber` crate
regenerates it and fails when the committed copy differs.

## Manually dispatched workflows

Four workflows never run on a push or a pull request. Someone starts them with
`gh workflow run <file> --ref <branch>`, and GitHub allows that only once the
workflow file is on `main`.

- `tui-demo.yml` builds the two static Linux binaries of the tui-prototype demo
  (`research/tui-prototype/demo`).
- `dependency-probe.yml` runs a probe on Linux x86_64, Linux arm64 and macOS
  arm64 and uploads one table per runner. Its `probe` input picks
  `dependency-rss` (the default; `docs/dependencies.md`, "Measuring memory")
  or `session-search` (`research/session-search/run.sh`), e.g.
  `gh workflow run dependency-probe.yml --ref main -f probe=session-search`.
- `attach-profile.yml` runs `bench --only timing` on Linux x86_64 under
  `perf record`, with fiber built with symbols and frame pointers, and uploads
  one artifact: `perf.data`, the folded stacks and flamegraph SVGs for every
  process and for the fiber processes alone, the fiber processes' self time per
  function (`fiber-symbols.txt`), and the harness's JSON. Its timings run under perf,
  so they are not budget evidence.
- `build-timings.yml` builds the whole workspace with `--timings` on Linux
  x86_64 and macOS arm64 from a warm dependency cache, times an incremental
  rebuild after touching one file each in `crates/tui` and `crates/support`,
  and uploads both timing reports as artifacts (#3789, step 1).

## Job time limits

Every job sets `timeout-minutes`. A hung runner then fails at the bound, `gh-ci`
reports a failure, and `ci-triage` classifies it as infrastructure instead of
the pull request waiting out GitHub's 6-hour default.

A bound is about twice the job's median duration over its last 20 runs, rounded
up to a round number, and never under 5 minutes, because runner start-up
varies. A job with too little history gets a generous bound. There are two
exceptions. The `bug_red` legs are bounded at about twice their slowest recent
red commit, not their median, because a red commit that changes a signature
rebuilds every package that depends on it. Run 42403359939's red commit touched
ten packages: its Linux leg took 10m48s and its macOS leg was cancelled at the
old 17-minute bound, so both legs get 30 minutes. The other exception
is a mutants shard: a shard of `crates/main` mutants, whose tests drive the
binary, takes about 20 minutes (run 38420177975), so its bound is twice that
slow group, not twice the median. A shard of up to 15 mutants gets 40 minutes.
A larger shard, which only the cap makes, gets 40 minutes per 15 of its
mutants, rounded up, at most 360 minutes, GitHub's limit for a job. The selection
computes it (`cargo xtask plan`). A job that builds the workspace is bounded at
twice its cold-cache duration, since a pull request's first run after a cache
eviction or a `Cargo.lock` change builds cold. The median behind each bound is a comment beside its
`timeout-minutes` line. A job that gains work past its bound has the bound
raised in its workflow. The release gates (`release-gate` and `nightly-gate`)
bound macOS at 50 minutes because a macOS cold build plus the full suite
measures 24-26 minutes (runs 39679547149/39660697905), twice that per the
cold-cache rule; Linux is 30, the backstop test job's bound, because the Linux gate measures 17-20 minutes (runs 40487347149/40677917738/40865045602, the last cancelled by the earlier bound of 20).

Every `apt-get` step sets a 5-minute `timeout-minutes` and passes the workflow's
`APT_OPTS` (three retries, 20-second HTTP and HTTPS timeouts), so a silent mirror
fails the step, not the job. A mirror that trickles bytes never goes silent, so
the "Install zsh and fish" steps and the release job's "Install musl-tools
and time" step run `scripts/install-shells`. In `ci.yml` and
`mutants-scheduled.yml` each of them restores the `.deb` archives for zsh,
zsh-common, fish, fish-common and xsel, with any dependencies downloaded beside
them, from an Actions cache. The key holds the runner image (`ImageOS` and
`ImageVersion`), the architecture and the package list; the install step clears
the cache directory unless the restore reports an exact hit. On a hit the
archives are installed with dpkg from the cached archives, with no network call;
a dpkg failure fails the step, with no fallback to the network. On a miss each
download attempt (`apt-get update` and `apt-get install --download-only`) is
bounded at 90 s and retried after 5 s until it succeeds, apt resuming its
partial downloads, and the step's 5-minute bound ends the retries. After the
install passes, a save step stores the archives. The release job keeps its own
`musl-debs` cache for musl-tools and time, keyed by runner image, architecture
and package list with the same hit/miss semantics. Pull requests save this cache
as well as `main`, unlike the build cache. The `release.yml` steps restore no
cache and always download, with the same retry. The install is not bounded,
because killing dpkg mid-unpack leaves the dpkg lock held.

## Waiting on CI

Every workflow's third-party actions are pinned to a commit hash. The hash is
the latest release of each action's newest major version, which runs on the
Node.js version GitHub currently supports. An action still on a deprecated
Node.js version is moved to its newest major before it is pinned.

No job polls the Actions API in a loop. The backstop's one lookup of the
last passing `main` commit and the release job's stored-results lookup (one
artifact listing per commit, at most 10, and one download) are the Actions
API calls a workflow makes to find a run; the backstop's cache pruning also
lists and deletes cache entries.
Agents wait on the `CI` check with `gh-ci`, never with a `gh run watch`
loop, because such loops have tripped GitHub's Actions rate limit. `gh-ci` is
a small `gh` wrapper that waits with few API calls; it lives in the owner's
agent tooling ([switchyard](https://github.com/aakshintala/switchyard)), not
in this repository.
