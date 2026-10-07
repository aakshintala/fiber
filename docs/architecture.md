# The modules

The modules Fiber is made of, and the rules about which may call which. This is
what is true now, not a plan. It is settled by
[What modules exist, and who may call whom?](https://github.com/aakshintala/fiber/issues/7);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Watcher, driver, participant, seam, hook, session,
turn, event and tool call mean what it says there and nothing else.

How Fiber is started, the commands a driver may send, and which process runs
what are `docs/invocation.md`.

## Three kinds of participant

Everything that touches a running session is one of three things.

- **Watcher** — reads events, cannot reply. The terminal's rendering,
  `fiber ask`'s stdout, a future GUI, a log shipper. A watcher may be
  absent, slow, or added later, and nothing about the session changes.
- **Driver** — sends commands in. The terminal's input handling, a GUI, a
  parent session driving its delegate. A driver may only send commands Fiber defines.
- **Participant** — is asked during a turn and may refuse or change what
  happens. Tools, providers and extension hooks. A participant's answer
  changes the turn.

Map premise 5 ("Fiber is a set of modules with defined API semantics and no
cross-module reach. The TUI is an ordinary consumer of the event stream, with
no privilege a second GUI client would not have.") governs watchers and
drivers absolutely. Participants are a deliberate, named exception whose
entire surface is the three seams below.

An extension can be all three. Its watchers are watchers, the driver commands
it sends are a driver's, and only its tools, providers and hooks are
participants (`docs/extensions.md`).

**The terminal is a watcher and a driver, never a participant.** When the loop
needs an answer from a human it emits a request and waits for a reply
command. The terminal is one possible answerer; a calling harness is another,
answering identically. The TUI reads the same event stream as every other
client, through the hub (`docs/invocation.md`, "Processes"), so it holds no
private path to state; anything it renders exists in this contract, as an
ephemeral event where it is display-only.

## The modules

| Module | Job |
|---|---|
| `contract` | The vocabulary every other module speaks: what an event looks like, what a command looks like, and what a tool, a provider and a hook must each be able to do. It contains no behaviour at all. |
| `log` | Owns the session directory. The only thing that opens `events.jsonl`, holds the lock, mints `seq` and decides fsync order. Also hands events to whoever is watching. |
| `loop` | Runs turns and steps (`docs/loop.md`). The only thing that decides what happens next. |
| `provider` | Talks to model APIs: wire formats, credentials, streaming. Reached only through the provider seam. |
| `tools` | Runs the built-in tools that act on the workspace and the session: shell, file edits, search, web fetch, `ask_user`, session messaging and `tool_search`. Reached only through the tool seam. |
| `mcp` | The MCP client: both transports, OAuth, the cached tool lists and `mcp_resources` (`docs/mcp.md`), and `fiber mcp serve`. Reached only through the tool seam. |
| `jobs` | Background jobs and delegates: starting, watching and stopping them, the runner for delegates on another harness, and their worktrees, through `worktree` (`docs/delegates.md`). Reached only through the tool seam. |
| `worktree` | Creates and removes the git worktrees sessions and delegates run in, by running the `git` program (`docs/invocation.md`, "Isolation"). |
| `extensions` | Loads extension code, hosts the runtime, and wires what extensions register into the three seams. |
| `tui` | Draws the terminal, in its own process, as a client of the hub. Watches events, sends commands, knows nothing else. |
| `config` | Reads the configuration files in [Fiber home](state.md) and the repository's `.fiber/` ([Configuration](configuration.md)). Answers questions; never asks any. |
| `hub` | Lists, starts and resumes sessions and relays every client connection to a session's socket, over its local socket and, when installed with a port, a websocket on `127.0.0.1` that authenticates each device by token (`docs/invocation.md`, "The hub"). Holds no session and no push credential; does no TLS. |
| `doors` | `fiber ask` (argv or stdin in, JSON lines out), and the internal session command that it, the hub and a parent run. Which doors exist and what a driver may send is `docs/invocation.md`; this page only fixes that none has a privilege the TUI lacks. |
| `picture` | The image child (`docs/invocation.md`, "Processes"): decodes, refuses, fits and re-encodes one image under the limits in `docs/model-routing.md`, "Image limits". Only `main` depends on it, so no session process links image code. |
| `cli` | Every command that does not run a session: `login`, `logout`, `approve`, `sessions export` and `models` today, and later `sessions delete/search/prune`, `config get/set`, `upgrade` and `hub pair` as they are built. `main` dispatches to it. |
| `main` | The composition root. Parses argv, builds everything once, picks a door. No feature logic. |

### Why contract exists

Everything else points one direction: the loop calling down into providers,
tools and extensions, with nothing calling back up — except that an extension
mid-turn needs to ask Fiber something, such as what is in the session. That is
a call upward, and it is the one thing that would make the dependency graph
circular. Putting the types and the seam definitions in a module that depends
on nothing, and that everything else depends on, breaks that cycle: the
extension talks to `contract`, not to `loop`.

## The call rules

`contract` depends on nothing and everything but `picture` depends on it.
`picture` depends on nothing in the workspace. `log` and
`config` depend only on `contract`. `provider`, `tools`, `mcp`, `jobs` and
`extensions` depend on `contract` and never on `loop`, on each other, or on
`tui`. `extensions` also depends on `config`, for what an extension reads
while it runs. `loop`
depends on `contract`, `log` and the three seams, and never on `tui`, `doors`
or `main`. `tui`, `hub` and `doors` depend on `contract` and on `log`'s reading
side, and never on `loop`, `provider`, `tools`, `mcp`, `jobs` or
`extensions`. `cli` depends on `contract`, `log`, `config`, `doors` and
`extensions`, and never on `loop`. `worktree` depends only on `contract`, and
`jobs`, `doors` and `cli` may depend on it. `main` depends on
everything, and nothing depends on `main`.

`fakes` holds the shared fakes that tests and jigs run against
(`docs/testing.md`, "Fakes" and "Jigs"). It is not a module. It depends only
on `contract`. Any crate may take it as a test-only dependency, none takes it
as a normal one, and no release binary contains it.

1. Calls point one way. If A may call B, B may never call A. B answers, or it
   emits an event and A picks it up.
2. `loop` is the only module that decides what happens next.
3. Only `log` opens the session directory, holds the lock, or mints `seq`.
4. `loop` never names a tool, a vendor or a provider. It reasons about what a
   tool is allowed to do, never about which tool it is. An audit of the
   archived Zig tree found about 211 string literals naming
   built-in tools inside `src/core/` production code, plus a core enum listing
   every built-in by name
   ([Core reasons about tool kinds, not builtin names](https://github.com/aakshintala/fiber-zig/issues/138)).
5. Only `config` reads configuration files ([Configuration](configuration.md)).
   `main` distributes what it returns at startup. What an extension reads
   while it runs, such as a secret through `host.secret`, a manifest or the
   cached model list, `extensions` asks `config` for itself.
6. `main` holds no feature logic.

## How the boundaries are enforced

Fiber is a cargo workspace with one crate per module. Each crate's
manifest lists what it may use, so a call the rules above forbid does
not compile — it is a build failure, not a lint finding or a review
comment. The rationale is
[Module boundaries are crate boundaries](adr/0002-module-boundaries-are-crate-boundaries.md);
the measurements are on
[issue #7](https://github.com/aakshintala/fiber/issues/7#issuecomment-5756970986).
This is why `contract` depends on nothing: a cycle between crates is a
build failure, so the module holding the shared types has to sit at the
bottom.

## The three seams

### Tool seam

"run this and give me a result." Fiber's own built-in tools are compiled in
and register through this seam exactly as an extension's would. An extension
registering the same name replaces the built-in, and **the replacement is
recorded in the session log** (`replaced` on `preamble_built`,
`docs/events.md`), so a headless caller, a resumed session and an
audit all see it. The loop asks the registry for a name and runs what comes
back; it never learns whether the answer was Fiber's or an extension's.
An extension runs a tool through the same registry with `host.tool`: the
inner call takes the deny steps and the tool hooks, is logged as a call of
its own, and is never sent to the model as one of its calls
(`docs/extensions.md`, "Running a tool").

### Provider seam

"send this to a model and stream back actions." A provider is an extension
over a native wire protocol; see `docs/model-routing.md` and
[ADR 0007](adr/0007-protocols-are-native-providers-are-extensions.md).
A provider extension's Lua functions, `models()`, `quota()`, `credential()`,
`sign()` and `cost()`, reach the `provider` module through this seam, wired by
`extensions`, so `provider` never depends on `extensions`. `sign()` is the one
called on the request path, and `credential()` only when the cached token has
already expired (`docs/model-routing.md`, "Keys, tokens and OAuth").

### Hook seam

"here is what is about to happen: allow it, change it, or refuse it."
Synchronous: the loop stops, asks, waits and honours the answer, under a
timeout Fiber enforces. The hook points are `docs/extensions.md`, "Hooks". A hook is Lua in the
session's process or a process extension answering over a pipe; either
answers inside the turn, under the hook's timeout: the one it declared, or the extension's
`hook_timeout_ms` when the person set it (`docs/configuration.md`).

## Asking a human

**Anything Fiber itself needs from a human goes through the contract. A
medium-specific surface may only add capability a client is free not to
offer.**

Fiber ships one closed, versioned set of interactions — approval, confirm,
select, multi-select, text input and form — carried on the same
request events the loop uses to ask a human anything, and answerable by any
connected client including a headless one. An extension raises the same
interactions, and may also send status and widget lines as data for a client
to show or ignore.

A session never ships drawing code to a client. An extension that draws
carries a TUI extension, or a half for whatever other surface it draws on,
which runs in that client's process and reaches its session only through the
event stream and driver commands (`docs/extensions.md`, "Commands and
screens"). What a TUI extension may draw is the TUI's to settle.

What an approval actually asks about, and what answers it when nobody is at
the keyboard, is `docs/permissions.md`. How the model asks the person a
question, and what happens when a program drives the session, is
`docs/tools.md`, "Asking the person".

## Concurrency

Settled by
[The threading and streaming model](https://github.com/aakshintala/fiber/issues/9);
the rationale and the rejected runtimes are
[ADR 0004](adr/0004-blocking-threads-no-async-runtime.md).

### What a thread is here, and what owning means

A thread is a worker inside the Fiber process. It does one thing at a time and
parks — asleep, costing nothing — until something wakes it. Fiber needs
several because some work blocks: reading a model's answer off the network
takes twenty seconds, and nothing else can happen on that worker meanwhile.

**Owning** a piece of state means exactly one worker may touch it, and
everyone else asks that worker. It is the whole of Fiber's concurrency
discipline: the rules below say who owns what, and nothing else is shared.

Fiber uses blocking threads and no async runtime.

### The threads

| Thread | Owns | Lives |
|---|---|---|
| loop | the turn: what happens next, and every durable event | the session |
| two per client | one reads its commands and answers them, one writes its events | the connection |
| one per running tool call | that call's subprocess and its output | the call |
| signals | the process's SIGTERM, SIGINT and SIGHUP, and end of file on a delegate's lifeline; it starts a shutdown (`docs/invocation.md`, "Shutdown") | the process |
| accept | the session's socket: it accepts each connection and starts that client's two threads | the session |
| status | `session_status`: it folds the log as it is written into the session's status line (`docs/events.md`) | the session |
| `fiber ask`'s printer | `ask`'s stdout, as a watcher | the run |
| one per blocking wait | one wait and nothing else: a command's exit, a web fetch's deadline, one Lua host call's HTTP request, a background refresh, the shutdown bound | the wait |

A client is a reader and a writer on the session's socket, and every client
is the same code. `fiber ask`'s stdout is not a client: it only receives, so
it is a watcher, and `ask` spends its stdin on the prompt.

A session process runs one session. A Fiber delegate is a child process of
its parent, and its parent is an ordinary client over its socket. The
delegate's stdin is a lifeline its parent never writes to; end of file on it
starts a shutdown (`docs/delegates.md`).

An image child is a short-lived process, one per image: `fiber` run again
with an internal command, not a door (`docs/invocation.md`, "Processes").
The session process runs no image code.

The terminal runs in a separate process (`docs/invocation.md`,
"Processes") with two threads of its own: terminal input, which owns the
keyboard, and terminal render, which owns the screen. Neither is in the
session's process.

`log` is not a thread. It is a shared object behind a lock: whoever emits an
event calls it, and it mints `seq`, writes, fsyncs and fans out.

Background jobs, delegates, MCP servers and process extensions each add one
parked thread per blocking pipe or wait. Each Lua extension in use adds one thread,
which blocks on that extension's inbox (`docs/extensions.md`, "How an
extension runs"). That is affordable: 512 parked threads measured 11.6 MiB RSS
and 0.35 ms of CPU over ten seconds on macOS arm64.

### One inbox

Everything that wants the loop's attention between steps sends to one
queue: a driver's commands and news from a background job. A step waits on
its own tool calls directly ("Tool calls in a step"). The loop blocks on
that queue when it is idle, which is why an idle Fiber costs nothing.

The loop drains the queue **at step boundaries** — between one round-trip to
the model and the next. It does not drain it while a model response is
streaming, and it does not need to: cancellation does not travel through the
queue, and a steering message applies at the next step boundary anyway.
A budget reached by a delegate's spend is acted on outside the queue too: the
thread reading that delegate stops the running delegates at once
(`docs/loop.md`, "Spending budget").

A driver command that writes no durable event is not queued either. The
thread reading the client's connection handles it at once, even while a
model response streams: `subscribe`, `history`, `background`, `job_stop`,
`shell` with `send` false, and `tools`.
`job_stop` stops the job at once, and its `job_completed` is logged at the
next step boundary. Every other command waits for the drain.

### Streaming

The loop reads the model's response itself, on its own thread, blocking. There
is no separate reader thread and no parser task: during a model response the
loop has nothing else to decide.

As fragments arrive it emits them as ephemeral events and keeps reading.
Watchers receive events on bounded channels. **When a watcher falls behind,
ephemeral events are dropped and durable ones are not** — a lagging watcher
re-reads what it missed from the log by `seq`, which `docs/events.md` already
guarantees is possible. The loop never blocks on a watcher, so a slow screen
cannot stall a turn.

**Only the loop thread emits durable events.** Any thread may emit ephemeral
ones through an emitter handle defined in `contract`. This keeps `seq` minted
in one place and keeps the fsync ordering rule in `docs/events.md` — two
fsyncs bracketing each side effect — a property of one thread's sequence of
calls rather than of a race.

### Tool calls in a step

A step may request several tool calls. **Permission decisions are made in
order, before any of them runs**, so a person is never asked about four calls at
once and the reviewer is never asked about a call whose sibling has already
changed the workspace. Once they are all decided, they **run concurrently**,
one thread each, and their results are returned to the model in the order the
model asked for them.

Two tools writing the same file at the same time is the hazard this creates.
`tools` owns a per-path lock that its file-mutating built-ins take, and that is
available to an extension's tool. It is offered, not enforced: an extension
runs with the account's full rights, so an extension that skips it corrupts
its own writes and nothing else, exactly as `docs/permissions.md` already
accepts for misdeclared effects.

### Cancellation

Fiber owns the socket its model requests run on. The HTTP client is ureq
behind a custom connector that keeps the `TcpStream` handle, so a second
thread can close the socket and unblock a read that is stuck inside the
client — measured at 211 µs on macOS arm64.

One press of the cancel key ends the **turn**:

- the model stream stops,
- any in-flight tool call is stopped and then completes as `cancelled` (how
  Fiber stops each kind of tool is `docs/tools.md`),
- a pending approval or question in the turn ends with its resolved line
  before its call completes: `permission_resolved` with `decision: deny` and
  `decided_by: cancel`, or `interaction_resolved` with `by: fiber` and
  `declined: true`, so a client clears its panel as it does on any answer. A
  delegate's pending request is not in the turn and stays pending
  (`docs/delegates.md`, "Lifetime"),
- the turn ends with `turn_completed { outcome: interrupted }`,
- background jobs keep running, because a job outlives the turn that started
  it,
- queued steering messages are kept. Once `turn_completed` is written they
  start the next turn at once, as its input in queue order, so cancelling with
  messages queued means "stop, and read these". To stop with nothing sent, a
  person drops them first.

What the log shows afterwards follows from `docs/events.md` and adds nothing
new: the cancelled calls carry `status: cancelled`, there is no
`assistant_message_completed` for the interrupted response, and the partial
text is gone, because deltas are ephemeral. It is on screen for the rest of
the session and absent after a resume.

A cancelled turn always writes `turn_completed`. A `turn_started` with no
`turn_completed` therefore means the process died, not that someone pressed
escape.

### Steering

A message typed while a turn is running reaches the loop immediately and is
applied at the next step boundary: after the current tool calls finish, before
the next model call. It is a durable event, so the log shows exactly what the
turn received. A message that a turn ends before applying becomes the next
turn's input rather than being dropped.

### Every door

The threading is identical whoever started the session, as map premise 6
requires. The hub, `fiber ask` and a parent all run the internal session
command, so every session process is the same program: a thread per client,
and the loop, the inbox, the streaming, the cancellation and the tool-call
scheduling as above.
A door has no privilege the terminal lacks, and neither has a path to state
that the other does not.

## Not settled here


- Extensions are `docs/extensions.md`
- The tool contract is `docs/tools.md`; the tool set is indexed in
  [Epic: tools](https://github.com/aakshintala/fiber/issues/59)
- Background jobs are `docs/tools.md` ("Background jobs")
- Delegates are `docs/delegates.md`
- MCP is `docs/mcp.md`
- Handoff is `docs/handoff.md`
- Memory is `docs/memory.md`
