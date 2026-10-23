# TUI prototype

A throwaway ratatui program for [#15](https://github.com/aakshintala/fiber/issues/15). It replays a fixture session of `docs/events.md` lines in a real terminal, draws the ruled layout, and measures what the working line's glimmer costs. Stage 2 adds selection and copy, search, keyboard protocol detection, the approval panel and the question form. Stage 3 fixes trackpad scrolling and the running group's line, floats the search box and the jump to the end over the conversation, and adds one swapped view, the context breakdown. It is not a workspace member and nothing in Fiber depends on it.

The prototype has no Fiber process to talk to. Each command it would send (`reply`, `cancel`, `steer`, `steer_amend`, `steer_drop`, `prompt`) shows as a "→ would send" row above the input box and is appended to `--commands FILE`. It then plays Fiber's part: it applies the lines Fiber would write back (`permission_resolved`, `interaction_resolved`, `tool_call_completed`, `turn_completed`, `steering_queue`), so the state after an answer renders. Each command is the line `docs/invocation.md` defines: `id` and `command` at the top, the command's keys under `args`, and `session_id` on a `reply` to a delegate. A cancel first resolves the turn's pending requests (`decided_by: cancel`, `by: fiber`), then completes the open calls and ends the turn `interrupted`; queued steering messages then start the next turn, as `docs/architecture.md`, "Cancellation", says.

`CHECK.md` lists what to judge by eye in Ghostty. "The mock" below is the owner's HTML mock, `research/tui-mock/index.html`.

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
| `--hover` | also turns on mode 1003, every mouse motion, and tints the click target under the pointer; see "Hover's cost" |
| `--home [CASE]` | draws the full-screen fleet case instead of the conversation, or the live fleet with no case; see "Home (#2424)" |
| `--overlay CASE` | draws one overlay case over a dimmed conversation; see "Overlays (#1630)" |
| `--rail-case CASE` | draws one rail-cards case instead of the conversation; see "The rail (#692)" |
| `--quota-case CASE` | draws one quota-view case instead of the conversation; see "Quota view (#2526)" |
| `--credential-case CASE` | draws one credential-picker case instead of the conversation; see "Credential picker (#724)" |
| `--theme CASE` | draws the screen in another theme or colour depth; combines with any other flag; see "Themes (#1634)" |
| `--rail A\|B\|C` | the session rail design at start: list, cards, or tabs; the default is B; F2 cycles; see "The rail (#692)" |
| `--rail-share P` | the rail's width at start, as a percent of the window; the default is 15, clamped to [22, 48] columns |
| `--panel-share P` | the panel's width at start, as a percent of the window; the default is 21 (34 columns at 160), clamped to [30, 60] columns |
| `--picker CASE` | starts with the model picker open; see "Model picker (#1629)" |
| `--login CASE` | draws one login case over a dimmed conversation; see "Login (#1736)" |
| `--reviewer-setup CASE` | draws one reviewer-setup case over a dimmed conversation; see "Reviewer setup (#2480)" |
| `--stats FILE` | writes the measurement below to FILE on exit |
| `--exit-after S` | exits after S seconds |
| `--warmup S` | starts the measurement window after S seconds; default 2 |
| `--diff-audit` | also counts the cells and rows ratatui rewrites per frame (costs CPU, so the CPU runs leave it off) |
| `--commands FILE` | appends each command the TUI would send to FILE, one JSON line each |
| `--log-input FILE` | writes every read from the terminal, the events parsed from it, and each frame's scroll position, bytes and flushes to FILE |
| `--wheel-lines N` | rows scrolled per wheel event; the default is 1 |
| `--lua-renderer FILE.lua` | draws one tool's ledger rows with a Lua renderer, such as `lua/shell_row.lua`; see `SEAMS.md` |
| `--lua-uncached` | calls the Lua renderer for every visible row on every frame, instead of caching its rows |
| `--paged` | pages history from the file instead of folding it all, for a finished session; see `PAGING.md` |
| `--window S` | with `--paged`, screens of rows kept rendered above and below the viewport; default 1 |
| `--page-lines N` | with `--paged`, the fewest log lines in a page before it may be cut; default 64 |
| `--verify-copy` | with `--paged`, checks each copy against the whole file folded at once, and records the result in `--stats` |
| `--paging-bench` | prints the paging measurements that need no terminal, and exits |

The key map is under "Stage 2".

## Home (#2424)

`--home [CASE]` draws the full-screen fleet instead of the conversation:
the header with the logo, the version and the counts across projects, one
section per project with two-line live rows and dim exited rows, and the
prompt box with its chip row at the bottom where the conversation view's
input box sits. With no case, or a `live` case, home runs its event loop:
focus runs from the session list down to the entry bar and the chip row
(↓ past the last row reaches the entry, ↓ from the entry reaches the
chips, ↑ returns the same way, ↓ from a chip wraps to the first row);
↑ and ↓ move through every row, live and exited, across sections; Enter
opens the focused session, or starts one from a non-empty box; typing goes
to the entry bar; Delete deletes the focused exited row; → or Esc goes back
to the conversation view; Ctrl+C quits. Clicking the workspace chip opens
the workspace picker; the worktree switch toggles; the model and thinking
chips open the model picker; `/` opens slash completions. The other cases
each draw one still frame from the fixtures in `src/home.rs` and wait for a
key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still
loads but home ignores it.

The logo is real Fiber's: the pixel letters copied from
`crates/tui/src/logo.rs`, and where the terminal speaks kitty graphics (kitty,
Ghostty or WezTerm, never under tmux or screen) an image tints the checked-in
`assets/logo-mask.bin` copy with the theme's accents and replaces the pixel
letters in exactly the same cells. The pixel logo draws first and the image
replaces it from the second draw, so detection never delays the first frame.
iTerm2 and Sixel are not drawn; the pixel logo stays there.

```sh
cargo run --release -- fixtures/session.jsonl --static --home full-sessions
cargo run --release -- fixtures/session.jsonl --static --home full-empty
cargo run --release -- fixtures/session.jsonl --static --home full-focus-exited
cargo run --release -- fixtures/session.jsonl --static --home full-closing
cargo run --release -- fixtures/session.jsonl --static --home full-picker
cargo run --release -- fixtures/session.jsonl --static --home full-remote-empty
cargo run --release -- fixtures/session.jsonl --static --home full-remote-picker
cargo run --release -- fixtures/session.jsonl --static --home rail-focused
```

- `live`: the interactive fleet and its keys (see below); `--home` with no
  case runs this.
- `full-sessions`: the eight live fixture sessions under one section per
  project (launch project first) plus the exited ones, the first live row
  focused, a still frame.
- `full-empty`: no sessions: dim `no sessions yet` in the header and the
  centred middle lines, a still frame.
- `full-focus-exited`: the first exited row focused, a still frame.
- `full-closing`: one live row `CLOSING` after `/clear`, a still frame.
- `full-picker`: the workspace picker open over the fleet, browsing the
  launch directory with the recents below, a still frame.
- `full-remote-empty`: remote home with no workspace chosen: the same picker
  browsing `~/` with no recents, a still frame.
- `full-remote-picker`: remote home with no workspace chosen: the same picker
  browsing `~/` with the hub's recent workspaces below, a still frame.
- `rail-focused`: the conversation view with the rail focused on a
  NEEDS INPUT card (the lifted card, the dimmed input box, the rail foot
  hint), a still frame.

`./capture-home.sh` captures every still frame in tmux at 160 by 48, plain
text and SGR, for the ticket's PR body.

## Overlays (#1630)

`--overlay CASE` draws one overlay over a dimmed conversation instead of the
conversation: the key map, the quit and delete questions, the Ctrl+R
prompt-history panel, a notice's full text, and how an overlay closes by
mouse. Each case draws one static frame from the fixtures in
`src/overlays.rs` (every binding of docs/tui.md's Bindings table; the quit
question for 2 working sessions; home's delete question for "docs: rail
spec"; three recalled prompts matching `back`; a `key_clash` notice) and
waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture
still loads but the overlay ignores it.

```sh
cargo run --release -- fixtures/session.jsonl --static --overlay keymap
cargo run --release -- fixtures/session.jsonl --static --overlay keymap-tab
cargo run --release -- fixtures/session.jsonl --static --overlay keymap-search
cargo run --release -- fixtures/session.jsonl --static --overlay keymap-narrow
cargo run --release -- fixtures/session.jsonl --static --overlay quit
cargo run --release -- fixtures/session.jsonl --static --overlay delete
cargo run --release -- fixtures/session.jsonl --static --overlay history
cargo run --release -- fixtures/session.jsonl --static --overlay history-one
cargo run --release -- fixtures/session.jsonl --static --overlay history-none
cargo run --release -- fixtures/session.jsonl --static --overlay notice
cargo run --release -- fixtures/session.jsonl --static --overlay notices
cargo run --release -- fixtures/session.jsonl --static --overlay close-mouse
```

- `keymap`: every binding in one panel docked at the bottom, with group tabs,
  type-to-search, and three fixed columns (group, action, keys with other
  paths); the All tab selected and nothing typed.
- `keymap-tab`: the same panel with the Session tab selected.
- `keymap-search`: the same panel with `session` typed, narrowing the rows.
- `keymap-narrow`: the same panel at 100 columns, rows wrapped, opened
  scrolled, with an "↑ N more · ↓ M more" indicator; run it in a 100x40 tmux.
- `quit`: the quit question for 2 working sessions, with the three ways out.
- `delete`: home's delete question for the exited "docs: rail spec" session,
  naming the session and its id.
- `history`: the Ctrl+R panel with the typed query `back`, every hit marked
  and the first match selected.
- `history-one`: the same panel with one matching prompt, `1 match · newest
  first`, that match selected.
- `history-none`: the same panel with a typed query matching nothing, the
  dim `no matching prompts` in place of the count row, nothing selected.
- `notice`: the whole `key_clash` notice text, wrapped to the overlay.
- `notices`: every notice listed in full, newest first, each a `code · text`
  paragraph with the code dim.
- `close-mouse`: a small keymap sample with its ✕ hovered and a dim outline
  marking the click-outside target.

`./capture-overlays.sh` captures every case in tmux, plain text and SGR, for
the ticket's PR body: `keymap-narrow` at 100 by 40, the rest at 160 by 48.

## Quota view (#2526)

`--quota-case CASE` draws the `/quota` swapped view instead of the
conversation: one row per entry of the `quota` answer with each window's
reset time, the fetching mark while the fetch is in flight, and the empty
line when no provider reports quota. Each case draws one static frame from
the fixtures in `src/quota.rs` and waits for a key; Esc, q or Ctrl+C quits.
Combine with `--static`; the fixture still loads but the view ignores it.

```sh
cargo run --release -- fixtures/session.jsonl --static --quota-case quota
cargo run --release -- fixtures/session.jsonl --static --quota-case quota-empty
```

- `quota`: the cached rows with the fetch in flight: `codex · default` with
  two windows, each a dim name, its percent and its bar, then its reset
  time dim, and `claude · default` with its last value and `unreachable`
  in the error colour; the top row carries `fetching` dim.
- `quota-empty`: the same view with no entries: one dim line
  `No provider reports quota`.

`./capture-quota.sh` captures every case in tmux at 160 by 48, plain text
and SGR, for the ticket's PR body.

## Credential picker (#724)

`--credential-case CASE` draws the `/credential` swapped view instead of the
conversation. Each case draws one static frame and waits for a key; Esc, q
or Ctrl+C quits. The fixture loads but never draws beneath the picker.
These are look fixtures, not interactive choices: typing, clicking and the
footer's selection keys do nothing in the prototype.

```sh
cargo run --release -- fixtures/session.jsonl --static --credential-case credentials
cargo run --release -- fixtures/session.jsonl --static --credential-case credentials-reading
cargo run --release -- fixtures/session.jsonl --static --credential-case credentials-none
```

- credentials: the header Credentials bold, /credential dim and Esc returns dim right-aligned; default, home and work in order; only home marked ● current in accent and selected in reverse; fake plain above the bottom footer; the footer ↑↓ move · Enter choose · Ctrl+S this session · Esc close dim.
- credentials-reading: the same header and footer, no label rows, and Reading the labels… plain above the bottom footer.
- credentials-none: the same header and footer, no label rows, and scripted has no credential labels plain above the bottom footer.

There is no filter and no session-only marker row. The planned picker uses
Enter or a label click to switch and save; Ctrl+S switches for this session
only. The current label starts selected.

`bash capture-credential.sh` captures every case in tmux at 160 by 48, in
plain text and SGR. It writes to
`/home/aakshintala/work/fiber-worktrees/flywheel/run/fiber-724-captures`;
set `OUT` to choose another directory.

## Login (#1736)

`--login CASE` draws one `/login` case as a centred panel over a dimmed
conversation instead of the conversation: the provider list, then the
browser wait, key entry, and the two outcomes, each replacing the whole
panel. Each case draws one static frame
from the fixtures in `src/login.rs` (four providers, two with `browser`
tags and two with `key` tags; two extension credentials) and waits for a
key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still
loads but the login ignores it.

```sh
cargo run --release -- fixtures/session.jsonl --static --login providers
cargo run --release -- fixtures/session.jsonl --static --login waiting
cargo run --release -- fixtures/session.jsonl --static --login key
cargo run --release -- fixtures/session.jsonl --static --login done
cargo run --release -- fixtures/session.jsonl --static --login failed
```

- `providers`: four providers under `Providers`, OAuth rows tagged
  `browser` and key rows tagged `key`, then two extension credentials under
  `Secrets`; the first row focused.
- `waiting`, `key`, `done`, `failed`: each step replaces the whole panel
  (the provider list is not drawn). The title is the provider's name; the
  panel keeps the provider list's height and one width across the four
  steps, so it does not jump between steps. It is a few cells wider than
  the list, because the `key` legend is the longest line; the URL is cut to
  fit rather than widening it.
  - `waiting`: `Open this URL to log in to anthropic:`, the long URL cut
    from the left keeping its tail, and a dim `Waiting for the browser…`
    line. Legend: `y copy URL · Esc back`.
  - `key`: `Label (--as): default.` and `Key for google:`, the key masked as
    eight dots with a block cursor. Legend:
    `Tab key or label · Enter submit · Esc back`.
  - `done`: a `✓ Logged in to anthropic.` outcome line in the success
    colour. Legend: `Esc back`.
  - `failed`: a `Login to anthropic failed: token expired.` outcome line,
    the `token expired` reason in the error colour. Legend: `Esc back`.

`./capture-login.sh` captures every case in tmux, plain text and SGR, for
the ticket's PR body: `waiting-narrow` and `key-narrow` are the same cases
at 100 by 40, the rest at 160 by 48.

## Reviewer setup (#2480)

`--reviewer-setup CASE` draws one `/reviewer-setup` case as a centred
panel over a dimmed conversation instead of the conversation: the notes
setup with the person from docs/permissions.md, "Setting up the notes".
The flag is a prototype drawing aid only: the command has no
command-line form (docs/permissions.md says so), and a headless run never
starts it. Each case draws one static frame from the fixtures in
`src/reviewer_setup.rs` and waits for a key; Esc, q or Ctrl+C quits.
Combine with `--static`; the fixture still loads but the setup ignores it.
The `notice` case draws the notice the terminal shows when the reviewer
blocks while both layers are empty, naming `/reviewer-setup`. `question`,
`waiting` and `saved` draw the screens around the proposals.

```sh
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup first
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup outside
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup rerun
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup struck
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup flipped
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup diff
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup notice
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup question
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup waiting
cargo run --release -- fixtures/session.jsonl --static --reviewer-setup saved
```

Every lines frame (`first`, `outside`, `rerun`, `struck`, `flipped`) opens
on a `Topic n of 5 · <heading name>` row.

- `first`: first run in a project, topic 4 of 5 (routine actions): the
  topic's question, the session evidence (14 escalations answered allow for
  `gh pr merge … --squash`) and the `[per-project]` line barred. Legend:
  `Tab flip layer · Enter confirm · e edit · x strike · Esc back`.
- `outside`: first run from a launch directory outside git, topic 2 of 5
  (internal hosts): the topic's question, the person's answer as evidence
  and the `[global]` line barred, which is what an answer line gets in any
  project. Every session has a project, so the frame says nothing about
  one. Same legend.
- `rerun`: a rerun with earlier lines, topic 1 of 5 (source control): two
  kept `✓` lines under `Earlier lines`, then the evidence and the new
  proposal barred under `New proposals`; the question is left out. Legend:
  `Enter keep · x strike · Esc back`.
- `struck`: a struck line, dim with strikethrough on its bar, kept out of
  the notes, under the topic line and the `struck — kept out of the notes`
  label, with no question and no evidence. Legend: `Enter keep · Esc back`.
- `flipped`: the first run's topic with its question and the line flipped to `[global]`, with a dim `was
  per-project` line. Same legend as `first`.
- `diff`: the final diff, each layer's text against the file as it is now
  with the file named (`~/.fiber/config.json` and
  `~/.fiber/projects/fiber/config.json`): the topics' heading lines as
  saved (`### Source control`), kept bullets as context, a struck bullet
  with `-` in the removed colour, and added bullets and new headings with
  `+` in the added colour, then a dim line saying approving then offers r
  reload now or n from the next session, with the two keys bold. Legend: `Enter approve · Esc
  cancel`.
- `notice`: the notice naming `/reviewer-setup`, shown the first time in a
  session the reviewer blocks a call while both layers of the notes are
  empty. Legend: `Any key closes`.
- `question`: topic 1 of 5 (source control): the topic's question and a
  one-line answer field with a block cursor. Legend: `Enter answer · Esc
  back`.
- `waiting`: the wait for the topic's lines: a dim `Proposing lines…` row.
  Legend: `Esc back`.
- `saved`: after the save, each file named with whether it was saved: the
  global file saved with a dim sync error beside it, the per-project file
  saved. Legend: `r reload now · n from the next session`.

The keys match the two tools the person already uses, probed rather than
guessed: Claude Code's binary names `h`, `c`, `o` and `q + Enter`
shortcuts, submits on Enter (`Press return to submit`) and takes
Shift+Enter or Option+Enter for a new line (`/terminal-setup`); pi's help
names Tab for switching scope and Ctrl+P for cycling models. Enter
confirms, Esc backs out and Tab flips the layer, as in both tools and in
every prototype overlay; `e` edits and `x` strikes by initial letter, and
`r`/`n` answer the reload offer, are the screen's own.

`cargo run --bin gen` rewrites the fixtures from `src/bin/gen.rs`. `fixtures/session.jsonl` ends with a turn still running, waiting on an approval from the reviewer delegate and on a question form from the main session. `fixtures/idle.jsonl` is the same session cut after its last finished turn.

`cargo run --release --bin from_claude -- <session.jsonl> <out.jsonl> [--max-bytes N]` converts a Claude Code session (main thread only) into the same kind of durable-only fixture, stopping after the turn where the output passes N bytes (default 12 MB, so a normal session converts whole) and turning each Claude Code compaction into a handoff, and adding one wherever the context would pass 400k without one. `fixtures/real.jsonl` is that conversion of one of the owner's own sessions. It is kept out of git (`.gitignore`) because it is a private session, so regenerate it locally from a session under `~/.claude/projects/`; `demo/package.sh` adds it to the zip when the file exists.

`./measure.sh [runs] [seconds]` runs the measurement below inside tmux at 160 by 48. `./measure_hover.sh [runs] [seconds] [rate]` runs the hover measurement.

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

- clicking a ledger row to open that call's diff, output or error, and every panel item's view but the context breakdown and the model picker (usage, tools, a delegate's or job's transcript, a file's diff);
- the Delegates card scrolling on its own, and the Jobs card's list;
- the start page, the session list, handoff bands, the nudge, rewind, retries, a failed turn, crash recovery and interrupts, because the fixture has none of them;
- paging history from the log: by default the prototype folds the whole file into memory; `--paged` is a probe of paging, in `PAGING.md`;
- the 256-colour theme: colours are the mock's "atelier" truecolour values, written into the code.

## The rail (#692)

Eight fake sessions stand in for the hub's feed, so the owner can judge the rail's look in Ghostty. Session 1 is the replayed fixture (WORKING, NEEDS INPUT once it waits on its approval or question); its model, branch, spend and context % are the fixture's real numbers. The other seven are hard-coded: "docs: rail spec" WORKING, "review #688" NEEDS INPUT on an approval, "bump ratatui" NEEDS INPUT on a question, "lsp probe" CRASHED, "migrate every provider adapter to the new streaming contract" WORKING and "backfill embeddings" RETRYING in the `pi-rig` project, and "rewrite onboarding tour" READY in a third project, `beacon`. Sessions stay in start order, so ⌥N numbers never move; a waiting session draws attention by glyph, stripe, tint and pulse only. Clicking a card or pressing ⌥N moves the on-screen marker only; the conversation stays the replayed session, as the dim "click moves marker" line says.

Rail cards (#1632): five still frames from the fixtures in `src/rail.rs`,
each a four-row medium card from the rail builders above, at 160 by 48:

```sh
cargo run --release -- fixtures/session.jsonl --static --rail-case all-states
cargo run --release -- fixtures/session.jsonl --static --rail-case hovered
cargo run --release -- fixtures/session.jsonl --static --rail-case context-bars
cargo run --release -- fixtures/session.jsonl --static --rail-case floor-ceiling
cargo run --release -- fixtures/session.jsonl --static --rail-case hidden
```

`all-states` is the rail at 15% of 160 (24 columns) with two projects and
six sessions in every state; `hovered` forces the `--hover` tint onto the
needs-input card with no mouse; `context-bars` draws 30%, 65% and 90% twice,
once with the panel's ▆ track and once with the ░ track, so the owner picks;
`floor-ceiling` puts the 22-column floor beside the 48-column ceiling;
`hidden` shows the hidden rail's 1-column grip with "N waiting" on the
Session card. IDLE (`○` dim) exists only in this fixture: the live rail never
draws it, and docs/tui.md maps the hub's `idle` status to READY.

| Flag / key | What it does |
|---|---|
| `--rail A\|B\|C` | the design at start; the default is B |
| `--density full\|medium\|three\|compact` | the B card density at start; the default is three |
| chips | function keys do not reach every shell, so the rail switches by clicking the `A B C` chips (and, in B, the `full med three compact` chips) in the rail's bottom label; the tabs row carries its own `A B C` chips |
| ⌥1..⌥9 | jumps to the Nth session in start order and scrolls its card into view; needs Ghostty's `macos-option-as-alt`, as with the other ⌥ keys; the mouse is the fallback |
| ⌥A | jumps to the oldest waiting session; on macOS where Option is not Alt it arrives as "å", which the prototype also accepts |
| ⌥R | toggles the rail shown/hidden; `/rail` does the same; on macOS where Option is not Alt it arrives as "®", which the prototype also accepts |
| ← | with an empty input box, moves keyboard focus into the rail; with text in the box, ← moves the cursor as before; when the rail is hidden or the window too narrow for it, a single ← opens the full-screen fleet instead |

- A "list": 22 columns, one row per session (`1 ● name…`), waiting sessions with their wait reason on a second dim row, the on-screen session on a tint with a ▌ stripe, other projects under a divider behind "+1 other · show all".
- B "cards" (the default): rich cards grouped by project, mocked at 40 columns with tinted surfaces and no box borders. The bottom label's chips switch four densities, the current one highlighted; three is the card: medium's rows with cost and context moved into row 1 (number, glyph, state word; coarse elapsed, spend and coloured percentage right-aligned, no bar), both edges, 3 content rows. When row 1 does not fit the elapsed goes first, then the spend; the state word is never cut. Full keeps the 5-row edged card, medium the 4-row edged card and compact the 3-row flat card for comparison. The three default keeps every project on screen at once. Narrow rule as medium. Every row ends 2 cells short of the card's edge (a right inner margin matching the stripe + space on the left). The bar is blue under 60%, orange at 60–85%, red above. The stripe and tint follow the state: working blue, retrying orange, needs-input the pulsing attention colour, ready dim, crashed dimmed red. The on-screen card uses the brighter SEL surface; the rest use their dimmer state tint. A project header names the project in the accent colour, bold, with the group's live spend summed over its cards and a fake "N ✓" right-aligned; the launch project's group comes first, the rest after in start order, and every project shows. A "+" chip after the project name starts a session there (a "→ would start" row); clicking "N ✓" opens that project's session list (a "→ would open" row). Context % values span 12%–91% so all three bar colours show.
- C "tabs": no column; one row across the top of the conversation (`1 ⠋ fix flaky… │ 2 ● docs: rail…`), the on-screen tab tinted, waiting tabs in the attention colour, overflow as "+2 ›", and `A B C` chips at the row's end.
- Session states, on the rail in every variant: a braille spinner WORKING in blue (`●` under reduced motion, animating on the working line's tick, so the rail redraws with it), the spinner in orange for RETRYING, `!` bold orange NEEDS INPUT (pulsing, still under reduced motion), `✓` dim READY, `✗` bold red CRASHED. A crashed card shows ✕ at row 1's right end instead of the elapsed: clicking it dismisses the card, leaving its number's gap (numbers are stable per session); clicking elsewhere on the card shows a "→ would send resume" row and turns it WORKING. (Not built: the session list, so no `○` exited; the terminal title.)
- Width is a share of the window for the rail and the panel alike: the rail defaults to 15% clamped to [22, 48], the panel to 21% (34 columns at 160) clamped to [30, 60]. Dragging the rail's right edge or the panel's left edge (the one-column gaps beside them) resizes it; a dim ⋮ grip on 3 centred rows marks each handle, brightening with a tinted column on hover or while dragging (which also sets the col-resize pointer through OSC 22); the new share applies live and survives terminal resizes as a share, with the share shown while dragging ("rail 18% · 31 cols" in a dim pill). `--rail-share P` and `--panel-share P` set the start. Dragged below its 22-column floor, or when the window is too narrow for rail floor + conversation minimum + panel, the rail hides completely and "N waiting" joins the narrow-layout status rows or the panel's Session card (clicking it brings the rail back). While hidden a 1-column handle with the dim ⋮ grip stays at the screen's left edge, with the hover tint and col-resize pointer as the other handles; dragging it out restores the rail at the dragged width. An auto-hidden rail returns by itself when the window grows; a dragged-shut rail stays shut until dragged out, toggled with ⌥R, or restored from the Session card. A drag stops where the conversation would go under its 84-column minimum.
- The rail scrolls with the mouse wheel when its cards overflow the screen; ⌥N, ⌥A and a click scroll the card into view.
- With `--hover`, hovering a B card brightens the whole card; in compact it also shows a one-line dim footer at the rail's bottom with the full name, workspace, model · thinking level and spend, since compact hides them. In A hovering a row floats one tooltip line with the full name, workspace and spend. Elapsed times on card row 1 are coarse and one unit (`16s`, `2m`, `1h`, `3d`), so names get the space.

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

## Hover's cost

`--hover` also turns on mode 1003, so the terminal reports every motion as `CSI < 35 ; x ; y M`. The parser reads it as a motion with no button, finds the click target under the pointer with the same list and lookup a click uses, and tints that target's row or span: each cell's own background, 20 levels lighter. A frame is drawn only when the target under the pointer changes. Reduced motion changes nothing: a tint is not motion. Without `--hover` a motion report is parsed and dropped, drawing nothing.

Measured on macOS arm64 (Darwin 25.6.0), inside tmux 3.6b at 160 by 48, release build, `fixtures/idle.jsonl --static` (a settled session, no timer), median of 5 runs of 20 seconds each, with the counters of "The glimmer's cost". `./measure_hover.sh` sends motion reports through one tmux control-mode client, one `send-keys -H` per report, at 150 a second on a fixed schedule. The sweep moves the pointer one row down and 7 columns right per report, over the conversation and the panel. "One target" moves along the text of one tool group's summary line.

| | Hover, pointer still | Hover, sweep | Hover, one target | No hover, sweep sent anyway |
|---|---|---|---|---|
| Motion reports per second | 0 | 150 | 150 | 150 |
| Target changes per second | 0 | 26.7 | 0 | none |
| Frames per second | 0 | 25.5 | 0 | 0 |
| Bytes written per second | 0 | 6,508 | 0 | 0 |
| Bytes per frame | none | 255 | none | none |
| CPU | 0.003% of one core | 6.8% | 2.1% | 1.7% |
| Involuntary context switches in 20 s | 1 | 4,551 | 3,008 | 3,014 |
| Idle wakeups (IDLEW) in 20 s | 0 | 520 | 0 | 0 |
| Interrupt wakeups in 20 s | 1 | 1,556 | 1 | 2 |

- Idle is still zero. With the pointer still, mode 1003 sends nothing, so the process made no frame, wrote no byte and had no idle wakeup.
- Receiving a motion report costs about 115 µs of CPU, whether or not hover is on (1.7% of a core at 150 a second). That is the wakeup, the read and one pass of the loop, not the parse. A real terminal sends none of these without mode 1003, so this is the cost hover adds for every cell the pointer crosses.
- Motion that stays on one target costs about 0.4 points more than the same reports without hover, about 27 µs a report for the lookup. It is inside the spread of the runs (1.9 to 2.3% against 1.6 to 2.2%) and draws nothing.
- Each target change costs one frame, about 1.8 ms of CPU and 255 bytes, the same per-frame cost as a glimmer frame: the frame re-fits every visible row and diffs every cell, although only one or two rows change. 26.7 changes a second drew 25.5 frames, since the 16 ms frame gap merges changes that come closer together. The frame gap's timer is also where the sweep's idle wakeups come from.
- Most of the screen is not a target, so the sweep changed target on 18% of its reports. A pointer moving along a ledger or down the panel, where every row is a target, would change target more often.

Caveats: the 150 reports a second are tmux's delivery of commands written on a schedule; the process counted exactly 3,000 in every 20-second window, but tmux may bunch them. A real terminal's rate depends on how fast the pointer moves, one report per cell crossed. The CPU of tmux, of the injecting script and of the terminal emulator drawing the frames is not counted. Timings do not generalise to Linux; byte, frame and change counts do.

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
- Payload field names. `docs/events.md` gives every payload a key table. The fixtures and the fold use exactly those keys: for example `tokens.input`, `tokens.cache_read`, `tokens.cache_write`, `messages[].command_id` and `environment.git.branch`. The commands it sends are the lines of `docs/invocation.md`, "The command line".
- Where a step starts. The ledger numbers steps. The prototype takes each `assistant_message_started` as a step boundary, which follows from "Writing" (the line is fsynced before every model request) but is not stated where the kinds are listed. The same rule decides grouping: a tool-only reply opens and completes an assistant message with no text, so a group can only tell whether a message is "a piece of assistant text" once its first non-empty delta arrives. `docs/events.md` should say that every model call opens with `assistant_message_started`, and that a reply with no text completes with empty text.
- An MCP server coming back. `mcp_server_failed` marks a server down, but a successful automatic restart writes no line, so the Session card's "✗ linear down" never clears until a reload.
- The git branch is not a finding: the ruling has the terminal run git itself. The prototype reads it from `opening_message` instead, since it runs no commands.

## Stage 2

### What it adds

- Free text selection. Dragging over the conversation selects its text and highlights it. The copy is unwrapped: a line that wrapping broke copies as one line, and margins, stripes and surface edges are left out. The panel is never selected. Dragging past the top or bottom edge keeps scrolling while the button is held, so a selection can run over text scrolled off screen. Releasing the button copies through OSC 52 and, when not over SSH, also through `pbcopy`, `wl-copy` or `xclip`, and a row says what was copied. Shift-drag is left to the terminal.
- Search. Ctrl+F, or Cmd+F where the terminal forwards it, opens search, in a box over the conversation (see stage 3). It matches the rendered rows of the whole session, never the JSON lines, marks every match, and says "3 of 5". Enter, Shift+Enter and the arrows move between matches, and the view centres the current one. Once the query has three characters, every collapsed tool group with a match in its ledger opens, as in the mock. Esc closes the box and the groups fall back.
- Keyboard protocol detection. After the first frame the program writes kitty's flags query (`CSI ? u`) and then the primary device attributes query (`CSI c`), and reads the replies from the ordinary input stream. If the kitty reply comes before the DA1 reply it pushes flag 1 (disambiguate escape codes). The Session card's "keys" line says "detecting…", then "kitty · N ms" or "legacy · N ms". `--stats` records the times.
- The approval panel and the question form, each in place of the input box, sharing one queue in arrival order. The fixture's last turn ends with an approval from the reviewer delegate (`cargo mutants`, raised by a standing ask rule) and then a four-question form from the main session. The panel reads "Approval 1 of 2 · ◆ review: the wait_for_path sweep (s_d417a0)"; the form reads "Question from main · 4 questions … · 2 of 2".
- The approval choices: allow once; allow for this session and always allow in this project, each with the request's `rule.prefix` shown; deny, where typing goes to the feedback. A request with no `rule` shows only allow once and deny. The panel's second line says why it asked, from `standing_rule` or `escalation`. Esc puts the approval aside behind a "1 approval waiting · click to reopen" row.
- The question form as ruled: a tab per question header, marked ✔ once answered, then a Submit tab showing every answer and taking the note on the whole form. Options show their descriptions, with a last row to answer in words. Enter on a single-choice option chooses it and moves on, Space toggles a multi-choice option, and digits pick an option. "Chat about this" and Esc send `reply` declined, then `cancel`.
- After an answer, a "you answered" rule sits in the turn's card, one row per question with `skipped` and the note shown. The call's ledger row reads "answered" or "declined", and the group line counts "asked 4 questions". Turn 2 of the fixture has a form answered earlier, with one question skipped and a note, so both states render.
- The group whose call is asking opens while its form is on top.
- Esc closes whatever is on top: the selection's highlight, the search bar, an approval (put aside), a form ("Chat about this"), or the editing of a queued message. Only when nothing is open does it interrupt the turn, and the working line shows "Esc to interrupt" only then.
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
| Scroll the rail | wheel over the rail, when its cards overflow | same | no |
| Jump to a rail session | ⌥N, scrolls its card into view | click its card | no, where Option is Alt |
| Focus the rail | ← from an empty input box | same | no |
| Move between rail cards | ↑ ↓, across project groups | same | no |
| Switch to the focused rail session | Enter | click its card | no |
| Give focus back to the input box | → or Esc | click the conversation | no |
| Widen the rail to the full-screen fleet | ← again | same | no |
| Jump to the oldest waiting session | ⌥A | click its card | no; "å" where Option is not Alt |
| Toggle the rail shown/hidden | ⌥R | click the Session card's waiting line | no; "®" where Option is not Alt |
| Jump to the end | End | End, or click the "↓ N lines below" overlay | no |
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
| Open the context breakdown | `/context` and Enter | same, or click the Session card's context bar or its "ctx" summary | no |
| Scroll a swapped view | wheel, ↑ ↓, Page Up, Page Down | same | no |
| Leave a swapped view, back to where the conversation was | Esc | Esc, or click the view's header | an immediate Esc: yes |
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
- "Esc to interrupt" is wrong while a panel or the search bar is open, so the prototype hides it then.
- Ratatui's cells have no hyperlink attribute. The prototype rewrites each link's cells wrapped in OSC 8 after a frame that moves the conversation. A cell ratatui leaves unchanged later keeps its link in the terminal even if its text is no longer a link.

### Not built in stage 2

- Clicking a link: the prototype leaves links to the terminal, through OSC 8.
- A code block's click-to-copy target.
- Slash commands, so no `/approvals` or `/search`.
- Search over history paged from the log: the whole fixture is in memory, so "the whole session" is the whole fixture.
- Allowing an approval does not continue the delegate's call, and a prompt sent between turns starts nothing: only the lines that close a request are played back.

## Stage 3

### What it adds

- Trackpad scrolling that holds still. The causes and the fixes are under the findings.
- A running group's summary line is always one row.
- Search floats over the conversation's top-right corner as an editor's find box does: a tinted surface with ▄ and ▀ edges, "⌕ query" and "3 of 19". The input box stays in place with its draft, without a cursor while typing goes to the search box. Matches are marked as before, the current one brighter and centred. A click inside the box does not start a selection.
- While scrolled up, a small pill centred at the bottom of the conversation, " ↓ 212 lines below · End ", jumps to the end on a click, as End does. The input box no longer says anything about it.
- One swapped view, the context breakdown. `/context` and Enter in the input box opens it, as does a click on the Session card's context bar or the line under it, or on the "ctx" summary in the narrow layout. It takes the conversation area under a header row ("Context  /context", "Esc returns"). The side panel and the input box stay, and typing still goes to the input box. The wheel and the arrow keys scroll the view. Esc, or a click on the header, returns to the conversation at exactly the row it showed; while following the output, it is still following. `/context` is the only slash command, since the slash command panel is not designed.
- The view draws one bar of the context by category against the handoff point, marked "handoff 400k", then a row per category with its size and share, the five largest tool results, and a note on what is estimated.
- `--log-input FILE` records the raw input and each frame's scroll position, so the owner can capture what Ghostty sends for a trackpad movement.

Not built: usage, tools, and the other panel views. The model picker draws from a fixture: the stream names only the current model, so the list of models and their roles is made up.

`cargo test` adds tests for the scroll anchor, the one-row group line and the context view's totals, and for the sideways wheel buttons in the input parser.

### Findings

#### Scrolling: diagnosis and fix

Reproduced in tmux at 160 by 48, `fixtures/idle.jsonl --static`, by sending SGR wheel events with `tmux send-keys -H`. `--log-input` recorded what was read and drawn. The trackpad itself was not observed: `CHECK.md` asks the owner for a log from Ghostty.

- The bounce: the input parser read a wheel event's direction from the lowest bit of its button, so 64 and 66 both scrolled up and 65 and 67 both scrolled down. 66 and 67 are the sideways wheel buttons, which a terminal sends for sideways scrolling, and a trackpad movement is rarely straight up. A small upward swipe that drifts sideways mixes 64 with 66 or 67, and each 67 scrolled down. That Ghostty sends 66 and 67 for a trackpad's drift is what the owner's log is to confirm. Five events, "64 67 64 67 67", meant as a small scroll up, moved the view 3 rows down; repeated six times, the view went down 18 rows while the person scrolled up. The parser now reads the two direction bits, and sideways events are ignored and draw no frame. After the fix the same six gestures move the view up 2 rows each, and a burst of 30 events in mixed directions and all four buttons lands exactly on its net count of 14 rows.
- Scrolling too far per step: each wheel event scrolled 3 rows. Ghostty already scales its events (`mouse-scroll-multiplier = precision:1,discrete:3`): one event per row of trackpad movement, three per notch of a mouse wheel. Scrolling 3 rows per event made the trackpad move 3 rows for every row the finger moved. It is now one row per event, with `--wheel-lines N` to tune. Terminals that send one event per notch may feel slow at 1; `docs/tui.md` should name the terminals checked.
- The view moving while scrolled up: the position was counted as rows above the end, and only growth was made up for. When rows went away below the view (a group's line going from two rows back to one) or the area under the conversation changed height (a notice, a "copied" row), everything shown moved. The position is now the first row shown, which nothing below it can move. Scrolling to the end follows the output again.
- Not a cause: nothing re-engaged following or reset the position. Only End, a click on the jump target, and scrolling to the end went back to following. The glimmer's timer never touched the position, and the replay only added to it as rows arrived.
- The flicker: nothing clears the screen per frame; ratatui clears only when the size changes. But a scroll moves every row, so each frame rewrote almost the whole conversation, about 7 KB in three flushes (ratatui's cursor hide, its own flush, then the link rewrite's), with no synchronised output. Ghostty draws on its own thread, so it can show a frame half written. Each frame is now wrapped in DEC mode 2026 (`CSI ? 2026 h` before the first byte, `CSI ? 2026 l` after the last, including the OSC 8 link rewrite), so the terminal shows only whole frames.
- A frame per wheel event: every event drew its own frame. Frames are now at most one every 16 ms, so a burst of wheel events is one frame. 30 events sent as fast as tmux sends them drew 12 frames.
- Every input event also threw away the conversation's rendered rows and rendered the whole session again, once per wheel event. Now only events that change the fold or the rows (keys and clicks that reach the panels or the input box, a group opened) do. Scrolling, selecting, searching and opening a view reuse the rows. The cost grows with the session, so the large session will show it; it was not measured here.
- The cost: the mode 2026 pair adds 16 bytes to every frame, so a glimmer frame went from 85 bytes to about 103 (one 10-second run). Idle is unchanged: no frame, no byte.

For `docs/tui.md`: every frame is written inside synchronised output; a frame is drawn at most every 16 ms; the scroll position is the first row shown; sideways wheel buttons are ignored; and the rendered rows survive scrolling.

#### The group line

- The summary line was word-wrapped, and while a group runs it lists every call in flight. As calls started and finished the line grew past the width and shrank again, going from one row to two and back. While following the output, every row above it moved up or down a row each time.
- The kinds were already in a fixed order. A kind appearing for the first time is inserted in its place, which moves the text after it along the row but does not change its height.
- The line is now always one row: "● Read 3 files, ran 1 command, thought once · cargo test -p log --test lock", with the duration and the ▸ toggle right-aligned. When it is too long, what is in flight is cut first with "…", then the kinds. The duration and the toggle are never cut.
- This also cuts a finished group's summary at narrow widths. At 122 columns, the conversation's width beside the panel in a 160-column window, the fixture's largest group reads "…ran 4 commands, started 1 d…  2m 08s ▸" and loses "thought once". `docs/tui.md` should say the line is one row, and what gives way first.

#### Search and the jump overlay

- The search box covers the right-hand end of the conversation's first three rows. The current match is centred, so it is never under the box, but other matches in those rows can be.
- The box's edges take the colour of whatever is under them, so they meet a bubble, a card or the plain background without a seam in tmux. Whether that holds in Ghostty is in `CHECK.md`.
- The jump overlay sits on the conversation's last row and hides the middle of it while scrolled up.
- "N lines below" counts rendered rows, so the number changes when the window's width does.

#### The swapped view

- Returning to exactly where the person was needs nothing extra once the position is the first row shown: the view keeps its own scroll and never touches the conversation's.
- Esc now has one more thing to close. The order is: the selection's highlight, the search box, the swapped view, then the approval or form, then the turn.
- Ctrl+F closes the view and opens search over the conversation. Searching inside a view is not designed.
- Slash commands are the TUI's own: `/context` is caught before Enter would send a `prompt` or `steer`. The slash command design should say that a slash command never reaches Fiber as text.
- The view is rebuilt every frame, walking the whole session, a cost that grows with the session. Fiber should keep running totals in the fold.

#### Data the stream does not carry

- The size of each part of the context. The stream carries the text of every part: the system prompt and tool definitions in `preamble_built`, instruction files and the skills listing in `opening_message`, and messages, reasoning, calls and results in their own lines. But `usage_recorded` counts tokens only for the whole request, and nothing counts them per part. The prototype estimates each part at 4 characters a token and shows the rest of the reported total as "not attributed". The fixture shortens the system prompt and instruction files to "…", so 94% of its context is not attributed. A real split needs Fiber to count tokens per part, with the provider's token counting or a local tokenizer, and write the counts where the view can read them, such as on `preamble_built` and each result.
- The handoff point and the model's context window, as in stage 1. The bar is drawn against them.
- What is in context now. After a handoff or a rewind only part of the log is in context; the view has to work out which part from `handoff_completed` and the rewind lines. The fixture has neither, so the prototype counts the whole session.
- For the model picker, the models and their roles. The stream names only the current model (`preamble_built`, `model_changed`). The cache rebuild size can be estimated from the last `usage_recorded`.

## Model picker (#1629)

A second swapped view, drawn like the context breakdown: Ctrl+L and `/model` open it over the conversation area, under a "Models /model" header, with the side panel and the input box still there. Typing filters the list while it is open. It draws from a fixture in `src/model_picker.rs` (three providers, twelve models): the stream names only the current model, so the roles, thinking levels and rebuild costs are made up. A centred panel on the raised surface; dim provider sections with a blank row between them; the focused model on a full-width accent bar with a `›` marker; the model name in the accent colour, its thinking chips in the attention colour, the rest dim.

| Case | How to reach it |
|---|---|
| `list` | `cargo run --release -- fixtures/idle.jsonl --static --picker list`, or Ctrl+L / `/model` live; the search line reads `Type to search` until typed |
| `levels` | `cargo run --release -- fixtures/idle.jsonl --static --picker levels`: the current model's thinking chips focused, as after clicking a thinking chip |
| `scoped` | `cargo run --release -- fixtures/idle.jsonl --static --picker scoped`: the `scoped_models` set, five of twelve, with a show-all toggle |
| `scoped-all` | `cargo run --release -- fixtures/idle.jsonl --static --picker scoped-all`: all twelve, the scoped five marked |
| `refreshing` | `cargo run --release -- fixtures/idle.jsonl --static --picker refreshing`: one provider refreshing in the background, the others with an updated-ago age |
| `session-only` | `cargo run --release -- fixtures/idle.jsonl --static --picker session-only`: a non-current model focused with the Ctrl+S mark, "this session only · nothing saved" |
| `filtered` | `cargo run --release -- fixtures/idle.jsonl --static --picker filtered`: the query `mini`, four matching models across two providers, matched letters underlined |
| `filtered-empty` | `cargo run --release -- fixtures/idle.jsonl --static --picker filtered-empty`: the query `zzz`, one muted `No models match` line |
| `hub` | `cargo run --release -- fixtures/idle.jsonl --static --picker hub`: the hub's catalogue on a remote terminal, two providers with five models and none of the twelve local ids |
| `install-missing` | `cargo run --release -- fixtures/idle.jsonl --static --picker install-missing`: a not-installed first-party provider below the installed ones, marked not installed, its models dim |
| `install-running` | `cargo run --release -- fixtures/idle.jsonl --static --picker install-running`: the same row while its install runs, reading `installing` with the spinner |
| `install-failed` | `cargo run --release -- fixtures/idle.jsonl --static --picker install-failed`: the same row keeping its install error in error under its dim models |
| `install-done` | `cargo run --release -- fixtures/idle.jsonl --static --picker install-done`: the installed provider listed with its install summary and the `fiber login` line |

Live keys while it is open: typing filters the list over provider, id and display name with the matched letters underlined, Backspace edits the query, Esc clears the query first and then closes, Enter on an empty list stays open; ↑/↓ move between models, ←/→ between the focused model's thinking chips, Enter chooses and closes, Ctrl+S marks the choice this session only (then nothing is saved), Tab toggles show all when scoped, Ctrl+R refreshes every list (cosmetic). Clicking a row focuses it; clicking a chip focuses that chip.

## Completions (#1631)

Typing `/` opens one completion panel above the input box. Commands, skills, prompt templates and MCP prompts share one list, filtered as the person types. Each row is a name, a one-line description, a skill's `argument-hint` when it has one, and a tag: command, skill, template, the extension's name, or the MCP server's name. Tab completes and Enter runs. Typing `@` opens a file search panel, and choosing a file inserts its path as text. The panel draws from fixtures in `src/completions.rs` (forty entries over every tag kind; ten file paths, two under a `~/proj/` path search): the stream has no command list, so both are made up. At most eight rows show with the focused row kept on screen and a scroll hint (`1–8 of 40 · ↓ 32 more`); a long description is cut with an ellipsis keeping the tag, and matched letters read bold.

| Case | How to reach it |
|---|---|
| `slash` | `cargo run --release -- fixtures/idle.jsonl --static --completions slash`: `/` with an empty filter, eight of forty rows with a scroll hint |
| `slash-filtered` | `cargo run --release -- fixtures/idle.jsonl --static --completions slash-filtered`: the input reads `/re`, only matching entries, matched letters bold |
| `slash-hint` | `cargo run --release -- fixtures/idle.jsonl --static --completions slash-hint`: the `review` skill focused with its `<path>` hint, and the overlong `login` description cut with … |
| `at` | `cargo run --release -- fixtures/idle.jsonl --static --completions at`: the input reads `@test`, two file matches |
| `at-empty` | `cargo run --release -- fixtures/idle.jsonl --static --completions at-empty`: the input reads `@zzz`, a `no files match` row |
| `at-path` | `cargo run --release -- fixtures/idle.jsonl --static --completions at-path`: the input reads `@~/proj/`, a directory row and a file row |
| `narrow-slash`, `narrow-at` | the same panels; run in a 100x40 terminal for the existing narrow layout |

Live keys: typing `/` or `@` at the start of the input opens the panel. While it is open: ↑/↓ move, Tab completes and keeps the panel open, Enter on a `/` row completes it and runs it at once (`/context` and `/model` open their views, anything else sends), Enter on an `@` row inserts its path as text, Esc closes.

## Swapped views (#1633)

Ten still frames from the fixtures in `src/views.rs`, one per swapped view docs/tui.md lists past the context breakdown and the model picker. Each takes the conversation area under its header row (the view's name bold, its command dim, `Esc returns` dim right-aligned) and waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still loads but the views ignore it. Every case also renders at 100x40 for the narrow layout; capture at 160 by 48.

| Case | How to reach it |
|---|---|
| `delegate-view` | `cargo run --release -- fixtures/session.jsonl --static --view-case delegate-view`: the breadcrumb, the status card (harness, model, calls, elapsed, session, stop target), the transcript in turn cards, and the input box sending steering to the delegate |
| `job-pty` | `cargo run --release -- fixtures/session.jsonl --static --view-case job-pty`: the job's live 80x24 screen from attach, its status with the stop target, and the input box typing into it |
| `running` | `cargo run --release -- fixtures/session.jsonl --static --view-case running`: two rows per running delegate and one row per running job, in start order |
| `usage` | `cargo run --release -- fixtures/session.jsonl --static --view-case usage`: usage by turn, by model and by delegate, with billed and subscription cost and the budget left |
| `changed-files` | `cargo run --release -- fixtures/session.jsonl --static --view-case changed-files`: the changed files with the chosen file's hunks as a diff against HEAD; the untracked file wholly added |
| `search-results` | `cargo run --release -- fixtures/session.jsonl --static --view-case search-results`: every match one row each with its surrounding lines, each with a dim turn label, the current match brighter |
| `tools` | `cargo run --release -- fixtures/session.jsonl --static --view-case tools`: every tool by source with state and size, two on/off switches per MCP or extension tool, built-ins without a switch |
| `rules` | `cargo run --release -- fixtures/session.jsonl --static --view-case rules`: global rules then this project's, each with its prefix, origin and revoke mark |
| `settings` | `cargo run --release -- fixtures/session.jsonl --static --view-case settings`: the keys with their effective values and layers, and the reload cost |
| `skills` | `cargo run --release -- fixtures/session.jsonl --static --view-case skills`: one row per skill with its source, visibility, shadowed skill and switches |

## Opened tool calls (#2240)

Seven still frames for the opened-call cases #2240 draws as ruled on #2235: five calls opened inline under their ledger rows, the swapped view of a cut shell call, and the changed-files view from the section above redrawn on the shared diff format. Each inline frame draws one group through the ledger builders with the call opened, the same rows the conversation draws: a dim line-number gutter, `@@` lines dim, `-` lines in `removed`, `+` lines in `added`, differing words in reverse video on paired lines, and past twenty rows a dim "… N more lines (Enter to view)". Each waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still loads but the cases ignore it. Every case also renders at 100x40 for the narrow layout; capture at 160 by 48. `./capture-opened-calls.sh` captures every new case at both sizes, plain text and SGR: `OUT=<dir> ./capture-opened-calls.sh` writes `<case>-160x48.txt` and `<case>-100x40.txt` with `sgr/` beside them.

| Case | How to reach it |
|---|---|
| `opened-edit` | `cargo run --release -- fixtures/session.jsonl --static --view-case opened-edit`: an edit opened inline, its diff with a gutter and word pairs, clipped past twenty rows |
| `opened-read` | `cargo run --release -- fixtures/session.jsonl --static --view-case opened-read`: a read opened inline, its text numbered from the read's offset |
| `opened-search` | `cargo run --release -- fixtures/session.jsonl --static --view-case opened-search`: a search opened inline, matches grouped under each path with the pattern on the match tint |
| `opened-shell` | `cargo run --release -- fixtures/session.jsonl --static --view-case opened-shell`: two shells opened inline, a failure closing in `removed` and a command killed by a signal |
| `opened-mcp` | `cargo run --release -- fixtures/session.jsonl --static --view-case opened-mcp`: MCP content as plain text and an unknown tool's JSON indented |
| `cut-shell` | `cargo run --release -- fixtures/session.jsonl --static --view-case cut-shell`: the swapped view of a cut shell call, the whole output around the dim marker where the model's 16 KiB view ended |
| `changed-files` | `cargo run --release -- fixtures/session.jsonl --static --view-case changed-files`: the changed-files view from the section above, redrawn with the shared gutter and word pairs |

## Repository offer and one-question forms (#1636)

Five still frames from the fixtures in `src/offers.rs`: the repository's offer of what it ships, and the one-question forms an extension's `host.ask` and MCP elicitation raise. The offer takes the conversation area under its header row (the view's name bold, `repository code` dim, `Esc returns` dim right-aligned); each form draws at the bottom of an otherwise quiet frame, an `approval` slab with the `▌` stripe on every row, the title `Question from <session>` with `Esc: chat about this` right-aligned and no tab row, ending in `Chat about this`. Each waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still loads but the offers ignore it. Every case also renders at 100x40 for the narrow layout; capture at 160 by 48.

| Case | How to reach it |
|---|---|
| `offer` | `cargo run --release -- fixtures/session.jsonl --static --offer-case offer`: the pending extension, hook and MCP server, each with what an install shows and three choices starting at skip, the changed server's diff, the TUI-files sentence, and the choose/send/put-aside hint |
| `confirm` | `cargo run --release -- fixtures/session.jsonl --static --offer-case confirm`: the question with yes and no, and Chat about this with no Submit row |
| `select` | `cargo run --release -- fixtures/session.jsonl --static --offer-case select`: the question with labelled options and descriptions, and Chat about this with no Submit row |
| `multi_select` | `cargo run --release -- fixtures/session.jsonl --static --offer-case multi_select`: the question with toggles and choose any, the Submit row, then Chat about this |
| `text_input` | `cargo run --release -- fixtures/session.jsonl --static --offer-case text_input`: the question with only the answer-in-words row, the Submit row, then Chat about this |

## Images and links (#1637)

| Case | How to reach it |
|---|---|
| `tool_image` | `cargo run --release -- fixtures/session.jsonl --static --image-case tool_image`: the read ledger row with its blue tick and dimensions, the image below it through kitty placeholders where the terminal speaks the protocol and its one line everywhere else |
| `fallback` | `cargo run --release -- fixtures/session.jsonl --static --image-case fallback`: the one-line image row in the link look, and a second row for an image that cannot show with the notice saying why |
| `pasted` | `cargo run --release -- fixtures/session.jsonl --static --image-case pasted`: the prompt bubble with [Image #1] and the image through kitty placeholders where the terminal speaks the protocol and its one line everywhere else |
| `links` | `cargo run --release -- fixtures/session.jsonl --static --image-case links`: the reply with its focused and hovered links, the table with its focused and hovered link cells, the focused link on the selection tint with the Enter-opens hint |
| `screen_reader` | `cargo run --release -- fixtures/session.jsonl --static --image-case screen_reader`: the flat frame with no tints, edges, stripes or animation, one line per item in reading order, state in words, the image as its one line |
| `title_approval` | `cargo run --release -- fixtures/session.jsonl --static --image-case title_approval`: the labelled OSC 2 row for `! fiber · approval`, written to the terminal and restored to `fiber` on exit |
| `title_question` | `cargo run --release -- fixtures/session.jsonl --static --image-case title_question`: the labelled OSC 2 row for `! fiber · question`, written to the terminal and restored to `fiber` on exit |
| `title_waiting` | `cargo run --release -- fixtures/session.jsonl --static --image-case title_waiting`: the labelled OSC 2 row for `! fiber · waiting`, written to the terminal and restored to `fiber` on exit |
| `title_finished` | `cargo run --release -- fixtures/session.jsonl --static --image-case title_finished`: the labelled OSC 2 row for `✓ fiber · finished`, written to the terminal and restored to `fiber` on exit |

Each case draws one still frame and waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still loads but the images ignore it. Every case also renders at 100x40 for the narrow layout; capture at 160 by 48. With no `--image-case`, `cargo run --release -- fixtures/session.jsonl --static --screen-reader` draws the replayed fixture flat instead of the conversation: one line per item in reading order, state in words, images as their one line.
## Themes (#1634)

`--theme CASE` draws the screen in another theme or colour depth and
combines with any other flag: the same `--static` fixture frame, overlay,
home screen or swapped view, recoloured once at the backend. The default is
dark truecolour. `NO_COLOR` set and not empty turns colour off whatever the
flag says. At 256 colours each colour is the nearest xterm palette entry:
neutral tints take the grey ramp, other tints the colour cube, text either;
the terminal's own 16 colours are never used. With colour off every role is
the terminal's default, bold, dim, reversed and underline stay, and
half-block edges draw blank but keep their rows.

```sh
cargo run --release -- fixtures/session.jsonl --static --theme light
cargo run --release -- fixtures/session.jsonl --static --theme dark-256
cargo run --release -- fixtures/session.jsonl --static --theme light-256
cargo run --release -- fixtures/session.jsonl --static --theme dark-no-color
NO_COLOR=1 cargo run --release -- fixtures/session.jsonl --static
```

| Case | How to reach it |
|---|---|
| `light` | `--theme light`: the light theme in truecolour |
| `dark-256` | `--theme dark-256`: the dark theme forced to 256 colours |
| `light-256` | `--theme light-256`: the light theme forced to 256 colours |
| `dark-no-color` | `--theme dark-no-color`, or `NO_COLOR=1`: no colour anywhere |

## Retries, failed turns, crash recovery and rewind (#1635)

Eight still frames from the fixtures in `src/failures.rs`: the pending retry on the working line, the failed turn, the handoff nudge, the dropped-connection banner, the crashed turn, and the `/rewind` view with its `⟲` line. The wording is docs/tui.md's ("The working line", "Errors and retries", "Handoff", "A dropped connection", "After a crash", "Rewind"); the code names are docs/errors.md's. Each draws over a quiet conversation stub and waits for a key; Esc, q or Ctrl+C quits. Combine with `--static`; the fixture still loads but the failures ignore it. Every case also renders at 100x40 for the narrow layout; capture at 160 by 48.

| Case | How to reach it |
|---|---|
| `retry` | `cargo run --release -- fixtures/session.jsonl --static --failure-case retry`: the pending retry on the working line, and 1 failed model call on the group line above |
| `failed` | `cargo run --release -- fixtures/session.jsonl --static --failure-case failed`: the ✗ line with its code, the provider's words dim under it, then ▣ failed |
| `auth_failed` | `cargo run --release -- fixtures/session.jsonl --static --failure-case auth_failed`: the authentication_failed line with log in |
| `nudge` | `cargo run --release -- fixtures/session.jsonl --static --failure-case nudge`: the dim ◔ Context at 268k line where it happened |
| `connection_lost` | `cargo run --release -- fixtures/session.jsonl --static --failure-case connection_lost`: the banner replacing the working line, with the draft kept |
| `crashed` | `cargo run --release -- fixtures/session.jsonl --static --failure-case crashed`: ▣ cut short, the may-have-run ledger row, the ↺ resumed band, and the orphaned-jobs line |
| `rewind` | `cargo run --release -- fixtures/session.jsonl --static --failure-case rewind`: the /rewind view with turns and step boundaries |
| `rewound` | `cargo run --release -- fixtures/session.jsonl --static --failure-case rewound`: the ⟲ line after a rewind |
