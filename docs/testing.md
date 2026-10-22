# Testing

How Fiber is tested: what a test asserts, at which level, and against what.
This is what is true now, not a plan. It is settled by
[Testing posture: what a test asserts, and against what](https://github.com/aakshintala/fiber/issues/63);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. The event stream is `docs/events.md`; the crates
are `docs/architecture.md`. Which CI jobs run on which runners is
`docs/ci.md`. Latency, memory and
storage budgets are
`docs/performance.md`. No
test asserts a performance timing or sleeps to get a correct result. A wait's
deadline ("Waits and timeouts") is a hang guard, not a timing assertion; a
test of how much work something does counts the work, such as scans or
opens.

Parts. Read the part a job needs:

- `docs/testing/screens.md`: testing a terminal UI screen.
- `docs/testing/fakes.md`: model calls, fakes, testing an extension, jigs, and live calls and evals.
- `docs/testing/running.md`: running the suite, and working a flaky test.
- `docs/testing/mutants.md`: proving a test bites.

## Levels

Tests sit at three levels. Each proves something the others cannot.

- **Inside a crate.** Tests beside the code may call private functions freely.
  Every behaviour a crate exposes also has at least one test through its public
  API, so the wiring between helpers is tested, not just the helpers.
- **Across crates.** The loop runs with test tools and a test provider plugged
  into its seams, exactly as an extension would plug in. These tests prove turn
  and step behaviour without a binary or a network.
- **The binary.** A test starts the built `fiber`: a hub in the test's own
  `FIBER_HOME`, driven with the terminal's client code, or `fiber ask`. A
  test of one session may connect to that session's socket directly. It reads
  what a consumer reads: the JSON lines and the session directory left
  behind. Driving a session by hand is the `connect` jig ("Jigs").

A test at a higher level proves the wiring and one path per promise. It does
not repeat a branch case that a lower-level test pins with the same event
kinds and fields. A private-function test is deleted when a public-API test
kills the same mutants.

CI wiring has no Rust test of its own: a workflow step, or a script only CI
runs, is proven by the pull request's own CI run exercising it, and the pull
request links the job log that shows it working. Logic that wiring calls lives
in `xtask` or a crate, and is tested there like any other code.

A binary-level test needs no test-only switch in the shipped binary. It sets
`FIBER_HOME` to its own temporary directory, which holds an ordinary provider
definition whose base URL points at a local fake server, or it names the
built-in `scripted` provider ("Testing an extension").

A test inside a crate or across crates unwinds on a panic, because Cargo
builds the test harness without `panic = "abort"` (`docs/ci.md`). A test of
what a panic leaves (`docs/code-quality.md`, "Panics") panics in a child
process that aborts, such as the `fiber` binary or the test binary re-run with
the real panic hook, never in the test's own process.

Fiber's own tests are Rust, run by cargo. The one exception is an extension's
tests, Fiber's first-party extensions included: they are cases run by
`fiber extension test` ("Testing an extension").

## What a test asserts

A test asserts on what a consumer sees, as `docs/events.md` rules: "the lines
and the file left behind". Code no consumer sees is tested through its crate's
public API.

Every promise a `docs/<area>.md` page makes is tested at the level where a
consumer observes it. A security boundary, such as the credential deny or a
permission denial, is tested at binary level: the call is refused, it never
starts, and no protected bytes reach the output or an artifact.

Tests read a repository file only by compiling it in. The exception is a
first-party package under `providers/` or `extensions/`: a test may read one
at run time through a path joined onto `CARGO_MANIFEST_DIR`, and its crate
must be on the selector's list of package readers (`docs/ci.md`,
"Selection"). The gate fails on any other run-time read.

### Event streams

An event-stream test asserts two things:

- the complete, ordered list of event kinds, so a duplicated, missing or
  reordered event fails
- the fields under test, on the lines under test

A test may build the list from named segments held in `fakes`; the full
ordered list is still asserted.

A line the contract writes on its own trigger, with no position relative to
the session's other lines, is set aside from the ordered list rather than
placed in it: `clients`, written when a connection attaches; `session_status`
and the hub's `attention`, written when the session's summary changes. Where
its count is fixed, such as one `clients` line per attach, the test asserts
the count, so a duplicate or a missing line still fails. Otherwise the test
asserts it only when it is the line under test.

It ignores fields it is not testing, as a conforming consumer must: adding a
field is a compatible change (`docs/events.md`, "Versioning"), so a test that
failed on one would be stricter than the contract.

The central invariant, durable output equals the log byte for byte, is
asserted as plain equality between the two.

A test asserts at least one positive fact. A check that only says something
did not happen passes when the feature never ran.

A test on a large value, such as a page or a payload of more than a few KiB,
compares its length and its content without printing it whole on failure: an
`assert_eq!` that fails on a 2 MiB string writes megabytes to the CI log and
can time the job out. Report the length and the first differing offset.

### Values that change every run

Binary-level tests replace `ts`, ids and durations with placeholders before
comparing. Behaviour that depends on time, such as a monitor's deadline or a
retry's backoff, is tested at crate level under an injected clock. No test
sleeps on the wall clock (`docs/tools.md`, "Background jobs"), except `main`'s
test of the real clock's own `sleep`: it sleeps 1 ms and asserts the clock
advanced at least that much. The rule binds a test's own waits. Fiber's own
bounded retry, such as `doors::hub::connect` retrying the hub's socket on the
injected clock, runs on the real clock when a binary-level test drives it
against a real process.

A timeout the kernel keeps for each socket call, such as a connect or read
timeout, reads no clock in Fiber. It is tested by injecting a short limit
and waiting for the call's result with a deadline, and one test pins the
production values. Deadlines Fiber keeps itself stay on the injected clock.

### Screens

In `docs/testing/screens.md`.

### Invariants

The `log` and `loop` crates carry property tests: generated sequences, shrunk
to a minimal failing case and replayable by seed, checked against each crate's
stated invariants rather than a fixed expected output. For example:

- `log`: durable output equals the log; a session killed at each fsync
  boundary, or after a half-written line, reopens intact, and a tool call whose
  fate is unknown is never run again
- `loop`: no turn is lost; a cancelled call ends in one completion with status
  `cancelled`, and nothing from it follows: no delta after that completion, no
  `control` acted on, nothing after `turn_completed`; results return in request
  order

`web_fetch`'s HTML converter carries property tests too. Generated pages mix
nesting, broken markup, entities, scripts and huge attributes, and each is
checked against the converter's promises (`docs/tools.md`, "HTML to
markdown"): no visible text lost outside the dropped elements, no panic, and
output in proportion to the input. It is also checked against about ten real
pages under open licences, saved with their expected markdown. CI never
writes the expected markdown: a changed conversion fails and shows the
difference, and an accepted change appears in the pull request.

Races are forced, not waited for. Tests use barriers to put competing events in
each order that matters, such as a cancel arriving before, during and after a
tool result.

## Model calls

In `docs/testing/fakes.md`.

## Fakes

In `docs/testing/fakes.md`.

## Testing an extension

In `docs/testing/fakes.md`.

## Jigs

In `docs/testing/fakes.md`.

## Live calls and evals

In `docs/testing/fakes.md`.

## Proving a test bites

In `docs/testing/mutants.md`.

## Running tests

In `docs/testing/running.md`.

## Waits and timeouts


A test waits for the signal that proves the operation it needs: a socket
accepting a connection, an MCP server's tools listed, an artifact finished and
its completion event logged. A file existing is not that proof. A test never
waits for a generic sign that things have settled, because "the screen stopped
changing" is not "the server is listening".

Every wait has a deadline on the wall clock, also in a test that drives a fake
clock: fake time passes only when the test advances it. On expiry the test
fails with an assertion naming what it waited for. Calling code that blocks is a wait too, so the test runs
it on a thread and receives its result with a deadline. A fake's own sleep or poll
loop is a wait too, with a deadline on the wall clock. A fake that holds until
the test releases it blocks on a channel whose sender the test owns, and needs
no deadline of its own: the test sends to release it, and the test ending or
panicking drops the sender, which releases it too. That holds only when the
sender drops before anything joins the holder: inside a `thread::scope` the
test creates the sender in the scope's closure or moves it in, as
`a_writer_never_exposes_a_file_wider_than_0600`
(`crates/config/src/credential_file_tests.rs`) does, and a fake that joins its
thread on drop owns the release sender itself or drops it first. The test's own deadline on
the fake's signal names the failure. nextest's per-test timeout is at least twice the test's longest
single deadline plus its normal run time, so a hang, which fails at the first wait
that expires, reports that wait, not a harness kill.

A wait that takes several lines has one deadline for the whole wait, never one
per line, and needs no clock: a scoped thread reads the lines and sends them
over a channel, and the test takes them with one receive through `fakes::Deadline`, as `until` in
`crates/doors/tests/support/mod.rs` does. A binary-level test that must hand product
code a `contract::clock::Clock` uses `fakes::clock::SystemClock`, the process clock, or `StretchedClock`
in `crates/main/tests/support/mod.rs`, which runs it slower so a product deadline spans the test's own.

A test receives from a channel through `fakes::Deadline`, never with a raw `recv_timeout`: `recv` takes what remains of the deadline, and `recv_or_fail` fails naming what the test waited for, at the caller's line, as does any helper that fails a test on a receive. A wait's deadline is made once, before its first receive. A loop that takes the parts of one wait, consecutive receives that drain one stream, and a helper that receives on its caller's behalf all take that one deadline, never a fresh one per receive. Receives separated by an action whose answer the next one awaits are separate waits. A poll pause inside a loop is not a deadline. `clippy.toml` disallows `std::sync::mpsc::Receiver::recv_timeout`; product code whose receive is bounded by an injected clock's wait, or is a poll pause, turns the lint off for that statement with its reason (`docs/code-quality.md`, "Lints").

A test advances a fake clock only after a signal that the code under test is
waiting on that clock (past its own clock check); a parked caller alone is not
that signal. A test advances a fake clock to a named park only through
`fakes::clock::FakeClock::advance_when_parked`, naming the expected instant;
a poll driver that is the clock's only mover uses
`fakes::clock::FakeClock::try_advance_when_parked` instead;
the ban on raw advances lands with their migration in a later pull request.
A test creates every watcher, log watch or registration before it starts the
thread whose event it waits for. That second rule is a review rule: no helper
can see an event nobody watched, so the review traces each changed test's
registration to before its emitter's start.

Starting threads in order does not order their requests. A test that needs
calls to reach a fake in a set order has the fake acknowledge each call it
holds, and sends the next only after that acknowledgement. The MCP fixture
(`crates/fakes/mcp-fixture/server.sh`) writes a line to a `held-<tool>` FIFO
once it holds a call, and answers when the test writes to `release-<tool>`.

A test that proves a wait has not answered yet holds the awaited condition
false with a real signal, such as a child blocked reading a stdin pipe the test
holds open. It receives on the wait's result channel with a bounded receive (`recv` on its own short `Deadline::after`) that must time out, a bound of several of the wait's own probe
intervals, then releases the signal and requires the answer within the wait's
deadline. This is a receive with a bound, not a sleep: the test never sleeps to
get a correct result.
`group_empties_keeps_waiting_while_the_group_lives_and_returns_once_it_empties`
(`crates/fakes/src/process_group_tests.rs`) holds a `sh -c "read line"` group
alive this way, then drops its stdin.

A test that reproduces a race forces the bad interleaving with a pause point:
a committed, test-only seam where the code under test waits until the test
releases it. The race then happens on every run, so the red commit fails every
time ("Proving a test bites"). A child process the test starts, which the code
under test waits on and which blocks on a FIFO or pipe the test holds, is such
a seam and needs no change to the code under test. The test keeps its end open
until the child has read the release: closing the last writer before the
child opens the FIFO discards the line, and the child never returns.

A test that needs an inbox command admitted mid-turn holds the turn where the
loop reads its inbox, on a pending approval, and waits for that command's own
line, such as `steering_queue` for a `steer` (`docs/loop.md`, "One step"). A
held provider stream is not such a point: the loop reads nothing while it
streams, so no line can follow the send.

A test does not execute a file it wrote in the same run. On macOS, the first
run of a newly written executable can stall for seconds under load. The test
runs the script through its interpreter (`/bin/sh <path>`, `/bin/bash <path>`)
or uses a checked-in fixture. A test whose subject is direct execution, such as
a `PATH` lookup or the shebang line, keeps the executable file and passes a
`cargo nextest run --stress-count` run.

## Flaky tests

In `docs/testing/running.md`.

## What a change ships with

- **Any change:** the mutation check passes, and every behaviour a crate
  exposes has a public-API test.
- **A bug fix:** a test that reproduces the report at the level it was
  observed and fails on the code before the fix, which CI checks. A bug seen on screen gets a
  screen test; a bug in the JSON lines gets a binary-level test. A race
  the binary cannot be made to lose on every run, because its pause point
  sits inside a crate and the shipped binary takes no test-only switch, is
  reproduced at crate level through that pause point ("Waits and timeouts").
- **A new event kind:** a binary-level test in which a consumer sees it.
- **A breaking log format change:** its migration and the migration's test
  (`docs/events.md`, "Versioning").
- **A grown tool definition:** a raised size budget in the same pull request
  (`docs/tools.md`, "Size budget in CI").
