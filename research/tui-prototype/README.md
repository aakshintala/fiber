# TUI prototype

A throwaway ratatui program for [#15](https://github.com/aakshintala/fiber/issues/15). It replays a fixture session of `docs/events.md` lines in a real terminal, draws the ruled layout, and measures what the working line's glimmer costs. Stage 2 adds selection and copy, search, keyboard protocol detection, the approval panel and the question form. It is not a workspace member and nothing in Fiber depends on it.

The prototype has no Fiber process to talk to. Each command it would send (`reply`, `cancel`, `steer`, `steer_amend`, `steer_drop`, `prompt`) shows as a "→ would send" row above the input box and is appended to `--commands FILE`. It then plays Fiber's part: it applies the lines Fiber would write back (`permission_resolved`, `interaction_resolved`, `tool_call_completed`, `turn_completed`, `steering_queue`), so the state after an answer renders.

`CHECK.md` lists what to judge by eye in Ghostty.

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
| `--commands FILE` | appends each command the TUI would send to FILE, one JSON line each |

The key map is under "Stage 2".

`cargo run --bin gen` rewrites the fixtures from `src/bin/gen.rs`. `fixtures/session.jsonl` ends with a turn still running, waiting on an approval from the reviewer delegate and on a question form from the main session. `fixtures/idle.jsonl` is the same session cut after its last finished turn.

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

Stage 2 covers selection and copy, search, keyboard protocol detection, the approval and question panels and the full key map. Beyond those, the prototype leaves out:

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
- The panel's 29 usable columns cut long paths in the Changed files card. The card now cuts them from the left and keeps the file name ("…/tests/serve_attach.rs"), which reads better than cutting the name.
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

## Stage 2

### What it adds

- Free text selection. Dragging over the conversation selects its text and highlights it. The copy is unwrapped: a line that wrapping broke copies as one line, and margins, stripes and surface edges are left out. The panel is never selected. Dragging past the top or bottom edge keeps scrolling while the button is held, so a selection can run over text scrolled off screen. Releasing the button copies through OSC 52 and, when not over SSH, also through `pbcopy`, `wl-copy` or `xclip`, and a row says what was copied. Shift-drag is left to the terminal.
- Search. Ctrl+F, or Cmd+F where the terminal forwards it, puts a search bar in place of the input box. It matches the rendered rows of the whole session, never the JSON lines, marks every match, and says "3 of 5". Enter, Shift+Enter and the arrows move between matches, and the view centres the current one. Once the query has three characters, every collapsed tool group with a match in its ledger opens, as in the mock. Esc closes the bar and the groups fall back.
- Keyboard protocol detection. After the first frame the program writes kitty's flags query (`CSI ? u`) and then the primary device attributes query (`CSI c`), and reads the replies from the ordinary input stream. If the kitty reply comes before the DA1 reply it pushes flag 1 (disambiguate escape codes). The Session card's "keys" line says "detecting…", then "kitty · N ms" or "legacy · N ms". `--stats` records the times.
- The approval panel and the question form, each in place of the input box, sharing one queue in arrival order. The fixture's last turn ends with an approval from the reviewer delegate (`cargo mutants`, raised by a standing ask rule) and then a four-question form from the main session. The panel reads "Approval 1 of 2 · ◆ review: the wait_for_path sweep (s_d417a0)"; the form reads "Question from main · 4 questions … · 2 of 2".
- The approval choices: allow once; allow and add a rule, with the prefix shown; deny, where typing goes to the feedback. Esc puts the approval aside behind a "1 approval waiting · click to reopen" row.
- The question form as ruled: a tab per question header, marked ✔ once answered, then a Submit tab showing every answer and taking the note on the whole form. Options show their descriptions, with a last row to answer in words. Enter on a single-choice option chooses it and moves on, space toggles a multi-choice option, and digits pick an option. "Chat about this" and Esc send `reply` declined, then `cancel`.
- After an answer, a "you answered" rule sits in the turn's card, one row per question with `skipped` and the note shown. The call's ledger row reads "answered" or "declined", and the group line counts "asked 4 questions". Turn 2 of the fixture has a form answered earlier, with one question skipped and a note, so both states render.
- The group whose call is asking opens while its form is on top.
- Esc closes whatever is on top: the selection's highlight, the search bar, an approval (put aside), a form ("Chat about this"), or the editing of a queued message. Only when nothing is open does it interrupt the turn, and the working line shows "esc to interrupt" only then.
- The input box takes text. Enter sends `steer` during a turn and `prompt` between turns. ⌥↑ and ⌥↓ load a queued steering message into the input box, keeping the draft; Enter sends `steer_amend`, ⌥X sends `steer_drop`, and each row has a click target and a ✕.
- Links. URLs and file paths in replies are wrapped in OSC 8 hyperlinks, with file paths as `file://host/absolute/path`.
- The Changed files card cuts long paths from the left, keeping the file name.

The program now reads and parses the terminal's input itself (`src/input.rs`), because crossterm keeps the two detection replies private (see the findings). It turns on mouse modes 1000, 1002 and 1006 only: presses, drags and releases, not every motion. Idle cost is unchanged: no frame, no byte and no idle wakeup over 10 seconds.

`cargo test` runs the selection-to-text function over rows the conversation renderer produced, and the input parser, link finder, path cutting, search and base64.

### Detection timing

Measured on macOS arm64 (Darwin 25.6.0), inside tmux 3.6b at 160 by 48, release build, `--static`, 7 runs. Times are from the first line of `main`.

| | Median | Range |
|---|---|---|
| First frame drawn | 3.4 ms | 3.4 to 6.9 ms |
| Detection result, after the query was written | 0.02 ms | 0.01 to 0.04 ms |
| Result | legacy | legacy in every run |

tmux answers the DA1 query itself and does not answer kitty's query, so under tmux the result is always legacy. Nothing waits on the reply: the first frame is drawn before the query is written. Crossterm's `supports_keyboard_enhancement()`, called before the first frame, would have added the round trip, and 2 seconds in a terminal that answers neither query. How fast Ghostty answers, and that it answers kitty's query, is for the owner to check (`CHECK.md`).

### Key map

"Every action has a legacy path" is a ruling: a legacy key, a mouse target or a slash command. The prototype has no slash commands.

| Action | Binding | Legacy path | Needs the kitty protocol |
|---|---|---|---|
| Quit, printing the resume line | Ctrl+C | Ctrl+C | no |
| Scroll the conversation | wheel, ↑ ↓, Page Up, Page Down | same | no |
| Scroll the panel | wheel over the panel | same | no |
| Jump to the end | End | End, or click "↓ N lines below" | no |
| Open or close every ledger | Ctrl+O | Ctrl+O | no |
| Open or close one ledger | click the group's summary line | same | no |
| Dismiss a notice | click its row | same | no |
| Select and copy | drag, copied on release | same | no |
| Terminal's own selection | Shift-drag | same | no |
| Clear the selection's highlight | any key, a click, or Esc | same | no |
| Open search | Ctrl+F, Cmd+F | Ctrl+F | Cmd+F: yes, and Ghostty must not bind `super+f` |
| Next match | Enter, ↓, Ctrl+F | same | no |
| Previous match | Shift+Enter, ↑ | ↑ | Shift+Enter: yes; a legacy terminal sends it as Enter |
| Close search | Esc | Esc, after a 30 ms wait | an immediate Esc: yes |
| Type a prompt or a steer | keys, Enter sends | same | no |
| Interrupt the turn, when nothing is open | Esc | Esc, after a 30 ms wait | an immediate Esc: yes |
| Edit a queued steering message | ⌥↑, ⌥↓ | click the row | no, where the terminal sends Option as Alt; see findings |
| Amend it | Enter while editing | same | no |
| Drop it | ⌥X | click its ✕ | no, where Option is Alt; macOS types "≈" otherwise, which the prototype also accepts |
| Stop editing it | Esc | Esc | no |
| Approval: choose | ↑ ↓, Tab | click a choice | no |
| Approval: confirm | Enter | click a choice | no |
| Approval: feedback on a denial | type; Backspace | same | no |
| Approval: put aside | Esc | Esc | no |
| Approval: reopen | click "N approvals waiting" | click only | no key: flagged |
| Next waiting request | click "Approval 1 of N" or "1 of N" | click only | no key: flagged |
| Form: move between questions | ← →, Tab, Shift+Tab | same, or click a tab | no; Shift+Tab is `CSI Z` in legacy terminals |
| Form: move between options | ↑ ↓ | same | no |
| Form: choose (single) and move on | Enter, a digit | same, or click the option | no |
| Form: toggle (multi) | Space, a digit | same, or click the option | no |
| Form: answer in words, add the note | type; Backspace | same | no |
| Form: submit | Enter on the Submit tab | click Submit | no |
| Form: chat about this (decline, then cancel) | Esc | click "Chat about this" | no |
| Open a link | the terminal's own link gesture (Cmd+click in Ghostty) | same | no |

Two actions have a mouse target but no key: reopening an approval put aside, and moving to the next waiting request. A key or a slash command such as `/approvals` would give a person without a mouse a way back to a request they put aside.

What needs the kitty protocol, and the fallback without it:

- Shift+Enter. A legacy terminal sends Enter for it. The fallback for "previous match" is ↑.
- Cmd+F. Terminals only forward Cmd with the protocol, and Ghostty binds `super+f=start_search` by default. The fallback is Ctrl+F.
- An immediate Esc. In legacy encoding a lone Esc is also the start of every escape sequence, so the reader waits 30 ms before treating it as the Esc key. With flag 1 Esc arrives as `CSI 27 u` at once.
- ⌥ combinations on macOS. With Option not set as Alt (Ghostty's `macos-option-as-alt` is off by default), ⌥X types "≈" and ⌥↑ depends on the terminal. The mouse targets are the fallback.

### Findings

#### The rulings in a real terminal

- Crossterm cannot detect the protocol without blocking. It parses both replies, but only its blocking `supports_keyboard_enhancement()` sees them; `event::read()` drops them into a private queue. Detection after the first frame needs Fiber's own input reader, as `src/input.rs` is (about 300 lines with the SGR mouse, legacy and kitty key forms), or a change to crossterm.
- Crossterm's `EnableMouseCapture` also turns on mode 1003, which reports every mouse motion and wakes the process on each one. Presses, drags and releases (1000, 1002, 1006) are all the ruled design needs.
- tmux never gives the protocol. tmux 3.6b answers DA1 itself and ignores kitty's query, so under tmux the TUI always runs legacy keys, and Shift+Enter and Cmd+F never arrive.
- OSC 52 alone does not copy under tmux. Its default `set-clipboard external` drops OSC 52 from applications; the copy worked through `pbcopy`. Running the clipboard command every time, not only as a fallback, is what makes copying reliable on a local machine. Over SSH only OSC 52 can reach the local clipboard, and tmux then needs `set-clipboard on`.
- The selection's highlight stays after the copy. Because Esc interrupts when nothing is open, a person pressing Esc to clear a highlight would interrupt the turn. The prototype treats the highlight as open, so Esc clears it first. `docs/tui.md` should say so.
- Two Escs go from an approval to ending the turn. The first puts the approval aside and brings up the next request, the form; the second declines the form and cancels. Esc after putting an approval aside may need to stop there.
- Transient rows move click targets. The "would send" and "copied" rows, and notices, sit above the input box, so when one comes or goes everything under the conversation moves a row. A click aimed at the "approval waiting" row landed one row off when a transient row expired just before it. Transient messages need a place that does not move targets.
- Search matches rendered rows, so a match across a wrapped line break is not found. It should match the unwrapped text, which the selection already rebuilds. Collapsed groups open only once the query has three characters, as in the mock, so for shorter queries "N" leaves out matches in collapsed ledgers.
- A delegate's approval has no group to expand. The asking call is in the delegate's transcript, not the main conversation. The panel shows the whole call instead: the command wrapped in full, its effects, whether it is reversible, and why it was raised. The ruled expansion applies only to the main session's own calls.
- "esc to interrupt" is wrong while a panel or the search bar is open, so the prototype hides it then.
- Ratatui's cells have no hyperlink attribute. The prototype rewrites each link's cells wrapped in OSC 8 after a frame that moves the conversation. A cell ratatui leaves unchanged later keeps its link in the terminal even if its text is no longer a link.

#### Data the stream does not carry

- The rule that "allow and add a rule" would write. `permission_requested` carries the effects, the paths and the step that raised it (`docs/permissions.md`), not the prefix to offer, nor which argument is the primary one. The prototype offers the first two words of a shell command, and the whole argument for other tools.
- The fields of `reply`. `docs/invocation.md` says what `reply` answers but not its fields: the decision, the rule to add, the feedback, a form's answer, a declined form. Nor the command envelope (`command`, `id`). The prototype's guesses are in `--commands` output.
- The JSON shape of a form's answer. `docs/events.md` says each answer is "either `skipped` or the chosen option `labels` with the typed `text`", and a declined form carries `declined`, but not whether `skipped` is a string or a field, or where `declined` sits. The field naming who answered is not named either. The fixture writes `"skipped"`, `{ "labels": [...], "text": ... }` and `{ "declined": true }`, and the fold accepts either form.
- Why an approval was raised. `docs/permissions.md` numbers the steps of the order a call is judged in, but gives them no codes, and does not say `permission_requested` carries whether the call is reversible. The fixture writes `"step": "standing_ask"` and `"reversible": false`.
- What a cancel does to a pending interaction or approval. The call completes `cancelled`, but nothing says whether `interaction_resolved` or `permission_resolved` is written. The prototype drops the session's own pending requests when `turn_completed` arrives, and keeps a delegate's.
- What a cancel does to queued steering messages. The prototype leaves the queue as it is until the next `steering_queue`.

### Not built in stage 2

- Clicking a link: the prototype leaves links to the terminal, through OSC 8.
- A code block's click-to-copy target.
- Slash commands, so no `/approvals` or `/search`.
- Search over history paged from the log: the whole fixture is in memory, so "the whole session" is the whole fixture.
- Allowing an approval does not continue the delegate's call, and a prompt sent between turns starts nothing: only the lines that close a request are played back.
