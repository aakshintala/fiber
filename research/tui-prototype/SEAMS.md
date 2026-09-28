# A Lua renderer seam in the TUI prototype

Evidence for [#163](https://github.com/aakshintala/fiber/issues/163): what a TUI extension may change on screen, and through which seam. This probe builds one seam, a Lua renderer for one tool's ledger row, and measures what it costs on the heavy session. It is a probe, not a design.

## What the probe built

`--lua-renderer FILE.lua` loads a Lua script that replaces the ledger row of one tool. `lua/shell_row.lua` draws the `shell` tool's rows with a coloured exit-code badge, the command, a duration bar on a log scale and the line count.

```sh
cd research/tui-prototype
cargo run --release -- fixtures/large-heavy.jsonl --static --lua-renderer lua/shell_row.lua
```

Press Ctrl+O to open every ledger.

The script returns a table naming the tool and a render function:

```lua
return { tool = "shell", render = function(call, width) ... end }
```

`render` gets two arguments:

- `call`: `name`, `arguments`, `status`, `exit_code`, `duration_ms`, `lines`, `changes` (a list of `path`, `added`, `removed`) and `error` (`code`, `message`). Every field comes from the event stream. `duration_ms` is nil while the call runs.
- `width`: the cells it may draw in. The ledger's step gutter, 6 cells, stays with the TUI and is not part of the width.

It returns a list of lines. Each line is a list of spans:

```lua
{ text = " exit 0 ", fg = "#ffffff", bg = "#2e7d4f", bold = true, dim = false, click = "badge" }
```

`text` is required. `fg` and `bg` take `#rrggbb` or a palette name (`red`, `blue`, `orange`, `purple`, `cyan`). `click` makes the span's cells a click region with that id. A click shows "extension click: id" in the prototype, since nothing else consumes it yet.

The Lua state is created with the stripped standard library of `docs/extensions.md`: `table`, `string`, `math`, `utf8` and `coroutine`. It runs on the TUI's own thread, called synchronously while the conversation's rows are built.

The code is `src/lua.rs`, with small hooks in `src/main.rs`. `cargo test` covers search, copy and click regions on rows Lua drew, the fallback, and the cache.

Two modes:

- Cached, the default. Rows are cached by a hash of the call's input and the width, so Lua runs only when a row's content or width changes. This sits under the stage 3 cache of the whole conversation's rows.
- Uncached, `--lua-uncached`. Every rebuild of the conversation calls Lua for every row of the tool, and every frame calls Lua again for each visible row it drew, as a naive seam would.

## Results

Measured on macOS arm64 (Darwin 25.6.0, Apple M3 Pro), release build, inside tmux 3.6b at 160 by 48, on `fixtures/large-heavy.jsonl` (1,047 tool calls, 819 of them `shell`, 4.4 MiB). Every figure is the median of 5 runs. `measure_seams.sh` runs it all.

| | Built-in only | Lua, cached | Lua, uncached |
|---|---:|---:|---:|
| Load time, `--static`, to the first frame | 24.9 ms | 33.4 ms | 33.0 ms |
| Peak memory footprint | 39.6 MiB | 42.0 MiB | 41.6 MiB |
| CPU scrolling, every ledger open | 0.59% | 0.63% | 0.77% |
| CPU searching, every ledger open | 2.39% | 2.43% | 2.58% |
| CPU during live replay | 5.68% | 5.07% | 7.35% |
| Lua calls at load | none | 793 | 819 |
| Lua calls while scrolling, 10 seconds | none | 0 | 726 |
| Lua calls while searching, 10 seconds | none | 0 | 1,244 |
| Lua calls during live replay, 10 seconds | none | 90 | 4,179 |
| Lua state's own memory after load, `used_memory()` | none | 35 KiB | 29 KiB |

Method:

- Load time is `--stats`' `first_frame_ms` with `--static`: the whole file folded, every row built, and the first frame drawn. Ranges: built-in 23.3 to 37.8 ms, cached 25.4 to 34.0 ms, uncached 31.9 to 33.3 ms.
- Peak memory footprint is `/usr/bin/time -l` over the scrolling run, so every ledger is open. It is the macOS figure `docs/dependencies.md` says to use. It is not the Linux peak RSS that `docs/performance.md` gates on, and Linux was not measured.
- Scrolling: `--static`, Ctrl+O after 1 second, then wheel-up events at 10 Hz through `tmux send-keys -H` over a 10-second window after a 2-second warmup, as `measure_large.sh` does. About 72 frames in each window in every mode.
- Searching: `--static`, Ctrl+O, then Ctrl+F, "tool", and Enter every 0.2 seconds. Typing the query rebuilds the conversation once inside the window.
- Live replay: no `--static`, the default 12 times speed, over the same window. It covers only the session's first two minutes or so, where the conversation is still small. CPU varied widely between runs: built-in 5.6 to 5.8%, cached 4.4 to 6.0%, uncached 4.6 to 7.5%.
- Microseconds per Lua call are timed in the process with `Instant` around each call, and written by `--stats`.

Timings are macOS only and do not carry over to Linux. The call counts do.

## The cost of a Lua call

On macOS arm64, one call to `lua/shell_row.lua` costs about 8 µs, measured over the 800 calls at load: 8.0 µs in all, of which 3.7 µs is inside `render`. The other 4.3 µs is turning the call's JSON into a Lua table and reading the returned spans back into Rust. More than half of the cost is the crossing, not the Lua.

Called a few times a frame, as the uncached mode does while scrolling, a call costs 11 to 13 µs. Called sparsely during live replay, with other work in between, the mean rose to 22 to 40 µs uncached and 62 to 83 µs cached. The cached mode's 90 calls are too few to spread the cost of the first calls. Cold processor caches are the likely cause; this was not profiled.

The Lua state holds about 30 to 35 KiB after load, and the difference between modes is when the collector last ran. The renderer adds 2.1 to 2.5 MiB to peak footprint, which is not the Lua state. Each Lua row keeps a copy of its call's input so it can be drawn again, and the cached mode keeps a map of rendered rows. The split between those and the Lua library itself was not measured.

## What caching changes

With the per-row cache, Lua runs once for each distinct row and never again until that row changes. Scrolling and searching called Lua zero times, and cost the same as built-in rows within the runs' spread. The cache also removed duplicates: 819 `shell` rows are 793 distinct rows.

Without it, the seam costs in two places:

- Every frame calls Lua for each visible row it drew. At 160 by 48 with every ledger open that was about 10 rows a frame, 130 µs a frame, and 0.17 percentage points of a core while scrolling at 10 Hz on macOS arm64. This is small because only visible rows are called.
- Every rebuild of the conversation calls Lua for every row of the tool in the whole session. The stage 3 conversation cache is rebuilt whenever content changes, which during a live turn is every event. Early in the replay that was 4,179 calls in 10 seconds against 90 cached. At the end of the heavy session one rebuild is 819 calls, about 6.5 ms at 8 µs each on macOS arm64. At the replay's 26 frames a second that would be about 17% of a core. This figure is computed from the measured cost per call, not measured.

So the cost that grows with the session is the rebuild, not the per-frame call. A seam must cache each extension row by its input and width, and the cached conversation alone is not enough, because it is thrown away on every event. Load time still pays one call per row: 8.5 ms more for the heavy session on macOS arm64. A TUI that pages history from the log, as #15 rules, pays that only for the rows in its window.

## The output shape that kept selection, copy and search working

Selection, copy and search already worked from the plain text of each row, which is the spans' text joined. So a Lua row keeps working if and only if its cells are built from spans the TUI owns:

- Lua returns text and style, never bytes for the terminal. Rust builds the cells and the plain text from the same spans, so what is drawn and what is copied or matched cannot differ.
- Rust cuts or pads each line to the width, so Lua cannot overflow the row or move its neighbours.
- A span whose text holds any control character is the wrong shape. That rules out escape codes, and with them colour codes, cursor moves and OSC 8 links sent by hand.
- Click regions are attributes of a span, so their cells follow the text. The TUI turns them into click targets, offset by the gutter and the card's margin.

The tests check that search finds the text Lua drew, including the duration it formats, that a copy of a Lua row is the text behind its cells, and that the badge's click region covers exactly its 8 cells. In tmux, a drag over two Lua rows copied both lines through `pbcopy`, a search for "690ms" found 98 matches in Lua rows and marked them, and a click on the badge reported its id.

Colours are only hex values or the prototype's palette names. A renderer that uses hex values ignores the theme. A shape that lets a renderer name theme roles, such as "success" or "muted", would keep extension rows in step with the theme; the theme slot is not designed.

A zero-width character or a combining mark in span text would make the plain text's cells and the drawn cells disagree. Nothing in the probe rejects them.

## How failure fell back

A render that raises an error, returns something other than a list of lines, returns no lines, or has a bad span (text not a string, a control character, an unknown colour, `bold` not a boolean) fails for that row only. The row is drawn by the built-in renderer. The first failure becomes one notice above the input box, naming the tool and the Lua error with the script's file and line:

"⚠ extension · shell renderer failed, built-in rows shown: runtime error: target/broken.lua:2: too many lines"

Later failures add no notice. In the cached mode a failure is cached too, so a failing row is not called again until its content changes.

A script that fails to load, or does not return `tool` and `render`, stops the prototype at start. A real TUI would show a notice and use the built-in rows.

A renderer that never returns freezes the TUI, because it runs on the TUI's thread. The probe has no time limit on a call. An instruction-count hook in mlua could stop a call after a budget of time, at some cost to every call; that was not built or measured.

## Findings for #163's open questions

### Per-frame cost

A Lua call costs about 8 µs on macOS arm64, half of it crossing between Rust and Lua. Cached by the call's input and the width, an extension row costs nothing on a frame where it has not changed, and scrolling and searching cost the same as built-in rows. The cache must be per row: a cache of the whole conversation is rebuilt on every event, and without a per-row cache that rebuild would cost about 6.5 ms per event at the end of the heavy session on macOS arm64. Calling Lua for the visible rows on every frame costs little, about 130 µs a frame for 10 rows, but buys nothing. Invalidation needs nothing beyond the key: a row whose input or width changed gets a new key. Evicting rows is left for a paged TUI to settle.

### Output shape

Lines of spans, each span `text`, `fg`, `bg`, `bold`, `dim` and an optional `click` id, with no control characters, cut to the width by the TUI. That was enough for selection, copy, search and click targets to work unchanged. Still open: theme roles in place of colours, rows of more than one line (built, not measured), and what a click id does.

### Failure

Falling back to the built-in renderer for the failing row, with one notice, worked and kept the rest of the extension's rows. Still open: a time limit for a renderer that does not return, and whether one failure should switch the renderer off for the session.

### Language

Lua through mlua is fast enough for this seam: 8 µs a call, about 30 to 35 KiB for the Lua state, and 8.5 ms more load time for 800 rows on macOS arm64. Since the per-row cache makes the per-call cost matter only when rows change, a runtime several times slower would still be cheap on unchanged frames, and would show in load time and during live turns. No other runtime was measured here, so the probe does not compare them. The crossing between Rust and Lua was more than half of each call, so an API that passes less than the whole call, or passes it once, would cut the cost more than a faster language would.

The probe runs the renderer synchronously on the TUI's thread. `docs/extensions.md` gives each Lua extension its own thread and inbox, and runs callbacks as coroutines that can wait on host calls. A renderer cannot wait: a frame needs the row now. Either a renderer is a pure function that may make no host call, run on the TUI's thread, or its rows arrive later and the built-in row is drawn until they do. The probe does not settle which.

### Not answered

- API surface: which slots are public and how a slot's input is versioned. The probe has one slot, and its input is its own guess.
- Input and focus: how an overridden editor or view takes keys and passes back the ones it does not handle. A ledger row takes only clicks.
- Animation: how an extension asks for a timer. A Lua row redraws only when its call's events change it.
- Composition: two extensions overriding the same slot. The probe loads one renderer.
- Whether the owner's aim, that an extension can change everything on screen, holds for slots other than a row: panels, the input box, views and the root layout were not built.
- Linux timings and Linux peak RSS.
