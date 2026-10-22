# Testing with fakes

How model calls are tested, and the fakes, extension tests, jigs and live calls that stand in for the outside world. It is part of `docs/testing.md`.

## Model calls


Nothing in CI calls a live provider or the public network. Provider bytes in
tests come from two sources:

- **Recorded streams.** Real responses from each protocol, captured with live
  keys by the `provider` crate's `record` jig (`docs/testing/fakes.md`, "Jigs") and checked in. They
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
scripted host calls (`docs/testing/fakes.md`, "Testing an extension"). A protocol change that breaks a
shipped provider fails the pull request that caused it.

## Fakes


Anything outside Fiber that a test needs is a shared fake. A Fiber delegate
is a real child session process, because it is Fiber. The fakes are:

- a provider server serving recorded and scripted streams, and recording
  requests
- one shared HTTP/1.1 server core in `fakes`, which the provider server,
  the OAuth token endpoint, the CONNECT proxy and the gated streamable-HTTP
  fake all build on: each request is read within one bound on the injected
  clock, and the test synchronises on pauses at the read request, before
  the response and between the pieces of a stream; closing a connection
  with unread bytes on its socket resets it instead of ending the reply
  with FIN, so a server that closes after a fixed body discards the bytes
  already there with `Connection::drain_socket` before it replies
- MCP servers for both transports, stdio and streamable HTTP
- child processes that misbehave on purpose: ignore SIGTERM, leave descendants,
  escape their process group
- a fixture Lua extension that registers a tool, a provider and each hook
- a local OAuth token endpoint
- a refused address, loopback port 1: a connect is reset, and a bind of port 0
  never draws it, so no concurrent listener can take it
- a second client on a session's socket
- test helpers for what many test files share: `fakes::fifo` makes a FIFO
  by one function, `fakes::tool_args` builds a tool call's argument map
  and `fakes::text_of` reads a tool result's text, and `fakes::wait_output`
  returns a spawned system tool's output under a deadline, killing its
  group on expiry
- a fake clock, whose time moves only when the test advances it to a named
  park through `advance_when_parked`, naming the expected park and the advance
  separately; `advance_marked` marks each thread's latest park, so
  `await_parked_since` waits for that thread's next park after the advance
  even when an event woke it out of its park before the advance
- a counting allocator, which counts the blocks of 1 MiB or more a thread
  holds at once, and the bytes it allocated and has not freed, from the
  scope's start and never below 0; each block of 64 KiB or more takes a
  slot in a fixed table of 64, following the block through every `realloc`,
  so a test can read the peak without its output buffer; a block made
  before the scope and freed inside it lowers the total; `fakes` only
  exports it, and each test binary that measures installs it as its own
  global allocator and holds nothing else

The fakes live in one crate, `fakes`, which depends only on `contract` and `support` and
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
  any case fails (`docs/invocation.md`). Before the first case it compares each
  provider that has a data file with its Lua and fails the run on a `credential()`
  with no `login` or `credential_label`, or a `credential_label` with no
  `credential()`; a provider with no data file is exempt (`docs/extensions.md`,
  "Testing an extension"). While the runner drives a session, a
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
- **Its own logic is tested from shared support.** Code in a jig that a
  test checks without running the binary, such as its argument parsing,
  lives in a support file under the crate's `tests/support/`. The jig
  includes that file with `#[path]` (`docs/code-quality.md`, "Size"), and
  the test calls it directly. A jig is never marked `test = true`, because
  `cargo test` then builds it as a test harness instead of the binary jig
  tests run.
- **It never ships.** No release binary contains a jig, and the shipped
  `fiber` gains no switch for one. A jig that people need becomes a `fiber`
  subcommand through its area's ticket, with the tests a feature carries.
- **It is gated.** The tests and clippy compile every example, so a jig
  that stops building fails `scripts/check`.
- **It adds no fixture format.** It reads the files its layer already reads,
  and what it saves for a test is a format tests already replay: a recorded
  stream (the response bytes), an events file (JSON lines, the session log's
  format), or a session directory. A bug a jig reproduces ships its test
  (`docs/testing.md`, "What a change ships with"), built from the file the jig used or saved.

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
| `loop` | `review` | Prints the reviewer's stage inputs and handoff selection requests for a session directory, including unreplayable calls. |
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
