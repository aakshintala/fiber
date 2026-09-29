# Testing

How Fiber is tested: what a test asserts, at which level, and against what.
This is what is true now, not a plan. It is settled by
[Testing posture: what a test asserts, and against what](https://github.com/aakshintala/fiber/issues/63);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. The event stream is `docs/events.md`; the crates
are `docs/architecture.md`. Which CI jobs run on which runners is
`docs/ci.md`. Latency, memory and
storage budgets are
`docs/performance.md`. No
test asserts a performance timing or sleeps to get a correct result.

## Levels

Tests sit at three levels. Each proves something the others cannot.

- **Inside a crate.** Tests beside the code may call private functions freely.
  Every behaviour a crate exposes also has at least one test through its public
  API, so the wiring between helpers is tested, not just the helpers.
- **Across crates.** The loop runs with test tools and a test provider plugged
  into its seams, exactly as an extension would plug in. These tests prove turn
  and step behaviour without a binary or a network.
- **The binary.** A test spawns the built `fiber`, drives it through a door,
  and reads what a consumer reads: the JSON lines and the session directory
  left behind.

A binary-level test needs no test-only switch in the shipped binary. It sets
`FIBER_HOME` to its own temporary directory, which holds an ordinary provider
definition whose base URL points at a local fake server.

Tests are Rust, run by cargo. There is no second test language.

## What a test asserts

A test asserts on what a consumer sees, as `docs/events.md` rules: "the lines
and the file left behind". Code no consumer sees is tested through its crate's
public API.

Every promise a `docs/<area>.md` page makes is tested at the level where a
consumer observes it. A security boundary, such as the credential deny or a
permission denial, is tested at binary level: the call is refused, it never
starts, and no protected bytes reach the output or an artifact.

### Event streams

An event-stream test asserts two things:

- the complete, ordered list of event kinds, so a duplicated, missing or
  reordered event fails
- the fields under test, on the lines under test

It ignores fields it is not testing, as a conforming consumer must: adding a
field is a compatible change (`docs/events.md`, "Versioning"), so a test that
failed on one would be stricter than the contract.

The central invariant, durable output equals the log byte for byte, is
asserted as plain equality between the two.

A test asserts at least one positive fact. A check that only says something
did not happen passes when the feature never ran.

### Values that change every run

Binary-level tests replace `ts`, ids and durations with placeholders before
comparing. Behaviour that depends on time, such as a monitor's deadline or a
retry's backoff, is tested at crate level under an injected clock. No test
sleeps on the wall clock (`docs/tools.md`, "Background jobs").

### Screens

The terminal UI is tested by feeding a sequence of events to its drawing code
and comparing the whole in-memory screen (ratatui's `TestBackend`) against a
stored snapshot. CI never writes a snapshot: a changed screen fails and shows
the difference, and an accepted change appears in the pull request.

Input is tested the same way: key presses are fed to the TUI's input handling,
and the test asserts the commands it sends to the session.

A few tests run the real binary in a pseudo-terminal, for what memory cannot
show: raw mode, resize, the terminal restored on exit, and one journey that
types a prompt, sees the answer and cancels a turn.

### Invariants

The `log` and `loop` crates carry property tests: generated sequences, shrunk
to a minimal failing case and replayable by seed, checked against each crate's
stated invariants rather than a fixed expected output. For example:

- `log`: durable output equals the log; a session killed at each fsync
  boundary, or after a half-written line, reopens intact, and a tool call whose
  fate is unknown is never run again
- `loop`: no turn is lost; nothing a cancelled call produces is admitted after
  the cancel; results return in request order

Races are forced, not waited for. Tests use barriers to put competing events in
each order that matters, such as a cancel arriving before, during and after a
tool result.

## Model calls

Nothing in CI calls a live provider or the public network. Provider bytes in
tests come from two sources:

- **Recorded streams.** Real responses from each protocol, captured with live
  keys by the `provider` crate's `record` rig ("Rigs") and checked in. They
  are replayed byte for byte against the provider crate's decoders. A recording keeps response bytes only,
  never request headers or keys. It is re-recorded when a vendor change is
  suspected.
- **Scripted streams.** Hand-written in the real wire format, for scenarios a
  recording cannot produce on demand, such as a tool call, then a 429, then
  text. A local fake server serves them to the binary and to cross-crate tests.

The fake server also records every request it receives, and tests assert on
it: the path, the headers with credentials masked, and the body bytes. This is
how a test proves the prompt-cache rule that "two requests built from the same
inputs are the same bytes" (`docs/prompt-cache.md`), across turns, resume and
fork.

The five first-party provider extensions are tested in Fiber's CI, loaded into
the built binary by local path: against their vendor's recorded streams and
against scripted streams. A protocol change that breaks a shipped provider
fails the pull request that caused it.

## Fakes

Anything outside Fiber that a test needs is a shared fake. A Fiber delegate
is a real child `fiber serve`, because it is Fiber. The fakes are:

- a provider server serving recorded and scripted streams, and recording
  requests
- MCP servers for both transports, stdio and streamable HTTP
- child processes that misbehave on purpose: ignore SIGTERM, leave descendants,
  escape their process group
- a fixture Lua extension that registers a tool, a provider and each hook
- a scripted foreign harness, standing in for a delegate that is not Fiber
- a local OAuth token endpoint
- a second client on a session's socket, including a slow watcher

The fakes live in one crate, `fakes`, which depends only on `contract` and
is a test-only dependency of the crates that use it (`docs/architecture.md`,
"The call rules").

The concrete scenarios each area needs are that area's acceptance criteria,
written when its implementation tickets are.

## Rigs

A rig runs one layer on its own, so an agent building or diagnosing that
layer can see what it makes of a given input without writing a throwaway
program or waiting on a test.

- **A rig is a Cargo example in its layer's crate**, run as
  `cargo run -p <crate> --example <rig> -- <args>`. It can use only what its
  crate may depend on, so what it shows is that layer alone. It may use
  `fakes`, as a test-only dependency. It reads its arguments with
  `std::env::args`.
- **It never ships.** No release binary contains a rig, and the shipped
  `fiber` gains no switch for one. A rig that people need becomes a `fiber`
  subcommand through its area's ticket, with the tests a feature carries.
- **It is gated.** The tests and clippy compile every example, so a rig
  that stops building fails `scripts/check`.
- **It reads and writes only formats tests replay:** a recorded stream (the
  response bytes), an events file (JSON lines, the session log's format), or
  a session directory. A bug a rig reproduces ships its test
  ("What a change ships with"), built from the file the rig used or saved.

A crate's first implementation ticket ships the rigs listed for it:

| Crate | Rig | What it does |
|---|---|---|
| `log` | `dump` | Prints a session directory's events, one per line. |
| `log` | `check` | Checks a session directory against the log's invariants: `seq` has no gaps, durable output equals the log. |
| `config` | `resolve` | Prints the merged configuration for a Fiber home and project. |
| `provider` | `decode` | Runs a recorded stream through one protocol's decoder and prints what it produced. |
| `provider` | `record` | Captures a live response as a recorded stream, with live keys, response bytes only. |
| `fakes` | `provider-server` | Serves a scripted or recorded stream on a local port and prints each request it receives. |
| `tools` | `call` | Runs one tool call with the given arguments and prints its result. |
| `tui` | `draw` | Draws an events file at a given width and prints the screen as text. |
| `loop` | `turn` | Runs one turn against the fake provider and test tools and prints its events. |

`contract` has no rig: it holds types, not behaviour. A rig not in this table
is added to it by the ticket that builds it.

## Live calls and evals

Tests that need live credentials are opt-in by environment variable and never
run in CI.

An eval measures the model plus Fiber's prompting as a pass rate over many
runs. It is a development instrument for prompts and tool definitions. No eval
gates a merge or a release.

## Proving a test bites

A test that passes with its feature removed proves nothing. CI checks this on
every pull request with mutation testing: `cargo-mutants --in-diff` makes small
breaking edits to the code the diff changed, such as replacing a function body
with a default or flipping a comparison, and runs the mutated crate's tests
against each one. An edit that no test notices fails CI.

An edit that genuinely changes no behaviour is exempted in the code, with a
written reason. How many runners share the mutants is CI's to set.

A bug fix must also show that its test reproduces the bug. For a pull request
whose ticket is labelled `bug`, CI runs its new and changed tests against the base commit,
and at least one must fail there.

Diff-scoped mutation testing has two known limits. It cannot see a change in
one place leaving other code under-tested. A pull request that changes only
test code runs no mutants, so a rewritten test could lose its teeth unnoticed
until the next code change in that crate runs mutants against it.

## Running tests

Tests run under cargo-nextest. Each test runs in its own process and is killed
past its timeout. That does not contain what the test starts: a binary-level
test runs Fiber in its own process group, and at the end it asserts that no
child of its own remains, including after a timeout. A filter that matches no
tests fails: nextest exits 4 with "no tests to run", where `cargo test` prints
"0 passed" and exits 0. Doc-tests run under `cargo test --doc`.

Every portable test runs on all three release targets. A test that applies to
one platform is compiled only for that platform, never skipped at runtime. CI
reports the number of tests run on each target, so an empty suite cannot pass.

Each test uses its own temporary directory and checks only its own processes
and files, never a machine-wide count.

### Waits and timeouts

A test waits for the signal that proves the operation it needs: a socket
accepting a connection, an MCP server's tools listed, an artifact finished and
its completion event logged. A file existing is not that proof. A test never
waits for a generic sign that things have settled, because "the screen stopped
changing" is not "the server is listening".

Every wait has a deadline. On expiry the test fails with an assertion naming
what it waited for. nextest's per-test timeout is at least twice the sum of the
test's own deadlines, so a hang reports which wait expired, not a harness kill.

### Flaky tests

A failed binary-level test retries once. A pass on retry does not block the
merge. CI opens a flake issue naming the test and its first failure, or
comments on the open one. A flake issue closes when the test is rewritten to be
deterministic, never by rerunning.

Crate-level and cross-crate tests never retry: they are deterministic, so a
failure there is signal.

## What a change ships with

- **Any change:** the mutation check passes, and every behaviour a crate
  exposes has a public-API test.
- **A bug fix:** a test that reproduces the report at the level it was
  observed and fails on the code before the fix, which CI checks. A bug seen on screen gets a
  screen test; a bug in the JSON lines gets a binary-level test.
- **A new event kind:** a binary-level test in which a consumer sees it.
- **A breaking log format change:** its migration and the migration's test
  (`docs/events.md`, "Versioning").
- **A grown tool definition:** a raised size budget in the same pull request
  (`docs/tools.md`, "Size budget in CI").
