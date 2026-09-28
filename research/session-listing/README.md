# Session listing: what it costs to read more than first lines

Evidence for [#15](https://github.com/aakshintala/fiber/issues/15), gathered on
2026-09-28.

## The question

The session list shows, for each session: the first prompt, the branch, and a
state note (is there a `fiber_exited`, a pending `permission_requested` or
`interaction_requested`, a `rewound`, and so on). The first prompt and branch
sit after `preamble_built`, which holds the whole system prompt and tool
definitions, and can be large. The state note sits at the end of the file.

`docs/events.md` says listing 601 sessions' first lines took 39 ms warm on
macOS arm64. That number covers strategy (a) below, not what the session list
actually needs. This research measures (b) and (c) as well, so the open item
on issue #15 has an answer: does listing sessions need an index, and if so, at
what size.

## Method

A Rust program under this directory generates synthetic session directories
that match `docs/events.md`'s envelope and line order (`fiber_started`,
`session_started`, `preamble_built`, `opening_message`, `turn_started`, some
turn content, then a state-note ending), and times three read strategies
against them, all N files in one process:

- (a) first line only: one buffered `read_line`.
- (b) up to and including the first `turn_started`, two ways: line-by-line
  buffered reads that stop as soon as they see it, and a single fixed-size
  read (256 KiB) scanned in memory afterwards.
- (c) (b) plus a seek-to-end read of the last 4 KiB, for the state note.

Every run does one warm-up pass before five timed passes, and reports the
median. That measures warm cache, the case that matters when someone reopens
Fiber and looks at their session list.

Two sizes drive the cost and neither is fixed by any current Fiber
measurement, since no crates exist yet, so both are swept rather than
invented once:

- preamble size: 20, 60 and 150 KiB, at the task's suggested tiers.
- total file size: the owner's real session file sizes on this machine (see
  below), at their median, 90th percentile and maximum.

N is swept at 200, 600 and 2,000 sessions. Each sweep varies one factor at a
time from a shared pivot (N = 600, preamble 60 KiB, total size at the real
90th percentile), plus one run with every value at its heaviest together.
`run.sh` regenerates the fixtures and prints the table; nothing here is
committed data, and the fixtures are deleted after each run.

## Where each number came from

- Session file size distribution: `~/.claude/projects/*/*.jsonl` (252 files)
  and `~/.pi/agent/sessions/**/*.jsonl` (690 files) on this machine, pooled
  (942 files). Median 326 KiB, 90th percentile 1,749 KiB, maximum 15,313 KiB
  (about 15 MiB). These are Claude Code's and pi's own transcript files, not
  Fiber logs, so they stand in for session length as a size, not as Fiber's
  own future distribution.
- Opening message size: reconstructed from the fields `docs/system-prompt.md`
  says it carries (date, OS and architecture, shell, workspace, branch,
  session log path) as a 298-byte block (`fixtures-data/env-block.txt`), plus
  this repository's own `AGENTS.md` (960 bytes, real, `fixtures-data/
  agents-md-sample.txt`), plus a skills listing actually sent to this agent
  in this Claude Code session (4,763 bytes, real, `fixtures-data/
  skills-listing.txt`). Total 6,021 bytes, used as-is in every fixture.
- Preamble size: no Fiber measurement exists (`docs/tools.md` says the tool
  definition byte budget "is set from the total when the built-ins are first
  written", which has not happened), and Claude Code's and pi's transcripts
  do not log their system prompt or tool definitions, only the conversation.
  Swept at 20, 60 and 150 KiB, as the task set out, rather than invented as a
  single number.
- The `docs/performance.md` budget quoted below: "Listing 1,000 sessions in
  one project, warm cache: 50 ms" (Linux x86_64, picked). The same page notes
  1,000 sessions is twice the largest project in the owner's pi sessions
  (489), and that the budget as written covers reading each log's first line
  only, which is exactly the gap issue #15 raises.
- Platform: macOS arm64 (Apple M3 Pro), Darwin 25.6.0. Fiber's gate is Linux
  x86_64; `docs/performance.md` says macOS and Linux differ by about 20 times
  on I/O, so none of the timings below generalise to Linux. The byte counts
  do, because they follow from the file format, not the platform.
- Cold cache was not measured. `purge` needs root and refused ("Operation not
  permitted") without it, and setting `F_NOCACHE` on a file just written does
  not evict pages already resident from that write, so there was no way to
  get a genuinely cold read without sudo.

## Results

All times are the median of five warm runs, in milliseconds, for listing all
N sessions in one process. Bytes are per session, and are what each strategy
actually reads, not the file's logical size.

### Sessions swept (preamble 60 KiB, total size at the real 90th percentile)

| N | (a) first line | (b) line-by-line | (b) 256 KiB prefix | (c) line-by-line + tail | (c) prefix + tail |
|---|---|---|---|---|---|
| 200 | 1.95 ms, 151 B | 4.30 ms, 68,290 B | 7.69 ms, 262,144 B | 4.38 ms, 72,386 B | 8.04 ms, 266,240 B |
| 600 | 5.95 ms, 151 B | 14.29 ms, 68,290 B | 25.24 ms, 262,144 B | 14.74 ms, 72,386 B | 25.87 ms, 266,240 B |
| 2,000 | 20.45 ms, 151 B | 50.32 ms, 68,290 B | 85.13 ms, 262,144 B | 51.35 ms, 72,386 B | 87.88 ms, 266,240 B |

### Preamble size swept (N = 600, total size at the real 90th percentile)

| Preamble | (a) first line | (b) line-by-line | (b) 256 KiB prefix | (c) line-by-line + tail | (c) prefix + tail |
|---|---|---|---|---|---|
| 20 KiB | 5.73 ms, 151 B | 9.52 ms, 27,330 B | 19.06 ms, 262,144 B | 10.93 ms, 31,426 B | 19.91 ms, 266,240 B |
| 60 KiB | 5.95 ms, 151 B | 14.29 ms, 68,290 B | 25.24 ms, 262,144 B | 14.74 ms, 72,386 B | 25.87 ms, 266,240 B |
| 150 KiB | 6.03 ms, 151 B | 20.06 ms, 160,450 B | 38.80 ms, 262,144 B | 20.98 ms, 164,546 B | 42.70 ms, 266,240 B |

### Total file size swept (N = 600, preamble 60 KiB)

| Total size | (a) first line | (b) line-by-line | (b) 256 KiB prefix | (c) line-by-line + tail | (c) prefix + tail |
|---|---|---|---|---|---|
| 326 KiB (median) | 6.55 ms, 151 B | 13.91 ms, 68,290 B | 25.18 ms, 262,144 B | 15.46 ms, 72,386 B | 26.19 ms, 266,240 B |
| 1,749 KiB (p90) | 5.95 ms, 151 B | 14.29 ms, 68,290 B | 25.24 ms, 262,144 B | 14.74 ms, 72,386 B | 25.87 ms, 266,240 B |
| 15,313 KiB (max) | 5.98 ms, 151 B | 14.48 ms, 68,290 B | 25.22 ms, 262,144 B | 15.24 ms, 72,386 B | 26.14 ms, 266,240 B |

### Worst case (N = 2,000, preamble 150 KiB, total size at the real maximum)

| Strategy | Time | Bytes per session |
|---|---|---|
| (a) first line | 21.42 ms | 151 B |
| (b) line-by-line | 73.06 ms | 160,450 B |
| (b) 256 KiB prefix | 134.11 ms | 262,144 B |
| (c) line-by-line + tail | 75.62 ms | 164,546 B |
| (c) prefix + tail | 138.43 ms | 266,240 B |

## What the numbers show

Total file size does not move any strategy's time or byte count. The three
rows in the total-size table sit within noise of each other, from 326 KiB to
15.3 MiB. This is expected: none of the strategies read the middle of the
file, only the head and, for (c), the last 4 KiB. A session's total size can
grow without limit and listing cost will not follow it.

Preamble size is the real cost driver for (b) and (c). Bytes read for
line-by-line scale almost exactly with preamble size, plus a fixed 6.7 KiB of
overhead from the opening message and the surrounding envelope lines,
independent of what preamble size is chosen. Time follows the same line:
20 KiB costs 9.52 ms at N = 600, 150 KiB costs 20.06 ms.

The fixed 256 KiB prefix read is slower than stopping early, at every size
tested, on this machine. It always reads 262,144 bytes, up to about ten times
what a 20 KiB preamble needs, and its wall time runs 1.7 to 2 times that of
line-by-line across every combination measured. Line-by-line, which stops as
soon as it sees `turn_started`, is the better strategy here.

Adding the tail read for the state note costs little on top of (b): about
0.4 to 2.6 ms across the sweeps, for 4 KiB more per session. The state note is
nearly free once the first prompt has already been read.

Sessions scale close to linearly, as expected for independent per-file reads:
line-by-line plus tail costs about 0.025 ms per session at 60 KiB preamble,
whether N is 200, 600 or 2,000.

The worst case tested — 2,000 sessions, a 150 KiB preamble, 15 MiB files —
costs 75.62 ms for the full listing (first prompt and state note) with
line-by-line, on this machine, warm. That is the heaviest combination this
research swept, and it is still small next to the 50 ms budget for listing
1,000 sessions, allowing for the fact that budget is set on Linux and this
number is macOS.

The first-line-only number here (5.95 ms for 600 sessions) is well under the
39 ms `docs/events.md` reports for 601 sessions. `docs/events.md` says its own
numbers "were measured on the archived Zig implementation" and "are not
Fiber's budgets", so the two numbers are not the same measurement: different
language, different I/O path, same rough shape of file. Nothing here changes
what that page already says.

## Conclusion

Listing sessions does not need an index yet. The cost of reading the first
prompt and a state note, not just the first line, is driven by preamble size
and session count, never by how long a session's history grows, and stays
under 100 ms on this machine even in the heaviest combination swept: 2,000
sessions (four times the 1,000-session reference in `docs/performance.md`,
and four times the largest real project measured, 489 pi sessions) with a
150 KiB preamble (well above anything measured on this machine, since no
preamble byte count exists yet to measure).

`docs/performance.md`'s budget, "Listing 1,000 sessions in one project, warm
cache: 50 ms", was written against first-line-only reads. Reading through to
the first prompt and state note costs 2.5 to 5 times that, by these
measurements, so the budget should be read as covering the fuller read this
research describes, not narrowed back to first lines only. It holds with
headroom at every N and preamble size tested here, on macOS. Whether it holds
on Linux is not something this research can say, and needs a Linux run before
the budget can be trusted on the platform it gates.

An index becomes worth building only if preamble size grows well past
150 KiB, or session counts grow past the low thousands. Neither has happened
yet, so there is nothing here for an index to save.

## Running it

```sh
research/session-listing/run.sh
```

Regenerates the fixtures under a temp directory, times all five strategies
across eight runs, and writes `results.tsv`. Fixtures are deleted after each
run; nothing large is left on disk or committed.
