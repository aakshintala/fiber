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
verdict job's `needs`, except the jobs that report and gate nothing:
`backstop_report`, `bench_comment`, `cache_prune`. They run outside the verdict, so their
result never decides `CI`. The docs check fails when any other job is
missing from `needs`, so a new job, such as a new shard, cannot run
without gating the merge.

The selection is one job that every other job waits for, so it does as
little as it can: it works out the selection and, on a pull request, runs
the docs check, below.

A branch does not have to be up to date with `main` to merge, and there is no
merge queue. The backstop on `main` catches two pull requests that each
passed alone but break together.

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
  whole run. A file a crate compiles in is the exception, below. A file
  under `providers/` or `extensions/` is the other exception: it runs the
  package readers, below, even when it is Markdown.
- A diff that changes `Cargo.lock`, any `Cargo.toml`, `rust-toolchain.toml`,
  anything under `.github/` or `scripts/`, `clippy.toml`, `deny.toml`,
  `.cargo/config.toml` or `.config/nextest.toml` runs everything. These
  files are matched outside `research/`, which is not part of the
  workspace: a manifest there is a docs file.
- A file a crate compiles in runs that crate alone, not the crates that
  depend on it. This covers Markdown anywhere, and any file outside the
  crate. A change to `docs/events.md`, `docs/errors.md`,
  `docs/invocation.md` or `docs/tui.md` runs `contract`, whose tests check
  the code against them. A change to a built-in skill under `docs/skills/` runs `loop`, whose
  tests check it parses. The selector lists these files. The gate fails when
  the list and the source disagree, or when an include's argument is not a
  string literal.
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

The selector has its own tests.

## On every pull request that changes code

On a ready pull request, one job on each of Linux x86_64, Linux arm64 and
macOS arm64 runs `scripts/check`, the gate CI applies (`docs/workflow.md`,
"The gate"). A draft pull request runs only the Linux x86_64 leg. Each
job:

- compiles the selected crates with the debug profile, every target,
  through clippy and nextest; the whole workspace is built by the backstop
  ("The backstop on `main`")
- runs clippy with the workspace lints across all targets, so code compiled
  only for one platform is linted on that platform
- runs the selected tests under nextest, and doc-tests with
  `cargo test --doc`
- reports how many tests ran

The debug profile sets `panic = "abort"`, as the release profile does, so
the `fiber` binary that binary-level tests start behaves like the one that
ships. Cargo ignores the setting when it builds the test harness.

A failed binary-level test retries once. A pass on retry does not fail the
run: CI opens a flake issue naming the test, or comments on the open one
(`docs/testing.md`, "Flaky tests"). No other test retries.

On Linux x86_64 alone:

- `cargo fmt --check`
- the non-test source files over 800 lines are listed (`cargo xtask
  line-cap`); the list never fails the run (`docs/code-quality.md`, "Size")
- a process signal appears only in the guarded helpers (`cargo xtask signal-sites`)
- the compiled-in list matches the files crates compile in, Markdown
  anywhere or any file outside the crate, and every include argument is a
  string literal
- no crate but `picture` and `main` has `image` or `fast_image_resize` in its
  normal dependency tree (`cargo xtask image-isolation`), so the session
  process links no image code
- no crate but `tui` and `main` has `ratatui` or `crossterm` in its normal
  dependency tree (`cargo xtask tui-isolation`), so no other crate depends on
  a crate admitted only for the terminal
- `shellcheck --shell=sh` on `scripts/install.sh` and its test stubs
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
  the label starts a run. Shards start once the `test` job passes on every
  platform the run tests (a job can wait for another job, not one platform's
  leg of it),
  so a run whose tests fail starts none, and a run that selects no tests
  runs no mutants. The selection counts the diff's mutants
  (`cargo mutants --list`) and plans one shard per 15 of them, rounded up, at
  most 16; a diff with no mutants starts none. Recent runs (#1461, #1395)
  tested a mutant in 13 to 27 seconds on a runner and the unmutated baseline
  took 1 to 3.5 minutes, so 15 mutants keep a shard near 10 minutes. Shards
  split the list round-robin, so every shard holds the same mix of crates and
  the count alone sets its size. Past 240 mutants the cap makes shards larger,
  and their time limit grows with them ("Job time limits"). Two pull requests at the cap
  leave 8 of the 40 jobs for everything else.
  Mutants run under nextest's `mutants` profile (`.cargo/mutants.toml`),
  which stops every running test at the first failure. A mutant that makes
  one test hang until its deadline is then caught by a faster test, not
  reported as a timeout.
- for a pull request where any issue its body resolves is labelled
  `bug`, its new and changed tests run at its first commit, the red commit,
  and at its head: the red commit must build and at least one of them must
  fail there, and all must pass at the head (`docs/workflow.md`, "The pull
  request"). A pull request whose every changed file is a docs file, as
  "Selection" defines it, passes the check with a message saying so: the
  doc was wrong and the code was right, so no test can show the bug.

One more Linux x86_64 job builds the release profile for the target that
ships, `x86_64-unknown-linux-musl` (`docs/releasing.md`), at the pull
request's head and at its base commit (`scripts/release-size`). The base
binary comes from the build cache when the backstop stored one for that
commit, and is built otherwise. The scripts that build and measure both
binaries are the head's, so a base older than a script still compares. It checks that the stripped head binary is
under 20 MiB and runs the benchmarks that gate each pull request
(`docs/performance.md`). The base binary runs the same benchmarks in the
same job on the same runner: a timing gate compares against it, and a memory
or exact budget the base fails too does not fail the pull request.

A base that does not build, because `main` is red, does not fail the job. The
job compares against the nearest first-parent ancestor of the base that has a
stored binary, looked up among the 9 nearest ancestors. If none has one, it
skips the base comparison and the timing gates. In both cases the job
summary names the base, says it does not build, and names the commit it
compared against or says the comparison was skipped. The head's size check and
benchmarks still gate the pull request, and a head that does not build still
fails the job. The paging jig and the benchmarks follow the same rule: the jig is built at
the commit the base binary is from, and a base with no binary has no base
timings, and the comment says so.

The release profile sets `lto = "fat"` and `codegen-units = 1`, which
shrinks the binary and lengthens the release build.

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
(`cargo xtask unsafe-table`) and that every crate a `Cargo.toml` names is
listed in `docs/dependencies.md` (`cargo xtask dependency-list`). Each
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
and stores that commit's stripped binary in the build cache, for a later
pull request's base.

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

## Toolchain

`rust-toolchain.toml` pins the exact Rust version, 1.99.0. Local builds and
CI read the same file, and it is the only version Fiber supports
(`docs/dependencies.md`, "Toolchain").

A weekly scheduled job opens a pull request that bumps the pin when a newer
stable release exists. That pull request passes CI like any other. A lint
renamed in the new release fails it, because `unknown_lints` is denied
(`docs/code-quality.md`).

## Releases

Before the release workflow publishes anything, it:

- builds the release artifacts from the release commit
- runs the full test suite, with no selection, on all three platforms, with
  binary-level tests run against those exact artifacts
- runs cargo-deny's advisory check
- measures the benchmarks on Linux arm64 and macOS arm64 and reports them,
  without gating (`docs/performance.md`)

What the artifacts are, how a release is triggered and how it is signed is
`docs/releasing.md`.

## The user docs site

A workflow publishes `docs/user/` to GitHub Pages on every merge to `main`.
The site's address is `https://aakshintala.github.io/fiber/`.
The site carries the rest of `docs/` too, for readers who follow a link into
an area doc. Nothing else hosts Fiber's documentation. The command reference
in `docs/user/` is generated from the same command definitions as `fiber help`
and shell completion, and the docs check fails when the committed copy differs
from what the generator writes.

## Manually dispatched workflows

Two workflows never run on a push or a pull request. Someone starts them with
`gh workflow run <file> --ref <branch>`, and GitHub allows that only once the
workflow file is on `main`.

- `tui-demo.yml` builds the two static Linux binaries of the tui-prototype demo
  (`research/tui-prototype/demo`).
- `dependency-probe.yml` runs a probe on Linux x86_64, Linux arm64 and macOS
  arm64 and uploads one table per runner. Its `probe` input picks
  `dependency-rss` (the default; `docs/dependencies.md`, "Measuring memory")
  or `session-search` (`research/session-search/run.sh`), e.g.
  `gh workflow run dependency-probe.yml --ref main -f probe=session-search`.

## Job time limits

Every job sets `timeout-minutes`. A hung runner then fails at the bound, `gh-ci`
reports a failure, and `ci-triage` classifies it as infrastructure instead of
the pull request waiting out GitHub's 6-hour default.

A bound is about twice the job's median duration over its last 20 runs, rounded
up to a round number, and never under 5 minutes, because runner start-up
varies. A job with too little history gets a generous bound. The one exception
is a mutants shard: 8 % of shards take 9 to 12 minutes when a pull request
changes a widely used function, so its bound is twice that slow group, not
twice the median. A shard of up to 15 mutants gets 20 minutes. A larger
shard, which only the cap makes, gets 20 minutes per 15 of its mutants,
rounded up, at most 360 minutes, GitHub's limit for a job. The selection
computes it (`cargo xtask plan`). A job that builds the workspace is bounded at
twice its cold-cache duration, since a pull request's first run after a cache
eviction or a `Cargo.lock` change builds cold. The median behind each bound is a comment beside its
`timeout-minutes` line. A job that gains work past its bound has the bound
raised in its workflow.

Every `apt-get` step sets a 5-minute `timeout-minutes` and passes the workflow's
`APT_OPTS` (three retries, 20-second HTTP and HTTPS timeouts), so a silent mirror
fails the step, not the job. A mirror that trickles bytes never goes silent, so
the "Install zsh and fish" steps run `scripts/install-shells`: each download
attempt (`apt-get update` and `apt-get install --download-only`) is bounded at
90 s and retried once after 5 s, and the install from the downloaded archives is
not bounded, because killing dpkg mid-unpack leaves the dpkg lock held.

## Waiting on CI

Every workflow's third-party actions are pinned to a commit hash. The hash is
the latest release of each action's newest major version, which runs on the
Node.js version GitHub currently supports. An action still on a deprecated
Node.js version is moved to its newest major before it is pinned.

No job polls the Actions API in a loop. The backstop's one lookup of the
last passing `main` commit is the only Actions API call a workflow makes to
find a run; the backstop's cache pruning also lists and deletes cache entries.
Agents wait on the `CI` check with `gh-ci`, never with a `gh run watch`
loop, because such loops have tripped GitHub's Actions rate limit. `gh-ci` is
a small `gh` wrapper that waits with few API calls; it lives in the owner's
agent tooling ([switchyard](https://github.com/aakshintala/switchyard)), not
in this repository.
