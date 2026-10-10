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

Tests read a repository file only by compiling it in; the gate fails on a run-time read.

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

The terminal UI is tested by feeding a sequence of events to its drawing code
and comparing the whole in-memory screen (ratatui's `TestBackend`) against a
stored snapshot. CI never writes a snapshot: a changed screen fails and shows
the difference, and an accepted change appears in the pull request.

Input is tested the same way: key presses are fed to the TUI's input handling,
and the test asserts the commands it sends to the session.

A few tests run the real binary in a pseudo-terminal, for what memory cannot
show: raw mode, resize, the terminal restored on exit, and two journeys that
cross features: from a prompt through an approval, a resize and a quit, and
from a turn through a quit, a resume and a new answer.

The binary-level tests in `crates/main/tests` that assert on the terminal
UI's screen use one driver, `crates/main/tests/support/pty.rs`. It feeds
the bytes it reads into a `vt100` parser, which rebuilds the screen as a
grid, and the test asserts on that grid: its text, its rows, each cell's
colours and attributes, the cursor position, and the alternate-screen and
hidden-cursor flags. Each wait is a predicate over the grid with one named
deadline; a wait the terminal ends before failing shows the last grid.
The driver takes a grid snapshot for a wait only when no synchronized
output block (mode 2026) is open, so a wait sees whole frames and never a
mix of the old frame and the new one; a stream without mode 2026 snapshots
after each read.
The terminal is 120 by 32 unless a test picks the size its layout needs
(the look tests use 160 by 48). The driver pins `TERM` to
`xterm-256color` and answers the binary's capability queries for a fixed
dark terminal.

Bytes that never reach the cells, such as the window title, the OSC 9
desktop notification, a bell and the mode-enable sequences, are waited
for on the raw output instead. Such a wait searches from an offset the
test took before the input that causes the bytes, never from an earlier
match, so it does not depend on the order in which two markers arrive.

A screen predicate checks every cell property its assertions read, so a
partial redraw can't satisfy it. A test sends input only after output the
terminal already emits once it is ready for that input, such as the
mode-enable sequences or the end of the first frame, never after a grid
match alone. That signal is existing output: the shipped binary gets no
test-only switch ("Levels").

The driver reads the terminal on a thread from the first frame to end of
file, and takes the markers it waits for over a channel, as `watch` in
`crates/tui/src/pty_watch.rs` does. A reader that stops after a marker
lets the terminal's output queue fill, so the code under test blocks
writing a frame and the test hangs on a wait it caused itself. On quit
the driver reaps the binary, then reads to end of file, so every byte
written before the exit is read.

### Invariants

The `log` and `loop` crates carry property tests: generated sequences, shrunk
to a minimal failing case and replayable by seed, checked against each crate's
stated invariants rather than a fixed expected output. For example:

- `log`: durable output equals the log; a session killed at each fsync
  boundary, or after a half-written line, reopens intact, and a tool call whose
  fate is unknown is never run again
- `loop`: no turn is lost; nothing a cancelled call produces is admitted after
  the cancel; results return in request order

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

Nothing in CI calls a live provider or the public network. Provider bytes in
tests come from two sources:

- **Recorded streams.** Real responses from each protocol, captured with live
  keys by the `provider` crate's `record` jig ("Jigs") and checked in. They
  are replayed byte for byte against the provider crate's decoders. A recording keeps response bytes only,
  never request headers or keys. It is re-recorded when a vendor change is
  suspected. The probe recordings in `research/*-probe/raw/` do not
  share one shape. Among them: the stream's bytes in a JSON wrapper's
  `raw_sse` (`anthropic-messages-probe`) or `body` (`opencode-probe`), or in a
  `.sse` file (`opencode-probe`); the stream as parsed `events`, each an event
  name and its data (`codex-responses-probe`); or a non-streamed response
  object (`openai-responses-probe`). A test serves the bytes where a probe
  kept them, and otherwise rebuilds the stream from what the probe kept.
- **Scripted streams.** Hand-written in the real wire format, for scenarios a
  recording cannot produce on demand, such as a tool call, then a 429, then
  text. A local fake server serves them to the binary and to cross-crate tests.
  A script is a list of responses, each a status, headers and body bytes,
  served one per request in order.

The fake server binds a free local port and returns its address, which a
test puts in a provider definition's base URL. It also records every request
it receives, and tests assert on it: the path, the headers with each
credential replaced by its fingerprint, and the body bytes. This is
how a test proves the prompt-cache rule that "two requests built from the same
inputs are the same bytes" (`docs/prompt-cache.md`), across turns, resume and
fork.

Every first-party provider extension is tested in Fiber's CI, loaded into
the built binary by local path. What it declares for the wire (base URLs,
compatibility flags, headers) is tested as part of its protocol, against the
vendor's recorded streams and against scripted streams on the fake server,
because that is the native protocol's decoding and request shaping, which is
Fiber's own Rust. Its Lua functions (`models()`, `quota()`, `credential()`,
`sign()`, `cost()`) are tested as an extension, with `fiber extension test` and
scripted host calls ("Testing an extension"). A protocol change that breaks a
shipped provider fails the pull request that caused it.

## Fakes

Anything outside Fiber that a test needs is a shared fake. A Fiber delegate
is a real child session process, because it is Fiber. The fakes are:

- a provider server serving recorded and scripted streams, and recording
  requests
- MCP servers for both transports, stdio and streamable HTTP
- child processes that misbehave on purpose: ignore SIGTERM, leave descendants,
  escape their process group
- a fixture Lua extension that registers a tool, a provider and each hook
- a scripted foreign harness, standing in for a delegate that is not Fiber
- a local OAuth token endpoint
- a refused address, loopback port 1: a connect is reset, and a bind of port 0
  never draws it, so no concurrent listener can take it
- a second client on a session's socket, including a slow watcher
- a fake clock, whose time moves only when the test advances it;
  `advance_marked` marks each thread's latest park, so `await_parked_since`
  waits for that thread's next park after the advance even when an event
  woke it out of its park before the advance
- a counting allocator, which counts the blocks of 1 MiB or more a thread
  holds at once, and the bytes it allocated and has not freed, from the
  scope's start and never below 0; each block of 64 KiB or more takes a
  slot in a fixed table of 64, following the block through every `realloc`,
  so a test can read the peak without its output buffer; a block made
  before the scope and freed inside it lowers the total; `fakes` only
  exports it, and each test binary that measures installs it as its own
  global allocator and holds nothing else

The fakes live in one crate, `fakes`, which depends only on `contract` and
is a test-only dependency of the crates that use it (`docs/architecture.md`,
"The call rules").

The concrete scenarios each area needs are that area's acceptance criteria,
written when its implementation tickets are.

## Testing an extension

Fiber tests its own extensions only with tooling it ships to everyone. The
`fakes` crate tests Fiber's own Rust: the protocol decoders, the log writer,
the loop's edge cases. Anything that tests an extension, first-party or not,
uses what every author has, so that tooling is always good enough for a real
extension and first-party code gets no private path.

The shipped tooling has three parts:

- **The `scripted` provider.** A built-in provider whose model is a script
  file: steps such as reply with this text, call this tool with these
  arguments, fail with this error, stream slowly. A session names it like any
  model, so it is a provider, not a test switch, and nothing in the loop knows
  a test is running. Replies are served in order, never matched to the
  request, so an extension that changes the prompt still gets its scripted
  replies. It bypasses the vendor decoders, which the fake server tests
  (`docs/model-routing.md`, "The scripted provider").
- **Test cases.** A session case gives a script and prompt, then checks the
  complete ordered durable event stream and selected fields. A call case
  invokes a provider function and checks its return or error. Both can script
  host calls. See `docs/extensions.md`, "Testing an extension", for the case
  format.
- **A runner, `fiber extension test [<path>]`.** It runs an extension's cases
  against the built binary in a temporary `FIBER_HOME` and exits non-zero when
  any case fails (`docs/invocation.md`). While the runner drives a session, a
  case may script the extension's host calls (a `host.http` reply, a
  `host.exec` result) and advance a fake clock, so timers fire when the case
  says, not when the wall clock does. These exist only under the runner, as
  its documented feature; a session started any other way has neither.

Fiber's CI runs `fiber extension test` for every first-party package that has
cases whenever the binary-level tests run (`docs/ci.md`, "Selection"). A change
to a package, or to Fiber under it, that breaks one of its cases fails the pull
request that caused it.

## Jigs

A jig runs one layer on its own, so an agent building or diagnosing that
layer can see what it makes of a given input without writing a throwaway
program or waiting on a test.

- **A jig is a Cargo example in its layer's crate**, run as
  `cargo run -p <crate> --example <jig> -- <args>`. It can use only what its
  crate may depend on, so what it shows is that layer alone. It may use
  `fakes`, as a test-only dependency. It reads its arguments with
  `std::env::args`.
- **A test runs the jig's built binary.** `cargo test` builds the crate's
  examples, so a jig test starts `target/<profile>/examples/<jig>` (found
  from `std::env::current_exe`) directly. It never runs `cargo run`, which
  would wait on the build directory's lock inside the test's deadline.
- **It never ships.** No release binary contains a jig, and the shipped
  `fiber` gains no switch for one. A jig that people need becomes a `fiber`
  subcommand through its area's ticket, with the tests a feature carries.
- **It is gated.** The tests and clippy compile every example, so a jig
  that stops building fails `scripts/check`.
- **It adds no fixture format.** It reads the files its layer already reads,
  and what it saves for a test is a format tests already replay: a recorded
  stream (the response bytes), an events file (JSON lines, the session log's
  format), or a session directory. A bug a jig reproduces ships its test
  ("What a change ships with"), built from the file the jig used or saved.

A crate's first implementation ticket ships the jigs listed for it:

| Crate | Jig | What it does |
|---|---|---|
| `log` | `dump` | Prints a session directory's events, one per line. |
| `log` | `check` | Checks a session directory against the invariants the directory alone shows: every line parses as an event, and `seq` has no gaps. |
| `config` | `resolve` | Prints the merged configuration for a Fiber home and project. |
| `provider` | `decode` | Runs a recorded stream through one protocol's decoder and prints what it produced. |
| `provider` | `record` | Captures a live response as a recorded stream, with live keys, response bytes only. |
| `fakes` | `provider-server` | Serves a scripted or recorded stream on a free local port, prints that address, then prints each request it receives. |
| `tools` | `call` | Runs one tool call with the given arguments and prints its result. |
| `tui` | `draw` | Draws an events file at a given width and prints the screen as text. |
| `tui` | `paging` | Generates a large session and measures the terminal paging it: the opening pass, the frames that load pages, jumps, a search of the whole log, a re-count at a new width, and appending to a running turn. |
| `tui` | `hover` | Sends motion reports through the terminal's screen over an events file and prints the frames, bytes and time per report. |
| `loop` | `turn` | Runs one turn against the fake provider and test tools and prints its events. |
| `extensions` | `pin_check` | Times checking a repository's declared paths against `pinned.json`, cold and warm, in one process. |
| `doors` | `connect` | Connects to a running session through the hub, or to its socket, and sends the JSON commands typed on stdin, printing what comes back. |

`contract` has no jig: it holds types, not behaviour. A jig not in this table
is added to it by the ticket that builds it.

## Live calls and evals

Tests that need live credentials are opt-in by environment variable and never
run in CI.

A live test asserts the outcome, the fields under test, and the order of the
kinds it relies on (the turn's start first, its `text_completed` before its
`turn_completed`, `turn_completed` last), not the complete ordered list "Event
streams" requires: a real model's stream varies in its deltas and reasoning
events. The complete list is pinned by the same scenario against the fake
provider.

An eval measures the model plus Fiber's prompting as a pass rate over many
runs. It is a development instrument for prompts and tool definitions. No eval
gates a merge or a release.

## Proving a test bites

A test that passes with its feature removed proves nothing. CI checks this on
every pull request with mutation testing: `cargo-mutants --in-diff` makes small
breaking edits to the code the diff changed, such as replacing a function body
with a default or flipping a comparison, and runs the mutated crate's tests
against each one. An edit that no test notices fails CI.

A function is extracted and tested alone only where a surviving mutant would
signal the wrong process or lose data, such as the `kill(-1)` refusal
("Running tests"). Elsewhere a mutant that changes no behaviour a consumer
sees takes an exemption in the code, with a written reason, instead of a test
that pins it. The exemption goes on a function, since an exemption on a
statement or block is not reliably honoured. How many runners share the
mutants is CI's to set.

Mutants run on Linux, so code compiled only on macOS, or on any platform other
than Linux, is never built there, and every mutant of it would pass unnoticed.
A function gated that way is exempted the same way, with the reason that
mutants run on Linux, where it is not compiled. The macOS test job still runs
it, so its tests must reach it.

Code is written so a mutant fails fast. A loop that steps an index by hand can
spin forever when a mutant breaks the arithmetic, and a hung mutant fails CI as
a timeout; walk with an iterator instead. A test reaches the code under test
from outside, through a test seam such as a fake writer or the injected clock. A
`#[cfg(test)]` hook inside a production function is used only where no outside
seam can reach the behaviour, such as a race's pause point ("Waits and
timeouts"), and the plan that needs one says why.

A bug fix must also show that its test reproduces the bug. A pull request
where any issue its body resolves is labelled `bug` starts with a red commit:
the reproducing test and any new signature or test seam it needs, without the
fix (`docs/workflow.md`, "The pull request"). CI runs the new and changed
tests at the red commit and at the head. The red commit must build, at least
one of those tests must fail there, and all of them must pass at the head.
A pull request whose every changed file is a docs file, as "Selection"
defines it, passes the check with a message saying so: the doc was wrong and
the code was right, so no test can show the bug.

Diff-scoped mutation testing has two known limits. It cannot see a change in
one place leaving other code under-tested. A pull request that changes only
test code runs no mutants, so a rewritten test could lose its teeth unnoticed
until the next code change in that crate runs mutants against it.

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
fixtures ignore SIGTERM on purpose ("Fakes"). A binary-level test's
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

### Waits and timeouts

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
over a channel, and the test takes them with one `recv_timeout`, as `until` in
`crates/doors/tests/socket.rs` does. A binary-level test that must hand product
code a `contract::clock::Clock` uses `fakes::clock::SystemClock`, the process clock, or `StretchedClock`
in `crates/main/tests/support/mod.rs`, which runs it slower so a product deadline spans the test's own.

A test advances a fake clock only after a signal that the code under test is
waiting on that clock (past its own clock check); a parked caller alone is not
that signal.

Starting threads in order does not order their requests. A test that needs
calls to reach a fake in a set order has the fake acknowledge each call it
holds, and sends the next only after that acknowledgement. The MCP fixture
(`crates/fakes/mcp-fixture/server.sh`) writes a line to a `held-<tool>` FIFO
once it holds a call, and answers when the test writes to `release-<tool>`.

A test that proves a wait has not answered yet holds the awaited condition
false with a real signal, such as a child blocked reading a stdin pipe the test
holds open. It receives on the wait's result channel with a bounded
`recv_timeout` that must time out, a bound of several of the wait's own probe
intervals, then releases the signal and requires the answer within the wait's
deadline. This is a receive with a bound, not a sleep: the test never sleeps to
get a correct result.
`group_empties_keeps_waiting_while_the_group_lives_and_returns_once_it_empties`
(`crates/fakes/src/process_group_tests.rs`) holds a `sh -c "read line"` group
alive this way, then drops its stdin.

A test that reproduces a race forces the bad interleaving with a pause point:
a committed, test-only seam where the code under test waits until the test
releases it. The race then happens on every run, so the red commit fails every
time ("Proving a test bites").

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

### Flaky tests

A failed binary-level test retries once. The binary-level tests are the test
binaries in `crates/main/tests/`; no other test retries, including the unit
tests in `crates/main/src/`. A pass on retry does not block the
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
  screen test; a bug in the JSON lines gets a binary-level test. A race
  the binary cannot be made to lose on every run, because its pause point
  sits inside a crate and the shipped binary takes no test-only switch, is
  reproduced at crate level through that pause point ("Waits and timeouts").
- **A new event kind:** a binary-level test in which a consumer sees it.
- **A breaking log format change:** its migration and the migration's test
  (`docs/events.md`, "Versioning").
- **A grown tool definition:** a raised size budget in the same pull request
  (`docs/tools.md`, "Size budget in CI").
