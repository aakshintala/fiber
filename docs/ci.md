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
by every repository on the account. A draft pull request runs only the
Linux x86_64 leg, so its run uses no macOS job. A ready pull request's run
uses one macOS job, so five ready pull requests can run before a sixth
queues for macOS. The 40-job cap is the one CI reaches: from 2026-10-07 to
2026-10-09 every job that queued for more than 15 minutes waited while 38 or
more jobs ran, and the macOS cap held jobs back for 85 of 3,250 queued
minutes. A change that saves jobs, such as fewer mutants shards, shortens the
queue; one that saves only macOS jobs does not.

Every runner has a C compiler, which `mlua` needs to build Lua
([ADR 0006](adr/0006-extension-runtime-lua.md)).

## The merge gate

One check is required to merge: `CI`. It passes only when every job the
selection chose succeeded and every job it did not choose was skipped. If
the selection itself fails, `CI` fails. On a draft pull request the verdict
job reports as `CI (draft)`, so the required `CI` check stays pending until
the pull request is marked ready, which starts the full run.

Every job in `.github/workflows/ci.yml` but the verdict job is in the
verdict job's `needs`, except the jobs that report without gating it
directly: `backstop_report`, `bench_comment`, `cache_prune`, `flake_decide`,
`flake_report`, `bug_red_linux`, `bug_red_macos`. The bug-red legs report
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

A newer push to a pull request cancels that pull request's older run. A
label, a change from draft to ready or a reopen at the same head cancels
nothing: the run in flight finishes, then the new event's run, which waited,
runs the whole selection again at the same head. At most one run waits, and
a newer event replaces it before it starts. Rerunning everything keeps the
`CI` check, which is the latest run's, from hiding a failure in the earlier
run. Pushes
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
  own tests.
- Every diff that runs the binary-level tests also runs the release-profile
  job, `release` ("On every pull request that changes code"), and `fiber extension test`
  for every first-party package that has cases, not only the changed ones
  ("Testing an extension"). A change to Fiber's own code can break a package
  that did not change.
- A diff that touches the code the release job runs or the table it judges
  against, `crates/bench`, `xtask/src/bench*`, `scripts/release-size`,
  `scripts/stored-results` or `docs/performance.md`, also runs `release`, so
  a broken harness or judge fails its own pull request rather than the
  backstop.
- A diff that touches `crates/loop`, `crates/bench` or `xtask/src/bench*` also runs the "Session, memory kept after 10 busy turns" workload (`docs/performance.md`) in `release`. Any other pull request skips it, and the job summary says so. The loop holds the session's state, so a diff elsewhere rarely moves the row, and the backstop catches one that does.
- On a pull request, the head and base binaries run only the timing rows the diff selects (`docs/performance.md`). A diff touching one of a row's crates runs it, and the job summary names each skipped row on both passes. `session start` runs on `crates/contract`, `crates/support`, `crates/log`, `crates/loop`, `crates/doors`, `crates/provider`, `crates/routing`, `crates/tools`, `crates/mcp`, `crates/jobs`, `crates/net`, `crates/worktree`, `crates/extensions`, `crates/config`, `crates/hub` or `crates/main`; `sessions list` on `crates/config`, `crates/cli`, `crates/doors`, `crates/hub`, `crates/contract`, `crates/support`, `crates/log`, `crates/worktree` or `crates/main`; the terminal rows (`terminal first frame`, `terminal attach`, `terminal wheel`) on the session-start crates plus `crates/tui` or `crates/cli`; `paging` on `crates/tui`, `crates/contract`, `crates/support` or `crates/log`; `sandbox setup` on `crates/tools/`. Every other path runs every row, and a `.md` file under `crates/` counts as code. A diff touching only `docs/performance.md` selects no row, since the selector treats a docs file as selecting nothing: none of the seven timing rows is measured on either pass, and the pull request is gated by the table checks and the `main` backstop alone. A regression that reaches a row through a dependency the mapping misses surfaces at the backstop, the same as the 10-busy-turns row above.

The selector has its own tests.

## On every pull request that changes code

On a ready pull request, one job on each of Linux x86_64, Linux arm64 and
macOS arm64 runs `scripts/check`, the gate CI applies (`docs/workflow.md`,
"The gate"). A draft pull request runs only the Linux x86_64 leg, because `cargo xtask plan` leaves its `test` job unselected. Each
job:

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

The debug profile sets `panic = "abort"`, as the release profile does, so
the `fiber` binary that binary-level tests start behaves like the one that
ships. Cargo ignores the setting when it builds the test harness.

A failed binary-level test retries once. A pass on retry does not fail the
run, except for a test the pull request touched (below): CI opens a flake
issue for the test's binary, `Flaky tests: <binary>`, or comments on the open one
(`docs/testing.md`, "Flaky tests"). The retry is nextest's `ci` profile over
the `crates/main/tests` binaries, and two report jobs file the issue. The
read-only `flake_decide` job holds only `contents: read` and `issues: read`:
it downloads each selected test leg's exact `flaky-tests-<runner>.txt`
artifact, failing closed on a missing artifact, a false or missing
per-producer completion marker, or a cancelled or skipped selected leg
(an empty list with a true marker is a complete empty result), then looks
up the complete open and closed issues, asks
`cargo xtask flake-report-events` which issue numbers need event fetches,
fetches only those endpoints, and runs
`cargo xtask flake-report --tested-commit` to decide one JSON-lines action
per test binary, probing ancestry itself only for each selected
latest fix. It then runs `cargo xtask flake-classify`, which reads the
runner lists and the two file lists git diffs from the merge base, and
writes one classification line per test binary: a stale base, a
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
profile, which never retries; 100 times when the pull request fixes a
flaky test. One failure fails the run. The retry above applies only to
tests the pull request did not touch.

The `stress` job runs on a ready pull request on Linux x86_64 beside the
test jobs and builds from the same restored cache. The tests it runs are
the test functions the diff adds or changes, found by
`cargo xtask stress-plan` with the same test-file rules as the red-commit
check, and a diff that touches no test function skips the run. The count
is read from the linked issue's labels. The job is in `ci`'s `needs`, so
one failure stops the merge. It writes its wall
time, runner minutes and selected tests to its job summary. A touched
test still gets nextest's one retry but a pass on that retry fails the
test job (`cargo xtask touched-retries`).

The Linux x86_64 full-suite run uploads its JUnit XML as the `junit`
artifact, kept 7 days. It holds only XML this run wrote: the full-suite
`scripts/check` output after retry handling, uploaded before the focused
hub_service run overwrites `target/nextest/ci/junit.xml`.

On Linux x86_64 alone:

- `cargo fmt --check`
- a process signal appears only in the guarded helpers (`cargo xtask signal-sites`); the check scans Rust tokens, so comments never count, and its allowlist is a trust list: the files it names may send signals, every other file may not
- no test file holds a wait bound of 1 s up to `fakes::MUST_SUCCEED_WITHIN`, in a `const`, a `Deadline::after`, a wait call such as `await_requests`, a `.min` cap or a socket timeout setter (`cargo xtask must-succeed`, `docs/testing.md`, "Waits and timeouts"); the check scans Rust tokens, so comments never count, and its allow-list names only each kept bound (a poll slice, a fake-clock grace or step, a bound the code under test reads, a "nothing arrives" check); it reports each site by file path, then line
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
- no crate enables serde_json's `preserve_order` feature in any crate linked into a workspace binary or test, over every target and feature; build-dependency edges are excluded (`cargo xtask
  preserve-order`), so prompt bytes stay cacheable (`docs/prompt-cache.md`)
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` in
  `research/tui-prototype`, when the diff touches it. The prototype is not a
  workspace member, so the checks above never reach it; it has its own
  `clippy.toml`, which drops the clock bans because the prototype reads the
  wall clock. A diff that touches `research/tui-prototype/` selects `lint`,
  whether or not it touches anything else; any other `research/` path alone
  selects the docs job alone
- `shellcheck --shell=sh` on `scripts/install.sh` and its test stubs
- `shellcheck --shell=bash` on every other `scripts/` shell file: `bug-red`,
  `cache-prune`, `check`, `check-docs`, `flaky-tests`, `install-shells`,
  `leak-scan`, `paging-jig`, `release-pack`, `release-size`,
  `stored-results`,
  `test-bug-red`, `test-bug-red-cargo`, `test-flake-report`, `test-flaky-tests`, `test-leak-probe`,
  `test-leak-scan`, and `lib/release.sh`
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
  the label starts a run. Shards start once the Linux x86_64 test job, its own
  job `test_linux` (a job can wait for another job, not one platform's leg of
  it), passes on the head; the arm64 and macOS legs, the `test` job, run in
  parallel and still gate `CI`. Each shard runs `cargo mutants --baseline=skip
  --timeout 120`, since the tests it would rerun as a baseline just passed on
  this head. A systemd user scope caps the run at 12 GiB, leaving 4 GiB of the
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
  generating mutants across every workspace package. It runs one full-workspace
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
  The two red-test legs report nextest's status class to a collector, which
  keeps one required check named `Bug fix fails at the red commit`
  (`docs/workflow.md`, "The pull request"). A pull request whose every
  changed file is a docs file, as "Selection" defines it, passes the check
  with a message saying so: the doc was wrong and the code was right, so no
  test can show the bug. The label is read when the run's selection job
  runs, so a relabel takes effect at the next push or a whole-run rerun;
  rerunning only the failed job keeps the old reading.
- each profiling feature built once, without running it:
  `cargo check -p main --bins --tests --features alloc-stats` and
  `cargo check -p main --bins --tests --features dhat-heap`, so neither
  stops compiling unnoticed. The job's `timeout-minutes` is 10 to leave
  room for the two uncached checks alongside `scripts/check`.

One more Linux x86_64 job builds the release profile for the target that
ships, `x86_64-unknown-linux-musl` (`docs/releasing.md`), at the pull
request's head and at its base commit (`scripts/release-size`). The base
binary comes from the build cache when the backstop stored one for that
commit, and is built otherwise. The scripts that build and measure both
binaries are the head's, so a base older than a script still compares. It checks that the stripped head binary is
under 25 MiB and runs the benchmarks that gate each pull request, the rows the selection above chooses (the 10-busy-turns workload only when the selection chooses it),
(`docs/performance.md`), timing-only workloads and the `paging` jig at 3 runs and the rest at 5. The head and base
binaries run only the timing benchmarks the selection chooses, in the same job on the same runner, so a
timing comparison never crosses runners: one harness process alternates head and base timings,
by sample for every timing row. When a gated timing row breaches, the job runs a second harness process for that row alone, 4 more samples per side, and judges all samples. The `terminal attach` row and `session_list_ms` never remeasure: the first already takes 5 samples and the second maps to no row. A row the selection does not choose runs on neither side while the budgets it feeds read skipped. Memory and
exact rows no skipped row feeds still run on the head and compare
against the backstop's stored results for the base commit, the
`benchmarks-<sha>` artifact, and a memory or exact budget the base fails
there does not fail the pull request.

A base that does not build, because `main` is red, does not fail the job. The
job compares against the nearest first-parent ancestor of the base that has a
stored binary, looked up among the 9 nearest ancestors. If none has one, it
skips the base comparison and the timing gates. In both cases the job
summary names the base, says it does not build, and names the commit it
compared against or says the comparison was skipped. The head's size check and
benchmarks still gate the pull request, and a head that does not build still
fails the job. The paging jig and the benchmarks follow the same rule: the jig is built at
the commit the base binary is from, and a base with no binary has no base
timings, and the comment says so. Stored results follow the same lookup;
with none among the 9 ancestors, memory and exact rows are judged against
the table alone, and the comment says so.

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
- a backticked path under `docs/`, `crates/`, `scripts/`, `research/` or
  `.github/` does not exist. A path with a placeholder in it, such as
  `docs/<area>.md`, is not checked.
- a job in `.github/workflows/ci.yml` other than the verdict job and the
  report jobs is missing from the verdict job's `needs`, or the report
  jobs named in "The merge gate" differ from the check's own list
  (`cargo xtask ci-needs`)

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

## The backstop on `main`

Every push to `main` runs the backstop. It runs the lint, test and
release jobs the selection chooses from the diff
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
that cancels nothing, so the running backstop finishes, which lets `main` record
a pass during a stream of merges, and GitHub replaces a waiting backstop with
the newest push. The newest backstop's selection covers every commit it
replaced, because it diffs from the last passing commit. A replaced commit
stores no release binary; a pull request based on one compares against the
nearest ancestor that has one ("On every pull request that changes code").

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

## Toolchain

`rust-toolchain.toml` pins the exact Rust version, 1.99.1. Local builds and
CI read the same file, and it is the only version Fiber supports
(`docs/dependencies.md`, "Toolchain").

A weekly scheduled job opens a pull request that bumps the pin when a newer
stable release exists. That pull request passes CI like any other. A lint
renamed in the new release fails it, because `unknown_lints` is denied
(`docs/code-quality.md`).

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

Three workflows never run on a push or a pull request. Someone starts them with
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

## Job time limits

Every job sets `timeout-minutes`. A hung runner then fails at the bound, `gh-ci`
reports a failure, and `ci-triage` classifies it as infrastructure instead of
the pull request waiting out GitHub's 6-hour default.

A bound is about twice the job's median duration over its last 20 runs, rounded
up to a round number, and never under 5 minutes, because runner start-up
varies. A job with too little history gets a generous bound. The one exception
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
the "Install zsh and fish" steps run `scripts/install-shells`. In `ci.yml` and
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
install passes, a save step stores the archives. Pull requests save this cache
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
artifact listing per commit, at most 10, and one download) are the only
Actions API calls a workflow makes to find a run; the backstop's cache pruning
also lists and deletes cache entries.
Agents wait on the `CI` check with `gh-ci`, never with a `gh run watch`
loop, because such loops have tripped GitHub's Actions rate limit. `gh-ci` is
a small `gh` wrapper that waits with few API calls; it lives in the owner's
agent tooling ([switchyard](https://github.com/aakshintala/switchyard)), not
in this repository.
