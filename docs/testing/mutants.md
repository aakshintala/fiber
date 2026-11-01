# Proving a test bites

How a test is shown to fail when the behaviour it guards breaks. It is part of `docs/testing.md`.

## Proving a test bites


A test that passes with its feature removed proves nothing. CI checks this on
every pull request with mutation testing: `cargo-mutants --in-diff` makes small
breaking edits to the code the diff changed, such as replacing a function body
with a default or flipping a comparison, and runs the mutated crate's tests
against each one. An edit that no test notices fails CI. Only the mutated
crate's own tests run, so a test in another crate, the binary's included,
kills none of its mutants: each behaviour a crate exposes is tested in
that crate. Selection writes the
diff's full mutant list to its job summary on every pull request event, drafts
included, and prints it to the job's log
(readable as soon as Selection finishes, before the run ends):
`gh api --allow-escape-sequences repos/aakshintala/fiber/actions/jobs/<id>/logs`,
with the id from `gh run view <run> -R aakshintala/fiber --json jobs`
(`gh run view --log` waits for the whole run; the run's summary page shows the
list in a browser), so a lane maps each mutant to a test before CI runs the shards.

A function is extracted and tested alone only where a surviving mutant would
signal the wrong process or lose data, such as the `kill(-1)` refusal
(`docs/testing/running.md`, "Running tests"). Elsewhere a mutant that changes no behaviour a consumer
sees takes an exemption in the code, with a written reason, instead of a test
that pins it. The exemption goes on a function, since an exemption on a
statement or block is not reliably honoured. It is written
`#[cfg_attr(false, mutants::skip)]`, with the reason in a comment above it:
cargo-mutants silently ignores the attribute when it carries a `reason`
argument. How many runners share the
mutants is CI's to set.

Mutants run on Linux, so code compiled only on macOS, or on any platform other
than Linux, is never built there, and every mutant of it would pass unnoticed.
A function gated that way is exempted the same way, with the reason that
mutants run on Linux, where it is not compiled. The macOS test job still runs
it, so its tests must reach it.

Code is written so a mutant fails fast. A loop that steps an index by hand can
spin forever when a mutant breaks the arithmetic, and a hung mutant fails CI as
a timeout; walk with an iterator instead. Code also avoids forms that leave a mutant no
test can catch. Disjoint flags are combined with a method such as `union`,
not `|`: cargo-mutants swaps `|` for `^`, which gives the same value when no
bit overlaps. A test reaches the code under test
from outside, through a test seam such as a fake writer or the injected clock. A
`#[cfg(test)]` hook inside a production function is used only where no outside
seam can reach the behaviour, such as a race's pause point (`docs/testing.md`,
"Waits and timeouts"), and the plan that needs one says why. Integration tests under
`tests/` reach such a hook because it compiles whenever `debug_assertions`
is on, as in every dev and test build, while `[profile.release]` leaves
it off, so release builds never compile it.

cargo-mutants stops each mutant's test run after 120 s, set with `--timeout 120`
in `.github/workflows/ci.yml` (`mutants`). A mutant stopped there fails CI as a
timeout, not as caught. A mutant can turn a wait that returns
early into one that runs out its whole bound, as a flipped comparison in a
`wait_timeout_while` predicate does, so the deadlines one test can run out in
turn must add up to well under that timeout.

A bug fix must also show that its test reproduces the bug. A pull request
where any issue its body resolves is labelled `bug` starts with a red commit:
the reproducing test and any new signature or test seam it needs, without the
fix (`docs/workflow.md`, "The pull request"). CI runs the tests the red commit adds or changes at the red commit, and
every test the pull request adds or changes at the head. The red commit must
build, at least one of its tests must fail there, and all of them must pass
at the head.
A pull request whose every changed file is a docs file, as `docs/ci.md`, "Selection",
defines it, passes the check with a message saying so: the doc was wrong and
the code was right, so no test can show the bug.

Diff-scoped mutation testing has two known limits. It cannot see a change in
one place leaving other code under-tested. A pull request that changes only
test code runs no mutants, so a rewritten test could lose its teeth unnoticed
until the next code change in that crate runs mutants against it.
