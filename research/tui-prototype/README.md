# TUI prototype, stage 1

A throwaway ratatui program for [#15](https://github.com/aakshintala/fiber/issues/15). It replays a fixture session of `docs/events.md` lines in a real terminal, draws the ruled layout, and measures what the working line's glimmer costs. It is not a workspace member and nothing in Fiber depends on it.

## Running it

```sh
cd research/tui-prototype
cargo run --release -- fixtures/session.jsonl
```

| Flag | What it does |
|---|---|
| `--speed N` | plays the fixture's `ts` gaps N times faster; the default, 12, plays it in about 30 seconds |
| `--static` | loads the whole file at once |
| `--reduced-motion` | keeps the working line's word still; `FIBER_REDUCED_MOTION=1` does the same |
| `--stats FILE` | writes the measurement below to FILE on exit |
| `--exit-after S` | exits after S seconds |
| `--warmup S` | starts the measurement window after S seconds; default 2 |
| `--diff-audit` | also counts the cells and rows ratatui rewrites per frame (costs CPU, so the CPU runs leave it off) |

| Key or mouse | Does |
|---|---|
| `q`, Ctrl+C | quits, printing the resume line |
| Ctrl+O | opens or closes every ledger |
| click a group's summary line | opens or closes its ledger |
| wheel, ↑ ↓, Page Up, Page Down | scrolls the conversation; the wheel over the panel scrolls the panel |
| End, or click "↓ N lines below" | jumps to the end |
| click a notice | dismisses it |

`cargo run --bin gen` rewrites the fixtures from `src/bin/gen.rs`. `fixtures/session.jsonl` ends with a turn still running. `fixtures/idle.jsonl` is the same session cut after its last finished turn.

`./measure.sh [runs] [seconds]` runs the measurement below inside tmux at 160 by 48.

## What stage 1 covers

- A fixture of three turns: a short one answered in markdown (heading, bullets, a code block, a table); a long one with 44 tool calls over 15 steps, reads, searches, listings, edits with `changes`, shell calls with `process`, a failed edit and a failed command, a steer with `steering_queue` before it, a notice, an MCP server failure, a background job and a delegate whose lines are relayed; and a third turn still running, with a queued steer.
- Replay with the fixture's timing, deltas streaming in place, and `--static`.
- The ruled layout: a conversation column and a side panel of cards (Session, Changed files, Delegates, Jobs, and a stub Quota card); the input box under the conversation column only; one card per turn with the prompt as a tinted bubble on the right, at most 72% of the width, and its time under it; a steer as a labelled rule inside the card; a ▣ line closing each finished card.
- Tool groups: one dim summary line by kind, with +/− counts from `changes`, "thought N times" and the duration; the calls in flight while it runs, and "Thinking: <latest heading>" while reasoning streams. The ledger has one row per call, split by step, the step number in the gutter and the step's thinking line first. Thinking with no tool call before the next reply is one "+ Thought: <heading> · 14s" line.
- Surfaces as background tints with half-block edges and no borders, a ▌ stripe on the input box, replies at full text colour, nothing indented inside a card, and markdown as ruled.
- The fixed-width fallback: below 34 + 84 columns the panel goes away and the cards' summaries fill up to two status rows under the input box.
- The working line above the steering queue, with the glimmer, and reduced motion.
- Scrolling that follows new output, holds still while scrolled up, and jumps to the end.
- Notices stacked above the input box, dismissed with a click.
- The one-line resume message on exit.

## Not built

Stage 2 covers selection and copy, search, keyboard protocol detection, the approval and question panels and the full key map. Beyond those, stage 1 leaves out:

- clicking a ledger row to open that call's diff, output or error, and every panel item's view (model picker, context breakdown, usage, tools, a delegate's or job's transcript, a file's diff);
- the Delegates card scrolling on its own, and the Jobs card's list;
- the start page, the session list, handoff bands, the nudge, rewind, retries, a failed turn, crash recovery and interrupts, because the fixture has none of them;
- paging history from the log: the prototype folds the whole file into memory;
- the 256-colour theme: colours are the mock's "atelier" truecolour values, written into the code.

## The glimmer's cost

Measured on macOS arm64 (Darwin 25.6.0), inside tmux 160 columns by 48 rows, release build, median of 5 runs of 20 seconds each. The process counts its own output through a counting writer around stdout, its CPU time with `getrusage`, and its wakeups with `proc_pid_rusage` (`ri_pkg_idle_wkups` is what `top` shows as IDLEW). Timings do not generalise to Linux; byte and frame counts do.

| | Glimmer | Reduced motion | Idle, no turn running |
|---|---|---|---|
| Frames per second | 8.2 | 1.0 | 0 |
| Bytes written per second | 697 | 38 | 0 |
| Bytes per frame | 85 | 38 | none |
| CPU | 1.8% of one core | 0.24% | 0.000% |
| Involuntary context switches in 20 s | 171 | 21 | 1 |
| Idle wakeups (IDLEW) in 20 s | 0 | 0 | 0 |
| Interrupt wakeups in 20 s | 165 | 21 | 1 |

- The glimmer frame rewrites only one line. Over 163 frames ratatui's buffer diff changed 3 cells per frame on average, at most 6, and every frame touched exactly one row: the spinner and the three-cell band.
- About 6 of each frame's bytes are ratatui hiding the cursor, which it does after every draw, and each draw flushes twice (`hide_cursor` uses `execute!`, then the backend flushes).
- With reduced motion the line still changes once a second, for the elapsed time. The timer wakes at each whole second of the turn's clock.
- Idle has no timer: the loop blocks in `event::poll` with no deadline, so it made no frame, wrote no byte and had no idle wakeup.
- Each glimmer frame still re-fits every visible row into a fresh buffer and diffs all 7,680 cells, although the conversation's rows are cached between events. How much of the 1.8% that costs was not measured; a frame that repaints only the working line would cost less.
- The CPU of the terminal emulator drawing these bytes is not counted.

## Findings

### The rulings in a real terminal

- The ruling that only one short line redraws holds only if nothing else animates. The mock also spins the dot on a running group's summary line and on each running delegate in the panel. The prototype draws those as still glyphs (● and ○), and a running group's duration moves only when an event arrives. `docs/tui.md` should say which of these stay still.
- At 80 columns the Session card's summaries fill both status rows, so Changed files, Delegates, Jobs and Quota never appear. At 100 columns everything but Quota fits. The status line needs a width budget per card, or shorter Session summaries.
- The panel's 29 usable columns cut long paths in the Changed files card ("doors/tests/serve_attach.rs" leaves no room for its counts). Cutting from the left, keeping the file name, reads better.
- The person's bubble in the mock carries its stripe on the right (▐), while the Look ruling says the ▌ stripe on the left marks the person. The prototype follows the mock.
- tmux draws ▄, ▀, ▌ and ▐ as font glyphs and passes truecolour and SGR 2 (faint) through unchanged. Whether the stripe is one unbroken bar, and how faint text looks at Ghostty's `faint-opacity`, can only be checked in Ghostty itself.
- Dim text uses SGR 2, so it follows the terminal's own faint rendering rather than a colour mixed from the theme, as the mock does.

### Data the stream does not carry

- The handoff point. The context bar fills toward it and marks it, but only configuration holds it; `context_nudged.trigger_at` carries it only after a nudge. The prototype hard-codes 400k.
- The model's context window, for "12% of 1M". The prototype hard-codes 1M.
- The permission mode at the start. Only `mode_changed` names a mode, so a session that never changes mode never says which one it is in. The fixture includes a `mode_changed` so the card has something to show.
- Payload field names. `docs/events.md` describes most payloads in prose ("tokens (uncached input, input read from the cache, …)", "every steering message still queued, in order: the id of the `steer` command…"), so a client guesses the JSON keys. The fixture's keys (`tokens.input`, `tokens.cache_read`, `tokens.cache_write`, `messages[].id`, `steer_id`, `environment.git.branch` and others) are this prototype's guesses.
- Where a step starts. The ledger numbers steps. The prototype takes each `assistant_message_started` as a step boundary, which follows from "Writing" (the line is fsynced before every model request) but is not stated where the kinds are listed. The same rule decides grouping: a tool-only reply opens and completes an assistant message with no text, so a group can only tell whether a message is "a piece of assistant text" once its first non-empty delta arrives. `docs/events.md` should say that every model call opens with `assistant_message_started`, and that a reply with no text completes with empty text.
- An MCP server coming back. `mcp_server_failed` marks a server down, but a successful automatic restart writes no line, so the Session card's "✗ linear down" never clears until a reload.
- The git branch is not a finding: the ruling has the terminal run git itself. The prototype reads it from `opening_message` instead, since it runs no commands.
