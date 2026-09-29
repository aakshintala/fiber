# The terminal

What Fiber looks like at the terminal, and how a person works it. This is what
is true now, not a plan. It is settled by
[TUI: scrollback or full screen?](https://github.com/aakshintala/fiber/issues/15);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Session, turn, step, step boundary, action, tool
call, job, delegate, handoff, steering message, client and driver mean what it
says there and nothing else. The events the terminal reads are
`docs/events.md`; the commands it sends are `docs/invocation.md`, "Driver
commands".

## What the terminal is

- **The terminal is Fiber's only first-party client.** A GUI is a separate
  project that drives `fiber serve`. Fiber serves no web UI.
- **It is written in Rust with ratatui and crossterm,** in the `fiber` binary,
  as the `tui` crate (`docs/architecture.md`). It prefers existing crates to
  writing its own. Their memory is recorded but is not a veto; idle CPU, the
  first-frame budget and supply-chain checks stay strict
  (`docs/dependencies.md`, "Performance" below).
- **It reads the event stream and nothing else.** It is its own process and a
  client of its session (`docs/invocation.md`, "Processes"). When a screen
  needs data the stream does not carry, the stream changes; the terminal has no
  other path to state.
- **It is full screen only.** There is no scrollback mode. The mouse is a
  primary input, and every action also has a keyboard or slash-command path
  ("Keys").
- **History is paged from the session log.** The terminal holds a window of
  rendered history and reads the log again as the person scrolls ("History and
  paging").

What a TUI extension may draw, and through which seam, is
[TUI extension seams](https://github.com/aakshintala/fiber/issues/163). Which
language TUI extensions are written in is
[Epic: TUI](https://github.com/aakshintala/fiber/issues/82); it need not be
Lua.

## The start page

`fiber` opens on a start page, drawn full screen with no panel. It is centred.
From the top:

1. the logo
2. one dim line naming the new session, the workspace and the branch
3. any approval the repository needs ("Approving a repository's extensions")
4. a large input box, whose bottom row carries the model, effort and mode
   chips and "enter starts <session>"
5. under the box, at its width, the sessions waiting on the person and the
   three most recent, one row each; each is one click from attaching or
   resuming

A key hint sits at the foot. Until the first prompt, the input box's
placeholder says "/? for shortcuts". Typing a prompt starts the new session.

The first frame draws at once. The session lists fill in when listing returns
(`docs/performance.md`).

A failed attach at start says why on the start page: the session is held, or
its schema is too new.

### The logo

The logo is `⌇ fiber 0.0.1`: the ⌇ in the accent colour, the name in the
accent gradient, the version dim.

- **On the start page it is four rows tall,** in pixel letters drawn with half
  blocks, with each letter's counter shaded, as opencode draws its logo.
- **Where the terminal speaks an image protocol** (kitty, iTerm2 or Sixel), an
  image replaces the pixel letters in exactly the same cells: a smooth wave
  and the name set in JetBrains Mono ExtraBold, in the gradient. Detection
  never delays the first frame. The pixel logo draws first, and the image
  replaces it when the terminal's reply arrives.
- **The image is an alpha mask built from the font at build time,** tinted
  with the theme's accents at run time. Fiber bundles no font, and the logo
  follows any theme.
- **Where the screen is too short for four rows,** the logo is one row:
  `⌇ fiber 0.0.1`. Fiber cannot tell whether the terminal's font has ⌇, so
  `tui.logo_glyph` switches it to ≈ ("Configuration").

### Approving a repository's extensions

Extensions and MCP servers a repository brings are approved on the start page,
before the first prompt, so approving costs no prompt-cache rebuild. A swapped
view shows the content `docs/extensions.md` fixes, with three choices:
approve, skip for this session, or never. Typing a prompt first means skip for
now.

### The session list

`/resume`, a bare `fiber --resume`, or a click on the Session card opens the
session list. It shows the project's sessions newest first, two rows each:

- the first prompt, or the session's name when it has one
- the id, the turn count, the branch and a note, such as
  "approval: shell cargo publish --dry-run", "question: 2 of 3 answered",
  "no client; 2 jobs running" or "continues s_2b8e11 from seq 812"

A glyph gives the state: ● running, ◐ waiting on the person, ✗ stopped without
exiting, ○ exited. The right-hand column says what Enter does: attach, resume,
or "cannot attach" for a session whose `schema_version` this terminal cannot
read.

### On exit

The terminal is restored and one line is printed: the session id and the
command to resume it.

## Layout

The screen is a conversation column on the left and a side panel on the
right. The panel is always shown while the screen is wide enough. It replaces
a footer and status line, and the input box spans only the conversation
column.

### The panel

The panel is a column of cards. `tui.panel.cards` lists which cards show and
in what order, and an extension widget (`host.widget`, `docs/extensions.md`)
is a card in the same list. The panel scrolls when its cards outgrow the
screen.

The default cards, in order:

- **Session:** the working directory, with the permission mode as one word
  beside it; the git branch; model, effort and thinking; a context bar that
  fills toward the automatic handoff point, with a marker there; tokens, cache
  hit rate and cost; output speed and turns; and one "tools" line naming any
  MCP server that is down.
- **Changed files:** the five files with the most lines changed, and totals.
- **Delegates:** one card for all of them, two rows each, at most 6 rows
  shown. It scrolls on its own under the mouse wheel.
- **Jobs:** one line saying how many run. A click lists them, one row each.
- **Quota,** from its extension. Quota is not built in.

The context bar is drawn against `preamble_built`'s `context_window` and
`trigger_at`. The permission mode comes from `fiber_started` and
`mode_changed`. Per-file counts come from `tool_call_completed`'s `changes`,
never from `details`, which has no fixed shape a client may rely on
(`docs/events.md`).

Every panel item is clickable and opens its view: the model picker, the
context breakdown, usage, the tools, a delegate's or job's transcript, or a
file's diff. The same views back `/model`, `/context`, `/usage` and `/tools`.

There is no built-in task list
([Task list, and leaving readonly](https://github.com/aakshintala/fiber/issues/56)).
An extension's widget can supply one.

Running tool calls show in the conversation, on the live group's line, not in
the panel.

### Git

The branch and status come from the person's shell command
(`docs/invocation.md`, `shell`): `git --no-optional-locks status` when the
person clicks for it, and `git rev-parse --abbrev-ref HEAD` on attach and
after each turn. Nothing polls.

### Swapped views

A view swaps into the conversation area and takes all of it. Esc returns to
the conversation. The views are:

- **A delegate's or job's view.** Its header is a breadcrumb
  ("main › ◆ review: …"). A status card gives harness, model, calls, elapsed
  time and session, with a stop target. The transcript uses the same turn
  cards as the conversation, and the input box sends steering messages to
  that delegate.
- **A job running under a pseudo-terminal** has a live view of its screen. The
  input box types into it as raw keys, as `jobs write` does for the model, so
  the person can finish an interactive step the model started.
- **The model picker:** models by provider with roles marked, effort and
  thinking chips, and the size of the prompt-cache rebuild a switch costs.
- **The context breakdown:** one bar of context by category against the
  handoff point, with the largest tool results.
- **Changed files:** a file list with the chosen file's hunks.
- **The tools view,** `/tools` (`docs/tools.md`, "Seeing the tools"): every
  tool by source, full or deferred, and its approximate size. An MCP server or
  an extension has an on/off switch that writes `tools.enabled` and
  `tools.disabled` (`docs/configuration.md`). A change takes effect on
  reload, and the view says what the reload's cache rebuild costs. Built-in
  tools have no switch: they are always declared, so every session sends the
  same tool list, and the view shows them as rows without a switch.
- **`/rules`:** every standing rule by scope, global and then this project.
  Each shows the prefix it allows, when and from which session it was added,
  and a ✕ that revokes it. Revoking deletes the line and applies to the next
  call judged. Ctrl+G opens the rules file (`docs/permissions.md`, "Standing
  rules").
- **`/settings`:** the configuration keys, their effective values and the
  layer each comes from. It edits a key through the same path as
  `fiber config set`, and says when a change needs a reload and what that
  costs. Ctrl+G opens the file.
- **`/skills`:** one row per skill with its name, one-line description, where
  it comes from (global, this project, or an extension by name) and whether
  the model can see it. Enter shows the skill's text, Ctrl+G opens its file.
  Which fields a skill has, and whether one can be switched off here, is the
  Skills item of [Map: designing Fiber](https://github.com/aakshintala/fiber/issues/1).
- **`/rewind`** ("Rewind").
- **An extension's approval** on the start page.

### The working line

The working line ("Working 11m 14s · esc to interrupt") sits at the bottom of
the conversation, above the steering queue, not in the input box.

A glimmer runs across its word: a band three cells wide, in the spinner's
colour, sweeps left to right, then rests. The spinners on a running group's
line and on running delegates spin on the same tick, about every 120 ms. While
a turn runs they cost nothing extra, since the frame is drawn anyway. While
only delegates or jobs run, the tick keeps running for them. Under reduced
motion the word and the spinners stay still.

While Fiber waits to retry a failed model call, the working line says so:
"↻ Retrying in 4s · rate_limited · attempt 2 of 4".

### The narrow layout

The conversation has a minimum width. Below it the panel goes away, and rows
under the conversation take its place, as pi-rig lays them out. From the top:

1. the working line
2. the running delegates, up to 4 rows, while the conversation keeps at least
   half the screen; otherwise they collapse into the status line
3. the steering queue
4. an extension widget row, collapsed to one line, which a click expands
5. the input box
6. the status line, two rows under the input box

Each card contributes its segments to the status line in the configured order,
and each row is cut at the terminal's right edge, as pi-rig's footer is. The
second row opens with "N delegates running" and "N jobs running", so they are
never cut; each opens a list of delegate or job cards in the conversation
area. Clicking any other segment opens its card's view.

### Shedding

Below the narrow layout, the screen sheds in this order: the panel, then the
status rows, then the working line's detail. Below a floor of about 40 by 10
cells it shows one centred line, "Fiber needs 40×10 · now 32×8". The session
keeps running, nothing is lost, and the screen redraws on resize.

### A dropped connection

A banner replaces the working line: "Connection lost · reconnecting
(attempt 2)…". The terminal retries with backoff while the conversation and
the draft stay. If the session exited meanwhile, the banner says so and
offers resume.

## The conversation

### Turns

- **One card per turn.** The prompt that starts a turn floats above its card,
  as a tinted bubble on the right, at most about 70% of the width, with the
  time under it. A steering message sits inside the card where it landed, as a
  labelled rule ("steer · 14:15") over bold text. A ▣ line closes the card
  with the outcome, duration and call count.
- **Replies lead, tool groups sit back.** Nothing inside the card is indented:
  replies, summary lines, steering messages and answers all start at the
  card's edge. A tool group has no band of its own, and its summary line is dim
  throughout, the dot and the +/− counts included. Replies carry the full text
  colour.
- **A streaming reply renders in place,** formatted as it arrives. The
  conversation follows new output and pauses when the person scrolls up. While
  scrolled up, a small overlay centred at the bottom of the conversation reads
  "↓ New messages below" and jumps to the end on a click or End. It gives no
  count, so it needs no row count. Nothing is added to the input box.

### Tool groups and the ledger

- **A group is everything between two pieces of assistant text,** thinking
  included. Collapsed, it is one line: what was done by kind ("Read 34 files,
  searched 84 patterns, edited 24 files +175 −83, ran 9 commands"), how many
  times the model thought, and how long it took. While it runs, the line shows
  the calls in flight. The counts of changed lines come from
  `tool_call_completed`'s `changes`.
- **Clicking the line, or Ctrl+O, opens the ledger:** one row per call, split
  by step, with each step's number in the gutter and its thinking line first.
  A step is opened by `step_started` (`docs/events.md`, "Session and turn").
- **Clicking a row opens that call:** its diff, output or error.
- **Failed calls get no special treatment.** They are ordinary rows with their
  status. Nothing is hoisted above the fold.

### Thinking

Thinking belongs to the group it starts, since a group runs from one reply to
the next. The summary line counts it ("thought once") and, while it streams,
ends with "Thinking: <latest heading>". Thinking with no tool call before the
next reply is one dim line, "+ Thought: <first heading> · 22s", and clicking it
shows the text.

### Steering

Enter during a turn sends `steer`. There is no follow-up queue: a prompt that
arrives mid-turn is still rejected `busy` (`docs/invocation.md`). A draft left
in the input box during a turn is untouched until sent.

Queued steering messages sit above the input box, one row each, from the
`steering_queue` event, so every attached client sees and edits the same
queue. ⌥↑ and ⌥↓ select a row and load it into the input box, Enter sends
`steer_amend`, and ⌥X sends `steer_drop`. Each also has a mouse target.

### Interrupts

Esc interrupts the turn when nothing is open ("Keys"). The interrupt shows only
on the ▣ line that closes the card; there is no separate "Interrupted" line. A
call it stopped reads `cancelled` in its ledger row.

### Handoff

A handoff breaks the turn's card (`docs/handoff.md`). The card ends where the
handoff ran, and a tinted band carries:

- the trigger: "automatic at 400k", "you asked with /handoff", "the request
  did not fit" or "the model handed off"
- the context size before and after ("402k → 32k"); the size after comes from
  the first request after the handoff, so it reads "…" until that returns
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

## Approvals and questions

Approvals and questions waiting at the same time share one queue, "1 of N" in
the order they arrived, each labelled with the session asking.

### An approval

An approval request is a panel at the bottom that replaces the input box
(`docs/permissions.md`).

- The choices are: allow once; allow and add a rule, with the prefix shown, as
  a separate choice; and deny, with optional feedback. Typing while the panel is
  open goes to the feedback.
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
  Space toggles a multi-choice option, and the question ends with a `Next →`
  row, `Review →` on the last question, that moves on.
- "Chat about this", and Esc, decline the form and end the turn, so the person
  can answer in their own words. The terminal sends `reply` with `declined`,
  then `cancel`: the call completes `declined`, and the cancel ends the turn.
- Once answered, the answers sit in the turn's card as a "you answered" rule,
  like a steering message, one row per question, with `skipped` and the note
  shown. The call's ledger row reads "answered" or "declined", and the group
  line counts "asked 4 questions".

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
section expands it.

Search is Ctrl+F, and Cmd+F where the terminal forwards it. Ghostty binds
`super+f=start_search` by default; the line that frees it is:

```
keybind = super+f=unbind
```

How search reads a large session is "History and paging".

### Links

Mouse capture turns off the terminal's own link detection, so links are
marked with OSC 8 or handled on click.

## Keys

### Rules

- **Detection never blocks the first frame.** The terminal sends kitty's
  keyboard flags query followed by a primary device attributes query, and
  draws at once. Bindings that need the kitty protocol switch on when the
  reply arrives. crossterm parses both replies but hands them only to a call
  that blocks for up to 2 seconds, so Fiber reads terminal input itself
  ([fiber-zig#16](https://github.com/aakshintala/fiber-zig/issues/16)).
- **Every action has a legacy path:** a legacy key, a mouse target or a slash
  command.
- **Esc closes whatever is on top,** and interrupts the turn only when nothing
  is open. In a question form, Esc means "Chat about this".
- **Slash commands.** Typing `/` opens one completion panel above the input
  box. Commands and skills share one list, filtered as the person types. Each
  row is a name, a one-line description and a tag: command, skill, or the
  extension's name. Tab completes and Enter runs.
- **File search.** Typing `@` opens a file search panel, and choosing a file
  inserts its path as text. The model reads the file itself if it needs it.
  The search runs on demand, each keystroke cancels the last, and it covers
  the files git tracks, so a huge repository costs one listing, never a walk of
  the tree.

### The input box

- Enter sends. Shift+Enter inserts a line break, with Ctrl+J as the legacy
  path. Pasted text keeps its line breaks. The box grows to about a third of
  the screen, then scrolls.
- ↑ in an empty box recalls earlier prompts: from this session, then the
  project's earlier sessions, newest first. Ctrl+R searches them. They are
  read from the logs, one session at a time as the person steps back, because
  `docs/state.md` allows no history file. In a draft of several lines, ↑ moves
  the cursor until it reaches the first line.
- A paste over about 10 lines shows as one token, "[Pasted text #1 · 312
  lines]", and the full text is sent. Clicking the token, or Ctrl+G with the
  cursor on it, opens it in the editor.
- Ctrl+V with an image saves it to the session's `artifacts/` and shows
  "[Image #1]". The prompt carries its path, and the model looks with `read`,
  which returns images.
- `!cmd` runs a shell command and sends its output with the next prompt.
  `!!cmd` runs it and shows the output only to the person (`shell` with
  `send` false, `docs/invocation.md`).

### Bindings

| Action | Key | Other paths |
|---|---|---|
| Send a prompt, or a steering message during a turn | Enter | |
| Insert a line break | Shift+Enter | Ctrl+J |
| Close what is on top; interrupt the turn when nothing is open | Esc | |
| Clear the draft, then quit | Ctrl+C, twice within about a second on an empty box | `/quit` |
| Detach | Ctrl+D, twice on an empty box | `/detach` |
| Cycle the permission mode | Shift+Tab | click the mode on the Session card; `/mode <name>` |
| Recall an earlier prompt | ↑ in an empty box | |
| Search earlier prompts | Ctrl+R | |
| Move by word | ⌥← ⌥→, Ctrl+← Ctrl+→ | |
| Delete a word | ⌥Backspace | |
| Start or end of the line | ⌘← ⌘→, where the terminal passes them | |
| Open the draft, or a pasted token, in `$VISUAL` or `$EDITOR` | Ctrl+G | click the token |
| Paste an image | Ctrl+V | |
| Open or close the ledgers | Ctrl+O | click a group's line |
| Search | Ctrl+F; Cmd+F where forwarded | |
| Jump to the end | End | click "↓ New messages below" |
| Select a queued steering message | ⌥↑ ⌥↓ | its mouse target |
| Amend it | Enter | |
| Drop it | ⌥X | its mouse target |
| Reopen a request put aside, or move to the next | | `/approvals`; click the badge |
| Open the key map | | `/?` or `/help` |

The key map, `/?` or `/help`, is an overlay over the conversation listing every
binding by area with its legacy path, as codex's shortcut overlay does. Esc
closes it.

### Slash commands

| Command | What it does |
|---|---|
| `/resume` | Opens the session list. |
| `/model` | Opens the model picker. |
| `/context` | Opens the context breakdown. |
| `/usage` | Opens the usage view. |
| `/tools` | Opens the tools view. |
| `/rules` | Opens the standing rules. |
| `/settings` | Opens the configuration keys. |
| `/skills` | Opens the skills. |
| `/rewind` | Opens the rewind view. |
| `/handoff [instructions]` | Starts a handoff (`docs/handoff.md`, "A person"). |
| `/name <text>` | Names the session. |
| `/mode <name>` | Sets the permission mode. |
| `/login` | Logs in ("Logging in"). |
| `/approvals` | Reopens the waiting approvals and questions. |
| `/quit` | Quits ("Quit and detach"). |
| `/detach` | Detaches. |
| `/?`, `/help` | Opens the key map. |

An extension's commands appear in the same list, tagged with the extension's
name, and run with the `command` driver command (`docs/extensions.md`,
"Commands").

### Logging in

`/login` logs in from the terminal with the same flow as `fiber login`. It
lists providers and extension credentials. An API key goes in a hidden field
in a bottom panel. OAuth opens the browser and also shows the URL to copy, for
SSH.

### Quit and detach

Quit and detach are separate actions.

- **Ctrl+C clears, then quits.** It clears a draft in the input box. With the
  box empty, it says "Press Ctrl+C again to quit", and a second press within
  about a second quits. It never interrupts a turn on its own; Esc does that.
- **Quit** is a second Ctrl+C, or `/quit`. It stops everything: it sends
  `cancel`, stops each running job and delegate, then sends `close`, which ends
  the session for every client.
- **Detach** is a second Ctrl+D on an empty input box, or `/detach`. It closes
  the connection, and the session goes on under the lifecycle rules
  (`docs/invocation.md`, "Lifecycle"): it finishes its turn and jobs, then
  exits if no client is left.
- **When another client is attached, quit asks first:** "Another client is
  connected. Quit closes the session for it too · enter quit · d detach · esc
  stay". The `clients` event says how many are attached.

### The permission mode

Shift+Tab cycles the permission mode (`docs/permissions.md`, "Modes"). The new
mode shows for a moment on the working line and the Session card. Clicking the
mode on the Session card opens a picker.

### Getting the person's attention

When a session starts waiting on the person, for an approval, a question or a
finished turn, the terminal:

- sends an OSC 9 desktop notification where the terminal supports one
  (Ghostty, iTerm2, kitty, WezTerm), and a bell elsewhere
- shows the state in the terminal title, such as "◐ fiber · approval"

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

## Look

- **Surfaces, not lines.** The person's messages, each tool group, the input
  box, the approval panel and each card sit on their own background tint, with
  half-block edges (▄ above, ▀ below) and no borders.
- **A stripe marks state** on the side its surface is anchored to: ▌ on the
  left for a steering message, a running or finished job and an approval; ▐ on
  the right for the person's prompt bubble. The stripe is one unbroken bar,
  because ▌ and ▐ fill half of each cell as Ghostty draws them. Where a
  terminal cannot draw it unbroken, there is no stripe.
- **Colours come from the theme,** in truecolour where the terminal has it. A
  256-colour theme uses the grey ramp for its tints.
- **Markdown in replies:** headings in the theme's heading colour; code blocks
  on a darker tint with syntax colours, line numbers, a language label and a
  click-to-copy target; tables with a rule under the header and numbers
  right-aligned; bullets in the accent colour.

### Themes

Fiber ships a dark and a light theme. A person adds their own as files in
Fiber home and picks one in `/settings`, which writes `tui.theme`. With no
theme set, the theme follows the terminal's light or dark appearance and
switches when the terminal reports a change, as pi's `light/dark` setting
does.

A theme sets colours only. Anything bigger goes through the extension seams
([TUI extension seams](https://github.com/aakshintala/fiber/issues/163)).

### Reduced motion

Every animation has a still form. `tui.reduced_motion` turns reduced motion
on, and it is on whenever a screen reader is detected.

### Screen readers

Fiber detects VoiceOver on macOS and AT-SPI on Linux at start, as codex does,
and draws flat:

- no tints, half-block edges, stripes or animation
- one line per item, in reading order
- state in words rather than glyphs
- images as their one line, never inline

`--screen-reader` and `tui.screen_reader` force the flat mode on or off, as
Claude Code's `--ax-screen-reader` does.

### Images

Where the terminal speaks kitty's graphics protocol (Ghostty, kitty, WezTerm),
images show inline: an image a tool returned, in its ledger row, and a pasted
image, in the prompt bubble.

- An image is at most 40% of the conversation's width and 12 rows tall,
  keeping its proportions. A click opens it in the system viewer.
- The terminal holds the image and moves it with the text, through kitty's
  Unicode placeholders. Fiber keeps no decoded image and sends nothing again
  on a scroll.
- An image part in the log carries its width and height, so paging counts its
  rows without decoding it.

Elsewhere an image is one clickable line, `▣ screenshot.png · 1280×800`.
iTerm2's protocol and Sixel make the client redraw an image on every scroll,
so only the logo, drawn once, uses them. `tui.inline_images` turns inline
images off.

## History and paging

The terminal holds a window of rendered history and reads the log again as the
person scrolls, with range reads by `seq` over an offset table
(`docs/events.md`, "Resume").

- **Opening is one streaming pass** over the log. It builds the offset table,
  builds the panel's cards, and counts every row. No event is kept: each is
  applied to the panel's folds and dropped.
- **Pages are cut inside turns,** at `assistant_message_started`, about 64
  lines each, and never inside a tool group, so each page renders on its own.
- **The window** is the pages on screen plus one screen of rows above and
  below. Every other page is dropped.
- **Row counts are exact,** measured in the opening pass and again when the
  width changes. Every row has a stable index, which the scroll bar, search
  matches and the selection share.
- **Pages load inside the frame** that needs them. There is no background
  loading.
- **Search streams the log,** rendering each page to text and keeping only its
  matches. On a large session it does not run on every keystroke: it waits for
  a pause in typing, or runs off the frame thread.
- **A selection's ends are row indices.** Copying reads the rows between them,
  rendering again any page dropped since the drag began.

Measured in the prototype on macOS arm64, on a session of 6,135 lines and
1,047 tool calls (4.4 MiB), built to match the longest of the owner's sessions
([research/tui-prototype/PAGING.md](../research/tui-prototype/PAGING.md),
[LARGE.md](../research/tui-prototype/LARGE.md)):

| What | Measured |
|---|---|
| Peak footprint, whole log folded against paged | 36.7 MiB against 6.6 MiB |
| Panel pass at open | 18.7 ms |
| Counting rows | about 4 ms per MiB of log |
| Slowest frame that loaded pages | 5.1 ms |
| Search of the whole log | 19.6 ms |

Estimated row counts moved the scroll bar's thumb by up to 17 cells in one row,
which is why counts are exact. The timings do not carry over to Linux; the
row, page and match counts do.

Not yet measured:

- appending to the last page while a turn runs
- Linux
- sessions much larger than the one above
- dragging the scroll bar's thumb

## Performance

The terminal holds the budgets in `docs/performance.md`: its idle memory, idle
CPU and time to first frame. The design keeps to them this way:

- **Nothing runs while nothing happens.** With no turn, delegate or job
  running, there is no timer: no frame, no byte written. A still pointer sends
  nothing under hover.
- **The first frame waits on nothing.** Keyboard detection, the logo's image
  and session listing each arrive after it.
- **Memory follows the window, not the session,** because history is paged.
- **A frame redraws only the rows that changed.**

## Configuration

The terminal reads these keys (`docs/configuration.md`, "Keys"):

| Key | What it sets |
|---|---|
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

The tools view writes `tools.enabled` and `tools.disabled`.

## Not settled here

- What a TUI extension may draw:
  [TUI extension seams](https://github.com/aakshintala/fiber/issues/163)
- The command envelope, `reply`, approvals and cancel:
  [Contract: the command envelope, reply, approvals and cancel](https://github.com/aakshintala/fiber/issues/181)
- Key tables for every event payload:
  [Contract: key tables for every event payload](https://github.com/aakshintala/fiber/issues/182)
- What a skill is to Fiber: the Skills item of
  [Map: designing Fiber](https://github.com/aakshintala/fiber/issues/1)

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
- [research/tui-prototype/CHECK.md](../research/tui-prototype/CHECK.md): the
  checks run by eye in Ghostty.

## Related

- The event stream: `docs/events.md`, settled by
  [What is the event stream, and what is durable?](https://github.com/aakshintala/fiber/issues/6)
- The front doors and driver commands: `docs/invocation.md`
- Handoff: `docs/handoff.md`
