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
by every repository on the account. Each run uses one macOS job, so five
pull requests can run before a sixth queues for macOS.

Every runner has a C compiler, which `mlua` needs to build Lua
([ADR 0006](adr/0006-extension-runtime-lua.md)).

## The merge gate

One check is required to merge: `CI`. It passes only when every job the
selection chose succeeded and every job it did not choose was skipped. If
the selection itself fails, `CI` fails.

The selection is one job that every other job waits for, so it does as
little as it can: it works out the selection and, on a pull request, runs
the docs check, below.

A branch does not have to be up to date with `main` to merge, and there is no
merge queue. The backstop on `main` catches two pull requests that each
passed alone but break together.

A newer push to a pull request cancels that pull request's older run.

## Selection

A pull request runs only what its diff can affect.

- A diff in which every file is Markdown, or is under `docs/` or
  `research/`, runs no job after the selection, whose docs check is the
  whole run. A file a crate compiles in is the exception, below. A file
  under `providers/` or `extensions/` is the other exception: it runs the
  package readers, below, even when it is Markdown.
- A diff that changes `Cargo.lock`, any `Cargo.toml`, `rust-toolchain.toml`,
  anything under `.github/` or `scripts/`, `clippy.toml`, `deny.toml`,
  `.cargo/config.toml` or `.config/nextest.toml` runs everything.
- A file a crate compiles in runs that crate alone, not the crates that
  depend on it. This covers Markdown anywhere, and any file outside the
  crate. A change to `docs/events.md`, `docs/errors.md` or
  `docs/invocation.md` runs `contract`, whose tests check the code against
  them. A change to a built-in skill under `docs/skills/` runs `loop`, whose
  tests check it parses. The selector lists these files. The gate fails when
  the list and the source disagree, or when an include's argument is not a
  string literal.
- A diff under `providers/` or `extensions/` runs the binary-level tests,
  every crate whose tests read a first-party package, and
  `fiber extension test` for each changed package that has cases ("Testing
  an extension"). The selector lists the crates that read packages. The
  gate fails when the list and the source disagree.
- Any other diff runs the crates it touches and every crate that depends on
  them, read from the workspace's dependency graph
  ([ADR 0002](adr/0002-module-boundaries-are-crate-boundaries.md)).
  Binary-level tests depend on every crate that ships, so any change to one
  runs them. `xtask` ships in no binary, so a change to it alone runs its
  own tests.

The selector has its own tests.

## On every pull request that changes code

On each of Linux x86_64, Linux arm64 and macOS arm64, one job runs
`scripts/check`, the same command an implementer runs before every push to
a pull request (`docs/workflow.md`, "The gate"). It:

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
- no non-test source file over 800 lines
- the `unsafe` table in `docs/code-quality.md` matches the code
- a process signal appears only in the guarded helpers (`cargo xtask signal-sites`)
- the compiled-in list matches the files crates compile in, Markdown
  anywhere or any file outside the crate, and every include argument is a
  string literal
- every crate a `Cargo.toml` names is listed in `docs/dependencies.md`
- no crate but `picture` and `main` has `image` or `fast_image_resize` in its
  normal dependency tree (`cargo xtask image-isolation`), so the session
  process links no image code
- no crate but `tui` and `main` has `ratatui` or `crossterm` in its normal
  dependency tree (`cargo xtask tui-isolation`), so no other crate depends on
  a crate admitted only for the terminal
- cargo-deny's licence, source and ban checks
- the built-in tool definitions within their byte budget, with each
  definition's size printed
- mutation testing: `cargo-mutants --in-diff`, split across 6 runners. Each
  lists its own share of the diff's mutants and stops when it has none. The
  number was picked, not measured; it is reset from the first real runs.
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
request's head and at its base commit. It checks that the stripped head binary is
under 20 MiB and runs the benchmarks that gate each pull request
(`docs/performance.md`). A timing gate compares against the base binary
measured in the same job on the same runner.

Nothing in CI writes a snapshot, calls a live provider or reaches the public
network (`docs/testing.md`).

## The docs check

`scripts/check-docs` checks `docs/`, `GLOSSARY.md`, `AGENTS.md` and
`README.md`, offline. It fails when:

- a relative Markdown link, or its `#anchor`, does not resolve
- a section citation, such as `` (`docs/workflow.md`, "The gate") ``, names a
  heading the file does not have, including a citation broken across lines
- a backticked path under `docs/`, `crates/`, `scripts/`, `research/` or
  `.github/` does not exist. A path with a placeholder in it, such as
  `docs/<area>.md`, is not checked.

It runs in the selection job on every pull request, not only docs-only
ones, because the change that breaks a citation is usually a code change
that renames or deletes what a doc points at. External URLs are not checked: nothing in CI reaches the
public network.

## Advisories

cargo-deny's advisory check blocks a pull request that changes `Cargo.lock`,
and blocks a release. It also runs daily on `main` and opens an issue, or
comments on the open one, when an advisory applies. A pull request that does
not change `Cargo.lock` is not failed by an advisory published after it was
opened.

## The backstop on `main`

Every push to `main` runs the backstop on all three platforms. It compiles
the whole workspace and runs the tests the selection chooses from the diff
since the last `main` commit whose backstop passed. That is the parent
commit unless a run was cancelled or failed. A conflict between two merged
pull requests shows in a crate that depends on what the later one changed,
and the selection includes that crate.

When the backstop fails, it opens an issue, or comments on the open one. It
never blocks a merge.

The backstop is the only run that saves the build cache. Pull requests
restore it and never write it, so branches do not fill the repository's
10 GB cache.

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

## Waiting on CI

Every workflow's third-party actions are pinned to a commit hash. The hash is
the latest release of each action's newest major version, which runs on the
Node.js version GitHub currently supports. An action still on a deprecated
Node.js version is moved to its newest major before it is pinned.

No job polls the Actions API in a loop. The backstop's one lookup of the
last passing `main` commit is the only Actions API call a workflow makes.
Agents wait on the `CI` check with `gh-ci`, never with a `gh run watch`
loop, because such loops have tripped GitHub's Actions rate limit.
