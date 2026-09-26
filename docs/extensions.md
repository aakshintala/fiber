# Extensions

What is true now about Fiber's extension system. Vocabulary is `CONTEXT.md`.
The seams are `docs/architecture.md`; the runtime choice and its reasoning are
[ADR 0006](adr/0006-extension-runtime-lua.md); the trust model is
`docs/permissions.md`. The measurements behind all of it are in
`research/extension-runtime/` and `research/extension-process/`.

## What an extension is

An extension is a package Fiber installs and loads. Its code runs with the
account's full rights. It registers capabilities through the same three seams a
built-in uses — tool, provider and hook — and a registration by an existing
name replaces the built-in, recorded in the session log. An extension that registers a tool named
`read` becomes the `read` tool; the loop never learns whether the answer came
from Fiber or from the extension. It can also register watchers, which learn
what happened without changing it, and commands, which a person or a driver
invokes by name.

An extension is meant to build anything a person wants on top of a coding
agent: a memory system, a connector to an outside service, a goal that keeps
the agent working, a research prototype of a better handoff.

Its code runs in one of two ways, and a package may use both:

- a **Lua extension** runs Lua 5.4 inside the session's own process, at about
  150 KiB
- a **process extension** is a separate program in any language, which the
  session starts and talks to over a pipe

Both register the same things, receive the same calls and have the same host
calls, with the same names and meaning. The only difference is what the
operating system gives a process directly: files, sockets, timers and any
library in its language, npm's included. A package that draws in the terminal
also carries a TUI extension ("Commands and screens").

## What a package holds

An extension is one directory. Its manifest, `extension.json`, states
(`docs/configuration.md`, "An extension's manifest"):

- its name, which is also where it is fetched from (see [Names](#names))
- its version
- the lowest Fiber version it runs on
- the other extensions it depends on, each with a minimum version
- the native binaries it ships, if any, with one download URL and one sha256
  per platform
- for a process extension, the program to run and its arguments, whether the
  session needs it (`required`), and how long it may take to finish when the
  session ends (`exit_timeout_ms`, no default)
- an install step, if it has one, such as `npm ci`

Beside the manifest it may hold:

- data, such as a provider's models and flags
- Lua scripts, as many as it needs
- libraries it vendors: a copy of someone else's Lua code, kept inside the
  extension's own directory
- a process extension's program and its dependencies
- a TUI extension's Lua scripts
- skills, prompt templates and themes
- a prompt file, whose text goes in the system prompt (`docs/system-prompt.md`,
  "Extension texts")

What a skill, a prompt template and a theme are to Fiber is not yet specified.
This page covers only how they arrive.

A script loads another script with `require`. `require` finds files inside the
extension's own directory and nowhere else, so one extension cannot load
another's code by path. Code shared between extensions reaches an extension in
one of two ways: the extension vendors a copy, or it depends on the extension
that holds the code.

A native binary is run as a process extension, or through `host.exec`
("Host calls").

## Lua extensions

Lua extensions are written in **Lua 5.4**, embedded through `mlua` (vendored, so
Fiber's build compiles Lua's C itself and takes no system dependency). The
choice and the alternatives weighed — JavaScript via QuickJS, Luau, Starlark,
WASM, and full TypeScript — are in [ADR 0006](adr/0006-extension-runtime-lua.md).

The embedding is bare. An extension sees a stripped standard library — `table`,
`string`, `math`, `utf8`, `coroutine` — plus a `require` limited to its own
directory, and nothing else. There is no `io`, no `os`, no `package`, no
`debug`. Every capability reaches it through a host-provided global, and the
host calls give it what a process has natively: files, programs, HTTP and
timers. Routing them through the host lets Fiber stop them on cancel and at
shutdown, which it cannot do to a blocking C call inside Lua.

This is not a security boundary. Per `docs/permissions.md`, an extension runs
with the account's full rights; the stripped stdlib is a structural fact — the
host owns I/O — not a sandbox. A hostile extension is contained the way `npm
install` is contained: not at all at runtime, only by the decision to install
it. Trust is resolved when an extension is installed or approved, not while
it runs. How that works is [Distribution](#distribution).

## Process extensions

A process extension is a client of its session with extra rights. It is not
an MCP server: MCP covers tools, questions to a person and model calls, and
has nothing for hooks, watchers, state, providers or commands. An extension
that only brings tools can still ship an MCP server (`docs/mcp.md`).

The session starts the program with pipes on its stdin and stdout, in the
session's workspace. The pipe carries JSON lines with the event stream's
envelope and `schema_version` (`docs/events.md`), and three kinds of traffic:

- **what any client gets:** the event stream, and the driver commands it may
  send (`docs/invocation.md`, "Driver commands")
- **from Fiber:** a first message, `extension_hello`, with the session's id,
  why it started (new, resume, fork or rewind), its workspace, the extension's
  data directories and its folded state; then each hook, watcher delivery and
  command as a request the extension answers
- **from the extension:** its registrations, and host calls as requests Fiber
  answers

Every session starts its own process extensions, a delegate included, as it
starts its own MCP servers. Nothing is shared between sessions
([ADR 0009](adr/0009-each-session-is-one-process.md)). They start at session
start, in parallel with MCP servers, and must send their registrations within
the same startup deadline: 5 seconds, which configuration can change per
extension. One that misses it, or fails to start, is left out for the session
and the log records a `notice`; one marked `required` makes that fatal, as a
required MCP server does (`docs/mcp.md`, "Starting servers").

One that dies is restarted once, on its next call. If it dies again it stays
dead for the session: its tools stay declared, so the prompt cache holds, and
every call to them fails with code `extension_unavailable`. Its hooks then
fail as their `on_failure` says.

A process extension that misses a hook's timeout is not stopped. The hook has
failed ("When a hook fails"), and a late reply is dropped.

A process extension written in Node, Bun or Python costs a runtime: an idle
Node process measured 40 MiB, Bun 20 MiB and Python 10 MiB, against about
150 KiB for a Lua extension (macOS arm64, `research/extension-process/`). A
round trip over the pipe took 56 to 79 µs for 16 KiB, against under 10 µs into
Lua. An extension too heavy to run once per session is its author's to make
smaller.

## How an extension runs

A Lua VM runs one piece of code at a time, so each Lua extension has one
thread and one inbox, created the first time it is used. Hook calls, watcher
deliveries, command invocations, timer firings and replies to host calls all
arrive there.

**Session order is kept.** Hooks, watcher deliveries and commands form one
stream, in the order they happened in the session. Each finishes, including
any host call it waits on, before the next starts, so a hook never sees state
that misses an earlier event. Timers are not ordered against the session.
They run in the gaps: when the stream is empty, or while its current item
waits on a host call.

**A host call suspends the code that made it.** Fiber runs every callback as
a coroutine. A host call such as `host.http` or `host.model` suspends it until
the reply arrives, and the thread serves other work meanwhile, as `await` does
in JavaScript. So a timer that polls a slow service does not hold up the
extension's hooks. Code that reads and writes the extension's own globals must
expect another callback to have run in between.

**Every callback declares its timeout, with no default.** Hooks, watchers,
commands and timers alike. A hook's clock starts when Fiber asks, so time
spent waiting behind earlier items in the stream counts against it. A callback
past its timeout is stopped ("When an extension misbehaves").

A process extension keeps the same order: Fiber sends it the stream's items
one at a time and waits for each reply. How it schedules its own timers and
background work is up to the program.

Fiber's own Rust code has no async runtime
([ADR 0004](adr/0004-blocking-threads-no-async-runtime.md)). An extension's
loop runs on the extension's thread, or in its own process.

## What an extension can do

A Lua extension reaches everything through three globals: `fiber` to register,
`host` for capabilities, and `state` for its state in the session. A process
extension sends the same calls as messages.

### Registering

```
fiber.tool(name, { description, input_schema, effects, run })
fiber.provider(name, { models })
fiber.hook(point, { on_failure, timeout, run })
fiber.watch(kinds, { timeout, run })
fiber.command(name, { description, timeout, run })
```

A tool, provider or hook registers before the session's tool set is fixed
(`docs/prompt-cache.md`, "Tools"), so an extension that registers one is first
used at session start.

### Host calls

```
host.secret(name)                  -- the secret stored at credentials/<name>
host.http(opts)                    -- one HTTP request; returns { status, body }
host.model(opts)                   -- one model request; returns { text, usage }
host.exec(program, args, opts)     -- run a program; returns { exit_code, signal, stdout, stderr }
host.delegate(spec, on_finished)   -- start a delegate job; returns its job_id
host.fs.read / write / list / stat / mkdir / remove / rename
host.config.get(key) / host.config.set(key, value, scope)   -- scope: "machine" or "project"
host.data_dir(scope)               -- "machine" or "project"
host.after(ms, fn, opts) / host.every(ms, fn, opts)   -- timers; return a handle with :cancel()
host.drive(command, args)          -- send a driver command
host.ask(kind, spec)               -- raise an interaction; returns the answer, or declined
host.status(text) / host.widget(id, lines)
host.emit(data)                    -- data for this extension's own TUI extension
host.log(msg)                      -- write a debug line
json.decode(str) / json.encode(value)   -- JSON, host-provided (Lua has none built in)
```

- **`host.model`** takes a model reference or a role, messages and a token
  limit. It goes through the session's provider routing and credentials, and
  writes `usage_recorded` naming the extension. It uses its own prompt-cache
  key, the session's id plus the extension's name, so it never shares the
  session's key (`docs/prompt-cache.md`). Any callback may call it, bounded by
  that callback's timeout.
- **`host.exec`** runs a program in its own process group, which is stopped on
  cancel and at shutdown as a tool's is (`docs/tools.md`, "Shell"). Inside a
  tool call, the tool's declared `executes` effect is what the permission
  decision judges. Outside one, in a hook, watcher, timer or command, it runs
  without asking: the person approved the extension, and a process extension
  can start programs unseen anyway. Fiber logs each such run as
  `extension_exec` (`docs/events.md`), so the session shows it happened.
- **`host.delegate`** takes the arguments the model's delegate tool takes and
  starts a job the session owns, with the same limits and the same stop
  (`docs/delegates.md`). `job_started` names the extension. The delegate's
  final message goes to `on_finished`, not to the model.
- **`host.fs`** reads and writes anywhere, as a process can. The per-path lock
  Fiber's own file tools take is offered (`docs/architecture.md`, "Tool calls
  in a step").
- **`host.config`** reads and writes the extension's own settings, taking
  Fiber home's lock before a read-change-write (`docs/state.md`, "Concurrent
  access"). A picker that enables models writes its choice here. `get`
  returns the value merged across layers; `set` writes the machine or the
  project file, as `scope` says. The files and layers are
  `docs/configuration.md` ("Extension settings").
- **`host.drive`** sends any driver command except an answer to an approval.
  An extension never approves a tool call, here or in a hook. An inbox
  extension uses `steer` or `prompt`; a `/goal` extension may use `cancel`.
  This is the extension contract, not a wall: a process extension could open
  the session's socket as an ordinary client, and the trust model accepts
  that.
- **`host.ask`** raises one of the closed interactions ("Commands and
  screens"). With nobody to answer, as in a session started by `fiber ask`,
  it returns declined.

## State

An extension keeps what it knows in four places, by what the thing describes.

| What it describes | Where it lives | A rewind or fork |
|---|---|---|
| What happened in this session: a goal, what an inbox delivered, a test count | extension state, in the session's log | follows the key's fork rule |
| What a person set: enabled models, a workspace URL | `host.config`, in Fiber home | leaves it alone |
| A secret: an API key | `credentials/` (`docs/model-routing.md`) | leaves it alone |
| What it keeps across sessions: a memory store, an index | its data directories (`docs/state.md`, "Extension data") | leaves it alone |

Lua globals are none of these. They are memory, lost when the process exits
and never moved by a rewind. Use them for scratch and caches only.

### Extension state

```
state.get(key)
state.set(key, value, { on_fork })   -- on_fork: "at_point" (default), "latest" or "fresh"
state.unset(key)
state.keys()
```

- **A key holds one whole value.** Each `state.set` writes the key's whole new
  value as JSON, logged as `extension_state_set` (`docs/events.md`,
  "Extensions"). `state.unset` removes a key. `state.set` with `nil` is an
  error, so a missing value is never mistaken for a deletion.
- **A value is at most 64 KiB.** A larger write fails with code
  `state_too_large`. Across the owner's pi sessions over 60 days, 10,652
  extension state entries in 605 sessions had a median of 162 bytes, a 99th
  percentile of 3 KB and a largest of 27 KB (`research/extension-process/`).
  Bigger things go in a data directory.
- **History takes one key per record.** A value that grows with the session,
  such as one record per turn or per delegate, is written under one key per
  record, so no write repeats the ones before it (`docs/events.md`,
  "Writing").
- **Fiber folds it.** When an extension is loaded into a session, Fiber reads
  its latest values from the log and hands them over before its first call,
  including before `session_start`. The extension never replays the log
  itself.
- **The fork rule rides on each write.** A fork, a rewind or a delegate's fork
  folds its parent's log. For each key, the `on_fork` of its last write
  decides what the new session gets: `at_point`, its value as of the point;
  `latest`, its value at the end of the parent's log; `fresh`, nothing. A
  test budget is `at_point`. A list of files the person already approved is
  `latest`. A delegate that is not a fork starts with no extension state.
- **A hook's writes go with its change.** A write made inside a hook is
  logged just before the line the hook changed, and is dropped if the hook
  fails. Any other write takes effect for the extension at once and is logged
  at the loop's next drain of its inbox, so a crash before then loses it.
- **A handoff changes nothing.** The session continues, and so does its
  extension state.

## What writing a provider looks like

A provider extension is mostly data: its name, how its credential is found, and
its models, each with a protocol, a base URL, flags and metadata. The wire
protocols are native Rust, so a provider never parses a stream. What a provider
declares, and why, is `docs/model-routing.md`. The file is
`providers/<name>.json` (`docs/configuration.md`, "A provider's data").

The one piece of Lua a provider may have is a function that discovers its
models. Here is one for a gateway that lists its models at `/models`:

```lua
fiber.provider("acme", {
  models = function()
    local key = host.secret("acme.api_key")
    local reply = host.http({
      url = "https://api.acme.dev/v1/models",
      headers = { authorization = "Bearer " .. key },
    })
    local list = {}
    for _, m in ipairs(json.decode(reply.body).data) do
      table.insert(list, {
        id = m.id,
        protocol = m.id:find("^claude") and "anthropic-messages" or "openai-completions",
        base_url = "https://api.acme.dev/v1",
        context_window = m.context_length,
      })
    end
    return list
  end,
})
```

In plain terms: Fiber calls `models` when it needs the model list. The function
fetches the API key, asks the gateway for its models, and returns one entry per
model, choosing each model's protocol from its name. Fiber caches the list on
disk and refreshes it in the background at startup. The function never runs
while a request is being sent.

Writing a tool or a hook has the same shape: register a name, receive a call, do
pure work plus host calls, return. The hook points are [Hooks](#hooks).

## Hooks

A hook is asked at a fixed point in a session and may change what happens
there. Fiber has seven hook points. Each one runs before the thing it changes
is written to the session log or sent to the model, never after. That one rule
keeps two settled promises: the log holds exactly what happened
([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)), and no hook
rewrites a message the model has already been sent (`docs/prompt-cache.md`,
"Rules for other areas").

A hook only changes things. Something that only needs to know what happened,
such as a job that writes a worklog when a session ends, is a watcher: it reads
the event stream (`docs/events.md`) and has no hook point. How an extension watches is [Watchers](#watchers).

### The hook points

| Hook point | When it runs | What the hook sees | What it may return |
|---|---|---|---|
| `session_start` | A session starts, resumes, forks or rewinds, before the first model request | why it started, the session's id and workspace root | context to add |
| `before_message` | A person's or a driver's message arrives, as a turn's input or as steering, before it is logged | the message's text and images | a replacement message, a refusal with a reason, context to add |
| `turn_start` | A turn has started, before its first model request | the turn's input and what started it | context to add |
| `before_tool` | A call has passed its schema check and its effects function, before permission is decided | the tool's name, the arguments, the declared effects, paths and reversibility | replacement arguments, a refusal with a reason |
| `after_tool` | A call that ran has returned, before its output is cut, its artifact is written or it is logged | the tool's name, the arguments, `status`, the full output, `details`, `process` | replacement `content`, replacement `details`, text for the artifact |
| `turn_end` | The model has replied without calling a tool, so the turn would complete, before `turn_completed` is written | the model's final reply | a message to continue the turn with |
| `before_handoff` | A handoff has started, before the note request | the trigger, the person's instructions, and the conversation as the model would be sent it | a handoff note |

Returning nothing leaves things as they were.

**Context** a hook adds is appended to the conversation as its own message and
logged as `context_added`, naming the extension. An extension that delivers an
inbox of messages from other agents does it this way, at `session_start` or
`turn_start`. `session_start` runs again on every resume, and each time its
context is appended again. The hook sees why the session started, so an
extension that must not repeat itself keeps what it delivered in its
extension state ("State") and adds only what is new.

**`before_message`** covers every message a person or a driver sends, so a
secret pasted into a prompt can be removed before anything records it. A
refused message is neither logged nor sent, and the sender is told why.

**`before_tool`** may rewrite a call or refuse it, but never approve one.
Approval is the permission decision's (`docs/permissions.md`), and a mode is
never changed by an extension. Fiber checks rewritten arguments against the
tool's schema and runs its effects function again, so the permission decision
judges the call that will run, not the one the model asked for. A refused call
completes `denied` with reason `hook` and the extension's name, and never
starts.

The model's call is sent back to it exactly as the model wrote it. What ran is
stated in the result instead: Fiber begins the result's `content` with a line
naming the extension and giving the arguments that ran. That line is part of
`content`, so the tool's size cap applies to it. Editing the model's own call
would tell the model it asked for something it did not, and a provider that
signs the reasoning before a call may reject the edited request.

**`after_tool`** runs on every call that ran: `completed`, `failed` and
`cancelled` alike, since a cancelled command's partial output can hold a
secret too. It does not run on a call that never started. What the hook
returns is what the rest of the pipeline sees: the size cap applies to the
returned `content` (`docs/tools.md`, "Bounded results"), and the artifact holds
the hook's artifact text when it returns one, or the returned output when it
does not. So a redaction hook removes a secret from the model's view and from
disk in one pass, and the original output is never stored. An extension that
compacts build and test output returns a short summary as `content` and the
full log as the artifact text. The hook cannot change `status`: whether a call
succeeded is the tool's answer.

**`turn_end`** runs only when a turn would complete normally, not when it is
cancelled or fails. A returned message joins the turn at the step boundary as a
steering message would, and is logged as `steering_applied` with the extension
as its source. The model then takes another step, and `turn_end` runs again
when that step ends without a tool call. Fiber puts no limit on how often a
hook continues a turn. An extension that does this should keep its own limit in its extension state,
and a person can always end the turn with the cancel key. A goal that keeps the
agent working until a condition is met is built on this point.

**`before_handoff`** lets an extension write the handoff note instead of the
session's own model. When a hook returns a note, Fiber makes no note request.
When none does, Fiber makes its own (`docs/handoff.md`, "The handoff note").

### When several hooks share a point

Hooks at the same point run one after another. Configuration can set the
order of extensions at each hook point. Extensions it does not name run after
those it does, ordered by name, and one extension's hooks at the same point run
in the order it registered them. Each hook sees what the one before it
returned. A refusal ends the chain. The order is fixed so that the same inputs
give the same result on every run.

### When a hook fails

Every hook declares two things when it registers, and neither has a default.
A hook that leaves either out is not registered.

- `timeout`: how long it may run. Its author knows whether it only computes or
  calls a web service or a model. Configuration can override the timeout for
  any extension. A hook past its timeout is stopped ("When an extension
  misbehaves") and has failed.
- `on_failure`: `blocking` or `non-blocking`.

- **`non-blocking`**: if the hook errors or runs out of time, Fiber drops its
  change, carries on as if it had returned nothing, and gives a `notice` naming
  the extension. A formatter is `non-blocking`.
- **`blocking`**: if the hook errors or runs out of time, the thing it guards
  does not happen. A secret scanner is `blocking`, because carrying on without
  it would send the secret.

What a `blocking` failure stops, at each point:

| Hook point | What happens |
|---|---|
| `session_start` | The session does not start. Fiber exits with error `hook_failed`, naming the extension. |
| `before_message` | The message is neither logged nor sent, and the sender gets `hook_failed`. |
| `turn_start` | The turn completes `failed` with code `hook_failed`, before any model request. |
| `before_tool` | The call completes `failed` with code `hook_failed` and never starts. |
| `after_tool` | The call keeps the status the tool reported, since it ran. Its only content is a line saying its output was withheld because the extension's hook failed, and no artifact is written. |
| `turn_end` | The turn completes `failed` with code `hook_failed`. |
| `before_handoff` | The handoff completes `failed` with code `hook_failed`, as a failed note request does. |

### What a hook cannot do

- Change a message the model has already been sent, the system prompt or the
  tool definitions.
- Approve a tool call, or change a call's `status`.
- Run during a shutdown. When a signal stops Fiber, no hook runs
  (`docs/invocation.md`, "Shutdown").

### Recording a change

The log holds what a hook returned, never what it was given. Each line a hook
changed names the extensions that changed it in `changed_by`, so a reader can
see that a message or result was rewritten and by whom (`docs/events.md`).

Handing a value to a hook and taking its answer back is cheap next to the
hook's own work. A 16 KiB tool result crosses into Lua and back in under
10 µs, and a 1 MiB one in about 50 µs (macOS arm64,
[research/hook-conversion-cost](../research/hook-conversion-cost/README.md)).

## Watchers

A watcher learns what happened in a session and changes nothing. A worklog, a
memory written as a session ends, and a counter of test runs are watchers.

A Lua extension registers one with `fiber.watch(kinds, { timeout, run })`,
naming the event kinds it wants (`docs/events.md`). A process extension
already reads the whole event stream as a client does, and asks Fiber to send
it the kinds it wants in the stream it answers in order ("How an extension
runs").

A watcher that falls behind loses ephemeral events and never durable ones, as
any watcher does (`docs/architecture.md`, "Streaming"). A watcher that fails
or passes its timeout gives a `notice` naming the extension. There is no
`on_failure`, because a watcher changes nothing.

## Commands and screens

`fiber.command(name, { description, timeout, run })` adds a command. A person
types `/name` and any text after it, which reaches `run` as its arguments. A
driver sends the `command` driver command (`docs/invocation.md`). Names are
plain, as in pi and Claude Code: `/databricks-models`, not
`/databricks:models`. When two extensions register the same name, neither
gets it, a `notice` names both, and configuration can rename one. An
extension may replace a built-in command by name, as it may a tool.

A command talks to the person through the closed interactions every client
answers (`docs/architecture.md`, "Asking a human"): confirm, select,
multi-select, text input and form, raised with `host.ask`. A model picker is
a multi-select and a setup wizard is a series of forms. `host.status` sets one
line of status and `host.widget` sets a named block of lines. Both are data:
each client shows them or not, and a client that attaches late gets the
latest of each. In a session nobody can answer, such as one started by
`fiber ask`, `host.ask` returns declined.

A session never sends drawing code to a client. An extension that draws
carries a TUI extension: Lua scripts in its package that run in the
terminal's process, with the same runtime and rules as a Lua extension, and
reach the session only as a client does. It reads the event stream,
including what its session half sends with `host.emit`, and sends driver
commands, including its own extension's commands. A crash in a TUI extension
cannot stop the session (`docs/invocation.md`, "Processes"). What a TUI
extension may draw, and through which seam, is the TUI's to settle, after
[TUI: scrollback or full screen?](https://github.com/aakshintala/fiber/issues/15).
A future GUI's extensions take the same shape.

## Loading, and cost when nothing is loaded

Each Lua extension gets its own Lua VM and thread, created the first time the
extension is invoked, not at startup. A session that loads no Lua extension —
or loads one it never calls — creates no VM and pays no idle CPU and no
runtime memory for it. Lazy creation, not a cheap runtime, is what makes this
true. A process extension costs its process from session start, because it
must register before the session's first request.

One VM per extension (rather than one shared VM for all) costs about 120 KiB per
extension — measured, `research/extension-runtime/vm-isolation/` — and buys real
isolation: each extension has its own globals, its own garbage collector, an
optional per-extension memory cap, and a crash or runaway allocation contained to
that one VM. A shared VM with per-extension environments is leaner at large
extension counts and is the documented fallback if that ever matters.

The `reload` driver command (`docs/invocation.md`) reloads extensions. It is how
a running session picks up an installed or updated extension. Each reloaded
Lua extension's VM is created again the next time it is invoked, and each
process extension is restarted. Both are handed their folded state again.

## When an extension misbehaves

- **It errors.** A Lua error is caught at the call boundary. The extension's call
  fails; the session survives and the VM stays usable. Errors carry the
  extension's filename and line. A process extension's error reply fails the
  call the same way.
- **It loops or hangs.** Every callback answers under the timeout it declared
  ("How an extension runs"). In Lua, enforcement is a two-stage
  interrupt: a cheap instruction hook normally, escalating to fire on every
  instruction once the deadline passes, so an extension cannot swallow the
  deadline with `pcall`. Measured in `research/extension-runtime/pass1/`. A
  process extension is not interrupted: Fiber stops waiting, and its late
  reply is dropped.
- **It allocates without bound.** A per-extension memory cap turns this into an
  error in that extension's VM, not an out-of-memory kill of the process. A
  process extension's memory is its own process's.
- **It dies.** A process extension is restarted once ("Process extensions").
- **It is hostile.** Nothing stops it at runtime; it has the account's rights.
  This is the trust model, not a gap. Fiber's obligation is that installing an
  extension is a deliberate act.

## When a session ends

On a normal exit, when a session is idle with no client or has been sent
`close`, Fiber delivers every remaining event to each watcher and waits for
each to finish, within its timeout. That is where a worklog or a memory
written at session end runs. A process extension then has its manifest's
`exit_timeout_ms` to finish, and after that gets the shutdown sequence every
child gets: SIGTERM, 800 ms, then SIGKILL (`docs/invocation.md`, "Shutdown").

When a signal stops Fiber, no extension code runs, as no hook does. The
5-second shutdown bound has no room for it.

## Notes for authors

- The embedding is Lua 5.4, not LuaJIT and not Luau. Write ordinary Lua 5.4.
- `require` loads files from your extension's own directory only. To use
  someone else's Lua, vendor a copy into your directory or depend on the
  extension that holds it.
- Give your extension a version tag for every release. Dependents name a
  minimum version, and Fiber installs nothing newer than someone asked for.
- JSON is `json.decode` / `json.encode`, provided by the host. Lua has none.
- In Lua, do not reach for `io`, `os`, `fetch`, sockets or environment
  variables — they are absent. Route every side effect through `host`.
- Keep what must survive a resume in `state`, never in globals.
- Need npm, a long-lived connection or a language other than Lua? Write a
  process extension.

## Distribution

### Names

An extension's name is where it lives, as with Go modules:
`github.com/owner/repo/path`. A dependency is written the same way. Any git host
works, there is no registry, and two authors cannot claim the same name. A
local path also works, for an extension under development.

The five first-party provider extensions also have short names, so
`fiber install openrouter` means the first-party extension's full name.

Fiber fetches with the system `git`, so your SSH keys and credential helpers
apply. If `git` is missing, the command fails with a stable error.

### Versions

A version is a git tag, such as `v1.4.0`. When extensions depend on the same
extension, Fiber installs the lowest version that meets every stated minimum.
If `openrouter` needs `oauth-helper` 1.2 or later, `databricks` needs 1.4 or
later, and 1.9 is the newest, Fiber installs 1.4. The same inputs always give
the same result, so there is no lockfile and no solver. A newer version arrives
only when something raises its minimum.

Two extensions that need different major versions, such as 1.x and 2.x, stop
the install with an error naming both.

Fiber records the exact commit it installed and loads only that. Nothing is
signed. The fetch runs over TLS or SSH, and a binary is checked against the
sha256 in its manifest. Fiber downloads only the binary for the platform it is
running on.

### Installing

| Command | What it does |
|---|---|
| `fiber install <name>` | Installs an extension and its dependencies. If any part fails, nothing is installed. |
| `fiber update <name>` | Moves one extension to its newest version and re-resolves its dependencies. |
| `fiber remove <name>` | Removes an extension, and any dependency nothing else uses. |
| `fiber list` | Lists installed extensions with their versions and commits. |

In a terminal, `install` and `update` show a summary and ask before going
ahead. The summary is the same one described in
[Extensions a repository brings](#extensions-a-repository-brings), and on
update it adds the diff since the installed version. Without a terminal they go
ahead without asking, so scripts can set up a machine.

Install refuses an extension whose manifest needs a newer Fiber than the one
running.

Installed extensions live in [Fiber home](state.md), one directory each, at
`extensions/<name>/`.

Installing an extension runs none of its code, except an install step its
manifest declares, such as `npm ci`. Fiber runs that step in the extension's
directory at install and at every update, as pi runs `npm install` for its
packages. Like pi, Fiber does not pass `--ignore-scripts`, so a dependency's
own install scripts run too, and the install summary says so. A pure-data
provider is only ever read, and a Lua script first runs when the extension is
first used. An
extension that registers a tool is first used at session start, because the
tool set is fixed before the first request (`docs/prompt-cache.md`, "Tools").

### A fresh install

Installing Fiber also installs the five first-party provider extensions, so
the first run fetches nothing.

A Fiber binary that arrived some other way has no extensions. In the terminal,
the model picker offers the five first-party providers, and choosing one
installs it. A headless run fails with `extension_missing`.

### Staying current

`fiber upgrade` updates the Fiber binary and every installed extension
together, so a new Fiber and the extensions written for it arrive at the same
time. `fiber update <name>` updates one extension.

Nothing checks for updates on a timer. Extensions change only when someone runs
one of these commands, so an idle Fiber does no work.

### Extensions a repository brings

A repository can bring extensions in two ways:

- ship them in `.fiber/extensions/<name>/`
- declare them by name and version in `.fiber/config.json`, to be fetched

Fiber loads neither until a person approves it. The first time a session would
load one, the terminal shows:

- where it comes from and its version
- the tools it registers, each with its effects
- the providers it registers, each with its base URLs
- the hooks, watchers and commands it registers
- the program a process extension runs, and its install step
- the skills, prompt templates, themes, binaries and TUI extension it carries

The full source is one key away. Approving a declared extension fetches it.

An approval covers exact content. If the extension changes, Fiber shows the
diff since the approved content and asks again. Approvals are recorded per
machine in [Fiber home](state.md) at `approvals/<content-hash>`, so content
approved in one repository is not asked about again in another.

A headless run never fetches and never loads unapproved content. If a repository
declares an extension that is not installed, the run fails with
`extension_missing`. If it brings one nobody has approved, the run fails with
`extension_unapproved`, listing each one. `fiber approve`, run in the
repository from a terminal, shows the same summary for each and records the
approvals, so a later headless run can load them.

An MCP server a repository declares runs a program, so it needs the same
approval (`docs/mcp.md`, "A repository's servers").
