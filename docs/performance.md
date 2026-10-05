# Performance

The budgets Fiber holds, how each is measured, and what happens when one is
exceeded. This is what is true now, not a plan. It is settled by
[Performance budgets: what Fiber holds, and how it is measured](https://github.com/aakshintala/fiber/issues/67);
that ticket's resolution holds the rationale and the rejected alternatives.

Which CI job runs the gate, and on which runner, is
`docs/ci.md`. How memory is measured
is `docs/dependencies.md`, "Measuring memory".

## What a budget covers

A budget covers one process: a session or the terminal. MCP servers,
process extensions, shell commands, delegates, image children and the hub
are processes of their own and are not counted. A delegate is a session
process and holds the same budgets as any session. The hub's own budgets,
idle memory, idle CPU, threads and time to first response with N sessions,
are not set yet ([Control center](https://github.com/aakshintala/fiber/issues/256)). The image child's memory is its
own process's, not the session's. Peak over an empty program is 68,076 KiB
on Linux x86_64, 67,604 KiB on Linux arm64 and 72,352 KiB on macOS arm64;
an 81-megapixel PNG, which the 50-megapixel cap refuses, peaks at 308,372 KiB
(`research/image-limits/README.md`).

Every workload runs a fresh install's default load: the first-party provider
extension the session uses and the compiled-in tools. It runs no MCP server,
no process extension, no TUI extension and no other child process. An extension a person adds
costs what it costs (about 150 KiB and one thread for a Lua extension,
`research/extension-runtime/pass2/RESULTS.md`); that cost is its author's.
A session in a repository that declares code also checks each declared path
against `pinned.json` before its first request, hashing only a file whose size
or modification time changed (`docs/extensions.md`, "Code a repository
ships"). Measured on macOS arm64, in one process, with
`cargo run -p extensions --example pin_check`: one MCP server naming 200
files of 4 KiB each (819,200 bytes) takes about 9 ms with no `pinned.json`,
and about 4 ms with every size and modification time unchanged; a release
build takes about 8 ms and 2.5 ms. Both include resolving each path and
reading the index. No Linux figure is measured.

Memory follows the context window, not the transcript. After a handoff the
session holds the handoff note and what came after it, and a resumed session
reads from its last handoff (`docs/handoff.md`). A 2-million-token session and
a 20-thousand-token one fit the same ceiling.

## Budgets

| Budget | Ceiling | Gated on | Basis |
|---|---|---|---|
| Session, idle, headless | 12 MiB peak RSS | Linux x86_64 | from components |
| Terminal, idle | 8 MiB peak RSS | Linux x86_64 | from components |
| Session, busy or resumed | 24 MiB peak RSS | Linux x86_64 | from components |
| Idle CPU, session and terminal | zero context switches in the idle window, on every thread | Linux x86_64 | exact |
| Threads, idle headless session | 3, plus one per Lua extension in use | Linux x86_64 | exact |
| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact |
| Log bytes, 429-call turn | the turn's content plus 1 KiB per tool call | Linux x86_64 | exact |
| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked |
| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked |
| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked |
| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked |

Basis says where a number came from:

- **From components** is the sum of measured parts, times two. Idle session:
  every runtime crate linked together costs 5.1 MiB over an empty program
  (`docs/dependencies.md`). Idle terminal: ratatui's two screen buffers and
  crossterm, with room for the visible part of the transcript. Admitting a
  crate for the terminal raises its ceiling by twice the crate's measured cost
  in the same pull request (`docs/dependencies.md`); its idle CPU and first
  frame budgets do not move. Busy session: a
  300,000-token context is about 1.2 MB of text, and a 2 MiB conversation
  added about 3 MiB in `research/delegate-memory/`.
- **Exact** follows from a rule, so the gate checks an equality, not a
  ceiling. The three threads are the loop, signals and one client:
  the hub's connection, or `fiber ask`'s stdout
  (`docs/architecture.md`, "The threads"). Two fsyncs bracket each effect, and
  no line restates an earlier line in the same turn (`docs/events.md`,
  "Writing").
- **Picked** was chosen with no measurement behind it.
- **Measured** is a Fiber measurement times two. The first build that runs
  replaces every "from components" and "picked" number with its measured one.

The busy-or-resumed ceiling holds on three workloads:

- a turn of 429 tool calls with the context window full to the handoff point
- resuming a 20,000-token session
- resuming a 2,000,000-token session that has handed off 7 times

429 tool calls is the p99 of tool calls per user turn, measured on real
sessions in the archived Zig tree. That tree's session peaked at 2.1 GiB on
this turn.

Listing reads each log line by line through its first `turn_started`, for the
first prompt and branch, and the last 4 KiB, for the session's state. On macOS
arm64 that costs 51 ms for 2,000 sessions, warm
([research/session-listing](../research/session-listing/README.md)).
`docs/state.md` rules out a derived database, and the ruling stands while this
budget holds.

## Measuring

Each number is the median of 5 runs. Memory is peak RSS on Linux and peak
footprint on macOS, by the method in `docs/dependencies.md`. Idle CPU is the
voluntary and involuntary context switch counts in
`/proc/<pid>/task/*/status`, read before and after the idle window: 10
seconds on a pull request and 60 at release. A truly idle process switches
zero times in either. fsyncs are
counted at the call site and log bytes are the size of `events.jsonl`, so both
are exact on any platform.

Linux x86_64 gates every pull request, and the backstop on `main` measures
the same benchmarks. Linux arm64 and macOS arm64 are
measured at each release and reported, never gated: timings differ by about
20 times between macOS and Linux on I/O, and macOS memory counts system
frameworks Fiber does not control. Listing with a cold cache is reported,
never gated.

No test asserts a timing (`docs/testing.md`). The budgets are a benchmark job,
separate from the tests.

## When a budget is exceeded

An exact or memory budget fails the pull request from its first run. Its
author may raise the ceiling in the same pull request by editing the table
above with the new measurement and the reason.

A timing budget is advisory until 20 backstop runs on `main` have measured
it. Until then CI posts the head and base medians as a comment on the pull
request and never fails it. The pull request that turns a timing gate on
sets its tolerance from the spread those runs showed. From then on a timing
gate fails only when the median is over its ceiling and over the base
commit's median by more than that tolerance, measured in the same job on the
same runner, and its author may raise the ceiling as above. A benchmark's
own self-check failing is a failed run, not a slow sample.

At each release, every ceiling more than twice its measured value drops to
measured times two. Ceilings never rise at release; only a pull request
raises one.
