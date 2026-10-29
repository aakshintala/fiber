# The terminal

What Fiber looks like at the terminal, and how a person works it. This is what
is true now, not a plan. It is settled by
[TUI: scrollback or full screen?](https://github.com/aakshintala/fiber/issues/15)
and [Terminal: a control center over many sessions](https://github.com/aakshintala/fiber/issues/258);
those tickets' resolutions hold the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Session, turn, step, step boundary, action, tool
call, job, delegate, handoff, steering message, client and driver mean what it
says there and nothing else. The events the terminal reads are
`docs/events.md`; the commands it sends are `docs/invocation.md`, "Driver
commands".

## What the terminal is

- **The terminal ships in the `fiber` binary, and the hub serves no UI.** It
  ships in the binary so that a fresh machine or an SSH box has a door with
  nothing installed, not because it has powers another client lacks. A GUI, a
  phone app or a web page is a client of the hub with exactly the terminal's
  powers (`docs/invocation.md`, "The hub"). Which of them are first-party is
  decided by the application that needs them, not here.
- **It is a control center over many sessions.** It shows every live session
  through the hub, from any project, and one of them on screen at a time. It
  runs from any directory.
- **It is written in Rust with ratatui and crossterm,** in the `fiber` binary,
  as the `tui` crate (`docs/architecture.md`). It prefers existing crates to
  writing its own. Their memory is recorded but is not a veto; idle CPU, the
  first-frame budget and supply-chain checks stay strict
  (`docs/dependencies.md`, "Measuring memory", and "Performance" below).
- **It reads the event stream and nothing else.** It is its own process and a
  client of the hub: a `full` connection to the session on screen, a
  `summary` connection to each session it has opened, stopped or closed, and
  the hub's feed for every other live session (`docs/invocation.md`,
  "Processes"). When a screen
  needs data the stream does not carry, the stream changes; the terminal has no
  other path to state.
- **It is full screen only.** There is no scrollback mode. The mouse is a
  primary input, and every action also has a keyboard or slash-command path
  ("Keys").
- **History is paged from the session.** The terminal holds a window of
  rendered history and asks the session for more with `history` as the person
  scrolls ("History and paging"). It never reads a log from disk, so a local
  and a remote terminal page the same way.

An extension may change everything the terminal draws, through the slots in
"Extension seams".

## Home

Home is the rail at full width: the same sessions in the same project groups,
with room to say what each one waits on. `fiber` opens on it. From a session,
← in an empty input box focuses the rail, and a second ← widens it to home
("The rail"); ⌥0, `/home`, `/new` and `/resume` go there too. → or Esc goes
back to the session the person came from. Home has no panel and no rail of
its own. From the top:

1. the header: the logo at the top left ("The logo"), and beside it two lines
   centred on the logo's rows. The first is the version, dim. The second
   counts the live sessions across every project by state, each count in its
   state's colour ("State glyphs") and joined by a dim " · ", leaving out a
   zero count, then the summed spend of the live sessions, dim: "2 waiting on
   you · 2 working · 1 retrying · 1 crashed · 2 ready · $5.78 live". With no
   live session it reads "no live sessions", dim.
2. the session list, filling the rows between the header and the input box
   ("The session list")
3. the input box, at the bottom where a session's input box sits, so widening
   from a session never moves it. Its bottom row carries the workspace chip,
   the new worktree switch, the model and thinking chips and "Enter starts a
   session".

A key hint sits at the foot: "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit". Each key shows bold and the rest dim; Enter, Delete and Esc name their action's first bound key, leaving an unbound action's part out, while Ctrl+C cannot be rebound so its hint stays fixed. "→ or Esc back to the session" joins it after "↑↓ move", only when home was entered from a session whose row is still live. The key goes back to that session when the box is empty, nothing is open and no notice is up; Esc also goes back with a chip focused, while → then moves between the chips. A session that exited or closed hides the hint and leaves both keys untaken; with the link down or a `start` in flight the keys are taken as a no-op and the hint stays. Until the first prompt, the input box's
placeholder says "/? for shortcuts". Typing a prompt and pressing Enter asks
the hub to start a session in the chosen workspace, and the screen switches to
it. No session exists before Enter, so opening the terminal, glancing at home
and leaving creates nothing. The first turn waits for any MCP server that
starts with the session (`docs/mcp.md`, "Starting servers"), and the working
line says so: "Connecting 2 MCP servers…".

On the first interactive run, with no import receipt yet
(`docs/configuration.md`, "Importing"), home offers a one-time import of
another harness's configuration: Claude Code's or Codex's.
The offer runs the same shared import `fiber import` runs, and imported
items apply on the next `/reload` with the reload's usual cache cost
(`docs/mcp.md`, "Reload"). Accepting the offer opens one import report
overlay ("Look", "Overlays") and adds no notices. A `1` or `2` pressed before the hub connects
still imports once it does, so an early press is never lost. Over a
remote hub connection the receipts are the hub machine's, where that
import runs (`docs/configuration.md`, "Importing"). The offer fires once: answering it, or running
`fiber import` first, writes the receipt, and the offer never shows again.

**The workspace chip** names the workspace in use: the one last picked, else
the directory the terminal was launched in. Clicking it opens the workspace
picker, the same on every client. It is an overlay centred over home, unless
that would cover the input box's top row: then its bottom edge sits on the
row above the box ("Look", "Overlays", "Placement").

- It opens browsing, under the title "Workspaces". The first row is the typed
  path, "› " in `info`, the text and the cursor, filled with the chip's
  workspace and a trailing `/`, or `~/` while no workspace is chosen. The
  completions sit under it, the focused one on the selection bar, then a
  blank row, "── recents ──" dim and the recent rows, dim. With no recent rows
  the divider is left out too. The footer is "↑↓ move · Enter open · Esc
  closes".
- The recent rows are one list: the launch directory, when there is one, then
  the recent workspaces from `recent.jsonl` (`docs/state.md`), the live
  sessions' workspaces then the exited ones', distinct and absolute, ten rows
  at most. The list refills as sessions arrive until a key is pressed in the
  picker, and again on every row that arrives once Backspace leaves the
  recents alone.
- The completions are the subdirectories of the directory up to the typed
  path's last `/` whose names start with the text after it: one listing of
  that directory, filtered at each keystroke, never a walk of the tree. A
  hidden directory is listed only when the typed name starts with `.`. `~`
  is the hub's home, and a path that is neither absolute nor starts with `~`
  has no completions. The listing is the hub's `directories` command
  (`docs/invocation.md`, "What the hub speaks"), so a remote client's
  completions list the hub's machine.
- The first completion is focused. ↑ and ↓ move through the completions,
  then the recents. Typing edits the path. Tab puts the focused completion
  into the path with a trailing `/` and lists that directory. Enter opens the
  focused row as the workspace. With no completion matching, Enter takes the
  typed path when it names a directory; otherwise nothing opens and the list
  holds one dim row, "no such directory". Backspace past the last character
  leaves the recents alone, the first focused.

While no workspace is chosen, the chip reads "no workspace", dim, the
picker's footer is "↑↓ move · Enter open", and Esc does not close it. Enter
on a prompt in that state reopens the picker, sends nothing and keeps the
prompt. A remote client has no launch directory, so its home starts in that
state, with the picker open on `~/`.

Until the typed path's directory has answered, the picker shows the typed
row and the recents with no completions, and Enter does nothing. While the
connection is down the "Connection lost" banner shows over home and Enter
stays inert until the connection is back. Enter on `~` alone asks for the
hub's home listing; a later Enter, once it has answered, opens the home
directory. A failed listing shows no completions and no error: the recents
stay, and the directory is not asked again.

**The new worktree switch** sits beside the workspace chip. When it is on,
the session starts in a new worktree of the workspace, by the same rules a
delegate's worktree follows (`docs/invocation.md`, "Isolation"). Outside a git
repository the switch is not shown.

**The model and thinking chips** name the configured model and its thinking level. Clicking the model or thinking chip opens the model picker. The thinking chip opens it on the model's thinking levels, with focus starting on the level chips.

**The keyboard reaches every part of home.** Focus runs top to bottom: the
session list, the entry bar, the chip row. Widening from a session focuses
that session's row; `/new`, and launching `fiber`, focus the entry bar;
`/resume` focuses the first exited row. ↑ and ↓ move through every row, live
and exited, across the project sections. ↓ from the last row focuses the
entry bar, and ↑ from an empty entry bar focuses the last row; on home no
session is on screen, so ↑ there recalls no prompt. ↓ from the entry bar
focuses the chip row, on the chip focused there last, the workspace chip at
first, and ↑ goes back. ← and → move between the chips and stop at the ends. → or Esc goes back to the session home came from when the box is empty and nothing is open; Esc also goes back with a chip focused. Esc is `close_or_interrupt`'s bound key.
Enter on a focused chip does what a click does: the workspace chip opens the
workspace picker, the new worktree switch toggles, the model chip opens the
model picker, and the thinking chip opens it at the model's thinking chips.
The chip stays focused behind the picker. With no session, ↑ on the entry bar
does nothing. Typing or Backspace while a chip or a row is focused goes to the
entry bar. A focused chip is drawn as a hovered one ("Mouse and hover").

The first frame draws at once. The session list fills in when the hub's feed
arrives (`docs/performance.md`).

A failed attach says why on home: the session is held, or its status cannot be read ("The session list").

**When a session cannot start,** as on a fresh install with no key, or after
a person removed the provider they chose, home shows the same lines `fiber doctor` prints for each blocker, each
with its fix, above the input box (`docs/invocation.md`, "Commands and flags"). A person
who runs `fiber` before reading anything is told what to do next.

### The logo

The logo is `⌇ fiber 0.0.1`: the ⌇ in the accent colour, the name in the
accent gradient, the version dim.

- **On home it is four rows tall and 32 columns wide, at the top left of the
  header,** in pixel letters drawn with half blocks, with each letter's
  counter shaded. The version sits beside it in the header, not after the
  name.
- **Where the terminal speaks an image protocol** (kitty, iTerm2 or Sixel), an
  image replaces the pixel letters in exactly the same cells: a smooth wave
  and the name set in JetBrains Mono ExtraBold, in the gradient. Detection
  never delays the first frame. The pixel logo draws first, and the image
  replaces it when the terminal's reply arrives.
- **The image is an alpha mask built from the font with `cargo xtask logo-mask` and checked in,** tinted
  with the theme's accents at run time. Fiber bundles no font, and the logo
  follows any theme.
- **Where the screen is too short for four rows,** the logo is one row:
  `⌇ fiber 0.0.1`. Fiber cannot tell whether the terminal's font has ⌇, so
  `tui.logo_glyph` switches it to ≈ ("Configuration").

### Approving what a repository ships

Extensions, hooks and MCP servers a repository declares are offered by the
session that would load them (`docs/extensions.md`, "Code a repository
ships"). When the hub starts a session in a workspace whose repository
declares code with no approval for its content, the session raises one offer
before its first model request, so approving costs no prompt-cache rebuild,
and any client can answer it. The terminal shows it in the session's view, as
a swapped view listing every pending item with what an install shows, a diff
for an item whose content changed, and three choices for each: approve, skip
for this session, or never. A package's TUI files are never installed from a
repository, and the offer says so. Each item starts at skip; ↑ ↓ choose an
item, ← → its answer, and Send answers them all at once. Esc puts it aside
behind the approval badge, and ⌥A or `/approvals` reopens it, as for an
approval. Enter on an item opens its first file through `offered_file`; ← →
step between files, ↑ ↓ and PageUp/PageDown scroll, and Esc returns to the
list with its choices kept. The source view keeps the Offer top row, then
shows the item's kind, name, path and file number above its full text,
wrapped without highlighting or line numbers; while waiting it shows
"Loading…", on refusal the message, and for empty text "(empty file)".
The source view closes on reconnect; replay shows the offer list.

The view's top row names it "Offer", with "repository code" dim. Each item
is a row naming its kind and name with what identifies it ("Extension notes ·
v0.3", "Hook tidy · before_tool on read, shell", "MCP server linear ·
declared in .fiber/mcp.json"), "▸ " in `secondary` and the row in
`secondary` on the chosen item. Under it, dim, what an install shows: where
it comes from and where it loads, what it runs, and what it carries. A
changed item says "the declaration changed", dim, then its diff drawn as
"Changed files" draws a hunk. Last comes the item's choices on one row:
"( ) approve", "( ) skip for this session" and "( ) never", the chosen one's
mark "(•)" in `secondary` and its label bold, the other marks dim. After the
items, "A package's TUI files are never installed from a repository." and
the hint "↑↓ choose an item · ←→ its answer · Send answers them all · Esc
puts it aside", both dim.

### The session list

Home lists every project, in the rail's order: the launch project first, then
the others in the order their first session started. Each project is a
section, and the sections scroll when they outgrow the screen. When the
list is one row tall, it shows the focused row's first line.

- **The section's header** is the project's name, bold in the accent colour,
  and a "+" that starts a session in that project, as on the rail; then, dim,
  "N live · $spend", the count and summed spend of its live sessions; then
  "N waiting on you" in `attention` when any wait.
- **Each live session is two rows,** in the order the sessions started, as
  the rail's cards are. The first row: "⌥N", the card's number, dim; the state
  glyph ("State glyphs"); and the name, or the first prompt when it has none;
  then, at the right end, the state's word in its colour and how long the
  session has been in that state, dim, in the rail's units. The second row,
  indented and dim: the id, the turns, a ⎇ and the branch, the spend and the
  context percentage, joined by " · "; then, in `attention`, what it waits
  on, in full: "approval: shell cargo publish --dry-run", "question: 2 of 3
  answered".
- **A session closing after `/clear`** stays a live row, its word "CLOSING",
  dim, until it exits ("Slash commands"). The row gets that state from
  `session_status.closing`.
- **The project's exited sessions** follow under a dim "exited · N": up to
  three, the most recent first, each one dim row: ○, the name, and at the
  right the id, turns, spend and age ("3h ago"). A session that moved
  workspace appears like any other exited session. When there are more, a
  last row reads "… N more", and Enter or a click on it lists
  the rest of the project's exited sessions in place.
The focused row is drawn on the selection bar. With no session, the list's
rows hold two dim lines, centred: "No sessions yet." and "Type below and
press Enter to start one."

Live sessions come from the hub's feed, across every project, grouped by the
`project` on each `session_status`. Exited sessions come from `recent.jsonl`,
then from the hub's paged query for older ones, grouped by the `project` on
each row.

Each project's "exited · N", "… N more" and the rail's "N ✓" count from the `total` of a `recent` scoped to that project. The terminal asks for one when the project first appears, after a reconnect, and when one of the project's sessions leaves or is deleted, with one ask in flight per project; it never adjusts a count itself. Until the first answer arrives, N counts the rows loaded. "… N more" counts the rows the hub holds past those shown, and opening it pages the project's `recent`.

Clicking a row, or Enter on it, opens the session: an exited one is resumed by
the hub. A resume the hub refuses because the workspace is gone shows
"Cannot resume: <path> no longer exists. Delete removes the session.": it
adds no rail card, and the row stays under "exited". A ✕ on a live row stops that session ("Quit"). Delete on a
focused live row stops it too, after asking ("Quit"); on an unreadable row it does nothing, and with the link down it opens no question. Backspace never stops a session, whatever the row. Delete on a
selected exited row deletes that session through the hub, after asking, and
the question names any session `--cascade` would add (`docs/invocation.md`,
"Deleting and pruning"). `/delete` deletes the session on screen instead:
it asks the hub for the exact sessions the delete would remove, asks the
person with those sessions named, stops every running one first, then
deletes that same set, which never changes after the question. Esc, a click
outside the frame or on its ✕ cancels and deletes nothing. It waits for fresh
exit evidence only for held sessions represented in the feed; a delegate
shuts down with its parent, and the hub's exact-set `session_held` refusal is
the backstop. A stop the hub refuses deletes nothing and names the session
still running; a stop with no answer shows `Stop status unknown`. A delete
refused because a session is running again keeps the attached session with a
`Not deleted` notice and does not retry; any other post-stop refusal returns
home with its stopped rows and the hub's message. A row whose status this terminal cannot read says CANNOT ATTACH and why: "Cannot attach: this session runs Fiber <v>" when the hub runs another Fiber version or commit than this terminal, since a session runs the binary of the hub that started it, with `<v>` as `fiber --version` writes it; otherwise "Cannot attach: its status did not parse". Opening the row shows the same text as a `session_held` notice.

The mouse wheel over the list's rows scrolls the list by three rows,
leaving the focus where it is.

### On exit

The terminal is restored and one line is printed per live session: its id and
the command to resume it.

## Layout

The screen is a session rail on the left, a conversation column, and a side panel on the right. The conversation
column has no header row: its first row is conversation. The conversation keeps
one blank column on each side, so the turn cards start one column in from the
rail's edge and end one column before the panel's; with the rail hidden its
one-column grip sits at the screen's left edge and the cards start at the third
column. The panel is shown while the screen is wide enough, unless the person hides it with ⌥P. It replaces
a footer and status line, and the input box spans only the conversation
column.

The rail and the panel each take a share of the screen's width, kept between
a floor and a ceiling: the rail 15%, from 22 to 48 columns, and the panel 21%,
from 30 to 60. Dragging the edge between either and the conversation resizes
it, and saves the new share as `tui.rail.width` or `tui.panel.width`, so one
setting suits a laptop and a 4K screen alike. A drag stops where the
conversation would fall below its minimum width. Each draggable edge shows a
dim grip, `⋮` on three rows at mid-height; under the pointer, and while
dragging, the whole edge column takes the `rule` tint, the grip turns bold in
the accent colour, and the pointer becomes a resize arrow (OSC 22, where the
terminal supports it). The rail's and the panel's regions take the `panel`
tint from top to bottom. The numbers are starting values, to be tuned once the terminal
is built.

### The rail

The rail lists every live session, across every project, as cards grouped
under project headers. The launch project's group comes first, then the
others in the order their first session started. The rail is drawn from the
hub's feed (`docs/invocation.md`, "The hub") and shows beside every session,
one session included, unless the person hides it. ⌥R and `/rail` show or hide
it and save the choice as `tui.rail.shown`, so a person who never wants the
rail sets it once. Home is the rail at full width ("Home").

**← reaches the rail.** In an empty input box, ← moves focus into the rail,
on the card of the session on screen; with text in the box, ← moves the
cursor. The focused card is lifted: its tint lifted, as hover draws it
("Mouse and hover"); with no colour it is bold and its box is not dim, while the other cards' boxes stay dim. The input box's stripe and text dim while the rail has
focus, and the foot hint reads "← full screen · ↑↓ move · Enter switch · Delete stop · → or
Esc back to the session · ⌥1–9 jump". ↑ and ↓ move between cards across the
project groups, Enter switches to the focused card's session, and → or Esc
gives focus back to the input box. Delete on a focused live card asks "Stop <name>?" over the conversation ("Quit"), with or without a draft in the box; the draft stays as it was. On a crashed or CANNOT ATTACH card, a project header's "+" or "N ✓" it does nothing. Typing on a focused rail goes to the draft and keeps the focus on the rail, and Backspace edits the draft and never stops a session; a key that types nothing, Delete by default, is what stops, and a key rebound to a character types it. A second ← widens the rail to home. When the session on screen has exited, it has no card: ← opens home at once, with focus on that session's row under "exited". When
the rail is hidden, by the person or for width ("Shedding"), one ← from an
empty input box opens home.

A project header holds the project's name, bold in the accent colour, a "+"
that starts a session in that project, and, right-aligned and dim, the summed
spend of its live sessions and "N ✓", the count of its exited sessions, which
widens to home with focus on that project's first exited row.

Each card is four rows on its state's tint (the theme's `card_*` roles), inside ▄ and ▀ edges, with a ▌ stripe
on the left in the state's colour:

```
▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
▌1 ! NEEDS INPUT         16s
▌  fix flaky lock test
▌  approval: shell cargo mu…
▌  $1.35 ▆▆▆▆▆▆░░░░░░░░  12%
▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
```

1. the card's number, the state glyph and its word, and how long the session
   has been in that state, in one unit (`16s`, `9m`, `2h`, `3d`)
2. the name, or the first prompt when it has none
3. what it waits on, in the attention colour, while it waits; otherwise a dim
   ⎇ and its git branch
4. its spend, a bar of how full its context is, and the percentage. The bar
   fills with ▆ on a ░ track in `rule`; the fill turns the warning colour from
   60% and the error colour from 85%.

Text on each side keeps the same margin from the card's edge. Below a 30-column
rail the bar goes and the spend and percentage stay. The session on screen
has a brighter tint than the others. Hover brightens a card and shows one
line at the rail's foot with the full name, workspace and model.

Cards keep the order their sessions started, and a card keeps its number
for its whole life: numbers never move when a session starts or stops waiting,
and a dismissed card leaves a gap. ⌥1 to ⌥9, or a click, switch the
conversation and the panel to that session, and the rail scrolls the card
into view; the mouse wheel scrolls the rail. ⌥A goes to the oldest request
waiting anywhere ("Bindings").

Delegates are not on the rail; the parent's panel shows them. Exited sessions
are not on the rail either; their project header counts them.

A session whose process died stays on the rail as a ✗ CRASHED card until the
person resumes it, by clicking the card, or dismisses it, with the ✕ that
replaces its time. Both go through the hub, so every client agrees (`docs/invocation.md`, "The hub").

A CANNOT ATTACH card has the same ✕ in its time's place. It hides the card in this terminal only, until the terminal exits, and sends nothing to the hub, whose `dismiss` drops only a crashed session (`docs/invocation.md`, "The hub"). The card comes back when the session's status can be read again or the session crashes.

#### State glyphs

The rail, home's session list and the terminal title use one set:

| `session_status` | Glyph | Word | Colour |
|---|---|---|---|
| `streaming`, `tool` | a braille spinner, `●` under reduced motion | WORKING | the accent |
| `retrying` | the spinner | RETRYING | warning |
| `waiting` | `!` | NEEDS INPUT | attention |
| `idle` | `✓` | READY | dim |
| the process died | `✗` | CRASHED | error |

The spinners step on the working line's timer. A card that starts waiting
pulses its glyph and stripe for its first 10 seconds, then holds still, so a
screen with nothing working draws no frames; under reduced motion it never
pulses.

### The panel

The panel is a column of cards. `tui.panel.cards` lists which cards show and
in what order. An extension widget (`host.widget`, `docs/extensions.md`) is
listed as `"<extension>/<widget>"`, which places it. A widget the list does not
name shows after the listed cards, in the order widgets arrived. The panel
scrolls when its cards outgrow the screen. A mouse-wheel step moves it by three
rows; scrolling clamps at the top and bottom of the content.

The default cards, in order:

- **Session:** the working directory; the git branch; model and thinking level; a context bar that
  fills toward the automatic handoff point, with a marker there; tokens, cache
  hit rate, cost billed and cost on subscription, delegates included, against
  `budget.usd` when one is set; output speed and turns; and one "tools" line naming any
  MCP server that is down. Every number has a plain label that says what it
  measures, such as "tokens in / out", "cache hits" or "output speed, last
  reply", never a bare abbreviation. The branch slot, every figure row, the
  context bar and the two lines under it are drawn from the session's first
  frame. The branch slot stays blank until status supplies a branch name, and
  remains blank outside a repository. A figure not known yet, such as the
  speed before the first timed reply, shows a dim "—", so no row moves when
  its data arrives. The bar is drawn empty with 17 cells
  in rule and a dim "—" for context size until its figures are known. Zero is
  shown as zero. The handoff marker carries one line saying
  what a handoff is: when the context fills, Fiber writes a summary and the
  work continues in a fresh context (`docs/handoff.md`).
- **Changed files:** the five files with the most lines changed, and totals.
- **Delegates:** one card for all of them, two rows each, at most 6 rows
  shown, drawn from each delegate's `session_status` over a `summary`
  connection. It scrolls on its own under the mouse wheel.
- **Jobs:** one line saying how many run, shown only while a job runs. A
  click lists them, one row each.
- **Quota,** from each provider's `quota()`, as `/quota` shows it
  (`docs/tools.md`, "Provider quota"). A provider without one shows none.

Each card is a `surface` slab on the panel's tint, edged ▄ and ▀ in the
`surface` colour over the `panel` colour, with two columns of padding on the
left, one on the right, and a blank row after it. Its title row holds the
card's name in bold, with a figure right-aligned. A value right-aligned on a
row is pushed to the card's right padding, and a row too long for the card is
cut with "…". The cards draw:

```
  ~/work/fiber                     auto
  git main
  anthropic/claude-opus      high ∴
  ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆│          268k
  handoff at 400k    26% of window
  then a summary, fresh context
  tokens in / out     412k / 18.2k
  cache hits                   91%
  cost                       $4.12
  cost on subscription       $0.00
  speed, last reply   64 tokens/s
  turns                         12
  tools 31          ✗ github down

  Changed files                     7
  …/tests/serve_attach.rs  +120  −14
  crates/tui/src/app.rs     +38   −9
  … 2 more                +175 −83

  Delegates                 1 running
  ● review: the diff
    14 calls · 2m 10s · read app.rs
  ✓ docs check
    6 calls · 48s · done

  Jobs                    1 running ▸

  Quota                     extension
  claude weekly                   65%
  ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆
```

- **Session.** The title is the working directory in bold, with the
  permission mode right-aligned in `attention`. Then "git" dim and the branch
  in `secondary`; the model in `accent`, with the thinking level
  right-aligned in `attention` and a dim "∴" after it while the model thinks.
  The context bar is 17 cells of ▆, filled in `info` toward the handoff point
  and `rule` for the rest, then the handoff marker, │ in `attention`, and the
  context size right-aligned ("268k"). Under it, "handoff at 400k" dim, and
  the share of the window right-aligned, dim; then the one line saying what a
  handoff is, dim. Clicking the bar or the lines under it opens the context
  breakdown. Each figure after that is a row: its plain label dim, and the
  figure right-aligned in the text colour; a down MCP server is "✗ <server>
  down" right-aligned in `error`.
- **Changed files.** The title's figure is the count of files, dim. Each row
  is a path, cut from the left to keep its file name ("…/tests/x.rs"), with
  "+N" in `added` and "−N" in `removed` right-aligned. The last row is
  "… N more" dim, when there are more, and the totals.
- **Delegates.** The title's figure is "N running", in `attention` while one
  runs and dim otherwise. Each delegate is "●" in `attention` while it runs or
  "✓" in `success` once done, then its description; under it, dim, its calls,
  elapsed time and its last step, or "done", or "failed" or "cancelled"
  when it did not complete.
- **Jobs.** "N running" in `attention` while one runs, then a dim "▸".
- **Quota.** "extension" dim in the title. Each limit is its name dim with
  the share left right-aligned, and a bar of ▆ under it in `accent`, or in
  `error` with the share when little is left. A window shows `100 -
  percent_used`, floored at zero, as a rounded whole percent. A credit with
  a positive limit shows remaining divided by limit, clamped to 0–100%.
  The figure and bar use `error` at or past `quota.notice_at` percent used
  (default 80). A reset window says "reset" with a full `accent` bar.
  A credit without a positive limit shows remaining to two decimals, with
  no bar or `error` colour. The bar fills to the share left, rounded to
  the nearest cell, with at least one cell for any positive share.
  Each name is the provider and window name, or "credit". The credential
  label follows the provider only when it has more than one label.
  An unreachable label shows its last value without age, or nothing when
  it has none. With no rows, the card draws nothing.
  The terminal asks for cached quota on attach, on focus-in (DEC mode
  1004), and at each attached session's busy-to-settled turn end. A
  keypress asks only once the last answer is at least 5 minutes old
  (`docs/tools.md`, "Fetching"). At most one query is in flight; a turn
  end during it is not held. A rejection stops queries until reattach.

With the rail hidden and sessions waiting, the Session card ends with "! N
waiting · hidden · show" in `attention`, the "!" bold; a click on it
shows the rail.

The context bar is drawn against `preamble_built`'s `context_window` and
`trigger_at`. Per-file counts come from `tool_call_completed`'s `changes`,
never from `details`, which has no fixed shape a client may rely on
(`docs/events.md`).

⌥P shows and hides the panel. Every panel item is clickable and opens its view: the model picker, the
context breakdown, usage, the tools, a delegate's or job's transcript, or a
file's diff, or the quota view. The same views back `/model`, `/context`, `/usage`, `/tools`, `/quota` and `/diff`.

There is no built-in task list
([Task list](https://github.com/aakshintala/fiber/issues/56)).
An extension's widget can supply one.

Running tool calls show in the conversation, on the live group's line, not in
the panel.

### Git

The branch and status come from the person's shell command
(`docs/invocation.md`, `shell`): `git --no-optional-locks status` when the
person clicks for it, and `git rev-parse --abbrev-ref HEAD` on attach and
after each turn. Nothing polls. A rail card's branch comes instead from that
session's `session_status` (`docs/events.md`), which the session updates when
it starts and after each turn.

### Swapped views

A remote terminal's configuration views show the hub's machine: its
settings, rules, credentials, tool switches and skill switches and text.
`keys` and every `tui.*` key belong to the client instead, including `/keys`,
their `/settings` rows and the theme picker.

For a hub-owned file, Ctrl+G fetches its text and opens a temporary copy in
the client's editor. On return, the hub writes it only if its version still
matches. Otherwise the view says the file changed and nothing is written.
The view then rereads the hub's file, showing any configuration or rules
error just as it does after a local edit. The Ctrl+G sentences of `/rules`,
`/settings` and `/skills` in the list below describe the local editor; this
paragraph replaces them on a remote terminal.

A view whose rows take Enter or a switch marks the focused row with "› " in a
two-column gutter, as overlay choices do, without the bar.

A view swaps into the conversation area and takes all of it. Esc returns to
the conversation. Its top row, on the `surface` tint, is the view's name
bold, its command dim ("  /context"), and "Esc returns" dim, right-aligned;
a click on it returns too. The views are:

- **A delegate's or job's view.** Its header is a breadcrumb
  ("main › ◆ review: …"). A status card gives harness, model, calls, elapsed
  time and session, with a stop target. The transcript uses the same turn
  cards as the conversation, and the input box sends steering messages to
  that delegate. The breadcrumb is bold. The status card is a `surface`
  slab: "harness <name>", then " · model <model> · <level>" dim; under it,
  dim, the calls, "elapsed <time>" and "session <id>"; then "■ stop <session id>" in
  `error`. The input box's hint reads "steering goes to ◆ <description>",
  dim. A delegate that has not bound its socket yet answers
  `session_not_found`; while its parent lists it running, the terminal asks
  again every 500 ms. A delegate or job that has finished opens the same
  view read-only: its status row shows the outcome (`completed`, `failed`
  or `cancelled`) where the stop target would be, the transcript is the
  replay of its log, and Enter keeps the finished notice instead of
  steering.
- **A job running under a pseudo-terminal** has a live view of its screen. The
  input box sends `job_input` to type into it as raw keys, as `jobs write`
  does for the model, so the person can finish an interactive step the model
  started. Esc, Ctrl+C and Ctrl+L keep their meanings there (closing the view,
  clear then quit, the model picker), so they are not typed into the job; the
  status card's stop target ends it. The screen is
  80 columns by 24 rows, drawn from the job's output since the terminal
  attached. Carriage return, line feed, backspace, tab, cursor movement and
  erasing in a line or the display are applied; other escape sequences are
  dropped. Its status row is "job <id> · <command> · running <time>", then
  " · " dim and "■ stop <job id>" in `error`; under it, "live screen · 80
  columns × 24 rows from attach" dim. The screen sits on a `turn` slab 80
  columns wide, and the input box's hint reads "type into the job", dim.
- **The running delegates or jobs,** from "N delegates running" or "N jobs
  running" in the narrow layout: two rows for each delegate, or one row for
  each job, in the order they started. `/jobs` (`/tasks` and `/ps` run the
  same command) opens the jobs list. A row opens that item's view.
  "Delegates · N running" bold heads the delegates. A delegate's first row is
  "◆ <description> · <elapsed>"; its second, indented two columns, is dim:
  the model, the thinking level, the calls and the session, then its spend,
  a 10-cell bar of how full its context is and the percentage. The bar is ▆
  in `accent` for the share used and in `rule` for the rest, the fill turning
  the warning colour from 60% and the error colour from 85%, as a rail
  card's does. "Jobs · N running" bold heads the jobs, one row each: "◆
  <command> · <elapsed> · <id>". The last row is "a row opens that item's
  view", dim.
- **Every delegate,** from `/delegates` (`/agents` runs the same
  command): running delegates in the order they started first, then
  finished ones in the order they started, two rows for each delegate
  in the Delegates card's format. A row opens that item's view; a
  finished row opens its replay. The top row is `Delegates` with
  `/delegates` dim; there is no heading row and no count. With no
  delegates the view says "No delegates".

#### The model picker

Models by provider with roles marked, a chip for each
thinking level the model supports, each model's input and output prices,
and what a switch to it costs to rebuild the prompt cache. Choosing a model
saves the global `model`, and choosing a level saves `models."<provider/model>".thinking` to the global file. On a remote terminal it lists the hub's catalogue, the models the hub's machine can reach (`docs/invocation.md`, "What the hub speaks", `models`), and a session started from it runs on the hub. Ctrl+S
marks the choice as this session only, and then nothing is saved. A choice
sent while a turn runs says it applies when the turn ends: the chips hold
the old model and level until the session accepts it, and the choice
applies at the turn's end. The notice appears when the choice is sent; if
the session rejects the choice, the rejection's notice replaces it. When
`scoped_models` is set, the picker shows only those models, with a "show
all" toggle on Tab. A provider hidden by `scoped_models` has no section and no count row, and does not by itself make the picker show "No models match". Typing filters the list on every letter: each
space-separated word of the query must match, in order but not
necessarily next to each other and ignoring case, the model's provider,
id or display name. A left-out count row matches the same way, against
each left-out model's provider, id or display name, and shows only the
matched count; no row shows when none matches. The filter keeps the
list's order and only drops rows. Backspace removes a letter; Esc clears the query first and closes
the picker on the next press. Enter with nothing focused does nothing and
leaves the picker open; it installs only a row ↓ or a click has focused.
Ctrl+L opens it, as `/model` does. It draws from the cached
model lists at once and refreshes stale ones in the background; its refresh
button refreshes every list (`docs/model-routing.md`, "Model discovery").

Under the view's top row the picker is an overlay ("Look", "Overlays").
Its first row is the filter: "Type to search" dim until something is
typed, then "›" dim and the query bold, with a dim block cursor either
way. Its second row holds the count dim ("12 models", "scoped · 5 of 12"
under `scoped_models`, or "4 of 12 models" while a query is typed), then "[show all]" bold, which reads "[show scoped]"
while every model shows, and "⟳ refresh all" dim at the right end. Each
provider is a section, a blank row between them: the provider's name dim,
then "· updated 2m ago" dim, or "⟳ refreshing" and a spinner in
`attention` while its list refreshes. In the choosing picker, a provider
with models left out for a placeholder with no value shows one count row
after its heading and before its models, indented four columns and
uniformly dim: "2 models need workspace: run `fiber login acme`", or "1
model needs api_key: set it in `config/acme-ext.json`" naming the supplied
file when the provider's data declares a `credential_label`. The row never
focuses and takes no clicks; it shows under "show all" too, and never in
`/thinking` or `/scoped-models`. Under `scoped_models` with "show all"
off, a provider with no scoped installed model shows no section and no
count row. Each model takes two rows. The first
is the model's id in `accent`, each role dim in brackets ("[reviewer]"),
"● current" bold in `accent` on the session's model, "· scoped" dim on a
scoped model while every model shows, then its input and output prices per
million tokens dim ("$3.00 in · $15.00 out /M"): "free" when both are zero,
and nothing for a model whose provider data names no cost. A model priced
by request size shows the prices of the tier the session's last call falls
in, with the tier's threshold ("$6.00 in · $30.00 out /M · tier 200k+";
`docs/model-routing.md`, "Cost"), and its base prices before the session's
first call. On a screen too narrow for 60 columns inside the picker, the
prices are left out. Once the session has made a call, the switch cost is
dim at the right end: the prompt-cache rebuild a switch to that model costs
at its rebuild price ("switch ~84k tokens · $0.31", "switch ~250k · $1.88"
at a tier's price, "switch ~58k tokens" for a model with no price, "—" for
the current model). On home and before the session's first call, no row
shows a switch cost. The second,
indented four columns, is "thinking" dim and the model's levels, or "fixed
thinking" dim for a model with none. On the focused model the levels are in
`attention`, its saved level bold, and the chosen chip bold in brackets
("[high]"); elsewhere they are dim. ← and → move between the focused
model's chips. A model chosen for this session only gets a third row,
"ⓢ this session only · nothing saved" dim. While a query is typed, the
characters of each id it matched are underlined and bold; a query that
matches no model, no count row and no not-installed row shows one
line, "No models match" dim, and no provider sections. The footer is "↑↓ move · ←→ levels · Enter choose · Tab all ·
Ctrl+S session · Ctrl+R refresh · Esc close", keys bold.

Below the last provider section, after a blank row, the choosing picker
lists each first-party provider no installed extension declares, one row
each (`docs/extensions.md`, "A fresh install"): the gutter, the name in
`accent`, " · not installed" dim. The scripted provider is never listed
(`docs/model-routing.md`, "The scripted provider"), and `/thinking` and
`/scoped-models` list none. Typing filters these rows by provider name as it
filters models, and a query that matches no model, no count row and no not-installed row
still shows the one "No models match" line. "Show all" leaves the rows as they are. ↑ and ↓
walk the models and then these rows; no row is focused until ↓ or a click
puts it there, so Enter with no row focused does what it does with none
listed (a query that hides every model focuses nothing). The focused row
reads as a focused model does, with "› " in its gutter, bold, on the bar.
Enter, or a click on the name, installs the provider as `fiber extension
install <name>` does, asking nothing more: choosing the row is the approval.
While it installs the row reads " ⟳ installing" and the spinner in
`attention`; choosing another row does nothing until it ends, and closing the
picker does not stop it. A failed install keeps the row, still marked not
installed, and adds a row under it, indented four columns, "✗ " and the
error in `error`, its first line cut at 80 columns; choosing the row again
tries again. A successful install drops the row, asks one model refresh so
the provider's models list, and draws under the list, until the picker closes
and indented four columns: "✓ " in `success` and "installed <name>", the
lines of the install summary dim ("What an install shows",
`docs/extensions.md`), and, for a provider whose key is missing, "<name>
needs a credential: run `fiber login <name>`" dim, or, for a key that could
not be read, "✗ <name>'s credential could not be read: <error>" in `error`.
An install that ends while the picker is closed asks the refresh and, if it
failed, shows the notice "Installing <name> failed: <error>"; if it
succeeded, each credential line above is a notice instead
(`credential_missing`, `credential_failed`). On a remote terminal the install
runs on the hub (`docs/invocation.md`, `configure`, `install_provider`) and
the list is the hub's; with the hub not connected the row fails with "The hub
is not connected."; and the install holds the terminal's one hub connection
until it ends, up to 1,800 s for each install step (`docs/extensions.md`,
"Installing"), so other hub commands wait behind it; while it runs on a
remote terminal, `/settings` and the other config views are refused with "a
provider is installing; try again when it finishes" (a local terminal gets
no such refusal), and on either terminal Esc closes the picker with the
install still running.
- The credential picker, `/credential`: the header reads "Credentials" bold,
  "  /credential" dim and "Esc returns" dim, right-aligned. The provider's
  labels follow the header in the answer's order,
  one per row. Only the current label has "  ● current" in `accent` after
  it and starts selected in reverse. With no current label, the first row
  starts selected. The plain line above the bottom footer reads the provider's name, such as
  "fake", or "Reading the labels…" before the answer, or
  "<provider> has no credential labels" for an empty answer. A rejected
  query shows its message instead. The dim footer on the last row is
  "↑↓ move · Enter choose · Ctrl+S this session · Esc close".
  ↑ and ↓ move between rows. Enter or a click chooses a label and saves
  it; Ctrl+S, `session_only`'s key, chooses it for this session only.
  Esc closes. Typing does nothing: there is no filter. Choosing from an
  empty list does nothing. A session-only choice adds no extra row.
- **The usage view,** `/usage`: the session's `usage` (`docs/events.md`) broken
  down by turn, by model and by delegate, each with tokens by kind, cost billed
  and cost on subscription, and the budget left when `budget.usd` is set.
  "By turn", "By model" and "By delegate" are bold headings, then one row
  each: "turn 3 · 120k in · 15k out · $1.35 billed · $0.00 subscription".
  The last row is "budget left $17.90 of $20.00", dim.
- **The quota view,** `/quota`: one row per entry of the `quota` answer
  (`docs/events.md`), in its order, with the same name, figure and bar as
  the panel card. The top row reads "Quota" bold, "  /quota" dim and
  "Esc returns" dim, right-aligned. Each window's reset time follows its
  bar, dim ("resets in 3h12m"). An unreachable entry shows its last value
  and "unreachable" in `error`, or "unreachable" alone when it has no
  last value. With no entries, one dim line reads "No provider reports
  quota". While the fetch is in flight, the top row shows "fetching" dim.
  Opening the view sends `quota` with `fetch: true`.
  The view draws the cached answer at once and redraws on the fetch's
  answer, which also updates the panel card.
- **The status view,** `/status`: what the terminal already holds about the
  session on screen, asking nothing. The top row reads "Status" bold, "  /status"
  dim and "Esc returns" dim, right-aligned. Each row is a name dim, padded to
  one column, then the value:
  - "session": the session's id, then " · " and its name;
  - "workspace": its workspace, then " · branch " and the branch in a git
    repository, or " · detached" when HEAD is detached;
  - "model": the model, then " · thinking " and the level when it has one;
  - "credential": the label the session's requests use, or "none" when its
    provider takes no credential;
  - "permissions": "auto", the one mode (`docs/permissions.md`, "One mode");
  - "hub": "this machine", or the listed hub's name on a remote terminal;
  - "version": Fiber's version;
  - "context": the context's size and the model's window, "268k of 1M
    tokens";
  - "spend": "$1.35 billed · $0.00 subscription", delegates included;
  - "turns": "3 turns · 12 calls", or the calls alone when the session
    reports no turn count;
  - "clients": the `full` connections attached.

  The session's rows come from its latest `session_status`, and "credential"
  from its latest `preamble_built` or `model_changed` (`docs/events.md`); a
  value not received yet reads "not known yet", dim. The view redraws as
  they change.
- **The doctor view,** `/doctor`: the report `fiber doctor` prints
  (`docs/invocation.md`, "Commands and flags"), captured when the view
  opens and shown one plain row per line, from the same code that prints
  it, so the two never differ. The top row reads "Doctor" bold,
  "  /doctor" dim and "Esc returns" dim, right-aligned. ↑ ↓ and
  PageUp/PageDown scroll; Esc or a click on the top row returns to the
  conversation. The report is captured in the terminal's own process, so
  on a remote terminal it describes the client machine, not the hub.
  When the capture fails, a notice says so and no view opens.
- **The context breakdown:** one bar of context by category against the
  handoff point, with the largest tool results. The categories are the
  system prompt, tool definitions, tool results since the handoff in force,
  and messages, which takes the rest of the session's context total. Each
  of the first three is estimated from its bytes at the session's own
  tokens-per-byte rate, and the view says the sizes are approximate.
  Until a request gives a rate, it shows the total alone. It is drawn
  indented two columns: "268k tokens" bold, then " · 26% of the 1M window ·
  automatic handoff at 400k" dim; the bar, one █ run per category in its
  colour, ▁ in `rule` for the room left, and │ in `attention` at the handoff
  point, with "handoff 400k" in `attention` right-aligned under the marker;
  one row per category, "■ " in its colour, the name padded to 20 columns,
  the size and its share dim; then "Largest tool results" bold, one row
  each, what it was dim and its size; and the note that sizes are
  approximate, dim. Each category has its own colour; tool results are
  `error`.
- **Changed files:** the files Fiber changed, with the chosen file's hunks.
  The hunks are the workspace's current diff against `HEAD`, read when the
  file is chosen, so they include changes Fiber did not make; the title says
  "diff against HEAD". An untracked file shows as wholly added. Each file is
  a row: the chosen file's path bold, then " · " dim, "+N" in `added` and
  "−N" in `removed`; an untracked file's path, then "· untracked, wholly
  added" dim. Under the chosen file, its hunks in the diff format an opened
  edit uses ("Tool groups and the ledger").
- **The release notes,** `/release-notes`: the running binary's
  changelog section for its own version (`docs/releasing.md`, "The
  changelog"), embedded in the binary at build time, so the view asks
  nothing and opens with no session on screen. The section is the one
  headed by exactly `## <version>`; a version with no section shows "No
  notes for this version." with the releases page instead. The top row
  reads "Release notes" bold, "  /release-notes" dim and "Esc returns"
  dim, right-aligned. A bullet is "• " in `accent`, its wrapped lines
  hanging two columns in; other long lines wrap at the width.
- **A call's view,** from an opened call: the whole call, drawn as it is
  inline ("Tool groups and the ledger") with no 20-line cap. The top row is
  the call's kind capitalised, bold, then its target dim ("Shell  cargo test
  -p log"). When the result has an `artifact`, the view pages the whole
  saved output from the session with `artifact_page`
  (`docs/invocation.md`, "What each command does"), as the conversation
  pages history, and never loads the file whole. A page that fails to load
  leaves the view open with the pages it holds and a notice; scrolling to it
  asks again. A dim marker sits at each
  edge of the model's view, from the result's `kept`: "── end of the
  model's view ──" where it stops and "── the model's view resumes ──" where
  it resumes. A result cut at one end only has the one marker. The view
  scrolls as the conversation does, y copies the whole call, and search
  ("Search") matches inside the view only. The last row is "y copies the
  whole call", dim. Esc returns to the conversation with the call's row
  still focused.
- **Search results:** every match with its surrounding lines ("Search").
  The first row is "3 of 5 · ⌕ <query>". Each match is the lines around it
  under a row with the file of the tool call it sits in, ending in `turn N`,
  dim and right-aligned. A match with no such file has `turn N` alone.
  The match is on the `match` tint and the
  current match on `match_current`. The last row is "Enter jumps to the
  match", dim.
- **The tools view,** `/tools` (`docs/tools.md`, "Seeing the tools"): every
  tool by source, full or deferred, and its approximate size. Each MCP
  server's or extension's tool has two on/off switches, this project and
  everywhere, that write that source's `tools.enabled` and `tools.disabled`
  in the project's or the global file (`docs/configuration.md`). A change takes effect on
  reload, and the view says what the reload's cache rebuild costs. Built-in
  tools have no switch: they are always declared, so every session sends the
  same tool list, and the view shows them as rows without a switch.
  Each source is a bold heading ("Built-in", "MCP · linear", "Extension ·
  notes"), then one row per tool: "<name> · full · ~1.2k", or "deferred",
  then " · this project " and its switch and " · everywhere " and its switch.
  A switch reads "[on]" bold or "[off]" dim. On the focused row, a bold "›"
  marks the focused switch, and ← and → move it between the two. The mark
  takes the place of the space before the switch, so columns do not move. It
  shows on the focused row only. The last row is "a change rebuilds the
  cache (~84k tokens · $0.31) on reload", dim.
- **The hooks view,** `/hooks`: one row per hook and watcher the session
  registered, hooks first in hook-point table order and run order, then
  watchers in registration order. One row per hook: its source (the declared
  entry's name, else the extension's name), its point, and `blocking` or
  `non-blocking`; one row per watcher: its source, its kinds joined with
  `", "`, and `watcher`. With none registered, it says `No hooks`.
- **The MCP view,** `/mcp`: one row per configured server: its name, where it
  is declared (`global`, `project` or this repository), its state and its
  tool count. The state is `connecting` for a server never started this
  session, `ready` for a running one, `failed` with the hub's
  `mcp_server_failed` sentence, or `needs-login`. A failed row is in
  `error`, a needs-login row in `attention`, a connecting row dim, a ready
  row in the text colour. Enter on a failed server reconnects that one
  server; Enter on one that needs a login runs the login `fiber mcp login`
  runs (`docs/mcp.md`, "What Fiber does with MCP"): it opens the browser
  and waits for the loopback callback, and the row shows the wait with the
  login URL cut to fit, then success or the failure reason. Enter on a ready
  or connecting row does nothing. A repository's server nobody approved
  shows `not approved`, and Enter says to run `fiber approve` in the repository;
  the view never approves it. Rows sort by server name. The last row is
  "Per-tool on/off switches stay in `/tools`.", dim. On a remote terminal
  the browser opens on the hub's machine, so the row's URL is the one to
  open by hand.
- **`/rules`:** every standing rule by scope, global and then this project.
  Each shows the prefix it allows, when and from which session it was added,
  and a ✕ that revokes it. Revoking deletes the line and applies to the next
  call judged. Ctrl+G opens the rules file (`docs/permissions.md`,
  "Remembering a decision"); on a remote terminal it opens the hub's file
  instead ("Swapped views"). "Global" and "This project" are bold headings;
  a rule's row is "allow shell:git status · added 2d ago in review #688 · "
  and the ✕ in `error`. The last row is "Ctrl+G opens the rules file", dim.
- **`/settings`:** the configuration keys, their effective values and the
  layer each comes from. It edits a key through the same path as
  `fiber config set`, and says when a change needs a reload and what that
  costs. Ctrl+G opens the file; on a remote terminal it opens the hub's file
  instead ("Swapped views"). The keys are a table of aligned columns:
  a dim header row, "key", "value" and "layer", then one row per key, its
  name, its effective value and its layer: `default`, `global`,
  `repository`, `project` or `-c` (`docs/configuration.md`, "Layers").
  Under the table, the reload line ("changing handoff.tokens needs a reload
  (~84k tokens)") and "Ctrl+G opens the file", both dim.

  ```
  key                 value         layer
  model               opus-5-5      global
  thinking            high          project
  handoff.tokens      400000        repository
  budget.usd          20            project
  ```
- **`/keys`:** the rebinding screen ("Bindings").
- **`/skills`:** one row per skill with its name, one-line description, where
  it comes from (`repo`, `personal`, an extension by its bare name, or
  `built in`; an extension named `repo`, `personal` or `built in` reads
  `extension <name>`), whether the model can see it (`seen` or `hidden`),
  and any skill it shadows, plus whether it is switched off. A switch turns
  a skill off or on for this project or everywhere, writing
  `skills.disabled` in that layer; the change reaches the model at the next
  turn start. Enter shows the skill's text, after every skill it shadows
  with its source and path, Ctrl+G opens its file
  (`docs/system-prompt.md`, "Skills"); on a remote terminal it opens the
  hub's copy instead ("Swapped views"). A row is "review · checks a diff
  · repo · seen · this project " and its switch, then " · everywhere " and
  its switch, as on `/tools`; a shadowing skill adds " · shadows <source>,
  …" after the switches, cut with "…" so both switches stay whole.
  Several shadowed sources list after one "shadows", separated by ", ",
  most specific first (repo, personal, extensions by extension name, then
  built in), each source once. An unresolved shadow path — no matching
  skill entry, or an extension entry with no extension name — leaves out
  the whole "shadows …" clause. When the row runs out of width
  the description is cut first, then the name, then the skill's own source,
  keeping room for " · shadows …"; the clause is left out only when the
  fixed fields and both switches leave less than that minimum. The last row
  is "Enter shows the text · Ctrl+G opens its file", dim.
- **`/rewind`** ("Rewind").
- **`/fork`:** rewinds to the end of the conversation ("Rewind").
- **A repository's offer**, before a new session's first request ("Approving
  what a repository ships").

### The working line

The working line ("⠋ Working 11m 14s · Esc to interrupt") sits at the bottom of
the conversation, above the steering queue, not in the input box. It is
indented two columns: a braille spinner (⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏) in
`attention`, "Working", then the elapsed time dim and " · Esc to interrupt"
dim, the hint only while nothing is open ("Keys").

A glimmer runs across its word: a band three cells wide sweeps left to right,
its centre bold in `attention`, its two sides in `attention`, the rest of the
word dim; it moves one cell a tick and rests for a few ticks past the word's
end. The spinners on a running group's
line and on running delegates spin on the same tick, about every 120 ms. While
a turn runs they cost nothing extra, since the frame is drawn anyway. While
only delegates run, the tick keeps running for their spinners. Under reduced
motion the word and the spinners stay still: the spinner is a ● and the word
is plain.

While Fiber waits to retry a failed model call, the working line says so:
"↻ Retrying in 4s · rate_limited · attempt 2 of 4".

### The narrow layout

The conversation has a minimum width of 84 columns. Below it the panel goes away, and rows
under the conversation take its place. From the top:

1. the working line
2. the running delegates, up to 4 rows, while the conversation keeps at least
   half the screen; otherwise they collapse into the status line
3. the steering queue
4. an extension widget row, collapsed to one line, which a click expands
5. the input box
6. the status line, two rows under the input box

Each card contributes its segments to the status line in the configured order,
and each row is cut at the terminal's right edge. The
second row opens with "N delegates running" and "N jobs running", so they are
never cut; each opens a list of delegate or job cards in the conversation
area. Clicking any other segment opens its card's view, the Quota segment the quota view.

The status line sits on the `panel` tint, one column in, with segments joined
by a dim "  │  ". Each card's segments are short forms of its rows: the working
directory, "git" dim and the branch in `secondary`; the model in `accent` and
the thinking level in `attention`; the permission mode in `attention`; "ctx"
dim, an 8-cell context bar and the size ("ctx ▆▆▆▆▆▆▆▆ 268k"), which opens the
context breakdown; the cost and "cache" dim with its share; "tools N" dim and
any down server in `error`; "±" dim, "N files" and the `added` and `removed`
totals; "◆" in `secondary` before the delegates; "⚙" in `attention` before
the jobs; and "Q" dim before the quota, with a limit nearly spent in `error`.
The Quota segment joins each short name and figure with a dim " · ". A
short name is the provider, with its credential label when it has more than
one. The window name follows only when that label has more than one
window. With no quota rows, there is no Quota segment.
With the rail hidden and sessions waiting, the first row opens with "N
waiting" in `attention`.

### Shedding

The rail sheds before the panel. When the screen is too narrow for the rail's
floor, the conversation's minimum and the panel, the rail hides, and "N
waiting" joins the Session card, or the status line in the narrow layout; a
click on it shows the rail. A hidden rail leaves its grip at the screen's left
edge, and dragging the grip out shows the rail again, as does ⌥R. Shown while the
width has no room for it, the rail hides the panel, as ⌥P does; ⌥P brings the
panel back and the rail sheds again. A rail hidden
for width returns when the screen grows; one the person hid, by ⌥R, `/rail` or by
dragging it below its floor, saves `tui.rail.shown` as off and stays hidden,
across launches, until shown. Below the narrow layout, which
has already dropped the panel, the screen sheds the status rows, then the
working line's detail. Below a floor of about 40 by 10
cells it shows one centred line, "Fiber needs 40×10 · now 32×8". The session
keeps running, nothing is lost, and the screen redraws on resize.

### A dropped connection

A session that exits while on screen stays on screen until the connection is
lost, because its conversation is its log. Sending a prompt resumes it
through the hub, with no banner. On reconnect the terminal reopens the
attached session; it resumes idle and takes nothing until a prompt.

When the hub cannot be reached, a banner replaces the working line:
"Connection lost · reconnecting (attempt 2)…". The terminal retries with backoff while the conversation and the
draft stay. After reconnecting, the terminal sends `sessions` once and drops
every live row its answer does not list, unless a feed line for that row
arrived after `sessions` was sent.

## The conversation

### Turns

- **One card per turn.** Only the person's prompt floats above its card,
  after a blank row, as a bubble on the right in the `prompt` tint, at most 72%
  of the width: the text padded one column on each side, a ▐ stripe in
  `accent` at its right edge, and "you 14:12" dim and right-aligned under it.
  A blank row separates the bubble from the card. The card is a `turn` slab
  across the conversation's width, and its rows sit one column inside the
  tint on each side. Blocks inside the card are separated by a blank row.
  A steering message sits inside the card where it landed, as a labelled
  rule over bold text: "steer" bold in `attention`, " · 14:15 " dim, then a
  ─ rule in `rule` to the card's edge. A dim ▣ line closes the card
  with the outcome, duration, call count and the turn's usage, its delegates'
  spend during the turn included: "▣ completed · 38s · 12 calls · 18.2k tokens
  · $0.41 · $1.10 on subscription". A figure that is zero is left out, and a
  model with no price shows tokens only (`docs/model-routing.md`, "Cost").
- **Non-driver messages stay in the card.** A non-driver input message sits
  inside the card where it landed as a labelled rule. Start-input messages
  open the card, before any entry, in the input's order. A Fiber message has a
  muted `Fiber` label, a dim ` · <time> `, and dim text without one leading
  `Fiber: `; a session message has its current name or id as a secondary
  label, a dim ` · <time> ` and full-colour text; an extension message has its
  name as a secondary label, a dim ` · <time> ` and full-colour text. Images
  draw below text. A blank message draws only its labelled rule. A whole-turn
  copy includes each message's label and text in draw order; the labelled
  rule is decoration and is not copied.
- **Replies lead, tool groups sit back.** Nothing inside the card is indented
  past its one-column padding: replies, summary lines, steering messages and
  answers all start at the same column. A tool group has no band of its own, and its summary line is dim
  throughout, the dot and the +/− counts included. Replies carry the full text
  colour.
- **A streaming reply renders in place,** formatted as it arrives. The
  conversation follows new output and pauses when the person scrolls up. The
  mouse wheel over the conversation scrolls it 3 rows a step, a starting
  point, not a measurement; PageUp and PageDown scroll a screen. The
  conversation's last column is its scroll bar and never holds text:
  while the conversation has more rows than it shows, a thumb of ┃ in
  `scroll`, with no track, shows where the rows on screen sit among all of
  them. Either pauses following when it scrolls up. When new output arrives
  while scrolled up, a pill centred on the conversation's bottom row reads
  " ↓ New messages below · End " in `attention` on the `rule` tint, its ends
  rounded by ▐ and ▌ in `rule`, and jumps to the end on a click or End. It
  gives no count. Nothing is added to the input box.

### Tool groups and the ledger

- **A group is everything between two pieces of assistant text,** thinking
  included. Collapsed, it is one line: what was done by kind ("Read 34 files,
  searched 84 patterns, edited 24 files +175 −83, ran 9 commands"), how many
  times the model thought, and how long it took. While it runs, the line shows
  the calls in flight. A call the model is still emitting shows its raw
  argument text from `tool_call_arguments_delta`, because arguments are parsed
  only once the call finishes streaming (`docs/events.md`). The counts of changed lines come from
  `tool_call_completed`'s `changes`.
- **Clicking the line, or Ctrl+O, opens the ledger:** one row per call, split
  by step, with each step's number in the gutter and its thinking line first.
  A step is opened by `step_started` (`docs/events.md`, "Session and turn").
- **Clicking a row opens that call** inline under its row, at most 20
  lines; Enter on a focused row does the same. Enter again on the opened
  call, or a click on its "… N more lines (Enter to view)", opens the whole
  call in a swapped view ("Swapped views").
- **While an opened call's row, or an open ledger's group line, is above the
  conversation's top row and any of its content is on screen, that row stays
  pinned to the conversation's top row.** Scrolling into the opened output
  keeps the row over the covered content row, drawn as the row it stands for,
  on the card's tint, with hover highlighting it like any click target. When a
  call is opened inside an open ledger, only the innermost one pins: the
  call's row. Clicking the pinned row closes what it opened, and the
  conversation scrolls so the closed row sits at the top. Ctrl+O keeps its
  current meaning. Whenever a find, a search-results jump, keyboard focus or
  a reveal scrolls a row into view while a row is pinned, the visible top is
  the row under the pin, with the pin staying.
- **A call that changed a file is highlighted** in the ledger, with the file's
  name and its lines added and removed, so edits stand out from reads and
  commands. Its counts are in `added` and `removed`, where every other result
  is dim.
- **Failed calls get no special treatment.** They are ordinary rows with their
  status. Nothing is hoisted above the fold.

The summary line is one row, whatever runs: "● " dim, then the kinds in a
fixed order, each "<verb> N <noun>" dim: read files, searched patterns,
listed directories, edited files, wrote files, ran commands, started
delegates, checked jobs, asked questions, called tools. The first word is
capitalised. Read, edited and written files count distinct paths, and asked
questions count questions, not calls. An edit's totals follow it, "+N" in
`added` and "−N" in `removed`, both dim. Then ", thought N times" ("thought
once"); while calls run, " · " and their targets, dim; while the model
thinks, " · Thinking: <heading>" dim and italic. Right-aligned, dim, the
group's duration and ▸ while closed or ▾ while open. What does not fit is
cut with "…", so the duration and the toggle stay put.

```
● Read 34 files, searched 84 patterns, edited 24 files +175 −83, thought 3 times   4m 12s ▸
```

A ledger row is the step's number right-aligned in a five-column dim gutter
on the step's first row. A step number longer than five digits widens the
gutter, and the row is cut to its width. Then a glyph: ○ in `attention`
while the call runs or waits, ✓ in `success` once it completes, ⊘ dim when
cancelled, ✗ dim otherwise. Then the call's kind in `info`, padded to nine
columns, its target cut with "…", and its result: "N lines" for a read, "N
matches" for a search, "N paths" for a listing, "+N −N" for an edit, "exit N
· N lines" for a command, otherwise the first line of its output, all dim
but the edit's counts. A running call reads "running" in `attention`, and an
ask waiting on the person "waiting on you"; "cancelled", "denied" and a
failure's "<code>: <message>" are dim. A thinking row is "○ Thinking:
<heading>" or "+ Thought: <heading> · 22s", dim and italic.

```
    3 + Thought: Finding the flaky lock · 8s
      ✓ read     crates/hub/src/lock.rs                      212 lines
      ✓ search   try_lock                                    9 matches
    4 ✓ edit     crates/hub/src/lock.rs                      +12 −4
      ○ shell    cargo test -p hub lock                      running
```

An opened call is drawn under its row, indented eight columns, at most 20
lines, then "… N more lines (Enter to view)" dim when there is more. A
failed call shows "<code>: <message>" first. An opened thought shows its
text there. What an opened call shows depends on its kind, read from the
call's `details` (`docs/tools.md`, "What a result carries"); a call whose
`details` the terminal does not recognise shows its `content`:

- **An edit** shows its diff in the diff format below.
- **A read** shows the text read, with the line-number gutter starting at the
  first line read, from `details`. A write, a call whose arguments hold a
  string `content`, shows that text, numbered from 1.
- **A search** shows its matches grouped under each file's path, the path
  bold, line numbers in the gutter and the matched text on the `match`
  tint, from the built-in `grep`'s `details`. A search call without them,
  such as `rg` or `command grep`, opens as a shell call.
- **A shell call** shows the full command, "$ " and the command wrapped,
  then the output, then one closing line: "exit N", in `removed` when N is
  not 0 and dim when it is, or "killed by <signal>".
- **An MCP call, an extension's tool, or any tool whose `details` the
  terminal does not recognise** shows `content` as plain text. Text that is
  JSON is indented two columns per level.

The diff format is shared by an opened edit and the changed-files view. Each
line has a dim line-number gutter: the old file's number on a `-` line, the
new file's on the others. `@@` lines are dim, `-` lines in `removed` and `+`
lines in `added`. When one removed line pairs with one added line, the words
that differ are in reverse video. There is no syntax highlighting.

```
    1 ✓ edit     crates/tui/src/view.rs                      +6 −2
             @@ -12,5 +12,6 @@ fn paint(
          12     let spans = fit(&r.spans, w as usize);
          13 -    buf.set_line(x, y, &line, w);
          13 +    buf.set_line(x, y, &Line::from(spans), w);
          14     if let Some(bg) = r.bg {
          15 +    let line = Line::from(spans);
```

A group opens by a click, by Ctrl+O, when a search match falls inside its
ledger, and when one of its calls asks the person.

### Thinking

Thinking belongs to the group it starts, since a group runs from one reply to
the next. The summary line counts it ("thought once") and, while it streams,
ends with "Thinking: <latest heading>". Thinking with no tool call before the
next reply is one dim italic line, "+ Thought: <first heading> · 22s", or
"Thinking: <latest heading>" while it streams, and clicking it shows the
text. A heading is the first line of the thinking text that is a
Markdown heading (`#…`) or wholly bold (`**…**`), with the markers stripped,
or the first non-empty line when there is none, cut to fit the line.
"Thinking:" shows the latest heading in the step so far, and "+ Thought:" the
first.

### Steering

Enter during a turn sends `steer`. There is no follow-up queue: a prompt that
arrives mid-turn is still rejected `busy` (`docs/invocation.md`). A draft left
in the input box during a turn is untouched until sent.

Queued steering messages sit above the input box, one row each, from the
`steering_queue` event, so every attached client sees and edits the same
queue. ⌥↑ and ⌥↓ select a row and load it into the input box, Enter sends
`steer_drop` then `steer`, and ⌥X sends `steer_drop`. Each also has a mouse target.

The queue is indented two columns under the working line: "• Steering, joins
the turn at the next step" dim; each message "  ↳ <text>  ✕" dim, or "  ▸ "
in `attention` and the text in the text colour while it is selected; then
"⌥↑ edit · ⌥↓ next · ⌥X drop · click a row to edit, ✕ to drop" dim. A click
on a row selects it and a click on its ✕ drops it. While a message is being
edited, the input box's stripe is `attention` and the box ends, right-aligned
and dim, "editing a queued message · Enter amends · ⌥X drops · Esc stops".

### Interrupts

Esc interrupts the turn when nothing is open ("Keys"). The interrupt shows only
on the ▣ line that closes the card; there is no separate "Interrupted" line. A
call it stopped reads `cancelled` in its ledger row. With steering messages queued, they start
the next turn at once (`docs/architecture.md`, "Cancellation"), so Esc means
"stop, and read these". ⌥X drops them first for a plain stop.

### Handoff

A handoff breaks the turn's card (`docs/handoff.md`). The card ends where the
handoff ran, and a tinted band carries:

- the trigger: "automatic at 400k", "you asked with /handoff", "the request
  did not fit" or "the model handed off"
- the context size before and after ("402k → 32k"); the size after comes from
  the first request after the handoff that reports its input tokens, so it
  reads "…" until that returns
- the time

"▸ note" expands the note inside the band. While the note is being written the
band says so, with a spinner.

The band is one row on the `handoff` tint with ▄ and ▀ edges, across the
conversation's width: " ⇄ " and "handoff" bold, both in `secondary`, then the
trigger after " · ", dim; right-aligned, the sizes bold ("402k → 32k") and
" · 14:20" dim. While the note is written the sizes read "writing the note…"
dim; a failed or cancelled handoff reads "<outcome> · the context is
unchanged" in `attention`.

```
 ⇄ handoff · automatic at 400k                         402k → 32k · 14:20
```
 A second card continues the turn, and its ▣ line
closes it. A failed or cancelled handoff says the context is unchanged. The
Session card's context bar drops to the new size.

The nudge is one dim line where it happened: "◔ Context at 268k, two thirds of
the way to the 400k handoff. The model was told a handoff keeps the work
going."

### Errors and retries

The terminal shows Fiber's message as it is and never rewrites it
(`docs/errors.md`).

- While a retry is pending, the working line says so ("The working line").
- Once a retry gets through, only a count stays, on the group's summary line
  ("1 failed model call"). The ledger has the detail.
- A turn that fails ends with a ✗ line holding the message and the code, with
  the provider's own words dim under it ("anthropic said HTTP 529:
  “Overloaded”"), then ▣ failed.
- A turn that fails with `authentication_failed` offers "log in" as a click
  target on its error line ("Keys", `/login`).
- `mcp_server_failed` is a ⚠ line in the conversation with its
  `error.message`: "⚠ " in `attention`, the message dim, wrapped under
  itself. A server that comes back writes `mcp_server_ready`, which
  clears the Session card's mark in every client.

### Notices

Notices are rows at the bottom of the conversation, above the working line,
one row each until dismissed with ✕, oldest first: "  ⚠ " and the code in
`attention`, " · " and the message dim, cut with "…", and a dim "✕"
right-aligned. Clicking a notice shows the whole text in an overlay.

- At most 3 show, then "+N more", which lists them.
- The "N approvals waiting" row (see "An approval") sits above the notices, and on a screen too short for both it drops first.
- A notice that arrives during a drag waits for the release.
- In the screen-reader mode notices are lines in reading order.

An accepted `/import` answer opens one import report overlay ("Look",
"Overlays") and adds no notices. The report is titled "Imported from
Claude Code" or "Imported from Codex", or "Imported" for a bare `/import`
covering both sources. It has two sections, "Copied · N" grouped by kind
(MCP servers, skills, commands, instructions) and "Left behind · N" in the
hub's order with each reason dim, and its foot reads "Imported items apply
on the next /reload · Esc closes" (`docs/configuration.md`, "Importing").
A failed import is one notice, as any failed command is.

When the hub sends `stream_closed` for the open session (it cannot read
the session's log to serve this terminal), the conversation keeps every line
it received and one notice shows: code `stream_closed`, "this session's log
can't be read past here; reopen the session to try again". A session that is
not open gets no notice. The terminal does not subscribe again on its own:
the fold would reach the same line. The composer is unchanged, so a send is
rejected `not_subscribed` and the draft comes back with that notice.
Reopening the session subscribes again through the normal open path.

When the hub runs another Fiber version or commit than this terminal, one notice says so on each connection: code `hub_version`, "The hub runs Fiber <hub> (this terminal: <terminal>). `fiber stop` restarts it on the next launch." The same text shows for a remote (listed) hub. (`docs/invocation.md`, "What the hub speaks").

A notice is an event or a condition. An event reports a failure once and
then fades: it clears 10 seconds after it shows, or when dismissed with
✕. A condition reports a state the terminal re-evaluates on its own
loop, and it stays until its cause ends: reaching its end clears it,
✕ only hides a row whose cause still holds. The loop sweeps notices
before each draw and before arming its wake, so a faded row never draws
and an expiry wakes the loop even with no input.

| Code | Class | Clears when |
|---|---|---|
| `cache_rebuild` | event | Fades 10 s after it shows. |
| `closing` | event | Fades 10 s after it shows. |
| `command_conflict` | event | Fades 10 s after it shows. |
| `config_invalid` | event | Fades 10 s after it shows. |
| `config_key_ignored` | event | Fades 10 s after it shows. |
| `connection_failed` | condition | The connection cause clears when the hub reconnects; the search-incomplete cause clears when the find bar closes or a rescan completes. A reconnect alone never clears the search text. |
| `credential_failed` | event | Fades 10 s after it shows. |
| `credential_missing` | event | Fades 10 s after it shows. |
| `extension_failed` | event | Fades 10 s after it shows. |
| `extension_incompatible` | event | Fades 10 s after it shows. |
| `extension_shadowed` | event | Fades 10 s after it shows. |
| `history_loading` | condition | History lands for the pending turn. |
| `hook_failed` | event | Fades 10 s after it shows. |
| `hub_version` | event | Fades 10 s after it shows. |
| `indeterminate` | event | Fades 10 s after it shows. |
| `instructions_large` | event | Fades 10 s after it shows. |
| `invalid_arguments` | event | Fades 10 s after it shows. |
| `io_failed` | event | Fades 10 s after it shows. |
| `login_url` | condition | No automatic clear: finish the login in X, then dismiss with ✕. |
| `malformed` | event | Fades 10 s after it shows. |
| `model_invalid` | event | Fades 10 s after it shows. |
| `model_unconfigured` | event | Fades 10 s after it shows. |
| `models_loading` | condition | The model list arrives. |
| `no_jobs_running` | event | Fades 10 s after it shows. |
| `no_model` | event | Fades 10 s after it shows. |
| `notes_unchanged` | event | Fades 10 s after it shows. |
| `repository_code_skipped` | event | Fades 10 s after it shows. |
| `reviewer_selection_failed` | event | Fades 10 s after it shows. |
| `session_held` | event | Fades 10 s after it shows. |
| `session_not_found` | event | Fades 10 s after it shows. |
| `skill_invalid` | event | Fades 10 s after it shows. |
| `skill_shadowed` | event | Fades 10 s after it shows. |
| `skills_large` | event | Fades 10 s after it shows. |
| `stale_request` | event | Fades 10 s after it shows. |
| `stream_closed` | condition | The session reopens; opening another session drops other sessions' rows. |
| `switch_pending` | condition | The turn ends and the switch applies; rejecting a pending command voids its claim. |
| `tool_definitions_large` | event | Fades 10 s after it shows. |
| `web_search_unavailable` | event | Fades 10 s after it shows. |

Codes no table lists — a refusal or driver code the hub names
dynamically — are events: they fade after one showing.

Notices are not logged, so they are this terminal's own memory. A client that
attaches later never sees them.

### After a crash

The reopened session closes the cut-short turn's card with "▣ cut short: Fiber
stopped at 14:18", at the time of the last line written. A call that started
and never completed reads "? may have run; not run again" in its ledger row. A
↺ resumed band follows. The jobs the new process marked `orphaned` are one line
naming them all; clicking it shows each job's message.

### Naming the session

`/name <text>` names the session with the `name` driver command. The name is
written to the log as `session_named`, so every client sees it. The session
list, the rail card and the terminal title show it in place of the first prompt.
The model keeps the name current with `name_session` until a person's name
pins it (`docs/tools.md`, "Naming the session"). A name is drawn with each control character replaced by a space, in the session list, the header, the rail and the terminal title.

### Rewind

`/rewind` swaps a view into the conversation area (`docs/events.md`,
"Rewind"). `/fork` is a rewind to the latest point: it sends `rewind` with
`at_end`, opens no view and offers no summary.

- Each turn is one row, "before: <prompt>", or "before: rewind summary" for
  the turn a rewind's summary ran in. The chosen turn opens into every
  step boundary: after the person's message and after each step, with the
  step's summary. The default is the start of the latest turn that is not a rewind's summary.
- For the chosen point it shows the files Fiber's tools wrote since, the calls
  that may have changed files, and that none of it is undone. It lists every
  job that still runs, all of which stop.
- An optional summary, then Rewind. The conversation is cut at the point, and a
  ⟲ line names the new session and says what was not restored and which jobs
  stopped. The new session waits for a prompt.
- `session_held` shows as the error in place of the Rewind button.
- `summary_failed` shows the reason with three choices: retry, rewind without
  a summary, or cancel.

### Moving workspace

`/cd <path>` sends `change_workspace` (`docs/invocation.md`, "Driver
commands", `change_workspace`), and the screen switches to the new session.
The old session leaves as it does after a rewind: `session_left` has
`how: exited`. No move-specific text appears. With no path, the command notes
`/cd needs a path.` and sends nothing. When the session refuses the command, a notice
gives the refusal's code and message, and the screen stays on the session it
shows.

## Approvals and questions

Approvals and questions waiting at the same time share one queue, "1 of N" in
the order they arrived, each labelled with the session asking.

### An approval

An approval request is a panel at the bottom that replaces the input box
(`docs/permissions.md`).

- The choices are: allow once; allow for this session; always allow in this
  project; and deny, with optional feedback. Typing while the panel is open
  goes to the feedback. The two remembering choices show the prefix the rule
  allows, the one the request offers (`docs/permissions.md`, "What a rule
  matches"). A request that offers no rule shows neither.
- The panel says why it asked: the standing rule that asked, the reviewer's
  reason when the reviewer escalated, or that the reviewer failed.
- The panel's tint follows why it asked. A reviewer's escalation gets the
  alert tint; a standing ask gets the normal approval tint. A call that
  declares itself irreversible says "irreversible" in the panel's header, as
  text, whatever asked. Nothing judges danger by reading the command
  (`docs/permissions.md`).
- The asking call's tool group expands so the full call can be read.
- Esc puts the request aside. It stays pending behind a badge, and clicking the
  badge reopens it. Denying is always explicit.
- Esc steps through waiting requests one at a time. With a question form
  behind an approval, a second Esc declines the form.
- `/approvals` reopens the waiting queue at the first request. It is the key
  path for reopening a request put aside and for moving to the next.

The panel is a slab with ▄ and ▀ edges, every row starting with a ▌ stripe
and a space: the `approval` tint and an `attention` stripe for a standing
ask, the `alert` tint and an `error` stripe for a reviewer's escalation.

```
▌ Approval 1 of 1 · main                                     Esc puts it aside
▌ shell cargo mutants -p hub
▌ runs a command · asked by the project rule cargo
▌
▌ ▸ 1 Allow once
▌   2 Deny  type to add feedback
▌
▌ ↑↓ choose · Enter confirms · typing goes to the feedback · click a choice
```

A review that offers a rule shows the remembering choices:

```
▌ Approval 1 of 1 · main                                      Esc puts it aside
▌ shell cargo mutants -p hub
▌ runs a command · the reviewer blocked the last calls: rm -rf
▌
▌ ▸ 1 Allow once
▌   2 Allow for this session cargo mutants *
▌   3 Always allow in this project cargo mutants *
▌   4 Deny  type to add feedback
▌
▌ ↑↓ choose · Enter confirms · typing goes to the feedback · click a choice
```

- The first row is "Approval k of n" bold in `attention`, which moves to the
  next request on a click, " · " and the asking session in `attention`,
  " · irreversible" in `error` when the call declares itself so, and "Esc
  puts it aside" dim, right-aligned.
- Then the tool's name bold in `info` and its main argument bold, wrapped.
- Then one dim facts line: the call's declared effects joined by ", ",
  " · " and the paths it names, and
  " · " and why it asked: "asked by the <scope> rule <prefix>", "a standing
  ask rule matches it", "it would leave readonly", "the reviewer failed",
  "the reviewer blocked the last calls: <reason>" or "the reviewer has
  blocked too many calls: <reason>".
- After a blank row, the choices, numbered: "▸ " in `attention` on the
  focused one and two spaces on the others, the number dim, the label bold
  when focused. The remembering choices end with the rule's prefix and " *"
  in `info`. Deny ends with "  type to add feedback" dim, or the feedback as
  typed with a cursor. The numbers label the rows; a digit typed is
  feedback, like any other key.
- After a blank row, the hint, dim.

A request put aside leaves a row above the working line, "  ⚠ N approvals
waiting · click to reopen", bold in `attention`, the ⚠ not bold.

### A question form

An `ask_user` question form is a panel at the bottom that replaces the input
box, as an approval does (`docs/tools.md`, "Asking the person").

- One question shows at a time. Across the top is a tab per question header,
  marked when answered, then a Submit tab that shows every answer and takes a
  note on the whole form.
- Each question shows its options with their descriptions, and a row to
  answer in words. Enter on a single-choice option chooses it and moves on.
  In a multi-choice question, Space toggles the option under the cursor and
  Enter moves on to the next question, or to Submit after the last one. A
  multi-choice question ends with a `Next →` row, `Review →` on the last
  question, that does the same for the mouse.
- ← and → move between the tabs, except in a row that takes typed text (the
  answer in words, or the note), where they move the cursor. Tab and Shift+Tab
  move between the tabs from any row.
- "Chat about this", and Esc, decline the form and end the turn, so the person
  can answer in their own words. The terminal sends `reply` with `declined`,
  then `cancel`: the call completes `declined`, and the cancel ends the turn.
  The cancel follows only an interaction a tool call raised, one whose
  `interaction_requested` carries `action_ids`. One raised outside a tool call,
  such as an extension command's `host.ask`, is declined and leaves any running
  turn alone.
- Once answered, the answers sit in the turn's card as a "you answered" rule,
  like a steering message, one row per question, with `skipped` and the note
  shown. The call's ledger row reads "answered" or "declined", and the group
  line counts "asked 4 questions".

The form is an `approval` slab with ▄ and ▀ edges, every row starting with a
▌ stripe in `secondary` and a space:

```
▌ Question from main · 3 questions, any can be skipped · 1 of 2      Esc: chat about this
▌
▌ ←  ✔ Scope    ☐ Tests    ☐ Docs    ✔ Submit  →
▌
▌ Which crates should the change touch?
▌ choose any
▌
▌ ▸ 1 [x] hub  the session host
▌   2 [ ] tui  the terminal
▌   3 ✎ Type an answer, alone or with the options
▌   Next →
▌
▌ ←→ question · ↑↓ choose · Enter chooses and moves on · Space toggles · type to answer in words
```

- The title row is "Question" bold in `secondary`, " from <session> · N
  questions, any can be skipped" in `secondary`, " · k of n" bold in
  `secondary` when more requests wait, and "Esc: chat about this" dim,
  right-aligned. A blank row follows.
- The tab row: "← " dim, a tab per question, " ☐ <header> " or, once
  answered, " ✔ <header> " in `accent`, the current tab reversed, then
  " ✔ Submit " and " →" dim. A blank row follows.
- The question bold, wrapped, with "choose any" dim under a multi-choice
  question, then a blank row.
- Each option: "▸ " in `secondary` on the cursor's row, its number dim, a
  mark, "( )" or "(•)" for one choice and "[ ]" or "[x]" for any, in
  `secondary` when chosen and dim otherwise, the label, bold when chosen and
  underlined under the cursor, and its description dim. A digit chooses that
  option, outside a row that takes text.
- The row to answer in words, numbered after the options: "✎ " dim, then
  "Type an answer, alone or with the options" dim ("Type an answer" with no
  options), or the answer bold with a cursor. The dim text is a placeholder:
  with the cursor on the row, the cursor sits on its first character, drawn
  reversed, and the first key typed replaces it.
- In a multi-choice question, "Next →", or "Review →" on the last question,
  in `secondary`, bold under the cursor.
- After a blank row, the hint, dim: "←→ question · ↑↓ choose · Enter chooses
  and moves on · Space toggles · type to answer in words".

The Submit tab is "Review" bold, a blank row, one row per question, its
header padded to 13 columns in `accent` and the answer bold or "skipped" dim,
then "▸ " in `secondary`, "note" in `accent` and "Add a note on the whole
form" dim, or the note bold, with a cursor. After a blank row, " Submit "
reversed and " Chat about this " on the `rule` tint, then "  declines and
ends the turn, so you can answer in your own words" dim; after a blank row,
the hint, "←→ question · Enter submits · type to add the note · Esc: chat
about this", dim.

In the turn's card the answers are a rule like a steering message: "you
answered" bold in `accent`, " · 14:16 " dim and a ─ rule in `rule`. Under it,
each question's header padded to 13 columns in `accent`, then the answer bold
or "skipped" dim, and "note" with the note in quotes, bold. A declined form
reads "declined: the turn ended so you could answer in your own words" dim.

The other interactions, `confirm`, `select`, `multi_select` and `text_input`, raised by an extension's `host.ask` and by MCP elicitation, are drawn as one-question forms: the same panel with no tab row, the question, then its rows, ending with "Chat about this". `confirm` shows two options, yes and no, and Enter on one replies `confirmed`. `select` shows the options, and Enter on one replies `labels` with that label. `multi_select` shows the options: Space toggles one, and Enter replies `labels`, possibly empty. `text_input` shows only the row to answer in words, and Enter replies `text` as typed. The last two end with a `Submit` row that does what Enter does, for the mouse. Esc, and "Chat about this", decline as on a form.

## Reading and copying

### Selection and copy

Dragging selects text on any surface that shows it, and copies on release,
as the conversation does today. That covers the input box, notices, panel
cards, overlays (pickers, the login overlay, `/settings`), the approval
panel, question forms and swapped views. The press claims the surface it
starts on: a drag selects within that surface and does not spill into a
neighbour, even when the pointer leaves the window. "Copied" shows in
`info` in the conversation's top-right corner, below the search box when
it is open. The selection is on the `selection` tint, painted over every
surface's own rects. The terminal owns Cmd+C (Ghostty binds
`super+c=copy_to_clipboard`), which is why release copies.

A surface's renderer already draws its text as spans, the same spans that
make selection, copy and search work in the conversation. Every surface
uses that span shape. A span is either text or decoration. What a
selection copies is the text spans that fall inside it. Decoration never
goes into the copy: stripes, half-block edges, chips' brackets, key
hints, the focus gutter and mark, borders, the scroll bar, and the "…"
a cut adds. A selected paste token or image copies its visible label,
never its hidden payload or bytes. A row that wraps onto the next is
joined with no newline; a line break in the text is a newline. Trailing
spaces on each row are dropped. A cut element copies what is drawn,
without the "…". Its whole text is one click away, in the overlay.

The copy goes through OSC 52 and, on a local session, the system clipboard
command as well. tmux's default `set-clipboard external` drops OSC 52
silently, and over SSH only OSC 52 reaches the person's clipboard.

`/copy` copies the newest reply in the conversation on screen the same way,
and "Copied" shows: the text parts of the model's latest message that has
text, as the model wrote them, Markdown included, joined by a blank line.
With no reply yet, a notice says "No reply to copy yet." When the window
holds no reply newer than a page it has dropped ("History and paging"), the
newest reply may be on that page, so nothing is copied and a notice says "The
last reply is paged out; jump to the end, then /copy again." While an opening
pass folds a session's history ("History and paging"), the conversation on
screen stays as it was, so `/copy` waits and copies in the frame after the
pass ends. The next key, edit or click, a view opening or closing, leaving the
session, a session attaching and a lost connection each drop the wait, so it
never copies from a screen the person has moved on from. A job's view, and a
delegate's view with no transcript, hold no reply: there `/copy` says "No
reply to copy in this view."

Shift-drag stays the terminal's native selection. A selection's highlight
counts as open, so Esc clears it before it can interrupt the turn.

The input box keeps the selection as the editor's selection too. Typing
or a paste replaces it, and Backspace or Delete removes it. An unshifted
cursor key clears it: ← and Home leave the cursor at the selection's
start, and → and End at its end; ↑ and ↓ clear it and then move as usual.
A click in the box clears the selection and places the cursor. Shift with
the arrow keys stays out of this rule. Release still copies.

A press-and-release with no movement is still a click, so mouse targets
keep working. A drag that starts on a target selects instead of clicking:
once the pointer leaves the press's cell the click is disarmed and the
release clicks nothing. Rail and panel edge drags keep resizing and never
start a text selection.

A notice that would fade, or an overlay that would close by itself, waits
while a drag is in progress. On release the copy happens first, then the
waiting fade or close. A close the person asks for, such as Esc or a
click, is never part of a drag, so this rule does not touch it.

### Search

Search covers the whole session log, matching rendered text, never raw JSON
lines. It matches the unwrapped text and marks a match across the rows it
wraps onto.

The search bar floats over the conversation's top-right corner, as an
editor's find box does, and the input box keeps its draft. It is 40 columns
wide, a `surface` slab with ▄ and ▀ edges: " ⌕ " in `attention`, the query
bold, a dim █, and right-aligned "3 of 41", or "no matches" or "type to
search" dim. Every match is marked on the `match` tint, the current one on
`match_current`. A match inside a collapsed section expands it when it
becomes the current match.

Matching is plain text: it folds case per character, matches literally, and
keeps at most 10,000 matches ("10000+"). No model ranks the matches or
interprets the query. Enter or ↓ moves to the next match, Shift+Enter or ↑
to the previous; both wrap.

Ctrl+F a second time, or a click on the "3 of 41" count, opens the search
results as a swapped view ("Swapped views"): every match, one row each, with
the lines around it. Each match's first row ends with `turn N`, dim and
right-aligned, where N is the number of the turn the match is in, counted
from 1 as the usage view counts turns; a match outside any turn, or a row
too narrow to hold it after the path, has no label, and the path is never
cut for it. A match's path is the file of the tool call it sits in; a match
with no such file, in a prompt, a reply or another call, has no path, and
its row is `turn N` alone. Enter, or a click on a row, jumps to that match
in the conversation and closes the view.

Search is Ctrl+F, and Cmd+F where the terminal forwards it. Ghostty binds
`super+f=start_search` by default; the line that frees it is:

```
keybind = super+f=unbind
```

How search reads a large session is "History and paging".

### Links

Mouse capture turns off the terminal's own link detection, so links are
handled on click: an escape sequence in a cell breaks the width measure.
Markdown links (tables included) and bare `http://`/`https://` URLs open
on click; schemes are `http`, `https` and `mailto`. A click runs `open` or
`xdg-open` on the terminal's machine; over SSH or with no opener the click
copies the URL and shows "Copied". A link is in the accent colour,
underlined. A focused link sits on the `surface_raised` tint, with "Enter opens
· y copies" dim under it when the window has a row under it, and opens with Enter and copies with `y`. A hovered
link takes the `hover` tint.

A copy, link or image command still running after 10 seconds is stopped;
what it started keeps running. The image is sent to the terminal in one
write; a very large one can hold the screen until the terminal has read
it.

## Keys

### Rules

- **Detection never blocks the first frame.** The terminal sends kitty's
  keyboard flags query followed by a primary device attributes query, and
  draws at once. Bindings that need the kitty protocol switch on when the
  reply arrives. crossterm parses both replies but hands them only to a call
  that blocks for up to 2 seconds, so Fiber reads terminal input itself
  ([fiber-zig#16](https://github.com/aakshintala/fiber-zig/issues/16)).
- **Every action has a key.** Every action also has a mouse target or a slash
  command, except editing the draft in the input box and search, choosing
  in the model picker or the credential picker for this session only, and
  keeping, editing, striking and flipping a line's layer in the reviewer
  setup. A mouse target is drawn only where one fits naturally, never as a
  button added only so the mouse has a way in. A key that needs the kitty
  keyboard protocol also has one that does not, so every action keeps a key
  on any terminal.
- **Esc closes whatever is on top,** except the workspace picker while no
  workspace is chosen ("Home"). On home, Esc on a focused row goes back to the session the person came from ("Home"). It returns focus to the input box from
  the conversation, and interrupts the turn only when nothing is open and the
  input box has focus. In a question form, Esc means "Chat about this".
- **The reviewer setup** opens with `/reviewer-setup`. Inside it Enter, Esc,
  ↑ ↓, PgUp PgDn (on the diff), Tab, `x`, `e`, `r` and `n` act as the
  panel's legend says. The ✕ and a press outside the panel, which is Esc,
  are its mouse targets, and the mouse wheel does nothing while it is open.
  Enter on a diff that would change neither layer closes the setup with one
  notice, `notes_unchanged`, and writes no file.
  The notice that names the command, shown the first time in a session the
  reviewer blocks a call while both layers of the notes are empty, closes on
  any key, and that key goes nowhere else.
- **Slash commands.** Typing `/` opens one completion panel above the input
  box. Commands, skills, prompt templates and MCP prompts share one list,
  filtered as the person types. Each row is a name, a one-line description, a
  skill's `argument-hint` when it has one, and a tag: command, skill,
  template, the extension's name, or the MCP server's name. Tab completes and
  Enter runs. The panel is an overlay above the input box ("Look",
  "Overlays") with no title and at most eight rows. A row is the name in
  `accent` in one column as wide as the longest name shown, the letters
  that match the query bold; the description dim, cut with … so the tag
  always fits; the `argument-hint` in `attention`; and the tag dim at the
  right end. The first match is focused. When the list does not fit, a last
  dim row says where the rows shown sit: "1–8 of 43 · ↓ 35 more", with "↑ 3
  above · " in front once it has scrolled. With nothing matching, the panel
  holds one dim row, "no matches". A
  skill the model has loaded shows in the transcript as a skill, not as a file
  read.
- **File search.** Typing `@` opens a file search panel, and choosing a file
  inserts its path as text. It is drawn as the `/` panel is, one path per
  row in `accent`, cut from the left when long so the file name stays, and
  "no files match" dim when nothing does. The model reads the file itself if it needs it.
  `@foo` searches the files git tracks in the workspace's repository, so a huge
  repository costs one listing, never a walk of the tree. `@` followed by `/`,
  `~/` or `../` searches the machine instead: the text up to its last `/` is the
  base directory (`~/` starts at home, `../` at the workspace) and the rest is
  the query. The base's own matching entries come first, directories first, then
  matching entries deeper under it from one walk that honours `.gitignore`, skips
  `.git`, and stops at 50 rows or 100,000 examined entries (entries the walker
  skips are not counted). Rows read as typed, a directory ending in `/`: choosing
  one enters it and keeps the panel open on the new base, choosing a file inserts
  its path and closes the panel, and a base that cannot be read says why.
  Each keystroke cancels the last search, in either mode.
  A remote terminal searches the hub's machine in both modes: `~/` is the
  hub's home and `../` starts at the workspace there. The path listing stays
  on the hub; only the ranked rows reach the terminal.

### Moving through the conversation

The input box holds the keyboard until the person moves focus out of it.
Shift+Tab moves focus to the newest item in the conversation.

- ↑ and ↓, or k and j, move focus through turns, tool groups and their
  ledger rows, scrolling the conversation as needed.
- Enter opens the focused item, as a click does: a group's ledger, a call
  inline under its row, a delegate's view. Enter on an opened call opens
  its swapped view.
- y copies the focused item's text. Ctrl+G opens it in `$VISUAL` or
  `$EDITOR`.
- Tab moves focus to the panel, then the rail, then back to the
  conversation.
- Esc returns focus to the input box, keeping the draft, except on home,
  where Esc on a focused row goes back to the session the person came
  from ("Home").

The focus order is not defined screen by screen. It is every click target on
screen, the same targets hover highlights, and each turn, from top to bottom and left to
right. Every mouse target, an extension widget's included, is therefore a
focus stop with no extra work, and nothing the mouse can reach is out of the
keyboard's reach.

### The input box

The input box is a `surface` slab with ▄ and ▀ edges across the
conversation's width: a ▌ stripe in `accent`, a space, "› " in `info`, the
draft, and the cursor, a dim █. While search is open, typing goes to the
search box and the draft shows without a cursor.

- Enter sends. Shift+Enter inserts a line break, with Ctrl+J for terminals
  without the kitty keyboard protocol. Pasted text keeps its line breaks. The box grows to about a third of
  the screen, then scrolls.
- Ctrl+U deletes from the start of the cursor's line to the cursor, the
  line ending at a line break, not at the box's wrap; at a line's start it
  deletes nothing.
- ↑ in an empty box recalls earlier prompts: from this session, then the
  project's earlier sessions, newest first. Ctrl+R searches them. They come
  from the project's prompt history (`docs/state.md`), read through the hub,
  so a remote terminal recalls the same prompts. In a draft of several lines, ↑ moves
  the cursor until it reaches the first line.
- A paste over about 10 lines shows as one token, "[Pasted text #1 · 312
  lines]", and the full text is sent. Clicking the token, or Ctrl+G with the
  cursor on it, opens it in the editor.
- Ctrl+V with an image shows "[Image #1]" and sends the image as an image
  part in the prompt, which the session processes as it enters
  (`docs/invocation.md`). The terminal reads the image with the system
  clipboard command on the machine it runs on: `osascript` reading
  `«class PNGf»` on macOS, and on Linux `wl-paste --type image/png` under
  Wayland, else `xclip -selection clipboard -t image/png -o`. No terminal
  protocol carries the read. Where that machine has no readable clipboard,
  such as an SSH login, Ctrl+V shows a notice saying so and leaves the draft
  as it is. An image over 50 megapixels (width × height, read from its PNG
  header) or over 256 MiB is refused at the paste with a notice, and the
  draft is left as it is.
- `!cmd` runs a shell command and sends its output with the next prompt.
  `!!cmd` runs it and shows the output only to the person (`shell` with
  `send` false, `docs/invocation.md`).

### Bindings

| Action | Id | Key | Other paths |
|---|---|---|---|
| Send a prompt, or a steering message during a turn | `send` | Enter | |
| Insert a line break | `line_break` | Shift+Enter | Ctrl+J |
| Close what is on top; interrupt the turn when nothing is open | `close_or_interrupt` | Esc | click the overlay's ✕ or outside it; click "Esc to interrupt" |
| Clear the draft, then quit | `clear_then_quit` | Ctrl+C, twice within about a second on an empty box | `/quit, /exit` |
| Go home | `go_home` | ⌥0 | `/home` |
| Start a new session | `new_session` | Ctrl+N | `/new` |
| Switch to the session of rail card N | `rail_row_n` | ⌥1 to ⌥9 | click the card |
| Delete the selected exited, or a live one with Delete session in the session list | `delete_session` | Delete, or Backspace, on the row | click the row's ✕ |
| Recall an earlier prompt from the project of the session on screen | `recall_prompt` | ↑ in an empty box | |
| Move focus from an empty input box into the rail | `focus_rail` | ← in an empty box | |
| Search those prompts | `search_prompts` | Ctrl+R | |
| Move by word | `move_word` | ⌥← ⌥→, Ctrl+← Ctrl+→ | |
| Delete a word | `delete_word` | ⌥Backspace | |
| Delete to the start of the line | `delete_to_line_start` | Ctrl+U | |
| Start or end of the line | `line_start_end` | ⌘← ⌘→, where the terminal passes them | |
| Open the draft, or a pasted token, in `$VISUAL` or `$EDITOR` | `open_in_editor` | Ctrl+G | click the token |
| Paste an image | `paste_image` | Ctrl+V | |
| Open or close the ledgers | `toggle_ledgers` | Ctrl+O | click a group's line |
| Move focus from the input box into the conversation | `navigate` | Shift+Tab | click an item |
| Move focus to the next or previous item | `focus_next_prev` | ↓ ↑, j k | click an item |
| Open the focused item | `open_focused` | Enter | click it |
| Copy the focused item | `copy_focused` | y | select it |
| Move focus to the panel, the rail, then the conversation | `focus_area` | Tab | click the area |
| Show or hide the panel | `toggle_panel` | ⌥P | `/panel` |
| Show or hide the rail | `toggle_rail` | ⌥R | `/rail`; drag its edge |
| Search | `search` | Ctrl+F; Cmd+F where forwarded | |
| Open the search results | `search_results` | Ctrl+F with search open | click the match count |
| Next or previous match | `search_next_prev` | Enter or ↓, Shift+Enter or ↑, with search open | |
| Jump to the end | `jump_to_end` | End | click "↓ New messages below" |
| Select a queued steering message | `select_steering` | ⌥↑ ⌥↓ | its mouse target |
| Amend it | `amend_steering` | Enter | its mouse target |
| Drop it | `drop_steering` | ⌥X | its mouse target |
| Reopen a request put aside, or move to the next, the oldest first, switching to its session | `next_request` | ⌥A | `/approvals`; click the badge or a waiting card |
| Open the model picker | `model_picker` | Ctrl+L | `/model` |
| Choose in the model picker for this session only | `session_only` | Ctrl+S | |
| Show every model in the model picker, or only the scoped ones | `show_all` | Tab | click "[show all]" |
| Refresh every model list in the model picker | `refresh_lists` | Ctrl+R | click "⟳ refresh all" |
| Open the key map | `key_map` | F1 | `/?` or `/help` |

Every key bound to `delete_session` acts as Delete does. A rebound character key
stops on home's list and types on a focused rail. `[]` unbinds it.

The key map, `/?` or `/help`, is an overlay over the conversation listing every
binding by area with its other paths. Esc closes it. Ctrl+L opens the model picker,
so it does not redraw the screen as it does in some terminal programs.

The key map docks at the bottom, full width, in the overlay frame ("Look",
"Overlays"). Its title is "Key map" with a ✕, then two dim lines: "Every
binding by area, with its other paths." and the counts ("38 actions, 26 with
other paths"). After a blank row comes a row of tabs, "All" and then each
area, two columns apart: the chosen tab bold, drawn dark on white, the rest
dim; ← and → move between them. Under the tabs, the search line reads "Type
to search shortcuts" dim, then "› " dim and the typed query bold with a dim
█. Typing narrows the rows to bindings whose action, key or other paths
contain the query, ignoring case, across every area when the tab is All.

Each binding is one row over three columns that start at the same cell on
every row: the area dim, the action with any condition dim in parentheses
after it, and the keys dim, alternatives comma-separated and other paths
after " · ". Each column wraps inside its own width. The first row is
focused. When rows hide below, a dim ↓ takes the last shown row's gutter; a
screen too short for them scrolls, and its last line reads "↑ 4 more · ↓ 12
more" dim. The footer is "↑↓ move · ←→ tabs · Esc closes".

Hints that name a key, on home, name the action's
bound key from this table, leaving the key out when the action is unbound.

Every action acts in some of six contexts, and the terminal is in exactly one
of them when a key arrives:

- Overlay: something on top has the keyboard. That is the quit question, the
  close-all and stop questions, the import report, the reviewer setup, the
  `/delete` question on an attached session, the session-list delete question
  on home, the workspace picker, the key map, an approval or question, an
  offer, the hooks install offer, the Ctrl+R panel, or a notice's whole text;
  or the `/` or `@` completion panel while focus is not
  in the conversation. An overlay's own keys, such as an approval's ↑ and ↓,
  are not bindings.
- Picker: the model picker or the credential picker is open. Only its own keys and the global actions
  act.
- Search: conversation search is open.
- Conversation: focus is in the conversation.
- Steering: a queued steering message is selected.
- Input: the input box has the keyboard, home with nothing open included,
  and so does a focused rail: typing there goes to the input box. On home,
  ↑, ↓, ← and → move between the session list, the entry bar and the chip
  row ("Home"), and Esc on a focused row goes back to the session home came
  from; on a focused rail ↑, ↓, Enter, → and Esc move and
  switch ("The rail"); like an overlay's own keys, they are not bindings.
  Delete on a focused rail card stops that session after asking ("Quit"); with
  the input box focused it edits the draft.

A global action acts in all six. The rest act only where they are listed:

- Global: `close_or_interrupt`, `clear_then_quit`, `rail_row_n`,
  `toggle_ledgers`, `toggle_panel`, `toggle_rail`, `jump_to_end`,
  `next_request`, `key_map`.
- Picker: `session_only`, `show_all`, `refresh_lists`.
- Input, Steering and Conversation: `go_home`, `new_session`,
  `open_in_editor`, `navigate`, `search`, `select_steering`, `drop_steering`,
  `model_picker`.
- Input and Steering: `line_break`, `search_prompts`, `move_word`,
  `delete_word`, `delete_to_line_start`, `line_start_end`, `paste_image`.
- Input: `send`, `recall_prompt`, `focus_rail`.
- Steering: `amend_steering`.
- Conversation: `delete_session`, `focus_next_prev`, `open_focused`,
  `copy_focused`, `focus_area`.
- Search: `search_results`, `search_next_prev`.

Every action has a stable id, and `keys."<id>"` in the global configuration
binds it to a key or a list of keys (`docs/configuration.md`). The value
replaces all of the action's default keys, and `[]` leaves it unbound. An
entry equal to the defaults counts as unset.

A key name is any modifiers, then a key, joined by `+`, in any case:
`ctrl+t`, `alt+up`, `shift+enter`, `super+left`. The modifiers are `ctrl` (or
`control`), `shift`, `alt` (or `opt`, `option`, `meta`) and `super` (or `cmd`,
`command`). The key is one of `enter` (or `return`), `esc` (or `escape`),
`tab`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`,
`pagedown`, `up`, `down`, `left`, `right` and `f1` to `f12`, or a single
character. An uppercase letter means Shift and that letter, and Shift on a
character that is not a letter is dropped. `+` alone, or after a modifier as in `ctrl++`,
is the plus key.

Six actions have ordered variants: `move_word` (left, right),
`line_start_end` (start, end), `focus_next_prev` (next, prev),
`search_next_prev` (next, prev), `select_steering` (up, down) and
`rail_row_n` (1 to 9). Their list holds keys in groups of one key per variant,
in that order, so `["ctrl+b", "ctrl+f", "alt+b", "alt+f"]` gives `move_word`
two keys each way. A list whose length is not a multiple of the number of
variants is invalid, and `[]` unbinds the action. A group identical to an
earlier one is dropped, and a key repeated in a one-variant action counts
once.

Two actions clash when they share a key, at any variant slot, and act in a
context in common. Actions whose contexts do not overlap may share a key, as
`send`, `open_focused`, `amend_steering` and `search_next_prev` share Enter.
At startup each clash gives one `notice` naming both actions. When an entry
the person set clashes with a default, that entry reverts to its defaults;
when two entries the person set clash, both revert. A revert can bring back a
default that clashes with another entry, so the check runs again until a pass
finds no clash.

An invalid entry gives one `notice` at startup, and the action keeps its
defaults. An entry is invalid when its id names no action, its value is not a
string or a list of strings, a string is not a key name, a variant action's
list has the wrong length, or one key sits in two variants of the same action.

No action can be bound to Ctrl+C, and `clear_then_quit` cannot be rebound,
because the second Ctrl+C always quits ("Input and focus"). Either entry gives
a `notice` and keeps the defaults.

`/keys` opens the rebinding screen: every action with its keys, id and
description, the unbound ones included. Selecting an action, pressing Enter
and then a key binds it, so only a key the terminal actually delivers can be
bound; an action with variants takes one key for each variant, in order. A key
already bound to another action shows inline, with a choice to swap or cancel.
A swap gives the other action, in that key's place, the key this action held
for the same variant; when there is none, or it would clash, the other action
loses that key's group instead. `r` resets an action to its defaults, through
the same swap or cancel when a default is now another action's. Delete, or
Backspace, unbinds it, saving `[]`. The screen's own keys are fixed: ↑ ↓
PageUp PageDown move, and Esc cancels a capture or closes the screen, so Esc
is bound only by hand. Ctrl+C still clears, then quits, there. The screen saves
only the bindings that differ from the defaults, so a default changed in a
later release still applies.

### Slash commands

| Command | What it does |
|---|---|
| `/home` | Goes home, focused on the session on screen's row. |
| `/new` | Goes home with the cursor in the input box, to pick the workspace, worktree, model and thinking level for a new session. The session left keeps running. |
| `/resume` | Goes home, focused on the first exited row. |
| `/model` | Opens the model picker. |
| `/thinking [<level>]`, `/effort [<level>]` | Sets the thinking level for the session's model, saving `models."<model>".thinking`; the default model is unchanged. Sent while a turn runs, the choice applies when the turn ends. The notice appears when the choice is sent; if the session rejects the choice, the rejection's notice replaces it. With no level, opens the model picker on the model's chips: Enter saves the level, Ctrl+S applies it to this session only. |
| `/credential [<label>]` | With no label, opens the credential picker ("Swapped views"): Enter switches to the chosen label and saves it as the provider's `credential`, Ctrl+S switches for this session only and saves nothing. With a label, switches to it and saves it. The label is saved under the provider the session accepted the switch on. The terminal first says the switch rebuilds the cache, with its size (`docs/model-routing.md`, "Which credential a session uses"). |
| `/scoped-models` | Opens the model picker as a checklist over every installed model: each row starts marked from `scoped_models`. Typing filters the list as it does in the picker, Space included in the query. Enter or a click on the mark toggles the focused row's mark, and Ctrl+S, `session_only`'s key, saves the marked list as `scoped_models` instead. Esc clears the query first and closes on the next press, saving nothing. Marks survive a query change. Each row carries its mark between the gutter and the id: `[x]` marked, `[ ]` not, bold on the focused row and dim when unmarked. The count row reads ` {m} of {n} marked ` unfiltered and ` {v} of {n} models · {m} marked ` filtered, with no show-all toggle and no refresh button. The row below each model is its thinking row, dim with nothing to choose. Ctrl+S saves and closes. The footer is "↑↓ move · Enter toggle · Ctrl+S save · Esc close", keys bold. Marking none saves `[]`, which means every model; a saved entry that is not installed is kept. |
| `/tools` | Opens the tools view. |
| `/hooks` | Opens the hooks view. |
| `/mcp` | Opens the MCP servers view. |
| `/context` | Opens the context breakdown. |
| `/usage`, `/cost` | Opens the usage view. |
| `/quota` | Opens the quota view and fetches every label's quota. |
| `/status` | Opens the status view ("Swapped views"). |
| `/doctor` | Opens the doctor report. |
| `/diff` | Opens the Changed files view ("Swapped views"), as the panel's card does. |
| `/release-notes` | Opens the running version's release notes, or says none are available. |
| `/copy` | Copies the newest reply ("Selection and copy"). |
| `/pwd`, `/cwd` | Shows the session's workspace; on home, shows the notice `No session on screen.` |
| `/add-dir <path>` | Adds an existing folder to the session's boundary. Relative paths resolve from the workspace. A folder already inside the boundary is accepted without a change. The folder lasts for the session; `/cd` or a new session drops it. With no path, the terminal shows the command's usage. |
| `/panel` | Shows or hides the panel ("The panel"). |
| `/rail` | Shows or hides the rail ("The rail"). |
| `/rules`, `/permissions` | Opens the standing rules. |
| `/reviewer-setup` | Sets up the reviewer's notes with you (`docs/permissions.md`, "Setting up the notes"). |
| `/settings`, `/config` | Opens the configuration keys. |
| `/theme [<name>]` | Opens the theme picker; with a name, sets the theme at once and saves `tui.theme`. |
| `/keys`, `/keybindings`, `/keymap` | Opens the rebinding screen ("Bindings"). |
| `/skills` | Opens the skills. |
| `/rewind` | Opens the rewind view. |
| `/cd <path>` | Moves the session to another workspace ("Moving workspace"). With no path, notes `/cd needs a path.` If the resolved path is the current workspace, refuses `invalid_arguments`, `already in that folder`. |
| `/fork` | Rewinds to the end of the conversation: sends `rewind` with `at_end`, and the terminal switches to the new session. |
| `/handoff [instructions]`, `/compact [instructions]` | Starts a handoff (`docs/handoff.md`, "A person"). |
| `/name <text>`, `/rename <text>` | Names the session. |
| `/login` | Logs in ("Logging in"). |
| `/logout` | Logs out ("Logging out"). |
| `/reload` | Reloads configuration, MCP servers and extensions (`docs/mcp.md`, "Reload"), and re-enables a TUI extension a timeout switched off ("When an extension fails"). |
| `/import` | Copies another harness's configuration into Fiber home, once (`docs/configuration.md`, "Importing"). |
| `/clear` | Starts a fresh session in place, closing the session on screen ("Clearing"). |
| `/close [all]`, `/stop [all]` | Stops the session on screen ("Quit"). `/close all` stops every live top-level session on this hub after one question ("Quit"): with no live session it says "No sessions to close", and Enter closes exactly the sessions the question counted. |
| `/delete` | Asks before stopping the session and its cascade, then deletes the exact confirmed set permanently; it never retries a refusal ("The session list"). |
| `/feedback <description>`, `/bug <description>` | Writes a feedback report with the description, version, platform, doctor output and the diagnostics of the session on screen, and prints its path and the issue link. With no session on screen, the diagnostics are the hub's lines only. A session this machine holds no diagnostic lines for, such as one on a remote hub, still gets its id, the instruction to export its log privately and the hub's lines, with a notice that this machine has no diagnostic lines for it. The report is the one `fiber feedback` writes (`docs/invocation.md`, "Commands and flags"). |
| `/quit`, `/exit` | Quits ("Quit"). |
| `/approvals` | Reopens the waiting approvals and questions. |
| `/?`, `/help` | Opens the key map. |
| `/delegates`, `/agents` | Opens the delegates view: every delegate of the session on screen, running and finished. |
| `/jobs`, `/tasks`, `/ps` | Opens the running jobs view: every running job of the session on screen, in the order they started. With no session on screen it shows the notice `No session on screen.`; with no jobs running it shows the notice `No jobs running` and opens nothing. Words after the command are ignored. |

The `/` list shows built-in commands in this order.

`/add-dir path/to/shared` sends this driver command:

```json
{"id":"c_7f3a","command":"add_directory","args":{"path":"path/to/shared"}}
```

The range line's total counts every entry the query matches: the matching built-in rows plus the matching session commands.

A row's other names, such as `/exit`, are aliases: typing one runs the same command, and the `/` list shows them after the name on that command's row, never as rows of their own.

The rest of the list is the session's answer to the `commands` driver command
(`docs/invocation.md`, "What each command does"): its skills, prompt templates
and extension commands, each with the tag the answer gives. The terminal sends
`commands` when it attaches to a session and again after each `reloaded`,
each `extensions_loaded`, which an offer's approval writes with no `reloaded`
(`docs/events.md`, "`extensions_loaded`"), and each `turn_started`, and
fills the list from the answer to the latest one it sent; until that answer
arrives, the list holds the commands above only. A skill, prompt template or MCP server's prompt named like a command above is left out. An extension's command named like one, which the `commands` answer marks with `extension`, takes its row instead, with its own description and hint, and runs in its place; it loads only when its manifest names that command in `replaces` (`docs/extensions.md`, "What a package holds"). An extension's command named like an alias takes that name the same way: its own row shows, and the built-in row drops the alias. An extension's command named like a built-in's name takes that row's aliases with it: they run the extension's command and show on its row. An extension's commands run
with the `command` driver command (`docs/extensions.md`, "Commands and
screens").

An unrecognised `/name` is refused: the terminal keeps the text in the
input box and shows the notice `Unknown command /<name>. Type / for the
list.` A first word with another `/` in it, such as `/usr/bin/env`, is a
path and is sent as a message. On home, the same rule applies against
the built-in commands only.

`/add-dir <path>` adds a folder to the session's boundary: it sends
`add_directory` with the path (`docs/invocation.md`, "Driver commands").
With no path, it shows the command's usage. There is no removal: an added
folder lasts for the session.

### Clearing

`/clear` asks the hub to start a fresh session in the workspace of the session
on screen (its worktree included), with that session's model, thinking level
and credential label, and switches the screen to it with an empty
conversation. The new session builds its own prompt cache; nothing in the old
session's prefix changes. The new session takes the old one's card on the
rail, keeping its number. The terminal keeps the old session on screen until
the hub answers with the new session's id. It sends the old session's workspace
path unchanged.

Before the session has built its first prompt, its credential label is the one
its `start` named; with none named, the new session picks its credential as any
new session does. When the hub refuses or fails the `start`, the screen stays
on the session it shows, a notice gives the start's code and message, and no
`close` is sent.

The old session is sent `close` without `now` (`docs/invocation.md`, "What
each command does"): it takes no more prompts, finishes the turn and the jobs
in flight, and exits. Until it exits, home shows it as a live row marked
"CLOSING" ("The session list"); then it joins the project's exited rows, where
it reopens like any exited session. To stop a turn instead of letting it
finish, the person presses Esc before `/clear`.

When the old session waits on an approval or a question, a `close` without
`now` would wait for the answer, so `/clear` asks first, in an overlay
("Look", "Overlays"): the title "Clear", then "review #688 waits on an
approval." dim, then two choices, "Enter" to clear anyway and "Esc" to stay,
with Enter's focused. Clearing anyway sends `close` with `now`, which drops
the wait. A wait that begins after `/clear` asked nothing keeps the close
polite: the old session's request waits in the queue for an answer.

### Logging in

`/login` logs in from the terminal with the same flow as `fiber login`. It
opens an overlay ("Look", "Overlays") titled "Log in" with a ✕: the providers under
"Providers", each tagged `browser` for OAuth or `key` for an API key, then
the extension credentials under "Secrets", the first row focused.

On a remote terminal, login runs on the hub and stores its credential in the
hub's Fiber home. OAuth always uses the device-code step below; Esc cancels
the hub's login. The reviewer step lists the hub's models.

Once a provider is picked, each step replaces the whole overlay; nothing is
added under the list. The title is the provider's name and the list is not
drawn. The steps keep the provider list's height and share one width, so the
overlay does not jump between them; the URL is cut to fit rather than
widening it. Esc goes back to the provider list.

- **The browser wait.** OAuth opens the browser and also shows the URL to
  copy, for SSH: "Open this URL to log in to anthropic:", the URL cut from
  the left to keep its tail, and "Waiting for the browser…" dim. Footer:
  "y copy URL · Esc back".
- **Device code.** "Enter the code {code} at this URL to log in to {name}:",
  the code shown whole across wrapped rows, the URL cut from the left, and
  "Waiting for the browser…" dim. Footer: "y copy URL · Esc back"; `y` copies
  the whole URL.
- **Key entry.** "Label (--as): default." and "Key for google:", the key
  masked as dots with a block cursor. Tab moves between the key and the
  label. Footer: "Tab key or label · Enter submit · Esc back".
- **Done.** "✓ Logged in to anthropic." in `success`. Footer: "Esc back".
- **Failed.** "Login to anthropic failed: token expired.", the reason in
  `error`. Footer: "Esc back".
- **Reviewer.** After the key is stored, and after a browser login stores
  its credential, the provider's models with the saved pick preselected,
  else the package default: "1) fast-1", the saved row marked
  " (saved)", or "Saved reviewer `gone` is not listed; Enter keeps it."
  when the pick left the list. With nothing preselected no row is
  highlighted until the person moves or clicks; Enter saves the highlighted
  row, or the kept pick, and stays open otherwise. An empty list keeps the
  login with a note naming the provider; a failed save keeps it with an
  error naming the key. Footer: "↑↓ move · Enter save · Esc back". Esc
  returns to the provider list writing nothing.

### Logging out

`/logout` logs out from the terminal with the same delete steps as `fiber logout`. It
opens an overlay ("Look", "Overlays") titled "Log out" with a ✕: the providers under
"Providers", one row per stored or configured label with the default marked,
then the extension credentials under "Secrets", the first deletable row focused.
A row whose key comes from an environment variable, a file or a command is dim
with its source, and Enter on it does nothing, as on a heading or the note.

Enter on a deletable row asks for confirmation, naming the provider and the label,
and, when other providers read the same stored directory, naming them: "Delete
acme/work?" with "Also used by: beta" below it. The confirm keeps the list's
height and shares its width, so the overlay does not jump between the steps.
Footer: "Enter confirm · Esc back"; Esc goes back to the list deleting nothing.

Confirming deletes the stored file only, leaving `providers."<name>".credential` as
is, then re-lists the rows with the overlay still open: the deleted row is gone,
and "Removed credentials/acme/work." shows below the list, with ", which beta also
reads" when siblings share the stored directory. When a configured source covers
the deleted label its dim row remains, and later sessions use that source. A
failed delete keeps the row with the seam's message below the list. Footer on the
list: "↑↓ move · Enter delete · Esc close".

### Quit

- **Ctrl+C clears, then quits.** It clears a draft in the input box. With the
  box empty, it says "Press Ctrl+C again to quit", and a second press within
  about a second quits. It never interrupts a turn on its own; Esc does that.
- **Quit** is a second Ctrl+C, or `/quit`, `/exit`. With no session working, meaning
  no turn, job or delegate running, the terminal exits without asking. With
  some working, it asks in an overlay ("Look", "Overlays"): the title
  "Quit", "2 sessions working" dim, then three choices, "Enter" to leave them
  running, "c" to close all and "Esc" to stay, with Enter's focused. The
  footer reads "click a choice · they keep running meanwhile".
  - Enter, the default, leaves them running. The terminal closes its
    connections, and each session follows the lifecycle rules
    (`docs/invocation.md`, "Lifecycle").
  - "Close all" sends each working session `close` with `now`. The prompt says how many of them are also
    open elsewhere: a session's `clients` on its `session_status`, less the terminal's own `full` connection to it when it holds one.
- **Stopping one session** is `/close`, the ✕ on its home row, or Delete on
  its focused home row or rail card. It sends `close` with `now`, which ends
  its turn, stops its jobs and delegates, and exits it
  (`docs/invocation.md`, "Shutdown"). Delete asks first, in an overlay: the
  title "Stop <name>?", a dim line that reads "working", "open elsewhere",
  both as "working, open elsewhere", or nothing, then two choices, "Enter" to
  stop and "Esc" to cancel, with Enter's focused. The footer reads "click a
  choice". The line is counted as the quit prompt counts, once, when the
  question opens, and the overlay never live-updates. It draws over the
  conversation when asked from a rail card. Enter sends `close` with `now`;
  when the stopped session is the one on screen the terminal goes home, and
  stopping another card leaves the screen as it is. Esc, the ✕ and a click
  outside the frame close the question and send nothing. With the link down
  Delete opens nothing, and Enter does nothing and leaves the question
  open. If the session left while the question showed, Enter sends nothing
  and closes it.
- **Stopping every session** is `/close all`, on home or over a session. It asks in an overlay: the title "Close all", "2 sessions, 1 working, 0 open elsewhere" dim (live top-level sessions on this hub, how many of them are working, how many are open elsewhere, counted as in the quit prompt), then two choices, "Enter" to close all and "Esc" to cancel, with Enter's focused. The footer reads "click a choice". Enter sends each live top-level session `close` with `now` and leaves the terminal on home; Esc closes the question. With no live session it opens nothing and says "No sessions to close."; with the link down it opens nothing and keeps the draft. `/quit` straight after it exits without asking once those sessions have left, since none is working. Quit's own behaviour does not change.

### Getting the person's attention

When any live session starts waiting on the person, for an approval, a
question or a finished turn, whether or not it is on screen, the terminal:

- sends an OSC 9 desktop notification where the terminal supports one, and a
  bell elsewhere. Support is read once at start: Ghostty, iTerm2, kitty or
  WezTerm, by `TERM_PROGRAM`, `TERM` or `KITTY_WINDOW_ID`, and no tmux or
  screen in between (`TMUX` and `STY` unset), since a multiplexer swallows the
  escape. The text is "Fiber: <name> needs you: <summary>", or
  "Fiber: <name> finished", with the session's name, or its id when it has
  none, cut to 60 characters, the summary cut to 120, and control characters
  dropped.
- shows the state in the terminal title: "! fiber · approval", "! fiber ·
  question" or "! fiber · waiting" while that session still waits, and
  "✓ fiber · finished" after a finished turn until the next key or click. The
  latest attention wins, and the title goes back to normal once nothing it
  names applies.

Each can be turned off: `tui.attention.notification`, `tui.attention.bell` and
`tui.attention.title`.

## Mouse and hover

Mouse reporting is modes 1000, 1002, 1006 and 1003, every motion. Mode
1004, focus reporting, is also on for Quota queries ("The panel"); restore
turns it off. Hover
highlights the click target under the pointer. The highlight is instant, with
no transition. A frame redraws only the rows that changed.

`tui.hover` turns hover off, which drops mode 1003.

Measured in the prototype on macOS arm64
([research/tui-prototype/README.md](../research/tui-prototype/README.md),
"Hover's cost"): a still pointer costs nothing; each cell the pointer crosses
costs about 115 µs; each change of target costs one frame, about 1.8 ms and
255 bytes; a fast sweep costs 6.8% of a core. The timings do not carry over to
Linux; the byte and frame counts do.

Measured in Fiber on macOS arm64 (Darwin 25.6.0) with the `hover` jig,
`cargo run --release -p tui --example hover -- crates/tui/examples/hover.jsonl`,
at 160 by 48, 20,000 motion reports per case, the median of 5 runs, with
truecolour themes: a still pointer, and a pointer moving along one target,
wrote 1 frame and 135 bytes in 20,000 reports; a change of target on every
report wrote 20,000 frames and 2,590,000 bytes, painted with the theme's
colours; a fast sweep over a conversation whose lines are targets wrote 1,458
frames and 568,722 bytes in 20,000 reports.

Measured in Fiber on Linux x86_64 (7.3.0-9-generic, a 16-core, 24-thread 13th Gen
Intel Core i7-13700T, load average 1.25 to 3.05 across the runs) with the
same jig, command, size and runs: a still pointer, and a pointer moving
along one target, wrote 1 frame and 135 bytes in 20,000 reports; a change
of target on every report wrote 20,000 frames and 2,590,000 bytes; the
fast sweep wrote 1,458 frames and 568,722 bytes in 20,000 reports.

## Look

- **Surfaces, not lines.** The person's messages, each turn's card, code
  blocks, the handoff band, the input box, the approval panel, the question
  form, the search box, each panel card and every overlay sit on their own
  background tint,
  with half-block edges (▄ above, ▀ below) and no borders. An edge's cell
  takes the surface's colour over the colour around it: the terminal's
  background in the conversation, the turn's tint for a code block inside a
  card, the `panel` tint for a panel card.
- **A stripe marks state** on the side its surface is anchored to: ▌ on the
  left for the input box, a running or finished job, an approval and a
  question form; ▐ on the right for the person's prompt bubble. An overlay
  has no stripe. The stripe is one unbroken bar, because ▌ and ▐ fill half of each cell as Ghostty draws them.
  Where a terminal cannot draw it unbroken, there is no stripe. Fiber draws
  stripes in Ghostty, WezTerm and kitty, and not inside tmux or screen;
  elsewhere the stripe's cell keeps its tint.
- **Colours come from the theme,** in truecolour where the terminal has it:
  `COLORTERM` of `truecolor` or `24bit`, or a `TERM` of `xterm-ghostty`,
  `xterm-kitty`, `wezterm` or one ending in `-direct`, which survives SSH where
  `COLORTERM` is usually dropped. Anywhere else the terminal has 256 colours,
  and each colour is the nearest entry of the xterm palette: the neutral tints
  (`background`, `panel`, `surface`, `surface_raised`, `turn`, `prompt`,
  `code`, `approval`, `rule`, `hover`) take the grey ramp, the other tints take
  the colour cube so they keep their hue and an alert stays red, and text
  takes either. The terminal's
  own 16 colours are never used.
- **`NO_COLOR`** set and not empty turns colour off: every role is the
  terminal's default colour. Bold, dim, reversed and underline stay, so focus,
  selection, matches and state words still show. With no tint there is no
  surface to see, so each surface with half-block edges is drawn instead as
  a thin box, ┌─┐ above, │ on each side and └─┘ below, dim, in the edges'
  rows and one column on each side, so a rail card's text is two columns
  narrower. The person's prompt bubble is the exception: its edges draw as
  blanks and its rows stay.
- **Markdown in replies:**
  - Headings are bold in `heading`, their `#` markers stripped.
  - Paragraphs are in the text colour, `**bold**` is bold with no colour of
    its own, and inline code is in `info`, with no tint and its backticks
    stripped.
  - A bullet is "• " in `accent`, its wrapped lines hanging two columns in.
  - A code block is a `code` slab with ▄ and ▀ edges. Its first row is the
    language dim, one column in, and "click to copy" dim, right-aligned. Each
    line is its number right-aligned in four columns in `rule`, two spaces,
    then the code with syntax colours; a comment is italic as well.
  - A table's header row is bold, and under each column is a ─ rule in
    `rule` as wide as the column; columns are three spaces apart, and the
    rules break with them. A column whose cells are all numbers is
    right-aligned, in the text colour.

```
 rust                                                         click to copy
   1  pub fn try_lock(path: &Path) -> Result<Lock> {
   2      // one holder at a time
   3      let file = File::create(path)?;

Crate   Tests   Lines
─────   ─────   ─────
hub        41   3,120
tui       212   9,804
```

### Overlays

Every surface drawn over the screen shares one frame: the
key map, the quit question, the close-all question, the stop question, the
hooks install offer, the `/delete` question on an attached session, the
session-list delete question on home, the workspace picker, the `/login`
overlay, the Ctrl+R panel, a notice's whole text, the import report, the
reviewer setup, the `/` and `@` completion panels, and
the model picker inside its view.

- **The frame** is a `surface` slab with a ▄ edge above and a ▀ edge below,
  and no border glyphs. Each row that holds text opens with two blank
  columns and ends with two blank columns, with no stripe. One blank
  row sits inside each edge, so no text touches either.
- **The width** is the content's plus those four columns, kept between 40
  and 150 columns and never wider than the screen. Content that wraps is
  wrapped at a preferred width first and the overlay shrinks to what the
  wrapped content needs: 71 columns for a question or notice, 55 for the
  workspace picker, and 96 for the model picker and the completion panels,
  less four columns on a narrower screen.
- **Placement.** An overlay is centred across the area it covers. A question,
  a notice, the Ctrl+R panel and the workspace picker are also centred down
  it, unless that would cover the input box's top row: then the workspace
  picker, when it fits there, sits with its bottom edge on the row above the
  box. The key map docks at the bottom, full width. The completion panels
  sit above the input box, and the model picker under its view's top row.
  The overlay's side margins are blank, with no tint, so the screen behind
  reads around it.
- **Rows.** A title, when there is one, is bold in `accent`, with a ✕ or a
  count dim at its right end. The title, body and footer keep one blank row
  between them; the body's sections keep their own blank rows.
- **Choices.** Each choice row starts with two columns of gutter. A row of
  keys and descriptions keeps the keys in one column as wide as the widest
  key, two spaces, then the description dim; a description that wraps hangs
  under its own start. The focused choice has "› " in its gutter, reads bold
  throughout, and sits on a full-width bar in `accent` with its text in
  `#000000`. Exactly one row is barred.
- **The footer** is dim and aligned left with the body, never centred. It is
  a legend: each key bold and its label dim, joined by " · ". It never
  repeats a key the body already lists as a choice; where the body lists every
  key, the footer says what the mouse does instead.

```
▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
▌  Quit                                       ✕
▌
▌  2 sessions working
▌
▌  › Enter  leave them running
▌    c      close all
▌    Esc    stay
▌
▌  click a choice · they keep running meanwhile
▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
```

### Themes

Fiber ships a dark and a light theme. A person adds their own as files in
Fiber home, an installed extension can carry more, and either is picked in
`/settings`, which writes `tui.theme`. `/theme` opens the same theme picker:
moving focus previews each theme live, Enter saves the choice, and Esc or
the close button restores the theme that was showing.
`/theme <name>` sets the named theme at once, saving `tui.theme`. With no
theme set, the theme follows the terminal's light or dark appearance and
switches when the terminal reports a change.

A theme sets colours only. It gives each colour role a value, and an
extension's spans name the same roles ("What a renderer returns"). Anything
bigger goes through "Extension seams".

The roles, in order, with each built-in theme's values:

| Role | What it colours | Dark | Light |
|---|---|---|---|
| `text` | the full text colour: replies, the draft, anything with no role of its own | the terminal's foreground | `#1f2328` |
| `muted` | what the doc calls dim: READY, labels and hints, summary lines and results, block quote bars, grips, the logo's counters | `text`, dim | `text`, dim |
| `accent` | bullets, the logo's mark, WORKING and the rail's spinner, a running job's stripe, the prompt and input stripes, the model's name, answered questions' headers, a quota bar, an overlay's title and selection bar, a project header's name, links and the ▣ line | `#6eaafe` | `#0969da` |
| `heading` | markdown headings | `#ff9f43` | `#bc4c00` |
| `success` | a completed call's ✓, a finished delegate's ✓, a finished job's stripe when it succeeded, a login's ✓ | `#6eaafe` | `#0969da` |
| `warning` | RETRYING and its spinner, a rail card's context bar from 60% | `#ff9f43` | `#bc4c00` |
| `error` | CRASHED, a failed job's stripe, an escalation's stripe, "irreversible" in an approval's header, a rail card's context bar from 85%, a down MCP server, a quota nearly spent, tool results in the context breakdown, a failed login's reason | `#ff5d73` | `#cf222e` |
| `attention` | NEEDS INPUT, what a card waits on, a standing ask's stripe, the working line's spinner and glimmer, a running call or delegate, the permission mode, the thinking level, the handoff marker, "steer", notices, approvals put aside, the pill's label, the search box's ⌕ | `#ff9f43` | `#bc4c00` |
| `added` | lines added: an edited file's `+N`, added lines in a diff | `#73c991` | `#1a7f37` |
| `removed` | lines removed: an edited file's `−N`, removed lines in a diff | `#ff5d73` | `#cf222e` |
| `code_text` | code with no syntax role | the terminal's foreground | `#1f2328` |
| `keyword` | keywords | `#6eaafe` | `#0969da` |
| `string` | string and character literals | `#ce9178` | `#a31515` |
| `comment` | comments, which are also italic | `#7a7a8a` | `#6e7781` |
| `number` | number literals | `#b5cea8` | `#116329` |
| `function` | function and macro names | `#7dd3fc` | `#0b7285` |
| `type` | type names | `#7dd3fc` | `#0b7285` |
| `constant` | constants: `true`, `null`, `ALL_CAPS` names | `#7dd3fc` | `#0b7285` |
| `operator` | operators | the terminal's foreground | `#1f2328` |
| `info` | inline code, a call's kind, an approval's tool name and rule prefix, the input box's › and the workspace picker's, "Copied", the context bar's fill | `#7dd3fc` | `#0b7285` |
| `secondary` | the git branch, the handoff band's label, the question form's stripe, title and marks, the delegates' ◆ | `#b39ddb` | `#6f42c1` |
| `rule` | rules, line numbers, a bar's empty cells, the pill, a grip's column under the pointer, "Chat about this" | `#3a3a4a` | `#d0d7de` |
| `scroll` | the scroll bar's thumb | `#808080` | `#b0b7bf` |
| `background` | every cell of the conversation no surface covers | the terminal's background | `#ffffff` |
| `panel` | the rail's and the panel's regions, the narrow layout's status line | `#0c0c11` | `#f6f8fa` |
| `surface` | the input box, the panel's cards, the search box, a swapped view's top row, overlays, the rail's working cards | `#1a1a22` | `#eaeef2` |
| `surface_raised` | the card on screen, a hovered card | `#3a3a4a` | `#d0d7de` |
| `card_retrying` | a rail card in RETRYING | `#262014` | `#ebe5d9` |
| `card_needs_input` | a rail card in NEEDS INPUT | `#241a10` | `#efe5db` |
| `card_ready` | a rail card in READY or IDLE | `#121217` | `#e8e8ed` |
| `card_crashed` | a rail card in CRASHED | `#201114` | `#eedfe2` |
| `turn` | a turn's card | `#101017` | `#f3f5f8` |
| `prompt` | the person's prompt bubble | `#343541` | `#dde3ea` |
| `code` | code blocks | `#181821` | `#eff1f4` |
| `handoff` | the handoff band | `#1f1a2e` | `#efe9f9` |
| `approval` | the approval panel for a standing ask, and the question form | `#1a1a22` | `#eaeef2` |
| `alert` | the approval panel for a reviewer's escalation; red in both built-in themes | `#501c20` | `#ffd7d9` |
| `hover` | the click target under the pointer where it has no tint; a target on a tint is drawn with that tint lifted: 0x14 on each channel away from the page, lighter on a dark tint and darker on a light one (a tint whose channels average below 0x80 is dark) | `#1e1e26` | `#e3e7ec` |
| `selection` | selected text | `#264f78` | `#b6d7ff` |
| `match` | a search match, its text white | `#5a4a1a` | `#fff1a8` |
| `match_current` | the current search match, its text black | `#ff9f43` | `#ffd67e` |

In the dark theme `text`, `code_text`, `operator` and `background` are the
terminal's own default colours, and `muted` is the text colour drawn dim, so
the conversation sits on the terminal's own background. They stay the
default at 256 colours. A theme file may still give any of them a `#rrggbb`.

The light theme paints its own text and page: `text`, `code_text` and
`operator` are `#1f2328` and `background` is `#ffffff`, so it looks the same
in any terminal. At 256 colours its page is the colour cube's white, 231,
which the grey ramp lacks. A tint whose nearest entry is 231 takes the grey
ramp's lightest, 255, so every tint stays visible on the page.

The dark and light themes, and every surface's look in this doc, are the
prototype's
([research/tui-prototype/src/main.rs](../research/tui-prototype/src/main.rs),
[theme.rs](../research/tui-prototype/src/theme.rs)), which the owner judged
by eye.

A stripe takes its state's colour, and the logo's five letters step through
`heading`, `accent`, `string`, `type` and `keyword`.

A theme is a file, `themes/<name>.json` in Fiber home or in an installed
extension (`docs/extensions.md`, "What a package holds"), and `tui.theme`
names it by `<name>`; `dark` and `light` always name the built-ins:

```json
{"base": "light", "roles": {"accent": "#0b7285", "alert": "#5f1e22"}}
```

`base`, `dark` or `light`, gives every role the file leaves out, so a theme
keeps working when a role is added. `roles` maps role names to `#rrggbb`. The
file is strict: any other key, an unknown role or a value that is not
`#rrggbb` refuses the whole file. A file that is missing or refused shows one
notice naming the theme and the reason, and the theme follows the terminal's
appearance. Nothing writes the bad name back. Fiber home's file wins over an
extension's, and between extensions the first by directory name in
`extensions/` wins; `/settings` lists each name once. Only a missing file
passes the name on: a file that is there but cannot be read or is refused is
the theme. A damaged extension's themes are left out, as are a switched-off
extension's (`extensions."<name>".enabled` is `false`): they are not listed,
and a `tui.theme` naming one reads as a missing file. Colours are given once, in 24
bits, and the 256-colour form is computed ("Look").

### Reduced motion

Every animation has a still form. `tui.reduced_motion` turns reduced motion
on, and it is on whenever a screen reader is detected.

### Screen readers

Fiber detects VoiceOver on macOS and AT-SPI on Linux at start,
and draws flat:

- no tints, half-block edges, stripes or animation
- one line per item, in reading order
- state in words rather than glyphs
- images as their one line, never inline

Each line starts with what the item is: "you 14:03:" and the prompt,
"reply:", "thought:", "tool read src/main.rs: 120 lines", "image:" and the
image's one line, "waiting: approval for shell rm -rf /tmp/x", "running:
shell, elapsed 12s", "done: completed, 2 tool calls".

`--screen-reader` and `tui.screen_reader` force the flat mode on or off.

### Images

Where the terminal speaks kitty's graphics protocol (Ghostty, kitty, WezTerm),
images show inline: an image a tool returned, in its ledger row, and a pasted
image, with or without text, in the prompt bubble.

Kitty's graphics protocol takes only PNG, so the loop converts (`fiber
image --png /dev/stdin` with the decoded bytes streamed to its standard
input): the terminal spawns one image child at a time and keeps that one
worker on the loop (each completion frees it for the next queued
conversion), and transmits only after converting. The child's standard
output must stay under the 32 MiB cap, or the child is killed and the
image fails; refusal and other failures reuse the child's stderr, kept to
16 KiB and trimmed of trailing whitespace, in the existing notice. The
draw never waits on the child: conversion runs on a worker thread and each
completion arrives as loop input. A completion from an earlier session's
generation is dropped, and a forgotten image never transmits.

- An image is at most 40% of the conversation's width and 12 rows tall,
  keeping its proportions. A click opens it in the system viewer, from a
  copy the terminal writes to `cache/images/` in Fiber home
  (`docs/state.md`).
- The terminal holds the image and moves it with the text, through kitty's
  Unicode placeholders. Fiber keeps no decoded image and sends nothing again
  on a scroll.
- When the session changes, Fiber deletes the previous session's inline
  image placements before drawing the next session and does not write those
  placements again; images the next session presents are written for the new
  session.
- An image part in the log carries its width and height, so paging counts its
  rows without decoding it.

Elsewhere an image is one clickable line, `▣ screenshot.png · 1280×800`, in
the link look ("Links"); in a ledger row it sits under the call's row.
An image the terminal cannot show, because its file is missing or too
large or the terminal refused it, is its one line, with a notice saying
why, and is not asked for again in that session.
iTerm2's protocol and Sixel make the client redraw an image on every scroll,
so only the logo, drawn once, uses them. `tui.inline_images` turns inline
images off.

## History and paging

The terminal holds a window of rendered history and asks the session for more
as the person scrolls, with the `history` command, which reads by `seq` over
the session's offset table (`docs/invocation.md`, "Driver commands";
`docs/events.md`, "Resume").

- **Opening reads the tail before the first frame, then counts history and
  makes one streaming pass.** The
  terminal subscribes at `summary` and asks `history`, with no `from_seq`,
  for the session's last lines. The terminal checks each answer for
  ascending, contiguous sequence numbers. It folds each eligible whole-turn
  run into fresh pages. If an older answer enlarges the run before install,
  the terminal rebuilds from fresh pages and discards the prior candidate.
  It installs the newest candidate without another fold. The terminal asks
  for older own-segment windows until the preview fills the screen up from
  its last row, beginning at a turn's start. The first frame draws the
  preview. After it, the terminal reads older windows down to the session's
  `seq 0` and each ancestor named by `forked_from` to the chain root, carrying
  ancestor runs above the preview for the joint total. These count reads
  never rebuild the displayed preview. A run that reaches `seq 0` short of
  the display crosses into its ancestors before the first frame instead. The
  terminal then raises the subscription to `full`.
  The pass goes over the session's lines as the `full` subscription sends
  them. It builds the panel's cards and counts every row, keeping each
  row's `seq`. No event is kept: each is applied to the panel's folds and
  dropped. The pass renders no page for display.
  The terminal opens a pass on a `full` acknowledgement with `last_seq`,
  or on `stream_reopened` with `last_seq` (`docs/events.md`). It ends the
  pass when it folds that `seq`. No `last_seq` means no pass is open, and
  an empty log means the first frame draws no history.
  A pass of any session the terminal holds at `full`, an open item's
  included, blocks `history`. Until it ends, the conversation on screen
  doesn't change: a frame draws the pages already held, and the panel
  shows the cards folded so far. The scroll bar appears when the pass
  ends. Scrolling, recounting at a new width, search and copy wait.
  The one exception: rows that depend on turns before the held pages (an
  orphaned job's completion, an automatic handoff's trigger, a ledger
  toggled earlier: only rows of these kinds) may change in the frame where
  the pass ends. An opening that fills only after crossing `seq 0` draws
  through the chain root, so the landing rewrites nothing across the seam.
  If the own segment fills first, the count phase adds ancestors above the
  preview without refolding its visible pages. On one segment the preview
  holds the filled run and the pass counts the rest, so the landing adds the
  rows above it.
  EOF, `stream_closed`, a failed read, a disconnect and lowering
  to `summary` end the pass too. The feed's `session_left` for a session
  ends that session's pass. Waiting work then runs against what is held,
  or fails as usual with no connection.
  If `stream_reopened` arrives during a `history` wait, the wait ends at
  once. A `history` wait with no answer after 10 seconds ends as a lost
  connection, and the terminal reconnects as it does after a disconnect.
  Stashed lines fold in order before the new fold, whose lines are
  never stashed. The late answer is discarded by its id, and the page
  loads in a frame after the pass ends.
- **A rewound or moved session pages across its `forked_from` chain as one history,**
  cut at each continuation point. The terminal reads `forked_from` from the
  session's `session_started`, asks `history` with `session_id` set to that
  ancestor and `to_seq` set to its `seq`, pages that session the same way,
  and repeats for each further ancestor. Pages, row counts, search and the
  opening pass treat the chain as one history. The terminal never reads a
  log from disk. `land` carries the preview's closed ancestor pages across
  the swap the way it carries open items; a dropped ancestor page loads
  again with `history` under its `session_id`, and the fold still replays
  the own log only. Ancestor rows are counted by the same `history`
  streaming before the scroll bar appears, never from the subscription:
  "until the screen is full" governs the display phase on its segment,
  and past a segment start the count phase keeps streaming to the chain
  root.
- **Pages are cut inside turns,** at `step_started`, about 64 lines each, and
  never inside a tool group, so each page renders on its own.
- **The window** is the pages on screen plus one screen of rows above and
  below. Every other page is dropped.
- **Row counts are exact,** measured in the opening pass and again when the
  width changes. Every row has a stable index, which the scroll bar, search
  matches and the selection share.
- **Pages load inside the frame** that needs them. There is no background
  loading.
- **Queued inputs share a frame by kind.** Queued hub lines and ticks share
  one frame in arrival order, up to 4,096 inputs, so a stream that never
  empties still draws. A run of queued terminal reads shares one frame, so a
  run of wheel steps draws once. A key, click or resize draws its own frame at
  once, so click targets always come from a frame the person saw, and a key
  that swaps the screen does not carry the next key's work.
- **Search streams the session's lines** with `history`, rendering each page to text and keeping only its
  matches. On a large session it does not run on every keystroke: it waits for
  a pause in typing of a quarter of a second, or runs off the frame thread.
- **A selection's ends are row indices.** Copying reads the rows between them,
  rendering again any page dropped since the drag began.

Measured in the prototype on macOS arm64, on a session of 6,135 lines and
1,047 tool calls (4.4 MiB), built to match the longest of the owner's sessions
([research/tui-prototype/PAGING.md](../research/tui-prototype/PAGING.md),
[LARGE.md](../research/tui-prototype/LARGE.md)), and in Fiber by the `paging`
jig (`docs/testing.md`, "Jigs") on macOS arm64 (Apple M3 Pro), release build,
on a session it generates in the same shape: 10 turns, 965 steps with text and
1,051 tool calls, 8,074 lines, 4.7 MiB with the running turn below. Fiber's
session has more lines than the prototype's, because Fiber writes
`step_started` and `text_completed` lines the prototype's fixture does not
have. Its screen is 160 by 48; the session draws 2,915 rows in 120 pages.
Fiber's figures are the median of three runs.

| What | Prototype | Fiber |
|---|---|---|
| Peak footprint, whole log folded against paged | 36.7 MiB against 6.6 MiB | 11.2 MiB paged, of which 4.7 MiB is the jig's copy of the session, standing in for the hub's log |
| Panel pass at open | 18.7 ms | 55.0 ms, the opening pass and the first frame |
| Counting rows | about 4 ms per MiB of log | in the opening pass; 12.1 ms to count every page again at a new width |
| Slowest frame that loaded pages | 5.1 ms | 1.1 ms |
| Search of the whole log | 19.6 ms | 188.3 ms |

Estimated row counts moved the scroll bar's thumb by up to 17 cells in one row,
which is why counts are exact. The timings do not carry over to Linux; the
row, page and match counts do.

Also measured in Fiber, on the same machine:

- **Appending to the last page while a turn runs.** A running turn of 30
  steps, 481 lines with its deltas, fed one line a frame: the slowest frame
  took 0.85 ms, and at most 5 pages held cards throughout.
- **A session ten times larger:** 80,709 lines, 10,505 tool calls, 46 MiB.
  The opening pass and the first frame took 580 ms, the slowest frame that
  loaded pages 0.62 ms, and counting every page again at a new width 133 ms.
  Peak footprint was 61.9 MiB, of which 46 MiB is the jig's copy of the
  session.
- **Jumping the way dragging the scroll bar's thumb does.** The scroll bar's
  thumb does not drag, so the jig moves the top row to 20 rows spread across
  the session, one frame each. The slowest frame took 1.9 ms, and 1.1 ms on the
  larger session.

On Linux x86_64 the benchmark job (`docs/performance.md`, "Measuring", the
`paging` rows) runs the jig on every pull request. Its run of 7 October 2026
(GitHub Actions run 37700922577) gave, as the median of 5 runs: the opening
pass and first frame 673 ms, the slowest frame that loaded pages 3.0 ms, the
slowest jump frame 5.4 ms, counting every page again at a new width 61 ms, and
the slowest append frame 1.4 ms. The jig's session had 8,285 lines, 10 turns
and 1,051 tool calls, and the counts match macOS: 2,915 rows in 120 pages.
Peak resident memory was 8,796 KiB, the jig's copy of the session included.

## Extension seams

An extension may change everything the terminal draws. It does this through a
TUI extension (`docs/extensions.md`, "Commands and screens"): Lua 5.4 scripts
that run in the terminal's process. Settled by
[TUI extension seams](https://github.com/aakshintala/fiber/issues/163).

The terminal is built from named slots. Each slot has a built-in renderer in
Rust, and an extension may replace it or add slots of its own. The machinery
around the slots stays the terminal's: paging, scrolling, selection, search and
the panel's scrolling. So every extension keeps what "History and paging" and
"Reading and copying" promise. A program that wants to draw the whole screen
itself is a separate client of the hub, not a TUI extension.

### The slots

| Slot | What it draws |
|---|---|
| `layout` | Where each region goes: the conversation, the panel, the working line, the steering queue, the input box, the status rows, and any region an extension adds |
| `home` | Home |
| `turn_card` | A turn's card and its ▣ line |
| `prompt` | Only a person's prompt bubble: a non-driver message never calls it |
| `reply` | An assistant reply |
| `group_line` | A tool group's summary line |
| `ledger_row:<tool>` | A ledger row for the named tool |
| `ledger_row` | A ledger row for any tool without its own |
| `thinking_line` | A thinking line |
| `steer` | A steering message in its card |
| `answers` | The "you answered" rule after a question form |
| `handoff_band` | A handoff's band |
| `error_line` | A failed turn's ✗ line |
| `notice` | A notice |
| `card:<name>` | A panel card: `session`, `changed_files`, `delegates`, `jobs`, or one an extension adds. `card:quota` is not a slot: the Quota card stays built-in, and an extension reads quota through `status_rows` |
| `working_line` | The working line |
| `steering_queue` | The queued steering messages |
| `input_box` | The input box |
| `status_rows` | The narrow layout's status rows |
| `view:<name>` | A swapped view, built in or one an extension adds |
| `overlay:<name>` | An overlay an extension adds, drawn over the conversation |
| `approval` | The approval panel, draw-only |
| `question_form` | The question form, draw-only |
| `repository_offer` | The offer of what a repository ships, draw-only |

A slot receives what its built-in renderer receives. Most of it comes from the
event stream, except whether the row is open, which the person's clicks and
keys set, and notices the terminal raises itself, such as a configuration or
I/O failure. Chrome slots may also read outside the stream: the branch comes
from `git status`, the cards follow the panel's configuration, and `spend`
falls back to the attached spend when the session reports none. A ledger row, for example, receives the call's name,
arguments, status, exit code, duration, line count, `changes` and error, folded
from its `tool_call_*` events. No slot sees the terminal's internals. A ledger row's renderer draws what follows the step's six-column gutter and receives the width less 6; a renderer that returns no lines draws no row.

Each slot's input holds exactly what its built-in renderer reads. Every input listed here also carries `flat`, which is true in screen-reader mode when the terminal drops every style. Timestamps come as source values, never as elapsed strings. The person's own interaction state (selection, open or closed) is included only where the built-in draws it. An optional key is absent when it does not apply, never `null`. The terminal's internals stay out: no layout, width fit, cache, spinner glyph or motion clock.

A replacement owns its rows whole: where the built-in row carries one target over the row, each replacement line keeps it. A target placed on part of a row goes away when the extension owns those rows. A `notice` keeps its notice target on every replacement line and its dismiss target spans those painted lines. A `notice` replacement with more lines than the notice rows left keeps its first lines in order and drops the rest, as a line too wide is cut; the rows it paints are the rows it takes, and its targets span only those. An `error_line` keeps its failed-login target on each line. A `steer` replacement drops image-line targets because it owns those lines. A `prompt` keeps the target for a single image on each replacement line and drops targets for several images. A writing `handoff_band` has no target, and `reply`, `turn_card` and `answers` remain without targets. A `steering_queue` replacement drops the per-message edit and drop targets, and a card replacement drops the panel targets that differ from row to row; keys still do everything the dropped targets did. The working line keeps its interrupt target, and a card keeps the one target shared by every row it draws, on each replacement line.

**`turn_card`** draws a turn's card whole: its prompts, every entry, and its
ending. A replaced card is drawn whole by the extension, so no nested slot
runs inside it; `prompt` draws only when the card itself is built in.

| Field | What it holds |
|---|---|
| `prompts` | The turn's person's prompts in order — only `Origin::Driver` messages — each with the `prompt` slot's `text` and `images` (`path`, `width`, `height` in pixels) |
| `entries` | The card's items in order, each tagged with a `kind` and carrying the slot input shown below |
| `started` | When the turn started |
| `calls` | How many model calls the turn made |
| `usage` | The turn's totals: `tokens` (`input`, `cache_read`, `cache_write`, `output`), `cost`, `subscription_cost` |
| `ended` | How the turn ended: `outcome`, `ended_ts`, and on a failure its `error` (`code`, `message`); absent while the turn runs |
| `running` | Whether the turn is still running |

The entries carry these slot inputs and extra fields:

| Kind | Fields |
|---|---|
| `reply` | The `reply` input: `text` (Markdown included) |
| `steer` | The `steer` input: `text`, `images`, `ts` |
| `message` | `source` (`fiber`, `session` or `extension`), `label`, `text`, `images`, `ts` |
| `group` | The `group_line` input, plus `cut` and `sections` |
| `aside` | No slot: `{kind = "line", text}`, `{kind = "warning", message}`, or `{kind = "orphans", jobs, open}`; each job has `name` and `message` |
| `band` | The `handoff_band` input: `trigger`, `ts`, `state`; a `done` state carries `before` and optional `after` context sizes |
| `answers` | The `answers` input: `outcome`, `parts`, `note?`, `ts?`; each part has `header`, `text`, `skipped` |

A session message's `label` is the sender's current non-empty session name,
when one is known, else its session id. An extension message's label is its
extension name. An unknown `kind` is skipped.

**`prompt`** draws only a person's prompt bubble: a non-driver message never calls it, and goes in `entries` as `message` instead.

| Field | What it holds |
|---|---|
| `text` | The prompt's text |
| `images` | Its images in order, each `path`, `width` and `height` in pixels |

**`reply`** draws an assistant reply.

| Field | What it holds |
|---|---|
| `text` | The reply's text, Markdown included |

**`thinking_line`** draws a thinking line.

| Field | What it holds |
|---|---|
| `text` | The thought's text |
| `started` | When the thought started |
| `ended` | When it ended; absent while it streams |
| `open` | Whether the thought is open |

An empty `thinking_line` result draws no row and no opened body.

**`group_line`** draws a tool group's summary line.

| Field | What it holds |
|---|---|
| `running` | Whether the turn runs and this is its open group |
| `calls` | The group's calls in order, each with the call fields a card's group entry carries |
| `streaming` | The calls the model is still emitting, each with `message`, `index`, `name` and `text` |
| `thoughts` | The group's thinking blocks, each with `text`, `started`, `ended` and `open` |
| `failed` | The retried model calls, each with its code and attempt |
| `first` | The earliest timestamp the group spans |
| `last` | The latest timestamp the group spans |
| `open` | Whether the group's ledger is open |

**`steer`** draws a steering message.

| Field | What it holds |
|---|---|
| `text` | The message's text |
| `images` | Its images in order, each `path`, `width` and `height` in pixels |
| `ts` | When it landed |

**`answers`** draws what a form left: the answers or the decline.

| Field | What it holds |
|---|---|
| `outcome` | `given` or `declined` |
| `parts` | Each question's part, in field order: its `header`, `text`, and whether `skipped` |
| `note` | The note on the whole form, when one was left |
| `ts` | When the form was resolved; absent when declined |

**`handoff_band`** draws a handoff's band, without its note.

| Field | What it holds |
|---|---|
| `trigger` | What the handoff runs at |
| `ts` | The band's timestamp |
| `state` | `writing`, `done`, or `unchanged` with `cancelled`; a `done` state carries `before` and optional `after` context sizes |

**`error_line`** draws a turn's failure line.

| Field | What it holds |
|---|---|
| `code` | The failure's code |
| `message` | Fiber's own sentence |
| `provider` | When present, the provider's `name`, optional HTTP `status`, and `message` |

For example, for a failed login, clicking any line an extension returns for this row opens the login view.

**`notice`** draws a notice, in the conversation and on home. The
terminal's own notices carry the same fields: a configuration or I/O
failure raises the same input an extension draws.

| Field | What it holds |
|---|---|
| `code` | The notice's registry code |
| `text` | The notice's message |

The `extension_failed` switched-off notice always draws built-in: a
notice extension cannot hide its own switch-off failure.

**`working_line`** draws the working line. It reads the attached session,
even with a delegate transcript on screen.

| Field | What it holds |
|---|---|
| `started` | When the turn started, as the turn card's value; absent before the turn starts |
| `retry` | The pending retry: `code`, `attempt`, `last_attempt`, `delay_ms` and `ts`; absent otherwise |

A replacement that returns no lines hides the working line. The built-in
retry line carries no interrupt target but Esc still interrupts; a
replacement keeps one interrupt target over each line's full width either
way.

**`steering_queue`** draws the queued steering messages: its heading, the
queue rows and the footer. A replacement that returns no lines hides all
three.

| Field | What it holds |
|---|---|
| `messages` | The queued messages oldest-first, each with `text`, `images` (`path`, `width` and `height` in pixels), `droppable` (sent while steering), `selectable` (droppable with no image) and `selected` (the person's ⌥↑/⌥↓ choice) |

Nothing draws and no renderer runs on an empty queue.

**`status_rows`** draws the narrow layout's status rows.

| Field | What it holds |
|---|---|
| `cards` | The cards the narrow layout shows, in order |
| `session` | The same input `card:session` draws |
| `changed_files` | `files`, every changed file in the order the built-in ranks them |
| `delegates_running` | How many delegates run |
| `jobs_running` | How many jobs run |
| `widgets` | The extension widgets on screen, each with `extension`, `widget` and `lines` |
| `waiting` | How many sessions wait; absent at zero |
| `quota` | The `quota` answer (`docs/events.md`, "Payload types"): an array ordered by provider name, then credential label, each with `provider`, `credential` and `windows` or `credit` or `unreachable`, plus `notice_at`; absent when the built-in draws no Quota segment |

**`card:<name>`** draws one panel card through its slot: `session`,
`changed_files`, `delegates`, `jobs`, or one an extension adds. A widget's
narrow row and a running delegate's narrow row draw through the same slot
with the same input. Its narrow form caps what the built-in shows, as the
five-file cut does, and the replacement inherits the cap.

`card:session` draws the session card:

| Field | What it holds |
|---|---|
| `workspace` | The workspace's path |
| `branch` | The branch from `git status`; absent when unknown |
| `model` | The model |
| `thinking` | The thinking level |
| `context` | `tokens`, `window` and `trigger_at` |
| `spend` | `cost` with `tokens` (`input`, `output`, `cache_read`, `cache_write`) and `subscription_cost`; when the session reports none, the attached spend falls back to `cost` alone; absent when there is no spend at all |
| `budget` | The spend budget; absent when unset |
| `speed` | The last reply's output speed in tokens per second; absent before the first timed reply |
| `turns` | How many turns the session ran |
| `tools_down` | The tools that are down |
| `waiting` | How many sessions wait; absent at zero |

`card:changed_files` draws the changed-files card:

| Field | What it holds |
|---|---|
| `files` | Every changed file in the order the built-in ranks them, each with `path`, `added` and `removed` |

`card:delegates` draws the delegates card:

| Field | What it holds |
|---|---|
| `delegates` | Running delegates in the order they started first, then finished ones in the order they started, each with `job_id`, `description`, `harness`, `model`, `calls`, `step` and `started`; a finished one also carries `outcome` and `ended` |

`card:jobs` draws the jobs card:

| Field | What it holds |
|---|---|
| `jobs` | The jobs, each with `job_id` and `description` |
| `open` | Whether the person opened the jobs list |

`card:<extension>/<widget>` draws an extension widget's card:

| Field | What it holds |
|---|---|
| `extension` | The extension's name |
| `widget` | The widget's name |
| `lines` | The widget's lines |
| `open` | Whether the person opened the widget |

`layout` places regions and never draws them. It receives the screen's size
and returns a rectangle for each region it shows. A region it leaves out is not
shown. A layout does its own narrow layout and shedding. The "Fiber needs
40×10" floor stays the terminal's.

The three draw-only slots decide how an approval, a question form or a
repository's offer looks, never what it sends. An extension may restyle and
reorder their choices. It cannot add, remove or relabel one, and its key
handler never sees their keys. The choices, their keys and the `reply` they
send stay the terminal's, so an extension never approves a tool call
(`docs/extensions.md`, "Host calls").

A TUI extension also binds keys to its own commands or to built-in actions, and
adds slash commands, which join the one list ("Slash commands"). A theme stays
a file that sets colours only ("Themes").

### What a renderer returns

A renderer receives the slot's input and the width, and returns lines. Each
line is a list of spans:

```lua
{ text = " exit 0 ", fg = "success", bg = "surface", bold = true,
  dim = false, italic = false, underline = false,
  link = "https://…", click = "badge" }
```

- `text` is required. Everything else is optional.
- `fg` and `bg` name one of the theme's colour roles, such as `success`,
  `muted` or `accent`. There are no hex colours and no escape codes, so every
  extension follows the theme, a light or dark switch and `NO_COLOR`. The
  roles are listed with the themes and belong to the extension API.
- `link` makes the span a link, opened on click as "Links" says.
- `click` makes the span's cells a click target with that id ("Input and
  focus").

The terminal builds the cells and the plain text from the same spans, so what
is drawn and what is selected, copied or searched cannot differ. It measures
width by grapheme cluster and cuts or pads each line to the width, so an
extension cannot overflow its slot or move its neighbours. A span whose text
holds a control character is the wrong shape. So is a `link` whose scheme is not `http`, `https` or `mailto`, and an `fg` or `bg` that is not a colour role.

In the screen-reader mode ("Screen readers") the input carries `flat = true`.
The terminal drops every style, and a renderer should return one line for the
item.

A renderer may call `builtin(input, width)`, which returns the built-in
renderer's spans. An extension that only adds a badge to a row decorates the
built-in rather than drawing the row again.

### How a TUI extension runs

- **From the terminal's own Fiber home only.** A TUI extension belongs to the
  client. A repository's packages bring session halves only, and their TUI
  extensions never load (`docs/extensions.md`, "Client halves").
- **One VM per extension, for every session.** Every callback and event
  handler receives the `session_id` it is for, and the extension keys its own
  state by it. Timers belong to the extension, not to a session. `on_focus`
  runs with the `session_id` when the session on screen changes, and
  whole-screen slots (`layout`, `input_box`, keys, slash commands) receive the
  session on screen.
- **Only for a session that loaded its session half,** at a version inside
  the range the TUI extension declares. For any other session it draws
  nothing and the built-in rendering stands, with one notice naming the
  extension and both versions. A later `extensions_loaded` switches it on or
  off without a restart.
- **On the terminal's thread.** Each TUI extension has its own Lua VM there.
  Its event handlers and timers run as coroutines, and a host call suspends
  the coroutine until the reply arrives through the terminal's event loop, as
  on the session side. Between callbacks the VM is idle, so a renderer is
  always called at once and answers at once. A renderer makes no host call.
- **Every callback declares its timeout.** Renderers, key and click handlers,
  the layout, event handlers, timers and loading alike, as on the session side
  (`docs/extensions.md`, "How an extension runs"). The same two-stage
  instruction hook enforces it.
- **Loading comes before the first frame.** A replaced layout or input box
  changes the first frame, so the terminal loads its TUI extensions before
  drawing it ("Performance"). Reading, compiling and running the entry script share the loading timeout, as on the session side. An extension that passes it is switched off, and the frame goes ahead without it.
- **The renderer is a trait the terminal defines.** `tui` defines a
  `Renderer` trait, and `main` adapts the `extensions` Lua VM to it, so `tui`
  depends on no crate that holds the VM (`docs/architecture.md`, "The call
  rules"). `tui` tests use a fake renderer.
- **Each drawn item is cached** by its input and the width. Its spans name
  colour roles, which the theme resolves as the frame is written, so a theme
  change runs no renderer. A renderer runs again only when its input, the width,
  or an on-screen animated item's or notice's frame changes ("Animation"), so an
  unchanged frame calls no Lua: one call per cache miss or frame advance, none on a hit.
  The cache lives with the paging window and is dropped with its pages.
  The key also includes the page's session and index, the selected extension,
  and its session-half version when it has one. A change to any of these
  reruns the renderer; an unrelated session-half change does not. Failures
  use the same key without width.
- A range-bound client half stays inert without notice until the session's
  first `extensions_loaded`. An omitted or out-of-range half stays inert
  with one `extension_incompatible` notice per TUI extension and session-half
  version until `/reload`; omission counts as `not loaded`. The notice names
  the TUI version and the session-half version, or `not loaded`.
  A client half with no session half, such as a layout or a ledger row for a built-in tool, draws for every session.
- **Home has no session.** Its notices reach extensions without one: the
  input carries no `session_id` key (Lua reads nil), never another
  session's id and never an empty one. A client-only half draws for every
  home notice; a range-bound half stays inert without notice until the
  session's first `extensions_loaded`, as on a session it has not seen.
- **A TUI extension binds whole-screen keys with `fiber.key`.**
  `fiber.key(name, { timeout, run })` runs `run` for the key when nothing
  built in should, and `fiber.key(name, { action, variant })` dispatches a
  built-in action through its checked canonical event for that variant
  (`docs/extensions.md`, "Whole-screen keys"). `run` receives exactly
  `{ session_id = "…" }` and returns `true` when it handled the key;
  anything else falls through to the built-in key map. Only `true`
  handles: `false` and `nil` do not, and any other value is the usual
  wrong-shape failure. A timeout switches the extension off until
  `/reload`, exactly as a still call does. A client-only extension stays
  compatible before the first `extensions_loaded`, with no session half at
  all.

Measured in the prototype on macOS arm64, on a session of 1,047 tool calls
([research/tui-prototype/SEAMS.md](../research/tui-prototype/SEAMS.md)): a
Lua ledger row costs about 8 µs to draw, more than half of it crossing between
Rust and Lua. With the cache, scrolling took 0.63% of a core against 0.59% for
built-in rows, and Lua ran zero times while scrolling or searching. Without
it, every rebuild of the conversation would call Lua for every row. The timings
do not carry over to Linux; the call counts do.

### Input and focus

One slot has focus at a time: the input box, an open view, or the overlay on
top. An extension's view or overlay takes focus when it opens.

- **Keys** reach the focused slot first, as names such as `ctrl+o`, `alt+up`
  or `shift+enter`, and typed text as text. A paste is one event. The handler
  returns whether it handled the key. A key it did not handle goes to the key
  map as if no extension were there.
- **Whole-screen bindings act before the built-in key map.** An eligible
  `run` binding runs before every built-in early path; an action binding
  runs before them where the Bindings table lists it for the context on
  screen ("Bindings"). Neither kind acts with no session on screen, on a
  job's live PTY view, or for a key the person set in `keys`. A `run`
  binding never acts while an overlay, a picker or search has the keyboard,
  and bare typing never reaches it in the input box: a single character
  with no modifier but Shift, the space bar, and shift+space type instead.
  A handled `run` settles with nothing built in running; drained notices
  settle before the fallthrough, and action events keep their built-in
  settle behavior. Only Ctrl+C reaches the bindings from the `/keys`
  screen, so no screen code skips dispatch: Ctrl+C itself is unregistrable.
- **Esc** closes whatever is on top, unless the focused slot handles it
  itself.
- **The second Ctrl+C always quits** ("Quit"), whatever the focused
  slot does, so a slot that swallows every key cannot trap the person. The
  first Ctrl+C is the slot's.
- **A click** on a span with a `click` id goes to that slot's click handler.
  The wheel, a drag and any click it does not handle go to the terminal, so
  scrolling and selection keep working.

A key that needs the kitty keyboard protocol, such as `shift+enter`, works
once keyboard detection finishes ("Keys", "Rules"). An extension that binds
one also binds a key that works without the protocol, as the built-ins do.
Every click target an extension draws is a focus stop in navigate mode
("Moving through the conversation"), so its widgets need no focus order of
their own.

### Animation

A renderer may declare `animate = { interval_ms }`. For every animated slot — conversation items, notices and chrome — the terminal calls it again when the frame advances, with `frame` in its input. An animated call's input also carries `now`, the time of the frame in Unix milliseconds, so a renderer can draw elapsed time or a countdown. The terminal uses one timer for all motion. It asks for a timer only
while an animated item or notice row was painted in the previous frame.
Intervals below 120 ms use that floor instead.

The terminal calls an item when at least one of its rows was painted in the
previous frame. It may make one due call on the frame the item scrolls off.
After that unpainted frame, it makes no further calls until the item is painted
again. A swapped view that hides the conversation counts as an unpainted
frame; the item resumes at its stored frame when the conversation returns.

Frame 0 is the still form. Under reduced motion, the terminal draws frame 0
once and makes no animation calls. When reduced motion ends, the next paint
resumes at frame 1. A renderer that returns `animate = false` stays stopped
until its input changes. A renderer that declares no `animate` gets no `now`:
it draws from timestamps alone, and its input changes only when the events
do. A width change redraws a stopped item once at its
stored frame, then uses the cached lines for that width. A dropped page starts
again at frame 0. Closed draws such as search and counts do not advance or
schedule animation. They use stored lines when available; on a miss, they may
cache frame 0 without changing animation state.

A notice animates as a conversation item does: frame 0 is still, and later
frames advance only while a replacement row was painted in the prior frame. An
advanced open frame's one result both measures and draws. After fade or
dismissal, a re-raised notice is new and starts at frame 0; its animation
state goes when the notice does. Live notices with identical code and text
have independent frames. The animation key is stable across page changes;
a new per-`Screen` cache starts a live notice at frame 0. A replacement
returning no lines stays hidden until it fades or is pruned, and never advances
or schedules animation while hidden. An event notice's fade and a condition's
re-evaluation are unchanged, so an animation never holds a notice on screen.

An extension's own timers (`host.after`, `host.every`) are for work other than
drawing. They wake the terminal whether or not anything is on screen, which is
the extension author's cost to keep down.

A renderer that errors or returns the wrong shape keeps its built-in fallback
with no further calls until its input changes ("When an extension fails"),
and a timeout switches the extension off as a still one does.

### When an extension fails

- **An error or a wrong shape** falls back to the built-in for that item. The
  failure is cached against the item's input, so the renderer is not called
  again until that input changes. One notice names the extension, the slot
  and the Lua error, once for each extension and slot until `/reload`. A key or click handler that errors
  counts as not having handled the input. An extension's own view or overlay
  has no built-in, so it closes. A key handler that errors or returns the
  wrong shape likewise counts as not handled, falling back to the built-in
  key map with one `hook_failed` notice for each extension and key slot
  until `/reload`; a key handler that passes its timeout switches the
  extension off until `/reload`, with one `extension_failed` notice, as a
  still renderer does.
- **A timeout** switches the extension off in this terminal until `/reload`
  (the terminal command; the `reload` driver command acts on the session
  only), with one notice. Every slot it replaced
  goes back to its built-in, so one freeze of the declared length is the most
  it can cost.
- **A replaced input box that fails** hands over to the built-in box with the
  draft intact, because the terminal holds the draft.

### When two extensions want one slot

Panel cards, views, overlays and slash commands are keyed by the extension's
name, so they stack. A slot that holds one thing conflicts: a ledger row for
one tool, the input box, the layout, or one key.

- **Two extensions replace the same slot:** neither gets it. The built-in
  stays, and a notice names both. `tui.slots."<slot>"` names the one that
  wins (`docs/configuration.md`). Two extensions binding the same key follow
  the same rule, as `tui.slots."key:<key>"`. This is the rule for commands
  (`docs/extensions.md`, "Commands and screens").
  The notice comes once for each slot and set of contenders until `/reload`. A `tui.slots."<slot>"` that names none of them picks no winner, and the notice names the setting.
- **A key the person bound in `keys`** is the person's. An extension that
  binds the same key does not get it, and a notice names the extension. The
  registration stays dormant and reactivates at once when the person binding
  lifts, so a `/keys` change flips the owner per stroke. A key the person
  owns stays theirs in every context, even where the action does not act.

Whole-screen keys canonicalize before they contend, with the same grammar
as `keys` ("Bindings"): modifier order and the case of modifier and key
names never distinguish two spellings, while an uppercase letter reads as
Shift with that letter, so `Ctrl+C` parses as `ctrl+shift+c` and stays
bindable, and plain `ctrl+c` never binds. The first registration to
canonicalize to a key wins; a later one is skipped with one notice naming
the extension. A key with one contender and no setting still selects that
contender. An unparseable key suffix, or a setting naming no registered
contender, selects no winner, with one notice naming the full setting, for
example:

`Invalid key binding setting: tui.slots."key:ctrl+"; the key does not parse; no extension wins.`

A setting that names an extension that does not contend for the key never
silently falls back to the one that does.
- **A per-tool ledger row beats the catch-all** `ledger_row`, with no notice.
- **An extension replacing a built-in** is not a conflict. The extension wins,
  as it does for a tool it registers by name.

## Performance

The terminal holds the budgets in `docs/performance.md`: its idle memory, idle
CPU and time to first frame. The design keeps to them this way:

- **Nothing runs while nothing happens.** With no turn, delegate or job running and
  no animated item or notice on screen, there is no timer: no frame, no byte written (an
  on-screen animated item or notice holds the timer, "Animation"). A still pointer sends nothing under hover.
- **The first frame waits on nothing but the session's last page and loading TUI extensions.** Keyboard detection, the logo's image and session listing each arrive after it. The budget is measured with no TUI extension installed. Attaching reads the last page with `history` before the first frame, and the whole log once after it ("History and paging"). The pass takes about 12 ms per MiB of log on macOS arm64, so the scroll bar of a large session appears later than a small one's.
- **Memory follows the window, not the session,** because history is paged.
- **A frame redraws only the rows that changed.**
- **Each frame is written inside synchronized output, mode 2026.** The first frame and frames drawn on resize are included; a terminal without support ignores the sequences.

## Configuration

The terminal reads these keys (`docs/configuration.md`, "Keys"):

| Key | What it sets |
|---|---|
| `tui.rail.shown` | Whether the rail shows; on by default ("The rail") |
| `tui.rail.width` | The rail's share of the screen's width |
| `tui.panel.width` | The panel's share of the screen's width |
| `tui.panel.cards` | Which cards the panel shows, and their order |
| `tui.theme` | The theme; unset, it follows the terminal's appearance |
| `tui.reduced_motion` | Reduced motion |
| `tui.screen_reader` | Forces the flat screen-reader mode on or off |
| `tui.attention.notification` | The OSC 9 desktop notification |
| `tui.attention.bell` | The bell |
| `tui.attention.title` | The state in the terminal title |
| `tui.hover` | Hover, and mode 1003 |
| `tui.inline_images` | Inline images |
| `tui.logo_glyph` | ⌇ or ≈ in the logo |
| `tui.slots."<slot>"` | Which extension fills a slot or key two extensions want ("When two extensions want one slot") |

The skills view writes `skills.disabled`. The tools view writes an MCP
server's or an extension's `tools.enabled` and `tools.disabled`.

## Evidence

- [research/tui-surface/](../research/tui-surface/): the four toolkits
  probed. ratatui with crossterm is the only one with zero idle wakeups, at
  1.9 MiB peak footprint on macOS arm64, against 11 to 89 MiB for Bubble Tea,
  OpenTUI and Textual.
- [research/tui-prototype/README.md](../research/tui-prototype/README.md): a
  throwaway ratatui program that replays a fixture session in a real terminal.
  It measures the glimmer's and hover's cost, keyboard detection, and holds
  the prototype's key map.
- [research/tui-prototype/LARGE.md](../research/tui-prototype/LARGE.md): the
  prototype on sessions sized from the owner's usage.
- [research/tui-prototype/PAGING.md](../research/tui-prototype/PAGING.md): the
  paging probe.
- [research/tui-prototype/SEAMS.md](../research/tui-prototype/SEAMS.md): a
  Lua renderer for one tool's ledger row, its cost with and without the cache,
  and the span shape that kept selection, copy and search working.
- [research/tui-prototype/CHECK.md](../research/tui-prototype/CHECK.md): the
  checks run by eye in Ghostty.
- [research/tui-prototype/README.md](../research/tui-prototype/README.md),
  "The rail (#692)": the rail's prototype. The owner compared a list, cards
  and tabs, then card heights, in Ghostty, and chose the cards above (#692).

## Related

- The event stream: `docs/events.md`, settled by
  [What is the event stream, and what is durable?](https://github.com/aakshintala/fiber/issues/6)
- The front doors and driver commands: `docs/invocation.md`
- Handoff: `docs/handoff.md`
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
