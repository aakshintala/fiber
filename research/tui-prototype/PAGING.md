# Paging history in the TUI prototype

Evidence for [#15](https://github.com/aakshintala/fiber/issues/15). This is a narrow probe, not a design.

## The question

#15 rules that history is paged from the session log: the TUI holds a window of rendered history and reads the log again as the person scrolls. `docs/events.md`, "Resume", gives the primitive: an offset table over the file and range reads by `seq`. The prototype folds the whole log into memory instead, and `LARGE.md` found that costs 36 MiB of macOS footprint on the heavy session, against an 8 MiB idle-terminal budget on Linux (`docs/performance.md`).

The probe asks five questions:

1. How much memory does a window of rendered history save, and what must stay resident for the panel?
2. The scroll bar needs the total row count, which depends on the width. Should the TUI measure every row at open, or estimate and correct?
3. When scrolling reaches history that is not rendered, is there a visible stall?
4. Can search match the rendered text of the whole log without holding it?
5. Does a selection stay complete and in order when it crosses pages that load and drop during the drag?

## What the probe built

`--paged` opens a finished session paged instead of folded. The whole-file mode is still the default, so every earlier measurement reproduces.

```sh
cd research/tui-prototype
cargo run --release -- fixtures/large-heavy.jsonl --paged
```

The code is `src/paged.rs`, with hooks in `src/main.rs`.

### Opening: one streaming pass

The pass reads the log line by line and keeps:

- the byte offset of every line, 8 bytes a line
- the first line of every turn
- where each page starts and ends
- the panel's folds

The panel's folds come from the ordinary fold code. Each event is applied and then trimmed at once: tool output, reply text, reasoning text and a finished turn's blocks are dropped. No event is kept. What stays resident for the panel is:

- the Session card: working directory, mode, model, effort, branch, tool count, MCP servers down, token totals, cost, speed and the turn count
- Changed files: one entry per file with its added and removed lines (88 files in the heavy session)
- Delegates and Jobs: one entry per job
- the steering queue, requests waiting on the person and notices
- the running turn's blocks, if a turn is still running

These grow with the number of files and jobs, not with the log. A unit test checks that the panel drawn from this pass is identical to the panel drawn from the whole-file fold on `fixtures/session.jsonl`, which has delegates, a job, an approval, a form and a steering queue.

### Pages

A page is a run of lines inside one turn. A page closes at an `assistant_message_started` line once it holds at least 64 lines, and only where no tool group, call, reasoning or message is open. So a page folds on its own and never splits a tool group. A turn's first page draws the bubble and the card's top edge, and its last page draws the bottom edge.

Pages are keyed by line number, which in these fixtures is `seq` less one. Fiber would key them by `seq`.

A unit test checks that the pages' rows, joined, are exactly the rows the whole-file fold renders, on three fixtures, at two widths, with 8 and 64 lines a page, and with every ledger open. The bench checks the same on all three large fixtures.

### The window

The pages holding the visible rows, and `--window` screens of rows above and below, are kept folded and rendered. Every other page is dropped. Missing pages load inside the frame that needs them: a range read, a fold and a render. The probe swept 0, 1 and 4 screens.

A group opened by a click is remembered by page and block, so its page loads again as the person left it.

### Row counts

After the open pass, a count pass reads every page, folds it, renders it at the current width, and keeps only its row count. That is option (a) below. It runs again when the width changes, on Ctrl+O, and when the search query changes. Row counts and their prefix sums give every row a stable global index, so the viewport, search matches and selection all use one coordinate.

Option (b), estimating counts, was not built into the TUI. It is simulated in `--paging-bench` from the real counts: see question 2.

### Search

Search is a pass like the count pass that also records each match as page, row in the page, first cell and width: 32 bytes a match. It runs on every keystroke that changes the query. Matching is on rendered rows, as in the whole-file mode.

### Selection

The selection's ends are global rows. On copy, the rows between them come from resident pages, and any page dropped since the drag began is read and rendered again.

### The jump overlay

With `--paged`, the overlay reads "↓ New messages below · End", with no count, as the owner ruled.

## Results

Measured on macOS arm64 (Darwin 25.6.0, Apple M3 Pro), release build, inside tmux 3.6b at 160 by 48. Every figure is the median of 5 runs. `./measure_paging.sh` runs everything. Timings are macOS only and do not carry over to Linux. Row, page and match counts do.

The conversation is 124 cells wide and 45 rows high at 160 by 48.

| Session | Lines | Turns | Pages | Rows, groups closed | Rows, every ledger open |
|---|---:|---:|---:|---:|---:|
| Median | 316 | 5 | 7 | 193 | 242 |
| p90 | 1,741 | 17 | 33 | 1,364 | not measured |
| Heavy | 6,135 | 10 | 85 | 2,917 | 4,001 |

A page renders to 18 to 35 rows at the median, and at most 178 rows (p90, a long reply).

### 1. Memory

Peak memory footprint and maximum RSS from `/usr/bin/time -l`, over a run that opens the session at the end and pages up to the top at 20 presses a second. The whole-file mode is `--static`. Footprint is the macOS figure `docs/dependencies.md` says to use. Neither is the Linux peak RSS the budget is gated on, and Linux was not measured.

Peak memory footprint, MiB:

| Session | Whole file | Paged, window 0 | Paged, window 1 | Paged, window 4 |
|---|---:|---:|---:|---:|
| Median | 5.44 | 4.53 | 4.64 | 4.63 |
| p90 | 13.17 | 5.88 | 6.17 | 6.73 |
| Heavy | 36.69 | 6.23 | 6.64 | 7.38 |

Maximum RSS, MiB:

| Session | Whole file | Paged, window 0 | Paged, window 1 | Paged, window 4 |
|---|---:|---:|---:|---:|
| Median | 6.59 | 5.72 | 5.83 | 5.81 |
| p90 | 14.33 | 7.06 | 7.34 | 7.91 |
| Heavy | 37.80 | 7.41 | 7.83 | 8.55 |

Most pages kept at once: 3 or 4 at window 0, 6 or 7 at window 1, 7 to 15 at window 4.

Page size, heavy session, window 1:

| Lines a page, at least | Pages kept at most | Peak footprint | Frames that loaded pages |
|---|---:|---:|---:|
| 16 | 14 | 6.55 MiB | 67 |
| 64 | 7 | 6.64 MiB | 65 |
| 256 | 3 | 7.27 MiB | 27 |

The open pass on macOS arm64:

| Session | Index and panel pass, warm | Index and panel pass, in the TUI, cold | Whole-file load, warm |
|---|---:|---:|---:|
| Median | 0.73 ms | 1.42 ms | 0.92 ms |
| p90 | 3.51 ms | 5.83 ms | 5.61 ms |
| Heavy | 12.12 ms | 18.65 ms | 17.57 ms |

"Warm" is inside `--paging-bench`, after one discarded pass: the first pass in a process pays for page faults and the allocator's growth, whichever pass it is. "Cold" is the TUI's own first pass. The whole-file load reads, parses, folds and renders everything.

### 2. The scroll bar

Option (a), every page rendered at the width once, keeping only its count, macOS arm64, warm:

| Session | At open | On a resize to 120 columns | On Ctrl+O |
|---|---:|---:|---:|
| Median | 1.15 ms | 1.02 ms | 1.04 ms |
| p90 | 6.13 ms | 5.89 ms | 6.24 ms |
| Heavy | 17.86 ms | 17.40 ms | 18.71 ms |

Time to first frame in the TUI, which includes the terminal's setup:

| Session | Whole file, `--static` | Paged |
|---|---:|---:|
| Median | 2.67 ms | 4.97 ms |
| p90 | 9.74 ms | 16.48 ms |
| Heavy | 24.53 ms | 40.48 ms |

Option (b), each page's rows estimated from its bytes or its lines, at the ratio of rows to bytes or lines over the pages loaded so far, and corrected as each page loads. It opens at the end and scrolls to the top one row at a time, keeping the top row still, as the TUI's anchor does. The bar is 45 cells. With exact counts the thumb moves at most one cell a row and is always where it belongs.

| Session | Estimated from | Window | Total's error at open | Largest move in one row | Moves of more than a cell | Moves down while scrolling up | Furthest from where it belongs |
|---|---|---:|---:|---:|---:|---:|---:|
| Median | bytes | 0 | +155% | 17 cells | 5 | 3 | 10 cells |
| Median | lines | 0 | +43% | 5 cells | 3 | 3 | 4 cells |
| Median | bytes | 1 | +63% | 9 cells | 3 | 3 | 6 cells |
| Median | lines | 1 | +8% | 3 cells | 1 | 1 | 2 cells |
| Median | either | 4 | 0% | 1 cell | 0 | 0 | 0 cells |
| p90 | bytes | 0 | −30% | 4 cells | 7 | 6 | 11 cells |
| p90 | lines | 0 | −5% | 2 cells | 1 | 3 | 10 cells |
| p90 | bytes | 1 | +38% | 4 cells | 6 | 6 | 10 cells |
| p90 | lines | 1 | +77% | 2 cells | 2 | 1 | 9 cells |
| p90 | bytes | 4 | +44% | 3 cells | 3 | 2 | 8 cells |
| p90 | lines | 4 | +77% | 2 cells | 2 | 1 | 7 cells |
| Heavy | bytes | 0 | +81% | 1 cell | 0 | 9 | 3 cells |
| Heavy | lines | 0 | +17% | 1 cell | 0 | 6 | 3 cells |
| Heavy | bytes | 1 | +20% | 1 cell | 0 | 6 | 3 cells |
| Heavy | lines | 1 | +19% | 1 cell | 0 | 1 | 3 cells |
| Heavy | bytes | 4 | +12% | 1 cell | 0 | 9 | 3 cells |
| Heavy | lines | 4 | +18% | 1 cell | 0 | 3 | 3 cells |

At window 4 the median session's whole history is loaded at open, so the estimate is never used.

### 3. Scrolling into history that is not rendered

From the Page Up key to the last byte of the frame, for every frame that loaded pages, macOS arm64:

| Session | Window | Frames that loaded pages | Of those, frames whose visible rows were not loaded | Median, key to frame | Slowest, key to frame |
|---|---:|---:|---:|---:|---:|
| Median | 0 | 5 | 5 | 2.57 ms | 2.69 ms |
| Median | 1 | 3 | 1 | 3.29 ms | 3.29 ms |
| p90 | 0 | 25 | 25 | 2.17 ms | 3.43 ms |
| p90 | 1 | 25 | 1 | 2.22 ms | 3.43 ms |
| p90 | 4 | 23 | 1 | 2.26 ms | 3.43 ms |
| Heavy | 0 | 65 | 65 | 2.48 ms | 4.57 ms |
| Heavy | 1 | 65 | 1 | 2.42 ms | 4.15 ms |
| Heavy | 4 | 61 | 1 | 2.51 ms | 3.93 ms |

The one frame with unloaded visible rows at windows 1 and 4 is the first frame. The slowest frame in any paged scrolling run, over every session, window, page size and run, took 5.14 ms from key to frame.

One page loads in 0.15 to 0.20 ms at the median, and 0.43 ms at the slowest, measured over every page of each fixture in `--paging-bench`.

### 4. Search

A search of the whole log, streaming it and rendering each page to text without keeping it, macOS arm64, warm. The whole-file figure renders the whole fold again with the query and matches every row, which is what the whole-file mode does when the query changes.

| Session | Word | Matches | Memory for matches | Paged search | Whole-file search | Jump to the first match |
|---|---|---:|---:|---:|---:|---:|
| Median | tool | 42 | 1,344 bytes | 1.06 ms | 0.38 ms | 0.52 ms |
| Median | flaky | 1 | 32 bytes | 1.05 ms | 0.37 ms | 0.51 ms |
| p90 | tool | 347 | 11,104 bytes | 6.75 ms | 3.05 ms | 0.49 ms |
| p90 | flaky | 2 | 64 bytes | 6.69 ms | 2.96 ms | 0.50 ms |
| Heavy | tool | 633 | 20,256 bytes | 19.59 ms | 6.65 ms | 0.84 ms |
| Heavy | flaky | 1 | 32 bytes | 19.88 ms | 6.54 ms | 0.82 ms |

"tool" is common in the fixtures' filler text. "flaky" is rare: it is only in the first turn's prompt, so its match is the one furthest from the end. The jump loads the match's page and a screen either side: 3 or 4 pages. A common word and a rare word cost the same, because the pass renders every page either way.

In the TUI, typing the word a key at a time into Ctrl+F, macOS arm64:

| Session | Mode | Peak footprint, "tool" | Peak footprint, "flaky" | Slowest key to frame |
|---|---|---:|---:|---:|
| Median | whole file | 5.63 MiB | 5.69 MiB | not measured |
| Median | paged | 5.05 MiB | 4.72 MiB | 7.40 ms |
| p90 | whole file | 14.77 MiB | 14.44 MiB | not measured |
| p90 | paged | 5.59 MiB | 5.39 MiB | 18.18 ms |
| Heavy | whole file | 38.80 MiB | 38.48 MiB | not measured |
| Heavy | paged | 6.25 MiB | 5.92 MiB | 40.72 ms |

The slowest paged frame is a keystroke: it runs the whole search pass, then loads the pages around the first match. The jump itself is under a millisecond.

### 5. Selection across pages

The unit test `a_selection_across_pages_copies_whole_and_in_order_after_its_pages_are_dropped` builds a paged source over the median fixture with small pages. It starts a selection in a page on screen, moves the window five pages down so that page is dropped, and copies. It checks the text against the same selection over the whole-file rows, forwards and backwards, and again for a selection running up into pages that were not loaded when the drag began.

In tmux, on the heavy session with window 0, the check pressed the button 30 rows up from the bottom, dragged to the top edge and held it for 4 seconds of auto-scroll, then released. In all 5 runs:

- the copy matched the same selection over the whole file folded at once (`--verify-copy`)
- it was 129 lines
- at most 3 pages were loaded at any time, so the pages the drag began in had been dropped
- the clipboard, read back with `pbpaste`, held exactly the copied text

## Findings for docs/tui.md

### The window

- Paging bounds memory. On macOS arm64 the heavy session's peak footprint fell from 36.7 MiB to 6.2 to 7.4 MiB, and p90's from 13.2 MiB to 5.9 to 6.7 MiB. From the median session to the heavy one, 19 times the lines, paged footprint grew by 1.7 to 2.8 MiB, where the whole-file mode grew by 31 MiB.
- Keep the visible pages and one screen either side. A wider window costs memory and bought nothing measurable: loading is fast enough that at window 0 the slowest frame took 4.6 ms from key to frame, median of 5 runs. Window 1 means a page is usually loaded before it shows.
- Loading in the frame is fast enough. The slowest frame that loaded pages took 5.1 ms from key to frame on macOS arm64, a third of a 16 ms frame. A page is about 0.2 ms. No background loading is needed at these sizes.
- Page size hardly matters between 16 and 64 lines. At 256 lines each page holds more rows, peak footprint rose by 0.6 MiB and the slowest frame by 0.6 ms.
- Cut pages inside turns, not at turns. A heavy prompt is about 600 lines, so a turn is too coarse a page. Cutting only where no tool group, call, reasoning or message is open keeps every page foldable on its own, and the pages join into exactly the whole-file rows.
- The panel needs no history. Its cards are folds over the whole log, and one streaming pass that drops each event's text once applied keeps them. It took 18.7 ms on the heavy session in the TUI, macOS arm64.
- Some state must live outside the window: a group opened by a click, kept by page and block here. Fiber would key it by the call's action id.

### The scroll bar

- Measure every page's rows exactly, option (a). It costs one render of the whole log at open and on each resize or Ctrl+O: 17 to 19 ms on the heavy session, macOS arm64, about the same as the whole-file load. It makes every row's index stable, which search matches, the selection and the anchor all rely on.
- Estimating, option (b), is wrong in ways a person can see on short sessions. The thumb moved up to 17 cells in one row on the median session, and sat up to 11 cells of 45 away from where it belonged on the p90 session. Lines were a better estimator than bytes, since tool output takes bytes but only one ledger row. Neither was reliable.
- (a) makes the first frame slower: 40 ms against 25 ms on the heavy session, macOS arm64, because the probe reads and parses the log twice. Counting rows in the open pass itself would drop the second parse. Fiber could also draw the first screen from the last pages before the count finishes, since the viewport opens at the end.
- The cost of (a) grows with the log, at about 4 ms per MiB on macOS arm64. A session ten times the heavy one would pay about 180 ms on each resize. That is where caching counts per width, or counting off the frame thread, would be needed; this probe did not reach it.

### Search

- Search the whole log by streaming it, rendering each page to text and keeping only the matches. Matches cost 32 bytes each: 20 KB for 633 matches on the heavy session. Memory stays flat: 6.3 MiB paged against 38.8 MiB whole-file on the heavy session, macOS arm64.
- It is slower than searching rows already in memory: 19.6 ms against 6.7 ms on the heavy session, macOS arm64, and a rare word costs the same as a common one. Run on every keystroke, it stalls: the slowest keystroke took 40.7 ms from key to frame on the heavy session and 18.2 ms on p90. The TUI should not search on every keystroke of a large session. Waiting for a pause in typing, or searching off the frame thread and showing matches as they arrive, would each fix it; neither was built.
- Jumping to a match outside the window is cheap: under 1 ms to load the match's page and a screen either side.
- A query of three characters or more opens every group with a match, which changes row counts, so the search pass produces the counts too. Closing search needs another count pass unless the counts for no query are kept, which costs one number per page.

### Selection

- Anchor a selection's ends to stable row indices, and read the rows between them at copy time, rendering again any page dropped since the drag began. It copied complete and in order in the unit test and in every tmux run, with the drag's first pages dropped.
- This needs exact row counts. With estimated counts a row's index moves when a page above it loads, so the ends would have to be kept as a page and a row in it instead.

## What the probe did not answer

- Live sessions. The probe opens finished sessions only. Appending to the last page while a turn runs, and when to cut a page during a turn, were not built.
- Linux: no timing and no peak RSS was measured there. The paged footprints sit under 8 MiB on macOS, but that is not the Linux figure the budget gates.
- What makes up the paged mode's 4.5 to 7.4 MiB. The index is 49 KB on the heavy session and the window a few hundred rows, so most of it is fixed cost and the open and count passes' transient allocations. The split was not measured.
- Sessions far larger than the heavy one. The passes are linear in the log, so their cost was extrapolated above, not measured.
- A single pass that indexes, folds the panel and counts rows together.
- Dragging the scroll bar's thumb to a position, which is not built.
- The context view with `--paged`: it walks the fold's turns, which the paged mode does not keep, so it shows nothing. Fiber should keep its totals in the fold, as `README.md` already says.
- An approval's forced-open group, which the paged mode ignores, and delegates' own transcripts.
- Search matching across a wrapped line break, an earlier finding that paging does not change.

## Numbers chosen without evidence

- At least 64 lines a page, with 16 and 256 also measured on the heavy session.
- Windows of 0, 1 and 4 screens above and below the viewport.
- Page Up at 20 presses a second, 100 presses, to scroll to the top.
- The search words "tool", common in the fixtures' filler text, and "flaky", in the first prompt only.
- A resize to 120 columns, a conversation 84 cells wide, for the resize cost.
- The selection check's drag: from 30 rows up to the top edge, held for 4 seconds, at window 0.
- The estimates in option (b) take their ratio from the pages loaded so far, with no fixed starting ratio.
