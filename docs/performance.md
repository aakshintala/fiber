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
on Linux x86_64, 67,604 KiB on Linux arm64 and 67,825 KiB on macOS arm64;
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

The `paging` rows measure the `tui` crate's `paging` jig (`docs/testing.md`,
"Jigs"), a process that runs the terminal's paging over a generated session
it holds in memory, standing in for the hub's log; its memory row covers that
copy. It runs no provider extension and no compiled-in tool: it measures the
terminal's paging and drawing alone, so the fresh-install load above does not
apply to it.

Memory follows the context window, not the transcript. After a handoff the
session holds the handoff note and what came after it, and a resumed session
reads from its last handoff (`docs/handoff.md`). A 2-million-token session and
a 20-thousand-token one fit the same ceiling.

## Budgets

| Budget | Ceiling | Gated on | Basis |
|---|---|---|---|
| Session, idle, headless | 12 MiB peak RSS | Linux x86_64 | picked |
| Terminal, idle | 9,944 KiB peak RSS | Linux x86_64 | from components |
| Session, busy or resumed | 46,720 KiB peak RSS | Linux x86_64 | measured |
| `web_fetch` converting a 10 MiB HTML page, the download cap | within the busy session's 46,720 KiB peak RSS | Linux x86_64 | measured |
| Idle CPU, session and terminal | zero context switches in the idle window, on every thread | Linux x86_64 | exact |
| Threads, idle headless session | 5, plus 2 per client, plus 1 per Lua extension in use | Linux x86_64 | exact |
| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact |
| Log bytes, 429-call turn | the turn's content plus 2 KiB per tool call | Linux x86_64 | exact |
| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked |
| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked |
| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked |
| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked |
| `session_list`, waiting for every running session's status | 2 s for the whole call | Linux x86_64 | picked |
| `paging` jig, its session at scale 1 and 160 by 48 | 17,592 KiB peak RSS | Linux x86_64 | measured |
| `paging` jig, open pass and first frame | 1,347 ms | Linux x86_64 | measured |
| `paging` jig, slowest frame that loaded pages | 7 ms | Linux x86_64 | measured |
| `paging` jig, slowest jump frame | 11 ms | Linux x86_64 | measured |
| `paging` jig, slowest re-count at a new width | 123 ms | Linux x86_64 | measured |
| `paging` jig, slowest append frame | 3 ms | Linux x86_64 | measured |

Basis says where a number came from:

- **From components** is the sum of measured parts, times two. Idle terminal: ratatui's two screen buffers and
  crossterm, with room for the visible part of the transcript (8 MiB), then 856 KiB,
  twice pulldown-cmark's 428 KiB on Linux x86_64, for rendering replies as
  markdown (`docs/dependencies.md`, "Runtime dependencies"), then 896 KiB,
  twice the 448 KiB the system root store costs when the TLS connector loads
  it (`crates/net`; Linux x86_64, release run 38052645277). The terminal's
  syntax highlighting is its own code and admits no crate. Admitting a
  crate for the terminal raises its ceiling by twice the crate's measured cost
  in the same pull request; its idle CPU and first
  frame budgets do not move. Busy session: a
  300,000-token context is about 1.2 MB of text, and a 2 MiB conversation
  added about 3 MiB in `research/delegate-memory/`.
- **Exact** follows from a rule, so the gate checks an equality, not a
  ceiling. The five threads every session runs are the loop, signals, accept,
  status and the printer for its stdout. Each client adds its reader and its
  writer (`docs/architecture.md`, "The threads"). A
  pull request that adds a thread changes this count and says why. Two fsyncs bracket each effect, and
  no line restates an earlier line in the same turn (`docs/events.md`,
  "Writing").
- **Picked** was chosen with no measurement behind it. The idle session's
  12 MiB is held as a placeholder: every runtime crate linked together costs
  7.4 MiB over an empty program on Linux x86_64 (`docs/dependencies.md`,
  "Measuring memory"), and twice that is 15 MiB, so a first measured run
  above 12 MiB sets this ceiling.
- **Measured** is a Fiber measurement times two. The first build that runs a
  benchmark replaces a "from components" or "picked" number with its measured
  one only where the measurement exceeds that number. Every other ceiling stays
  as it is, and a ceiling the measurement does not exceed keeps its basis. A
  ceiling given as a formula, such as attaching's, stays picked: its benchmark
  reports each measurement against the formula until the row's timing gate
  turns on.

The busy-or-resumed ceiling holds on three workloads:

- a turn of 429 tool calls with the context window full to the handoff point
- resuming a 20,000-token session
- resuming a 2,000,000-token session that has handed off 7 times

`web_fetch` holds at most one whole copy of the 10 MiB page: an HTML page
streams into its artifact and its converter in 64 KiB pieces, so only its
markdown (5.4 MiB) is held whole, and a text page is held once, as the
result, when it is valid UTF-8. The fetch alone peaks at a 21,664 KiB
footprint on macOS arm64, and the session's peak RSS on Linux x86_64 is
17,428 KiB (35,376 KiB before the page was streamed).

429 tool calls is the p99 of tool calls per user turn, measured on real
sessions in the archived Zig tree. That tree's session peaked at 2.1 GiB on
this turn.

Listing is one `sessions` command to the hub: it copies each running
session's latest `session_status` from the feed and reads `recent.jsonl` once
(`docs/invocation.md`, "The hub"). It opens no session log. `docs/state.md`
rules out a derived database, and the ruling stands while this budget holds.

## Measuring

Each number is the median of 5 runs. Memory is peak RSS on Linux and peak
footprint on macOS, by the method in `docs/dependencies.md`. Idle CPU is the
voluntary and involuntary context switch counts in
`/proc/<pid>/task/*/status`, read before and after the idle window: 10
seconds on a pull request, the "Release build and size" job included, and 60
in the release workflow (`docs/releasing.md`). A truly idle process switches
zero times in either. fsyncs are the session's `fdatasync` calls, counted
with strace in one more run of the busy turn, which is never a timing or
memory sample, and log bytes are the size of `events.jsonl`, so both are
exact.

Linux x86_64 gates every pull request, and the backstop on `main` measures
the same benchmarks. Linux arm64 and macOS arm64 are
measured at each release and reported, never gated: timings differ by about
20 times between macOS and Linux on I/O, and macOS memory counts system
frameworks Fiber does not control. Listing with a cold cache is reported,
never gated. Listing is timed from spawning `fiber sessions --json` in a git
repository whose project holds 1,000 exited sessions to its output closing,
with the hub running and one listing before the timed ones. Attaching is timed
from spawning `fiber resume <id>` in a pseudo-terminal to the session's last
reply on screen, for a live, idle session whose log is 1 MiB and one whose log
is 10 MiB, with the hub running. The same run splits each attach into stages,
reported as `attach_stage_ms` but never gated. `terminal` is the spawn-to-tail
time above; `hub_replay` replays it with no terminal, timing a `subscribe` at
`full` over a raw hub-socket client from the send to its acknowledgement.
`parse`, `fold` and `frames_1`, `frames_4096` and `frames_64` run in the
paging jig, built in the release profile, over the session's `events.jsonl`:
parsing each line, folding it into the app in batches of up to 4,096 lines, and drawing one frame, one frame
per 4,096 lines and one per 64 lines. The table also shows the residual, the
terminal time minus the hub replay, the parse, the fold and the single frame:
process start, home and the extra frames.

No test asserts a timing (`docs/testing.md`). The budgets are a benchmark job,
separate from the tests. The harness, `cargo run -p bench --example bench`,
measures a `fiber` binary and writes its results to a file, and
`cargo xtask bench-report` judges that file against the table above. The
`paging` rows run the jig at scale 1 and 160 by 48, built in the release
profile for the target that ships, and read its peak RSS from GNU time.

## When a budget is exceeded

An exact or memory budget fails the pull request from its first run. Its
author may raise the ceiling in the same pull request by editing the table
above with the new measurement and the reason. A budget the base commit
already fails, measured in the same job, does not fail the pull request: the
comment marks the row over at the base and names the base, and the backstop
on `main` fails on it, which makes the fix a backstop ticket.

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

Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md#from-docsperformancemd).
