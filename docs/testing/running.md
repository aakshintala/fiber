# Running tests

How the suite is run, and how a flaky test is worked. It is part of `docs/testing.md`.

## Running tests


Tests run under cargo-nextest. Each test runs in its own process and is killed
past its timeout. `cargo test` is not a substitute: it runs a crate's tests as
threads of one process, and a child process one test spawns holds the file
locks another test holds until the child execs, so lock-release and
extension-load tests fail there and pass under nextest. A process per test
does not contain what the test starts: every test that starts a process, at
any level, runs it in its own process group, and reaps the group on every
path, including failure and timeout, with SIGKILL through the guarded helper
in `fakes` (`fakes::process_group::kill_group`, `Watchdog`), because the
fixtures ignore SIGTERM on purpose (`docs/testing/fakes.md`, "Fakes"). A binary-level test's
watchdog, started beside Fiber, kills Fiber's process group when the test
process dies. A shell test's fixtures stop themselves (a stop file, 120 s at most) and the test signals no process.
A filter that matches no
tests fails: nextest exits 4 with "no tests to run", where `cargo test` prints
"0 passed" and exits 0. Doc-tests run under `cargo test --doc`.

`scripts/check` exports `FIBER_CHECK_RUN` for the run, and `scripts/leak-scan`
fails the run when a process still carries it after the tests, listing each
leak's PID and command. The scan only reports: reaping stays the test's job,
above.
A test that clears a child's environment puts `FIBER_CHECK_RUN`
back (`fakes::check_run`), so `fiber` and the processes it starts carry
the nonce. The exception is the case processes of `fiber extension test`,
which Fiber starts with a cleared environment. A shell-tool command gets
the nonce through the session's environment: `fiber ask` passes its
caller's, and a hub a test starts gets no `SHELL`, so it uses its
inherited environment (`docs/invocation.md`, "A session's environment").
When the selection includes `main`, `scripts/test-leak-probe` leaks a
process through the shell tool under `fiber ask`, with its own nonce, and
requires the scan to name its PID.
On macOS `ps` withholds the environment of some processes, such as
`sh` and `sleep`, so the nonce scan cannot name them. After it the scan
reads the user's processes reparented to init that started after
`scripts/check` began: one whose executable is under this worktree's
`target/` fails the check and is named, and one whose command is `sh`,
`bash` or `sleep` is listed in a warning that says it may belong to another
run, without changing the exit status. Neither case sends a signal. An
orphan that is neither is still invisible on macOS: it is caught by the
test's own reap above, not the scan.

The completion tests need bash, zsh and fish on PATH, on Linux and macOS: they never skip at runtime for a missing shell, so a machine without one fails them.

A test that runs a command, directly or through a scripted tool call, uses
only the shells above and the standard Unix tools every runner has, such as
`grep`, `sed` and `awk`. `rg`, `jq` and other tools a developer installs are
absent on the Linux runners, and a scripted call to one fails there.

A test never depends on the length of a temporary path: macOS's are long and
Linux's short. A test that needs a line cut makes the line long enough to cut
on any temporary path.

The tests need a user other than root: root bypasses file modes, so the tests
that deny a file or socket by mode fail with a message saying so.

Every portable test runs on all three release targets. A test that applies to
one platform is compiled only for that platform, never skipped at runtime. CI
reports the number of tests run on each target, so an empty suite cannot pass.

Each test uses its own temporary directory and checks only its own processes
and files, never a machine-wide count.

Every process-group signal in a test goes through the guarded helper in
`fakes`, which refuses a group id of 1 or less: `kill(-1)` signals every
process the user owns. Code that signals a process or group refuses an id of
1 or less the same way, and `scripts/check` fails on a signal sent anywhere
else. A test of that refusal passes the probe signal `0`, or
tests the refusal as a function that signals nothing, so a mutant that removes
the check sends nothing. Mutation testing runs in CI only, never on a
developer machine.

## Flaky tests


A failed binary-level test retries once. The binary-level tests are the test
binaries in `crates/main/tests/`; no other test retries, including the unit
tests in `crates/main/src/`. A pass on retry does not block the
merge, unless the pull request added or changed that test, in which case
the test job fails (`docs/ci.md`). Flakes are tracked per test file. CI opens one flake issue for the file,
naming the test and its failure, or comments on the file's open issue. The
issue's title is `Flaky tests: ` followed by the binary and the test
file: a unit test names its file from the first `*_tests` module in its
test name, so `fakes process_group_tests::reaps_children` files
`Flaky tests: fakes process_group`. An integration binary keeps its
binary title, such as `Flaky tests: main::hub_exit_window`, as does a
unit test with no `*_tests` module. Each body the report writes (the issue, a
sighting comment, a pull request comment) names the tested commit on its own
line: `tested <sha> = main <base> + head <head>` on a pull request run, where
the merge commit's parents are main and the pull request head, and
`tested <sha>` on a push run. The shas are full, so the
`git merge-base --is-ancestor` check below takes them as written. The first
failure's text is kept to 6000 characters; longer text is cut and `...` is
appended. Within one report job a file's title files or comments once; a repeat
is skipped, and a repeated title takes its runner and detail from its first
occurrence in file-name byte order. On a pull request run the report reads
the failing file from the panic path: when main changed that file since
the run's tested base it files nothing and notes the stale base in the
summary, and when the pull request itself changed that file it comments on
the pull request instead of filing. A failure with no panic path, or any
failure on a push run, files as usual. A flake issue closes when the cause its tests share is fixed, never by
rerunning. A failure on a
commit that does not contain the fix (`git merge-base --is-ancestor <fix> <commit>` is false) neither
reopens the closed issue nor counts against the fix.

A test file has one flake issue for its whole life. When no issue with its title
is open, the report looks for a closed one. A failure on a commit that
contains that issue's fix reopens it, with the sighting as a comment, and
puts `needs-triage` back on it: the fix did not hold, so its root cause was
wrong. The fix is the most recent `closed` event's `commit_id`: the commit
that closed the issue. An issue whose latest close has a null `commit_id`
closed without a pull request or commit, and reopens on any failure. Where
a file has several closed issues, the most recently closed is the one: the
greatest close time wins, and on a tie the higher issue number. Where the
issue has several `closed` events, the latest wins the same way: the
greatest `created_at`, and on a tie the greater event `id`. Only a file with
no flake issue at all gets a new one. The decision emits exactly one action
per unique title, whatever the runner lists repeat; the unit tests assert
that contract.

A sighting outside CI, such as in a local gate, is a comment on the same
file's issue with its command and output; a file with no issue gets one.
The issue is worked as one ticket: its pull request names the cause the
file's flaky tests share, such as a fixture, a wait helper or a timing
pattern, and fixes that, not one test at a time. It shows the whole file
passing under repeated runs under load.

Crate-level and cross-crate tests never retry: they are deterministic, so a
failure there is signal.
