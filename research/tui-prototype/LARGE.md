# The TUI prototype on large sessions

Evidence for [#15](https://github.com/aakshintala/fiber/issues/15).

## The question

Stage 1 measured the glimmer's CPU on a small fixture: about 400 lines,
three turns (`README.md`, "The glimmer's cost"). It never measured memory,
and the fixture is far smaller than a real session. The prototype folds the
whole file into memory (`README.md`, "Not built"), while the ruled design
pages history from the session log (#15, "History is paged from the session
log"). This asks: on a session sized from the owner's real usage, what does
folding the whole file cost in memory, load time and CPU, and how does that
cost scale with the session's size?

## Where the sizes come from

Three kinds of evidence, kept separate as the ticket asked:

- **How many prompts and tool calls.** `research/tui-surface/README.md`
  (Claude Code column), read from `usage.py` over the owner's
  `~/.claude/projects/*/*.jsonl` main sessions: prompts per session, median 5
  / p90 17; tool calls per prompt, median 5 / p90 28 / p99 101; tool calls
  between two pieces of assistant text, median 2 / p90 6 / max 141.
- **Tool result sizes.** `research/tool-result-sizes/README.md`, from 648 pi
  sessions: median, p90 and p99 bytes per tool, and the call counts that give
  the tool mix. grep, ffgrep and find are folded into the shell tool, since
  `docs/dependencies.md` says that is what actually runs them.
- **Assistant text and reasoning lengths.** Not measured by `usage.py`, so
  measured for this ticket, over the same Claude Code main sessions: assistant
  text, median 193 / p90 2,378 / p99 4,788 / max 9,262 characters; reasoning,
  median 249 / p90 359 / p99 489 / max 830 characters, present on about one
  step in ten (702 reasoning blocks over 6,790 text blocks).

pi's own numbers differ enough to note. Its reasoning blocks are far
longer and far more numerous than its text blocks (median 185 / p90 1,202 /
max 31,431 characters, 12,654 blocks against 3,117 text blocks), against
Claude Code's reasoning running short and rare by comparison. The three
sessions below are built from the Claude Code column throughout, so that one
harness's shape is not mixed with another's variance; pi's numbers are given
here for comparison, not used in generation.

The longest real session, by tool-call count, is a Claude Code session with
984 tool calls (measured for this ticket, over the same sessions; the next four
are 698, 637, 526 and 422). The heavy fixture is built to land near that
figure.

## The three sessions

`src/bin/gen_large.rs` writes three fixtures to `fixtures/`, deterministic
under a fixed seed. Every line is a **durable** `docs/events.md` line only —
the session log holds no ephemeral line (`docs/events.md`, "Durable and
ephemeral"), so unlike `gen.rs`'s fixture there is no delta streaming and no
live-replay timing to preserve. A tool call's result size is drawn from a
lognormal fitted to its median and p90 (`research/tool-result-sizes`), capped at
three times its p99 so one draw cannot run away; the tool mix is weighted by
the call counts in that table. Each turn's tool-call count is drawn the same
way from the tool-calls-per-prompt figures, and tool calls run in batches of
one to four between pieces of assistant text, sized from the calls-between-text
figures. Reasoning opens one step in ten. Assistant text length is drawn from
its own fit, capped at the measured max.

| Session | Built as | Prompts | Tool calls | Lines | Bytes |
|---|---|---:|---:|---:|---:|
| Median | 5 prompts (Claude Code median), tool-call count per prompt fitted to median 5 / p90 28 | 5 | 47 | 316 | 242,387 (0.23 MiB) |
| p90 | 17 prompts (Claude Code p90), same per-prompt fit | 17 | 277 | 1,741 | 1,305,902 (1.25 MiB) |
| Heavy | prompts drawn centred on p99 (101), until the total reaches 984, the longest real session found | 10 | 1,047 | 6,135 | 4,578,247 (4.37 MiB) |

None is over 5 MiB, so all three are committed; `measure_large.sh`
regenerates them from the same seed if they are ever deleted.

Not modelled: delegates, jobs, background launches, approvals and question
forms. They sit in a minority of sessions (26 to 41% by kind,
`research/tui-surface/README.md`) and are a small fraction of any one
session's lines; the memory this page measures is dominated by ordinary
tool calls and text, which the fixtures do carry.

## Results

Measured on macOS arm64 (Darwin 25.6.0, Apple M3 Pro), release build, inside
tmux 3.6b at 160 by 48. Every figure is the median of 5 runs.

### Load time

macOS timings only; they do not generalise to Linux (`docs/performance.md`,
"Measuring").

| Session | Time to first frame, cold start | Time to load the whole file, `--static` |
|---|---:|---:|
| Median | 1.77 ms | 3.45 ms |
| p90 | 5.03 ms | 10.36 ms |
| Heavy | 14.04 ms | 25.13 ms |

A cold start applies no event before drawing the first frame, so that column
is near-constant across sessions; it is the terminal's own setup cost. The
`--static` column applies every event first, so it is the whole file's load
time plus that same setup cost.

### Peak memory

macOS reports peak memory footprint; `docs/dependencies.md` says that, not
RSS, is the macOS figure to use, because macOS RSS also counts shared
system framework pages. Maximum RSS is given alongside, labelled, for
comparison; neither is the Linux peak RSS the budget in `docs/performance.md`
is gated on. `--static`, idle for 10 seconds at the end.

| Session | Peak memory footprint | Max RSS |
|---|---:|---:|
| Median | 4.89 MiB | 5.92 MiB |
| p90 | 12.33 MiB | 13.36 MiB |
| Heavy | 36.39 MiB | 37.39 MiB |

### CPU

`--static`, `--stats` over a 10-second window after a 2-second warmup.
Scrolling is driven by wheel-up events at 10 Hz (SGR mouse, `tmux send-keys
-H`); searching is Ctrl+F, typing "tool" (common in the fixtures' filler
text), then jumping matches with Enter every 0.2 seconds; the live replay
runs without `--static`, at the tool's default 12x speed, over the same
10-second window (the sessions run far longer than that at 12x, so this is a
window onto steady-state replay, not the whole thing).

| Session | Idle | Scrolling | Searching | Live replay |
|---|---:|---:|---:|---:|
| Median | 0.001% | 1.18% | 0.83% | 3.85% |
| p90 | 0.001% | 4.70% | 3.32% | 3.92% |
| Heavy | 0.001% | 9.16% | 6.31% | 6.76% |

Idle CPU on a fully loaded, `--static` session is the same as stage 1 found
on the small idle fixture: no timer, no wakeup, nothing to redraw, whatever
the session's size.

Scrolling and searching both cost more CPU as the session grows, at a
similar rate: heavy costs about 8 times median's scrolling CPU and about 8
times its searching CPU, for 22 times the tool calls. Each redraw re-fits
and re-diffs every visible row from the whole in-memory conversation, so a
bigger conversation costs more per frame even though the frame rate itself
(about 7.5 fps scrolling, about 4.3 fps searching) barely moves. Live replay
scales more gently between median and p90 because both play back at the
same 12x speed and this is a fixed 10-second window onto that replay, not
the whole file; heavy's replay window catches more, and denser, events in
the same 10 seconds, so it costs more and draws more frames.

## Where the memory goes

Peak memory footprint scales with the log, not with a fixed cost. Median,
p90 and heavy hold 316, 1,741 and 6,135 durable lines and cost 4.89, 12.33
and 36.39 MiB of footprint: the heavy session holds 19 times the median
session's lines and costs 7.4 times its memory.

Fitting a line through the median and heavy points gives the marginal cost:
each extra 1,000 durable events costs about 5.4 MiB of resident memory, and
each extra mebibyte of the log on disk costs about 7.6 MiB of memory once
loaded. Projecting that line to p90's line count predicts 12.6 MiB against
the 12.33 MiB measured, within 2%, so a straight line is a fair model of
this design's memory cost across the range measured. The fixed part of that
line — the cost of an almost-empty session, terminal setup included — is
about 3.2 MiB.

That multiple, about 7.6 MiB of memory per MiB of log, is the tax of
`serde_json::Value` plus the prototype's own `Fold` structures: each event's
JSON is parsed into an owned tree, and the conversation's rendered rows are
kept alongside it, so a byte on disk becomes several bytes resident.

The ruled design pages history from the log instead of folding it. A paged
client would hold a window of rendered rows — enough to fill the visible
conversation column, a small multiple of the screen's height, not the whole
session — plus whatever index it keeps to seek back further. That cost is
roughly constant as the session grows, where the prototype's is not: the
prototype's memory keeps growing for as long as the session does, and a
session ten times the size of the heavy one measured here would cost
around ten times its memory, not the same window of rows a paged design
would still need.

## Against the idle terminal budget

`docs/performance.md`'s "Terminal, idle" budget is 8 MiB peak RSS, gated on
Linux x86_64. That is a different measure from what this page reports:
macOS reports peak memory footprint, which excludes shared system framework
pages that macOS RSS counts (`docs/dependencies.md`, "Measuring memory").
Linux was not measured here at all. So the figures above are not directly
comparable to the 8 MiB budget, and this page does not claim to pass or fail
it.

What can be said, numbers set side by side rather than compared as like for
like: the median session's macOS footprint (4.89 MiB) sits under the 8 MiB
figure, at 61% of it. The p90 session's footprint (12.33 MiB) is already
1.5 times that figure, and the heavy session's (36.39 MiB) is 4.5 times it.
A typical session (median tool-call count) leaves headroom against this
number; a session with as many prompts as the owner's p90 Claude Code
session does not, and the heaviest real session found is well past it. None
of this says whether Linux peak RSS, the figure the budget actually gates,
would cross 8 MiB at the same session sizes — that would need measuring on
Linux, which this page has not done.

## Conclusions

Folding the whole session log into memory does not scale. Memory grows with
the log, at about 7.6 MiB resident per MiB of log, because every event's
JSON is parsed and every rendered row is kept. The heavy session, sized to
the longest real session found, costs 36.39 MiB of macOS footprint; the
median session, at 4.89 MiB, is the only one of the three that would fit
under the 8 MiB Linux idle-terminal budget if that macOS figure were the
same measure, which it is not.

CPU stays cheap while idle — 0.001% of a core regardless of session
size, matching stage 1's idle finding — and load itself is fast: 25.13 ms
to fold and draw the heavy session's 6,135 lines. Scrolling and searching
do cost more as the session grows (up to 9.16% and 6.31% of a core on the
heavy session), because every redraw re-fits and re-diffs the whole
in-memory conversation, not just the visible rows. The problem #15's ruled
design addresses is memory, not CPU: paging history from the log bounds
memory to a window of rows instead of the whole session, and would likely
cut the scrolling and searching cost too, since a paged client would have
far fewer rows to re-fit per frame.

## Numbers chosen without evidence

- Placeholder prompt text and file, command and URL names in the generated
  fixtures: not measured, cosmetic only.
- Batch size of tool calls between two model round trips: one to four,
  uniformly, not evidenced beyond the calls-between-text figures that bound
  the overall batching.
- Mid-turn assistant text length is clamped to 10 to 400 characters (the
  final reply of each turn uses the full measured distribution instead);
  not evidenced, chosen to keep interim updates shorter than final replies.
- Tool result and reasoning size draws are capped at three times the
  measured p99, so one lognormal draw cannot dominate a whole fixture; the
  cap itself is not evidenced.
- The heavy session's per-prompt spread around p99 uses a smaller sigma
  (0.3) than the population fit, so the target is reached over about ten
  heavy prompts rather than one or two freak ones; this shape is a choice
  to match "p99 per prompt, many prompts", not a measured spread.
- Edit calls' added and removed line counts (1 to 25 added, 0 to 15
  removed): not evidenced.
- The scroll rate (10 Hz) and the search jump rate (one match every 0.2
  seconds): stated, as asked, but chosen, not measured against how fast a
  person actually scrolls or searches.
- Using the Claude Code column throughout, rather than pi's, for internal
  consistency: a judgement call, not a measurement; pi's very different
  reasoning-length shape is reported above but not used.
