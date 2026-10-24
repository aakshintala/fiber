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

Two rows hold work to the size of what it covers. Memory kept after 10 busy
turns, each followed by its handoff, stays within 1 MiB of the memory kept after one, so state that grows
with every tool call the session has ever made fails it. The `paging` jig's
scale-16 session is about 75 MiB, the size of the owner's longest sessions;
its re-count and search may take at most 24 times their scale-1 time in the same run, so the ratio, not the machine, decides. The page count is 16 times; the rest allows for cache capacity, which grows the time per page at 75 MiB while the work per page stays flat (#2187), and for runner noise: readings run about 20 to 20.6 times (#2640).

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

| Budget | Ceiling | Gated on | Basis | Fast ceiling | Middle ceiling | Slow ceiling | Fast tolerance | Slow tolerance |
|---|---|---|---|---|---|---|---|---|
| Session, idle, headless | 14,336 KiB peak RSS | Linux x86_64 | picked | — | — | — | — | — |
| Terminal, idle | 20,088 KiB peak RSS | Linux x86_64 | measured | — | — | — | — | — |
| Session, busy or resumed | 46,720 KiB peak RSS | Linux x86_64 | measured | — | — | — | — | — |
| `web_fetch` converting a 10 MiB HTML page, the download cap | within the busy session's 46,720 KiB peak RSS | Linux x86_64 | measured | — | — | — | — | — |
| Idle CPU, session and terminal | zero context switches in the idle window, on every thread | Linux x86_64 | exact | — | — | — | — | — |
| Threads, idle headless session | 5, plus 2 per client, plus 1 per Lua extension in use | Linux x86_64 | exact | — | — | — | — | — |
| fsyncs | 2 per model request, 2 per tool call | Linux x86_64 | exact | — | — | — | — | — |
| Log bytes, 429-call turn | the turn's content plus 2 KiB per tool call | Linux x86_64 | exact | — | — | — | — | — |
| Session start, the internal session command to its first line, no hub | 20 ms | Linux x86_64 | picked | 16 ms | 12 ms | 13 ms | 55 % | 9 % |
| Terminal to its first frame, new session | 50 ms | Linux x86_64 | picked | 7 ms | 8 ms | 11 ms | 18 % | 8 % |
| Terminal to its first frame, attaching | 50 ms plus 10 ms per MiB of session log | Linux x86_64 | picked | 1008 ms | 1101 ms | 1364 ms | 23 % | 4 % |
| Terminal, attaching a 10 MiB session | 45,512 KiB peak RSS | Linux x86_64 | measured | — | — | — | — | — |
| Session, memory kept after the busy turn | 16,176 KiB RssAnon | Linux x86_64 | measured | — | — | — | — | — |
| Session, memory kept after 10 busy turns, each followed by its handoff | the memory kept after the first, plus 1 MiB RssAnon | Linux x86_64 | picked | — | — | — | — | — |
| Terminal, memory kept after attaching a 10 MiB session | 12,048 KiB RssAnon | Linux x86_64 | measured | — | — | — | — | — |
| Listing 1,000 sessions in one project, warm cache | 50 ms | Linux x86_64 | picked | 32 ms | 34 ms | 41 ms | 21 % | 13 % |
| `session_list`, waiting for every running session's status | 2 s for the whole call, up to 1,000 running sessions | Linux x86_64 | picked | 1 ms | 1 ms | 72 ms | 0 % | 0 % |
| `paging` jig, its session at scale 1 and 160 by 48 | 17,592 KiB peak RSS | Linux x86_64 | measured | — | — | — | — | — |
| `paging` jig, open pass and first frame | 1,347 ms | Linux x86_64 | measured | 725 ms | 828 ms | 1049 ms | 11 % | 4 % |
| `paging` jig, slowest frame that loaded pages | 7 ms | Linux x86_64 | measured | 8 ms | 5 ms | 10 ms | 16 % | 4 % |
| `paging` jig, slowest jump frame | 11 ms | Linux x86_64 | measured | 9 ms | 9 ms | 13 ms | 11 % | 5 % |
| `paging` jig, slowest re-count at a new width | 123 ms | Linux x86_64 | measured | 67 ms | 77 ms | 106 ms | 16 % | 8 % |
| `paging` jig, slowest append frame | 3 ms | Linux x86_64 | measured | 7 ms | 4 ms | 6 ms | 56 % | 32 % |
| `paging` jig at scale 16, slowest re-count at a new width | 24 times the same run's scale-1 re-count | Linux x86_64 | picked | — | — | — | — | — |
| `paging` jig at scale 16, a search of the whole session with no match | 24 times the same run's scale-1 search | Linux x86_64 | picked | — | — | — | — | — |
| Terminal, wheel to frame, the `paging` jig's session at scale 1, 160 by 48 | 16 ms | Linux x86_64 | picked | 7 ms | 6 ms | 9 ms | 21 % | 12 % |
| Terminal, wheel to frame, one turn with an opened `read` of 300 lines, 160 by 48 | 16 ms | Linux x86_64 | picked | 5 ms | 4 ms | 7 ms | 40 % | 23 % |

The idle headless ceiling is 14 MiB of peak RSS. CI medians on main run 13,144 KiB, of which 3,220 KiB is RssAnon and the rest file-backed code pages (run 39924100235). The growth over
12 MiB is file-backed code pages; RssAnon is unchanged. Credential queries
require an admitted-model tracker and reader callbacks even before the first
query, so they can answer for a previously queued model switch. Label
enumeration is on demand; no picker or additional reader thread runs in a
headless session.

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
  reports each measurement against the formula, and the timing gate for the
  10 MiB fixture judges against the class ceiling instead.

The class columns hold the timing gates per runner speed class, keyed by CPU
model strings matched case-insensitively. A model's class is the slowest
cluster its stored runs reach. The fast class is `AMD EPYC 9V45 96-Core
Processor` (29 stored files, gates); the middle class is `Intel(R) Xeon(R)
Platinum 8370C CPU @ 2.80GHz` (2 stored files, advisory); the slow class is
`AMD EPYC 7763 64-Core Processor` (13 stored files, gates) and `AMD EPYC 9V74
80-Core Processor` (4 stored files, advisory). `INTEL(R) XEON(R) PLATINUM 8573C`
(1 stored file) has no class, and any other model, an empty string or a missing
CPU is unknown: all stay advisory, and the comment says why. A model gates
only with at least 5 stored files; turning another model on later is a pull
request that adds its rows. The attaching row's class columns judge only the
10 MiB fixture; the 1 MiB fixture stays advisory until #1546 lands a fix or a
re-ruled ceiling. Rows that are not absolute timings carry `—` in these
columns, and the ordinary Ceiling cell stays as advisory reference data.

Each class ceiling is the maximum individual stored sample for that metric
across every model in the class, rounded upward to the next whole
millisecond, so it covers every stored sample as well as every per-file
median. Each tolerance is `100 * (maximum per-file median / minimum per-file
median - 1)` for the gated model and row, rounded upward to a whole percent:
fast uses 9V45, slow uses 7763 only, so the advisory 9V74 never widens 7763's
tolerance. A per-file median under the 1 ms quantum counts as 1 ms,
so a zero median never divides the tolerance. Middle has no tolerance because no middle model gates. The fast
attaching ceiling and tolerance are pinned by a test against the 10 MiB
samples of the fast class's stored files
(`xtask/tests/class_data/9v45-attach-10mib.json`). The fast
session_list ceiling and tolerance are pinned by a test against the
plain-number samples of the fast class's stored files
(`xtask/tests/class_data/9v45-session-list.json`). `scripts/class-data`
regenerates that file from the `benchmarks-<sha>` artifacts of `main`
runs; entries whose artifacts have expired, and entries from non-main runs,
stay in the committed file by hand.

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

Each number is the median of 5 runs, except a pull request's timing-only
workloads and the `paging` jig, which are the median of 3. Every results file records the runner's CPU model.
Memory is peak RSS on Linux and peak
footprint on macOS, by the method in `docs/dependencies.md`. The retained rows
read RssAnon from `/proc/<pid>/status` 10 s after the work ends, once freed
pages have settled: 10 s after the 429-call turn completes, and 10 s after the
attached terminal shows the session's last reply. Their ceilings gate RssAnon,
not peak RSS. Idle CPU is the
voluntary and involuntary context switch counts in
`/proc/<pid>/task/*/status`, read before and after the idle window: 10
seconds on a pull request, the "Release build and size" job included, and 60
in the release workflow (`docs/releasing.md`). A truly idle process switches
zero times in either. The idle-session and idle-terminal benchmarks also report
RssAnon and RssFile from `/proc/<pid>/status` at the end of the idle window, as
diagnostics; the budget stays on peak RSS. fsyncs are the session's `fdatasync` calls, counted
with strace in one more run of the busy turn, which is never a timing or
memory sample, and log bytes are the size of `events.jsonl`, so both are
exact.

Linux x86_64 gates every pull request, and the backstop on `main` measures
the same benchmarks. On a pull request the base binary runs the timing
benchmarks only, and only the rows the diff selects (`docs/ci.md`, "Selection"); head and base
timings alternate in one harness process, by sample for every timing row, and rows
the base skips run on the head alone. Memory and exact rows compare against the backstop's
stored results for the base commit (`docs/ci.md`, "On every pull request
that changes code"). Linux arm64 and macOS arm64 are
measured at each release and reported, never gated: timings differ by about
20 times between macOS and Linux on I/O, and macOS memory counts system
frameworks Fiber does not control. Listing with a cold cache is reported,
never gated. Listing is timed from spawning `fiber sessions --json` in a git
repository whose project holds 1,000 exited sessions to its output closing,
with the hub running and one listing before the timed ones. Attaching is timed
from spawning `fiber resume <id>` in a pseudo-terminal to the session's last
reply on screen, for a live, idle session whose log is 1 MiB and one whose log
is 10 MiB, with the hub running. The same run records the attaching terminal's
peak RSS and its RssAnon 10 s after the last reply appears, for the 10 MiB
session; the busy turn records its session's RssAnon 10 s after the turn
completes. The same run splits each attach into stages,
reported as `attach_stage_ms` but never gated. `terminal` is the spawn-to-tail
time above; `hub_replay` replays it with no terminal, timing a `subscribe` at
`full` over a raw hub-socket client from the send to its acknowledgement.
`parse`, `fold` and `frames_1`, `frames_4096` and `frames_64` run in the
paging jig, built in the release profile, over the session's `events.jsonl`:
parsing each line, folding it into the app in batches of up to 4,096 lines, and drawing one frame, one frame
per 4,096 lines and one per 64 lines. The table also shows the residual, the
terminal time minus the hub replay, the parse, the fold and the single frame:
process start, home and the extra frames.

The harness also runs each timing workload and each busy workload once more,
in a home of its own whose `diagnostics.level` is `"debug"`, on the head
binary and only with `--only all`. It does not rerun the fsyncs pass, and this
run is never a timing or memory sample. It records, per workload, the run's
wall-clock time, each `op_timing` operation in the run's `logs/` files with its
count and total `ms`, and `provider_request`'s count and total `total_ms`
(`docs/state.md`), as `op_breakdown`. The `bench-report`
comment shows it under "Debug run"; it is reported, never gated.

No test asserts a timing (`docs/testing.md`). The budgets are a benchmark job,
separate from the tests. The harness, `cargo run -p bench --example bench`,
measures a `fiber` binary and writes its results to a file, and
`cargo xtask bench-report` judges that file against the table above. The
`paging` rows run the jig at scale 1 and 160 by 48, built in the release
profile for the target that ships, and read its peak RSS from GNU time.
The two scale-16 `paging` rows run the same jig at scale 16 and 160 by
48 in the same run, right after scale 1, so every scale-16 re-count and
absent search pairs with the same run's scale-1 sample. The gate takes
the median of the paired ratios, not the ratio of independent medians:
the ratio, not the machine, decides, and a base that is over its own
ratio never waives a head that is over.
The wheel rows burst 30 wheel-up reports, 8 ms apart, against `fiber resume`
in a pseudo-terminal at 160 by 48, in the `paging` jig's session at scale 1
and in one turn with a reply of 150 list items preceding an opened `read`
of 300 lines, of which Fiber draws the first 20 lines, and time each report
from its write to the frame showing the scrolled rows; each row holds the
slowest report's ms.

## When a budget is exceeded

An exact or memory budget fails the pull request from its first run. Its
author may raise the ceiling in the same pull request by editing the table
above with the new measurement and the reason. A budget the base commit
already fails, in the backstop's stored results for it, does not fail the
pull request: the
comment marks the row over at the base and names the base, and the backstop
on `main` fails on it, which makes the fix a backstop ticket.

A timing row gates per runner speed class when its runner's CPU model is
eligible, and stays advisory otherwise. The two scale-16
`paging` growth-ratio rows are the exception: like an exact or memory
budget, a paired ratio over 24 times its same run's scale-1 sample
fails the pull request on Linux x86_64, however the base compares.
GitHub's Linux x86_64 runners fall into three speed classes, whose
frames_64 medians across 26 stored results (October 11 to 13, 2026; 19 from
gated models) run about 1.49 to 1.65 times from the fast extremes to the slow
cluster: fast 302.150569 to 324.314527 ms, middle 446.141956 to 448.120506 ms
and the slow cluster 481.640278 to 497.654993 ms. The 9V74 fast VM at
384.165851 ms belongs to the slow class but stands outside the slow cluster's
range: a model's class is the slowest cluster its stored runs reach. Within
a model the frames_64 medians span 1.77% on 7763 and 7.34% on 9V45; the 9V74
fast VM is 22.80% faster than its own slowest VM (29.54% slower by the
opposite denominator). Across the 11 absolute timing rows the gated models'
max/min median spreads range from 3.22% to 55.83%: session start spans 8.47%
on 7763 and 54.37% on 9V45, listing spans 12.43% and 20.71% respectively, so
each row carries its own tolerance in the table. Budgeted numeric memory-row
medians across these files vary by about 1.0% to 7.0% depending on workload:
busy peak RSS spans 1.31% and web_fetch peak RSS 6.98%. A timing gate fails
only when the median is over its class's ceiling and over the base commit's
median from the same job by more than that tolerance, measured in the same
job on the same runner, and its author may raise the ceiling as
above. A model with fewer than 5 stored runs, an unknown CPU model, the
attach 1 MiB row and any row without a comparable same-job base timing stay
advisory, and the comment says why. Attach 1 MiB stays advisory until #1546
lands a fix or a re-ruled ceiling. A benchmark's
own self-check failing is a failed run, not a slow sample.

At each release, every ceiling more than twice its measured value drops to
measured times two. Ceilings never rise at release; only a pull request
raises one.

Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md#from-docsperformancemd).
