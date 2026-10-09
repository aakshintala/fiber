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

`fiber` opens on home, the command center, drawn full screen with no panel and
no rail. It is centred. From the top:

1. the logo
3. a large input box, whose bottom row carries the workspace chip, the new worktree
   switch, and the model and thinking chips and "enter starts a session"
4. under the box, at its width, the session list ("The session list")

A key hint sits at the foot. Until the first prompt, the input box's
placeholder says "/? for shortcuts". Typing a prompt and pressing Enter asks
the hub to start a session in the chosen workspace, and the screen switches to
it. No session exists before Enter, so opening the terminal, glancing at home
and leaving creates nothing. The first turn waits for any MCP server that
starts with the session (`docs/mcp.md`, "Starting servers"), and the working
line says so: "Connecting 2 MCP servers…".

**The workspace chip** defaults to the directory the terminal was launched in.
Clicking it opens a picker of recent workspaces, from `recent.jsonl`
(`docs/state.md`). A remote client has no launch directory, so it always
shows the picker.

**The new worktree switch** sits beside the workspace chip. When it is on,
the session starts in a new worktree of the workspace, by the same rules a
delegate's worktree follows (`docs/invocation.md`, "Isolation"). Outside a git
repository the switch is not shown.

The first frame draws at once. The session list fills in when the hub's feed
arrives (`docs/performance.md`).

A failed attach says why on home: the session is held, or its schema is too
new.

**When a session cannot start,** as on a fresh install with no key, or after
a person removed the provider they chose, home shows the same lines `fiber doctor` prints for each blocker, each
with its fix, above the input box (`docs/invocation.md`, "Commands and flags"). A person
who runs `fiber` before reading anything is told what to do next.

### The logo

The logo is `⌇ fiber 0.0.1`: the ⌇ in the accent colour, the name in the
accent gradient, the version dim.

- **On home it is four rows tall,** in pixel letters drawn with half
  blocks, with each letter's counter shaded.
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
approval.

### The session list

Home lists sessions, live ones first, then recently exited ones, one row
each, growing with the number of live sessions and scrolling past the screen:

- a glyph for the state, as on the rail ("State glyphs"), and ○ for an
  exited session
- the name, or the first prompt when it has none
- what it waits on, such as "approval: shell cargo publish --dry-run" or
  "question: 2 of 3 answered", and its spend
- the workspace's last path segment, for a session outside the launch
  project

Live sessions come from the hub's feed, across every project. Exited sessions
come from `recent.jsonl`, then from the hub's paged query for older ones.
When the terminal was launched inside a git repository, home shows only that
repository's project, every worktree of it, by the `project` on each live session's `session_status` and on each exited session's `recent.jsonl` row, with a line saying "N waiting in
other projects" and a toggle to show everything. Outside a git repository it
shows everything. The rail always shows every project ("The rail").

Clicking a row, or Enter on it, opens the session: an exited one is resumed by
the hub. A ✕ on a live row stops that session ("Quit"). Delete on a
selected exited row deletes that session through the hub, after asking, and
the question names any session `--cascade` would add (`docs/invocation.md`,
"Deleting and pruning"). A row whose
`schema_version` this terminal cannot read says "cannot attach".

### On exit

The terminal is restored and one line is printed per live session: its id and
the command to resume it.

## Layout

The screen is a session rail on the left while two or more sessions are
live, a conversation column, and a side panel on the right. The conversation
column's top row is a header holding the session's name, or its first prompt
when it has none. The panel is shown while the screen is wide enough, unless the person hides it with ⌥P. It replaces
a footer and status line, and the input box spans only the conversation
column.

The rail and the panel each take a share of the screen's width, kept between
a floor and a ceiling: the rail 15%, from 22 to 48 columns, and the panel 21%,
from 30 to 60. Dragging the edge between either and the conversation resizes
it, and saves the new share as `tui.rail.width` or `tui.panel.width`, so one
setting suits a laptop and a 4K screen alike. A drag stops where the
conversation would fall below its minimum width. Each draggable edge shows a
dim grip, `⋮` on three rows at mid-height; under the pointer the edge column
tints and the pointer becomes a resize arrow (OSC 22, where the terminal
supports it). The numbers are starting values, to be tuned once the terminal
is built.

### The rail

The rail lists every live session, across every project, as cards grouped
under project headers. The launch project's group comes first, then the
others in the order their first session started. The rail is drawn from the
hub's feed (`docs/invocation.md`, "The hub") and shows while two or more
sessions are live. Home never shows it.

A project header holds the project's name, a "+" that starts a session in
that project, the summed spend of its live sessions, and "N done", the count
of its exited sessions, which opens home's session list for that project.

Each card is four rows on its own tint, inside ▄ and ▀ edges, with a ▌ stripe
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
3. what it waits on, in the attention colour, while it waits; otherwise its
   git branch
4. its spend, a bar of how full its context is, and the percentage. The bar
   turns the warning colour from 60% and the error colour from 85%.

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
scrolls when its cards outgrow the screen.

The default cards, in order:

- **Session:** the working directory; the git branch; model and thinking level; a context bar that
  fills toward the automatic handoff point, with a marker there; tokens, cache
  hit rate, cost billed and cost on subscription, delegates included, against
  `budget.usd` when one is set; output speed and turns; and one "tools" line naming any
  MCP server that is down. Every number has a plain label that says what it
  measures, such as "tokens in / out", "cache hits" or "output speed, last
  reply", never a bare abbreviation. The handoff marker carries one line saying
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

The context bar is drawn against `preamble_built`'s `context_window` and
`trigger_at`. Per-file counts come from `tool_call_completed`'s `changes`,
never from `details`, which has no fixed shape a client may rely on
(`docs/events.md`).

⌥P shows and hides the panel. Every panel item is clickable and opens its view: the model picker, the
context breakdown, usage, the tools, a delegate's or job's transcript, or a
file's diff. The same views back `/model`, `/context`, `/usage` and `/tools`.

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

A view swaps into the conversation area and takes all of it. Esc returns to
the conversation. The views are:

- **A delegate's or job's view.** Its header is a breadcrumb
  ("main › ◆ review: …"). A status card gives harness, model, calls, elapsed
  time and session, with a stop target. The transcript uses the same turn
  cards as the conversation, and the input box sends steering messages to
  that delegate. A delegate that has not bound its socket yet answers
  `session_not_found`; while its parent lists it running, the terminal asks
  again every 500 ms.
- **A job running under a pseudo-terminal** has a live view of its screen. The
  input box types into it as raw keys, as `jobs write` does for the model, so
  the person can finish an interactive step the model started. The screen is
  80 columns by 24 rows, drawn from the job's output since the terminal
  attached. Carriage return, line feed, backspace, tab, cursor movement and
  erasing in a line or the display are applied; other escape sequences are
  dropped.
- **The running delegates or jobs,** from "N delegates running" or "N jobs
  running" in the narrow layout: the Delegates card's two rows for each
  delegate, or one row for each job, in the order they started. A row opens
  that item's view.
- **The model picker:** models by provider with roles marked, a chip for each
  thinking level the model supports, and the size of the prompt-cache rebuild
  a switch costs. Choosing a model saves the global `model`, and choosing a
  level saves `models."<provider/model>".thinking` to the global file. One key
  marks the choice as this session only, and then nothing is saved. When
  `scoped_models` is set, the picker shows only those models, with a "show
  all" toggle. Ctrl+L opens it, as `/model` does. It draws from the cached
  model lists at once and refreshes stale ones in the background; its refresh
  button refreshes every list (`docs/model-routing.md`, "Model discovery").
- **The usage view,** `/usage`: the session's `usage` (`docs/events.md`) broken
  down by turn, by model and by delegate, each with tokens by kind, cost billed
  and cost on subscription, and the budget left when `budget.usd` is set.
- **The context breakdown:** one bar of context by category against the
  handoff point, with the largest tool results. The categories are the
  system prompt, tool definitions, tool results since the handoff in force,
  and messages, which takes the rest of the session's context total. Each
  of the first three is estimated from its bytes at the session's own
  tokens-per-byte rate, and the view says the sizes are approximate.
  Until a request gives a rate, it shows the total alone.
- **Changed files:** the files Fiber changed, with the chosen file's hunks.
  The hunks are the workspace's current diff against `HEAD`, read when the
  file is chosen, so they include changes Fiber did not make; the title says
  "diff against HEAD". An untracked file shows as wholly added.
- **Search results:** every match with its surrounding lines ("Search").
- **The tools view,** `/tools` (`docs/tools.md`, "Seeing the tools"): every
  tool by source, full or deferred, and its approximate size. Each MCP
  server's or extension's tool has two on/off switches, this project and
  everywhere, that write that source's `tools.enabled` and `tools.disabled`
  in the project's or the global file (`docs/configuration.md`). A change takes effect on
  reload, and the view says what the reload's cache rebuild costs. Built-in
  tools have no switch: they are always declared, so every session sends the
  same tool list, and the view shows them as rows without a switch.
- **`/rules`:** every standing rule by scope, global and then this project.
  Each shows the prefix it allows, when and from which session it was added,
  and a ✕ that revokes it. Revoking deletes the line and applies to the next
  call judged. Ctrl+G opens the rules file (`docs/permissions.md`,
  "Remembering a decision").
- **`/settings`:** the configuration keys, their effective values and the
  layer each comes from. It edits a key through the same path as
  `fiber config set`, and says when a change needs a reload and what that
  costs. Ctrl+G opens the file.
- **`/skills`:** one row per skill with its name, one-line description, where
  it comes from (the repository, personal, an extension by name, or built in),
  whether the model can see it, any skill it shadows, and whether it is
  switched off. A switch turns a skill off or on for this project or
  everywhere, writing `skills.disabled` in that layer; the change reaches the
  model at the next turn start. Enter shows the skill's text, Ctrl+G opens its
  file (`docs/system-prompt.md`, "Skills").
- **`/rewind`** ("Rewind").
- **A repository's offer**, before a new session's first request ("Approving
  what a repository ships").

### The working line

The working line ("Working 11m 14s · esc to interrupt") sits at the bottom of
the conversation, above the steering queue, not in the input box.

A glimmer runs across its word: a band three cells wide, in the spinner's
colour, sweeps left to right, then rests. The spinners on a running group's
line and on running delegates spin on the same tick, about every 120 ms. While
a turn runs they cost nothing extra, since the frame is drawn anyway. While
only delegates run, the tick keeps running for their spinners. Under reduced
motion the word and the spinners stay still.

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
area. Clicking any other segment opens its card's view.

### Shedding

The rail sheds before the panel. When the screen is too narrow for the rail's
floor, the conversation's minimum and the panel, the rail hides, and "N
waiting" joins the Session card, or the status line in the narrow layout; a
click on it shows the rail. A hidden rail leaves its grip at the screen's left
edge, and dragging the grip out shows the rail again, as does ⌥R. Shown while the
width has no room for it, the rail hides the panel, as ⌥P does; ⌥P brings the
panel back and the rail sheds again. A rail hidden
for width returns when the screen grows; one the person hid, by ⌥R or by
dragging it below its floor, stays hidden until shown. Below the narrow layout, which
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

- **One card per turn.** The prompt that starts a turn floats above its card,
  as a tinted bubble on the right, at most about 70% of the width, with the
  time under it. A steering message sits inside the card where it landed, as a
  labelled rule ("steer · 14:15") over bold text. A dim ▣ line closes the card
  with the outcome, duration, call count and the turn's usage, its delegates'
  spend during the turn included: "▣ completed · 38s · 12 calls · 18.2k tokens
  · $0.41 · $1.10 on subscription". A figure that is zero is left out, and a
  model with no price shows tokens only (`docs/model-routing.md`, "Cost").
- **Replies lead, tool groups sit back.** Nothing inside the card is indented:
  replies, summary lines, steering messages and answers all start at the
  card's edge. A tool group has no band of its own, and its summary line is dim
  throughout, the dot and the +/− counts included. Replies carry the full text
  colour.
- **A streaming reply renders in place,** formatted as it arrives. The
  conversation follows new output and pauses when the person scrolls up. The
  mouse wheel over the conversation scrolls it 3 rows a step, a starting
  point, not a measurement; PageUp and PageDown scroll a screen. The
  conversation's last column is its scroll bar and never holds text:
  while the conversation has more rows than it shows, a thumb (█) on a
  track (│) shows where the rows on screen sit among all of them. Either
  pauses following when it scrolls up. When new output arrives while
  scrolled up, a small overlay centred at the bottom of the conversation reads
  "↓ New messages below" and jumps to the end on a click or End. It gives no
  count, so it needs no row count. Nothing is added to the input box.

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
- **Clicking a row opens that call:** its diff, output or error.
- **A call that changed a file is highlighted** in the ledger, with the file's
  name and its lines added and removed, so edits stand out from reads and
  commands.
- **Failed calls get no special treatment.** They are ordinary rows with their
  status. Nothing is hoisted above the fold.

### Thinking

Thinking belongs to the group it starts, since a group runs from one reply to
the next. The summary line counts it ("thought once") and, while it streams,
ends with "Thinking: <latest heading>". Thinking with no tool call before the
next reply is one dim line, "+ Thought: <first heading> · 22s", and clicking it
shows the text. A heading is the first line of the thinking text that is a
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
band says so, with a spinner. A second card continues the turn, and its ▣ line
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
  `error.message`. A server that comes back writes `mcp_server_ready`, which
  clears the Session card's mark in every client.

### Notices

Notices float in the conversation's top-right corner until dismissed with ✕,
under the search box and "Copied", newest on top, so nothing below the
conversation ever moves.

- A notice is at most 40% of the conversation's width, up to 60 columns, and
  wraps to 3 lines ending "… more". Clicking it shows the whole text in an
  overlay.
- At most 3 show, then "+N more", which lists them.
- A notice that arrives during a drag waits for the release.
- In the screen-reader mode notices are lines in reading order.

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
list, the header and the terminal title show it in place of the first prompt.
The model keeps the name current with `name_session` until a person's name
pins it (`docs/tools.md`, "Naming the session").

### Rewind

`/rewind` swaps a view into the conversation area (`docs/events.md`,
"Rewind").

- Each turn is one row, "before: <prompt>". The chosen turn opens into every
  step boundary: after the person's message and after each step, with the
  step's summary. The default is the start of the latest turn.
- For the chosen point it shows the files Fiber's tools wrote since, the calls
  that may have changed files, and that none of it is undone. Each job started
  since that still runs has stop, the default, or adopt. Jobs started before
  the point are listed as adopted with the session.
- An optional summary, then Rewind. The conversation is cut at the point, and a
  ⟲ line names the new session and says what was not restored and which jobs
  stopped or were adopted. The new session waits for a prompt.
- `session_held` shows as the error in place of the Rewind button.
- `summary_failed` shows the reason with three choices: retry, rewind without
  a summary, or cancel.

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

### A question form

An `ask_user` question form is a panel at the bottom that replaces the input
box, as an approval does (`docs/tools.md`, "Asking the person").

- One question shows at a time. Across the top is a tab per question header,
  marked when answered, then a Submit tab that shows every answer and takes a
  note on the whole form.
- Each question shows its options with their descriptions, and a row to
  answer in words. Enter on a single-choice option chooses it and moves on.
  In a multi-choice question, Space toggles the option under the cursor and
  Enter moves on to the next question, or to Submit after the last one. The
  question ends with a `Next →` row, `Review →` on the last question, that
  does the same for the mouse.
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

The other interactions, `confirm`, `select`, `multi_select` and `text_input`, raised by an extension's `host.ask` and by MCP elicitation, are drawn as one-question forms: the same panel with no tab row, the question, then its rows, ending with "Chat about this". `confirm` shows two options, yes and no, and Enter on one replies `confirmed`. `select` shows the options, and Enter on one replies `labels` with that label. `multi_select` shows the options: Space toggles one, and Enter replies `labels`, possibly empty. `text_input` shows only the row to answer in words, and Enter replies `text` as typed. The last two end with a `Submit` row that does what Enter does, for the mouse. Esc, and "Chat about this", decline as on a form.

## Reading and copying

### Selection and copy

Dragging selects conversation text, unwrapped and excluding the panel, and
copies on release. "Copied" shows in the conversation's top-right corner,
below the search box when it is open. The terminal owns Cmd+C (Ghostty binds
`super+c=copy_to_clipboard`), which is why release copies.

The copy goes through OSC 52 and, on a local session, the system clipboard
command as well. tmux's default `set-clipboard external` drops OSC 52
silently, and over SSH only OSC 52 reaches the person's clipboard.

Shift-drag stays the terminal's native selection. A selection's highlight
counts as open, so Esc clears it before it can interrupt the turn.

### Search

Search covers the whole session log, matching rendered text, never raw JSON
lines. It matches the unwrapped text and marks a match across the rows it
wraps onto.

The search bar floats over the conversation's top-right corner, as an
editor's find box does, and the input box keeps its draft. Every match is
marked, the current one brighter, with "3 of 41". A match inside a collapsed
section expands it when it becomes the current match.

Matching is plain text: it folds case per character, matches literally, and
keeps at most 10,000 matches ("10000+"). No model ranks the matches or
interprets the query. Enter or ↓ moves to the next match, Shift+Enter or ↑
to the previous; both wrap.

Ctrl+F a second time, or a click on the "3 of 41" count, opens the search
results as a swapped view ("Swapped views"): every match, one row each, with
the lines around it. Enter, or a click on a row, jumps to that match in the
conversation and closes the view.

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
copies the URL and shows "Copied". A focused link opens with Enter and
copies with `y`.

## Keys

### Rules

- **Detection never blocks the first frame.** The terminal sends kitty's
  keyboard flags query followed by a primary device attributes query, and
  draws at once. Bindings that need the kitty protocol switch on when the
  reply arrives. crossterm parses both replies but hands them only to a call
  that blocks for up to 2 seconds, so Fiber reads terminal input itself
  ([fiber-zig#16](https://github.com/aakshintala/fiber-zig/issues/16)).
- **Every action has a key.** Every action also has a mouse target or a slash
  command, except editing the draft in the input box and search, and choosing
  in the model picker for this session only. A mouse target is drawn only where one fits naturally, never as a
  button added only so the mouse has a way in. A key that needs the kitty
  keyboard protocol also has one that does not, so every action keeps a key
  on any terminal.
- **Esc closes whatever is on top,** returns focus to the input box from the
  conversation, and interrupts the turn only when nothing is open and the
  input box has focus. In a question form, Esc means "Chat about this".
- **Slash commands.** Typing `/` opens one completion panel above the input
  box. Commands, skills, prompt templates and MCP prompts share one list,
  filtered as the person types. Each row is a name, a one-line description, a
  skill's `argument-hint` when it has one, and a tag: command, skill,
  template, the extension's name, or the MCP server's name. Tab completes and
  Enter runs. A
  skill the model has loaded shows in the transcript as a skill, not as a file
  read.
- **File search.** Typing `@` opens a file search panel, and choosing a file
  inserts its path as text. The model reads the file itself if it needs it.
  The search runs on demand, each keystroke cancels the last, and it covers
  the files git tracks, so a huge repository costs one listing, never a walk of
  the tree.

### Moving through the conversation

The input box holds the keyboard until the person moves focus out of it.
Shift+Tab moves focus to the newest item in the conversation.

- ↑ and ↓, or k and j, move focus through turns, tool groups and their
  ledger rows, scrolling the conversation as needed.
- Enter opens the focused item, as a click does: a group's ledger, a call's
  diff or output, a delegate's view.
- y copies the focused item's text. Ctrl+G opens it in `$VISUAL` or
  `$EDITOR`.
- Tab moves focus to the panel, then the rail, then back to the
  conversation.
- Esc returns focus to the input box, keeping the draft.

The focus order is not defined screen by screen. It is every click target on
screen, the same targets hover highlights, and each turn, from top to bottom and left to
right. Every mouse target, an extension widget's included, is therefore a
focus stop with no extra work, and nothing the mouse can reach is out of the
keyboard's reach.

### The input box

- Enter sends. Shift+Enter inserts a line break, with Ctrl+J for terminals
  without the kitty keyboard protocol. Pasted text keeps its line breaks. The box grows to about a third of
  the screen, then scrolls.
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
| Close what is on top; interrupt the turn when nothing is open | `close_or_interrupt` | Esc | click the overlay's ✕ or outside it; click "esc to interrupt" |
| Clear the draft, then quit | `clear_then_quit` | Ctrl+C, twice within about a second on an empty box | `/quit` |
| Go home | `go_home` | ⌥0 | `/home` |
| Start a new session | `new_session` | Ctrl+N | `/new` |
| Switch to the session of rail card N | `rail_row_n` | ⌥1 to ⌥9 | click the card |
| Delete the selected exited session in the session list | `delete_session` | Delete, or Backspace, on the row | click the row's ✕ |
| Recall an earlier prompt from the project of the session on screen | `recall_prompt` | ↑ in an empty box | |
| Search those prompts | `search_prompts` | Ctrl+R | |
| Move by word | `move_word` | ⌥← ⌥→, Ctrl+← Ctrl+→ | |
| Delete a word | `delete_word` | ⌥Backspace | |
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
| Show or hide the rail | `toggle_rail` | ⌥R | drag its edge |
| Search | `search` | Ctrl+F; Cmd+F where forwarded | |
| Open the search results | `search_results` | Ctrl+F with search open | click the match count |
| Next or previous match | `search_next_prev` | Enter or ↓, Shift+Enter or ↑, with search open | |
| Jump to the end | `jump_to_end` | End | click "↓ New messages below" |
| Select a queued steering message | `select_steering` | ⌥↑ ⌥↓ | its mouse target |
| Amend it | `amend_steering` | Enter | its mouse target |
| Drop it | `drop_steering` | ⌥X | its mouse target |
| Reopen a request put aside, or move to the next, the oldest first, switching to its session | `next_request` | ⌥A | `/approvals`; click the badge or a waiting card |
| Open the model picker | `model_picker` | Ctrl+L | `/model` |
| Choose in the model picker for this session only | `session_only` | s | |
| Open the key map | `key_map` | F1 | `/?` or `/help` |

The key map, `/?` or `/help`, is an overlay over the conversation listing every
binding by area with its other paths. Esc closes it. Ctrl+L opens the model picker,
so it does not redraw the screen as it does in some terminal programs.

Every action acts in some of six contexts, and the terminal is in exactly one
of them when a key arrives:

- Overlay: something on top has the keyboard. That is the quit question, the
  home screen's delete question or workspace picker, the key map,
  an approval or question, an offer, or the Ctrl+R panel; or the `/`
  or `@` completion panel while focus is not in the conversation. An overlay's
  own keys, such as an approval's ↑ and ↓, are not bindings.
- Picker: the model picker is open. Only its own keys and the global actions
  act.
- Search: conversation search is open.
- Conversation: focus is in the conversation.
- Steering: a queued steering message is selected.
- Input: the input box has the keyboard, home with nothing open included.

A global action acts in all six. The rest act only where they are listed:

- Global: `close_or_interrupt`, `clear_then_quit`, `rail_row_n`,
  `toggle_ledgers`, `toggle_panel`, `toggle_rail`, `jump_to_end`,
  `next_request`, `key_map`.
- Picker: `session_only`.
- Input, Steering and Conversation: `go_home`, `new_session`,
  `open_in_editor`, `navigate`, `search`, `select_steering`, `drop_steering`,
  `model_picker`.
- Input and Steering: `line_break`, `search_prompts`, `move_word`,
  `delete_word`, `line_start_end`, `paste_image`.
- Input: `send`, `recall_prompt`.
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

`/keys` opens the rebinding screen: every action with its id, description and
current keys, the unbound ones included. Selecting an action and pressing a
key binds it, so only a key the terminal actually delivers can be bound. A key
already bound to another action shows inline, with a choice to swap or
cancel. One key resets an action to its default. The screen saves only the
bindings that differ from the defaults, so a default changed in a later
release still applies.

### Slash commands

| Command | What it does |
|---|---|
| `/home` | Goes home. |
| `/new` | Goes home with the cursor in the input box. |
| `/resume` | Opens home at the session list. |
| `/model` | Opens the model picker. |
| `/thinking [<level>]` | Sets the thinking level for the session's model, saving `models."<model>".thinking`; the default model is unchanged. With no level, opens the model picker on the model's chips: Enter saves the level, `s` applies it to this session only. |
| `/credential <label>` | Switches the session's credential label, saved as the provider's `credential` unless marked as this session only (`docs/model-routing.md`, "Which credential a session uses"). The terminal first says the switch rebuilds the cache, with its size. With no label, it lists the provider's labels. |
| `/scoped-models` | Chooses which models the model picker shows, saved as `scoped_models`. |
| `/context` | Opens the context breakdown. |
| `/usage` | Opens the usage view. |
| `/tools` | Opens the tools view. |
| `/panel` | Shows or hides the panel ("The panel"). |
| `/rules` | Opens the standing rules. |
| `/settings` | Opens the configuration keys. |
| `/keys` | Opens the rebinding screen ("Bindings"). |
| `/skills` | Opens the skills. |
| `/rewind` | Opens the rewind view. |
| `/handoff [instructions]` | Starts a handoff (`docs/handoff.md`, "A person"). |
| `/name <text>` | Names the session. |
| `/login` | Logs in ("Logging in"). |
| `/approvals` | Reopens the waiting approvals and questions. |
| `/reload` | Reloads configuration, MCP servers and extensions (`docs/mcp.md`, "Reload"). |
| `/close` | Stops the session on screen ("Quit"). |
| `/quit` | Quits ("Quit"). |
| `/?`, `/help` | Opens the key map. |

The rest of the list is the session's answer to the `commands` driver command
(`docs/invocation.md`, "What each command does"): its skills, prompt templates
and extension commands, each with the tag the answer gives. The terminal sends
`commands` when it attaches to a session and again after each `reloaded`, and
fills the list from the answer to the latest one it sent; until that answer
arrives, the list holds the commands above only. A skill or prompt template
named like a command above is left out. An extension command whose manifest
names that command in `replaces` takes its row instead, and runs in its place
(`docs/extensions.md`, "What a package holds"). An extension's commands run
with the `command` driver command (`docs/extensions.md`, "Commands and
screens").

### Logging in

`/login` logs in from the terminal with the same flow as `fiber login`. It
lists providers and extension credentials. An API key goes in a hidden field
in a bottom panel. OAuth opens the browser and also shows the URL to copy, for
SSH.

### Quit

- **Ctrl+C clears, then quits.** It clears a draft in the input box. With the
  box empty, it says "Press Ctrl+C again to quit", and a second press within
  about a second quits. It never interrupts a turn on its own; Esc does that.
- **Quit** is a second Ctrl+C, or `/quit`. With no session working, meaning
  no turn, job or delegate running, the terminal exits without asking. With
  some working, it asks: "2 sessions working · enter leave them running · c
  close all · esc stay".
  - Enter, the default, leaves them running. The terminal closes its
    connections, and each session follows the lifecycle rules
    (`docs/invocation.md`, "Lifecycle").
  - "Close all" sends each working session `close` with `now`. The prompt says how many of them are also
    open elsewhere: a session's `clients` on its `session_status`, less the terminal's own `full` connection to it when it holds one.
- **Stopping one session** is `/close`, or the ✕ on its home row. It sends
  `close` with `now`, which ends its turn, stops its jobs and delegates,
  and exits it (`docs/invocation.md`, "Shutdown").

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

Mouse reporting is modes 1000, 1002, 1006 and 1003, every motion. Hover
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
truecolour themes: every report draws the screen in memory and compares it
with the last frame, 93 to 97 µs, and writes nothing unless the target under
the pointer changed; a report that moves along one target, or repeats one
cell, costs the same and writes nothing; each change of target costs one
frame, painted with the theme's colours, 172 to 187 µs and 105 bytes for the
badge; a fast sweep over a conversation whose lines are targets wrote 1,332
frames and 405,992 bytes in 20,000 reports.

Measured in Fiber on Linux x86_64 (6.18.44, a 4-core Intel Xeon at 2.30
GHz) with the same jig, command, size and runs: a still pointer, and a
pointer moving along one target, wrote 1 frame and 86 bytes in 20,000
reports, 115 to 120 µs a report; a change of target on every report wrote
20,000 frames and 1,580,000 bytes, 79 bytes a frame, 271 µs a report; the
fast sweep wrote 1,332 frames and 358,360 bytes, 129 µs a report.

## Look

- **Surfaces, not lines.** The person's messages, each turn's card, the input
  box, the approval panel and each card sit on their own background tint, with
  half-block edges (▄ above, ▀ below) and no borders.
- **A stripe marks state** on the side its surface is anchored to: ▌ on the
  left for a queued steering message, a running or finished job and an
  approval; ▐ on the right for the person's prompt bubble. The stripe is one
  unbroken bar, because ▌ and ▐ fill half of each cell as Ghostty draws them.
  Where a terminal cannot draw it unbroken, there is no stripe. Fiber draws
  stripes in Ghostty, WezTerm and kitty, and not inside tmux or screen;
  elsewhere the stripe's cell keeps its tint.
- **Colours come from the theme,** in truecolour where the terminal has it:
  `COLORTERM` of `truecolor` or `24bit`, or a `TERM` of `xterm-ghostty`,
  `xterm-kitty`, `wezterm` or one ending in `-direct`, which survives SSH where
  `COLORTERM` is usually dropped. Anywhere else the terminal has 256 colours,
  and each colour is the nearest entry of the xterm palette: the neutral tints
  (`background`, `surface`, `surface_raised`, `prompt`, `code`, `hover`,
  `selection`) take the grey ramp, the other tints take the colour cube so they
  keep their hue and an alert stays red, and text takes either. The terminal's
  own 16 colours are never used.
- **`NO_COLOR`** set and not empty turns colour off: every role is the
  terminal's default colour. Bold, dim, reversed and underline stay, so focus,
  selection, matches and state words still show. A half-block edge draws as a blank,
  because with no tint there is no surface to edge, and its row stays.
- **Markdown in replies:** headings in the theme's heading colour; code blocks
  on a darker tint with syntax colours, line numbers, a language label and a
  click-to-copy target; tables with a rule under the header and numbers
  right-aligned; bullets in the accent colour.

### Themes

Fiber ships a dark and a light theme. A person adds their own as files in
Fiber home, an installed extension can carry more, and either is picked in
`/settings`, which writes `tui.theme`. With no
theme set, the theme follows the terminal's light or dark appearance and
switches when the terminal reports a change.

A theme sets colours only. It gives each colour role a value, and an
extension's spans name the same roles ("What a renderer returns"). Anything
bigger goes through "Extension seams".

The roles, in order:

| Role | What it colours |
|---|---|
| `text` | the full text colour: replies, the draft, anything with no role of its own |
| `muted` | what the doc calls dim as a colour: READY, line numbers, rules, block quote bars, grips, the scroll bar, the logo's counters |
| `accent` | bullets, the logo's mark, WORKING, the spinner while a turn works, a running job's stripe, the steering and prompt stripes |
| `heading` | markdown headings |
| `success` | a finished job's stripe when it succeeded |
| `warning` | RETRYING and its spinner, the context bar from 60% |
| `error` | CRASHED, a failed job's stripe, an escalation's stripe, the context bar from 85%, "irreversible" in an approval's header |
| `attention` | NEEDS INPUT, what a card waits on, a standing ask's stripe |
| `added` | lines added: an edited file's `+N`, added lines in a diff |
| `removed` | lines removed: an edited file's `−N`, removed lines in a diff |
| `code_text` | code with no syntax role |
| `keyword` | keywords |
| `string` | string and character literals |
| `comment` | comments |
| `number` | number literals |
| `function` | function and macro names |
| `type` | type names |
| `constant` | constants: `true`, `null`, `ALL_CAPS` names |
| `operator` | operators |
| `background` | every cell no surface covers, including the rail's and the panel's regions |
| `surface` | the input box, cards, notices, the handoff band |
| `surface_raised` | the card on screen, a hovered card, pickers |
| `prompt` | the person's prompt bubble |
| `code` | code blocks and inline code |
| `approval` | the approval panel for a standing ask |
| `alert` | the approval panel for a reviewer's escalation; red in both built-in themes |
| `hover` | the click target under the pointer |
| `selection` | selected text |
| `match` | a search match |
| `match_current` | the current search match |

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

`--screen-reader` and `tui.screen_reader` force the flat mode on or off.

### Images

Where the terminal speaks kitty's graphics protocol (Ghostty, kitty, WezTerm),
images show inline: an image a tool returned, in its ledger row, and a pasted
image, in the prompt bubble.

- An image is at most 40% of the conversation's width and 12 rows tall,
  keeping its proportions. A click opens it in the system viewer, from a
  copy the terminal writes to `cache/images/` in Fiber home
  (`docs/state.md`).
- The terminal holds the image and moves it with the text, through kitty's
  Unicode placeholders. Fiber keeps no decoded image and sends nothing again
  on a scroll.
- An image part in the log carries its width and height, so paging counts its
  rows without decoding it.

Elsewhere an image is one clickable line, `▣ screenshot.png · 1280×800`.
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

- **Opening is one streaming pass** over the session's lines, as its `full`
  subscription sends them. It builds the panel's cards, and counts every row,
  keeping each row's `seq`. No event is kept: each is
  applied to the panel's folds and dropped.
- **Pages are cut inside turns,** at `step_started`, about 64 lines each, and
  never inside a tool group, so each page renders on its own.
- **The window** is the pages on screen plus one screen of rows above and
  below. Every other page is dropped.
- **Row counts are exact,** measured in the opening pass and again when the
  width changes. Every row has a stable index, which the scroll bar, search
  matches and the selection share.
- **Pages load inside the frame** that needs them. There is no background
  loading.
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
| `prompt` | A person's prompt bubble |
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
| `card:<name>` | A panel card: `session`, `changed_files`, `delegates`, `jobs`, or one an extension adds |
| `working_line` | The working line |
| `steering_queue` | The queued steering messages |
| `input_box` | The input box |
| `status_rows` | The narrow layout's status rows |
| `view:<name>` | A swapped view, built in or one an extension adds |
| `overlay:<name>` | An overlay an extension adds, drawn over the conversation |
| `approval` | The approval panel, draw-only |
| `question_form` | The question form, draw-only |
| `repository_offer` | The offer of what a repository ships, draw-only |

A slot receives what its built-in renderer receives, and all of it comes from
the event stream. A ledger row, for example, receives the call's name,
arguments, status, exit code, duration, line count, `changes` and error, folded
from its `tool_call_*` events. No slot sees the terminal's internals.

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
holds a control character is the wrong shape.

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
  drawing it ("Performance"). An extension that passes its loading timeout is
  switched off, and the frame goes ahead without it.
- **Each drawn item is cached** by its input and the width. Its spans name
  colour roles, which the theme resolves as the frame is written, so a theme
  change runs no renderer. A renderer runs again only when its input or the
  width changes, so an unchanged frame
  calls no Lua and scrolling or searching costs what it costs for built-in
  rows. The cache lives with the paging window and is dropped with its pages.

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

A renderer may declare `animate = { interval_ms }`. While its item is on
screen and reduced motion is off, the terminal calls it again every interval
with a `frame` count in its input. It stops when the item scrolls off, its
view closes, or the extension stops it. Under reduced motion the renderer gets
`frame = 0` once and is not called again, so every extension animation has a
still form, as the glimmer does ("The working line"). A hidden animation costs
nothing.

An extension's own timers (`host.after`, `host.every`) are for work other than
drawing. They wake the terminal whether or not anything is on screen, which is
the extension author's cost to keep down.

### When an extension fails

- **An error or a wrong shape** falls back to the built-in for that item. The
  failure is cached against the item's input, so the renderer is not called
  again until that input changes. One notice names the extension, the slot
  and the Lua error, the first time only. A key or click handler that errors
  counts as not having handled the input. An extension's own view or overlay
  has no built-in, so it closes.
- **A timeout** switches the extension off in this terminal until `reload`,
  with one notice. Every slot it replaced goes back to its built-in, so one
  freeze of the declared length is the most it can cost.
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
- **A key the person bound in `keys`** is the person's. An extension that
  binds the same key does not get it, and a notice names the extension.
- **A per-tool ledger row beats the catch-all** `ledger_row`, with no notice.
- **An extension replacing a built-in** is not a conflict. The extension wins,
  as it does for a tool it registers by name.

## Performance

The terminal holds the budgets in `docs/performance.md`: its idle memory, idle
CPU and time to first frame. The design keeps to them this way:

- **Nothing runs while nothing happens.** With no turn, delegate or job
  running, there is no timer: no frame, no byte written. A still pointer sends
  nothing under hover.
- **The first frame waits on nothing but the opening pass and loading TUI
  extensions.** Keyboard detection, the logo's image and session listing each
  arrive after it. The budget is measured with no TUI extension installed.
  Attaching to a session reads its whole log once before the first frame
  ("History and paging"): about 12 ms per MiB of log on macOS arm64, so a
  large session takes longer to open than a new one.
- **Memory follows the window, not the session,** because history is paged.
- **A frame redraws only the rows that changed.**

## Configuration

The terminal reads these keys (`docs/configuration.md`, "Keys"):

| Key | What it sets |
|---|---|
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
