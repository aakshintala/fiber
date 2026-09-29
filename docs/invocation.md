# The front doors

How Fiber is started, what goes in, and what comes back out. This is what is
true now, not a plan. It is settled by
[Front doors: which invocation modes does v0.0.1 have?](https://github.com/aakshintala/fiber/issues/10);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Front door, driver command, session, turn, step
boundary, steering message, watcher and driver mean what it says there and
nothing else. The events named here are `docs/events.md`; this page is only
the process contract over them.

## Two doors

| | What it is |
|---|---|
| `fiber` | The terminal. Requires a tty; without one it is a usage error naming `fiber serve`. It starts a `fiber serve` session and drives it ("Processes"). |
| `fiber serve` | The non-interactive door. Stays open. Its stdin is the driver channel: one JSON command per line. This is the door a GUI frontend, a supervising tool or a script uses. It also listens on a local socket, so other clients can attach. |
| `fiber ask` | The same non-interactive door with the prompt already supplied and no further prompts accepted. Its stdin is the prompt. |

`fiber remote` is not a door. It is an optional daemon that lets web and
mobile clients reach sessions ("Remote clients").

`ask` is not a second door and not a second code path. It is `serve` with its
input already supplied and no more coming — the archived tree reached the same
conclusion in
[fiber-zig#190](https://github.com/aakshintala/fiber-zig/issues/190): "There is
no second execution path."

Map premise 6 governs both: "Both interactive and non-interactive sessions are
one agent loop behind two front doors: same session, same event stream, same
log." A door has no privilege the terminal lacks, and neither has a path to
state the other does not.

## Getting a prompt in

`ask` takes its prompt three ways, and supplying two is an error rather than a
precedence rule, because ambiguous input is a caller bug worth surfacing:

```sh
fiber ask "review the diff on this branch"
cat brief.md | fiber ask
fiber ask --prompt-file brief.md
```

`--prompt-file` is not redundant with argv. Linux caps a single argument at
`MAX_ARG_STRLEN`, 32 pages — 131072 bytes — independently of `ARG_MAX`, and
premise 9 makes Linux the platform that carries the usage weight. macOS is far
looser: a 1 MiB argument passes on Darwin 25.6.0, where `ARG_MAX` is 1048576.
A brief that runs fine on a Mac can fail with `E2BIG` on the machine that
matters, so the flag exists to take the size cliff off the table. The Linux
figure is documented, not measured here.

`fiber ask` with no prompt and stdin on a terminal is a usage error, not a
silent drop into the TUI.

## Why stdin means different things

On `serve`, stdin is the driver channel. On `ask`, stdin is the prompt. That
split is deliberate and it is the only difference between them worth naming.

The alternative was deciding what stdin holds by looking at it — treat a line
that parses as a command object as a command, anything else as text. That
fails in this repository specifically: a brief asking Fiber about Fiber's own
command set would open with exactly such a line. A mode switch on content is a
bug that arrives once, at the worst moment.

What `ask` gives up by spending its stdin on the prompt is the ability to
answer an interaction or steer mid-run. That costs nothing, because
`docs/permissions.md` already settles the unattended case — "With no client
attached and no answer possible, escalation is a block and the run continues
under the rule above until it exhausts the block budget" — and cancelling is a
signal, not a command. A caller that wants to talk back uses `serve`, which is
what it is for.

## Driver commands

The closed set a driver may send. Every command is answered with exactly one
ephemeral `command_accepted` or `command_rejected` echoing the command's id;
acknowledgements carry no `seq`, so they never reach the log.

### The command line

One JSON object per line. Its keys follow the rules of `docs/events.md`,
"Payload types": snake_case, and an optional key is absent, never `null`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes | minted by the client from random bytes, as Fiber mints its own ids (`docs/events.md`, "Identity and ordering"). Acknowledgements and events name the command by it, as `command_id` |
| `command` | string | yes | the command's name, from the table below |
| `session_id` | string | no | on `steer` and `reply`, the delegate the command is for; absent means the session this client drives |
| `args` | object | no | the command's own keys, below; absent when it takes none |

```json
{"id":"c_7f3a","command":"steer","args":{"content":[{"type":"text","text":"use the other test file"}]}}
```

A line that is not a JSON object, has no string `id` or `command`, has a
`session_id` or `args` of the wrong type, or has a key not in this table, is
rejected `malformed`. A `session_id` on a command other than `steer` and
`reply` is rejected `invalid_arguments`. `args` with a missing key, a key of the wrong type or a
key the command does not take is rejected `invalid_arguments`, so an older
Fiber says no to a newer client's key instead of ignoring it.

`content` is content parts (`docs/events.md`, "Content parts"). A client sends
an image part as `type`, `data`, the image's bytes in base64, and `mime_type`,
and nothing else: a remote client cannot write into the session directory, and
Fiber reads the size from the image. Fiber writes the image to `artifacts/`
and logs the part with its `path`, `mime_type`, `width` and `height`.

| Command | `args` |
|---|---|
| `prompt` | `content` |
| `steer` | `content` |
| `steer_amend` | `command_id` (string), the `steer` command's id; `content`, the new message |
| `steer_drop` | `command_id` (string), as `steer_amend` |
| `cancel` | none |
| `reply` | `request_id` (string) and the answer ("Replying") |
| `job_stop` | `job_id` (string) |
| `background` | none |
| `reload` | none |
| `tools` | none |
| `model` | `model` (string), a model reference as a person types one (`docs/model-routing.md`, "Naming a model"); `effort` (string, optional); `thinking` (string, optional) |
| `mode` | `mode` (string) |
| `name` | `text` (string); empty clears the name |
| `handoff` | `instructions` (string, optional) |
| `rewind` | `from_session_id` (string, optional), `seq` (integer, optional), `summarise` (boolean, default false), `adopt` (array of strings, default empty) |
| `shell` | `command` (string); `send` (boolean, default false) |
| `command` | `name` (string); `text` (string, optional), what the person typed after the name |
| `close` | none |

### What each command does

| Command | What it does |
|---|---|
| `prompt` | Starts a turn. Rejected `busy` if a turn is running. |
| `steer` | Sends a steering message, which joins the running turn at its next step boundary. A steering message also moves any running shell call to the background, so it reaches the model at the next step boundary. Takes an optional `session_id` naming a delegate. |
| `steer_amend` | Replaces a steering message's text while it is still queued. Names the message by the id of the `steer` command that sent it, as `steering_queue` lists it (`docs/events.md`). |
| `steer_drop` | Removes a queued steering message, named as `steer_amend` names it, so nothing is applied. |
| `cancel` | Ends the running turn (`docs/architecture.md`, "Cancellation"). Rejected `stale_request` if no turn is running. |
| `reply` | Answers an interaction the loop raised: approval, confirm, select, multi-select, text input or form ("Replying"). Takes an optional `session_id` naming a delegate. |
| `job_stop` | Stops a running job by `job_id`. Rejected `stale_request` if the job is not running. |
| `background` | Moves every shell call running in the current turn to the background (`docs/tools.md`, "Shell"). Rejected `stale_request` if none is running. |
| `reload` | Re-reads configuration, restarts changed MCP servers and extensions, and declares the tool set again (`docs/mcp.md`, "Reload"). Rejected `busy` if a turn is running. |
| `tools` | Answers with every declared tool: its source, whether it is full, deferred or loaded, and its approximate size (`docs/tools.md`, "Seeing the tools"). |
| `model` | Switches model, effort or thinking at the next turn boundary. Takes a model reference and optional effort and thinking. The switch rebuilds the prompt cache, and the terminal says so with the rebuild's size first (`docs/prompt-cache.md`, "Switching model"). Rejected `invalid_arguments` for an unknown model. |
| `mode` | Switches the permission mode at the next turn boundary (`docs/permissions.md`, "Modes"). Takes a mode. Rejected `invalid_arguments` for an unknown mode. |
| `name` | Sets the session's name, which pins it against the model's `name_session`. Takes the text; empty text clears the person's name and unpins it. Written as `session_named`. |
| `handoff` | Starts a handoff: the model's context restarts from a note the model writes (`docs/handoff.md`). Takes optional instructions saying what the next stretch of work focuses on. During a turn it applies at the next step boundary, as a steering message does; between turns it is a turn of its own whose input is the command. |
| `rewind` | Starts a new session that continues a session from an earlier point (`docs/events.md`, "Rewind"), and answers with the new session's id. Takes an optional `from_session_id`, default this session; an optional `seq`, default the start of the latest turn; whether to summarise; and `adopt`, the `job_id`s of the jobs started after the point that the new session keeps, default none, so every other such job stops. Rejected `busy` if a turn is running, `stale_request` if `adopt` names a job that is not running, `not_step_boundary` if `seq` is not a step boundary, `session_held` if another process holds the session, and `delegate_session` if it is a delegate. |
| `shell` | Runs a shell command the person typed, as `!` does in the terminal. Takes the command and `send`, default false. Answered when the command ends. Accepted during a turn. |
| `command` | Runs an extension's command by name, with the text after it as arguments, as a person typing `/name args` does (`docs/extensions.md`, "Commands"). Rejected `unknown_command` for a name no extension registered. |
| `close` | Accept no more prompts; finish the turn in flight, then any running jobs (`docs/tools.md`, "Background jobs"), and exit. |

Rejection codes: `malformed`, `invalid_arguments`, `unknown_command`,
`busy`, `stale_request`, `not_step_boundary`, `session_held`,
`delegate_session`.

**`reply` answers every interaction that asks something, not just approvals.**
`docs/architecture.md` fixes the set: "Fiber ships one closed, versioned set
of interactions — approval, confirm, select, multi-select, text input and
form — carried on the same request events the loop uses to ask a human
anything, and answerable by any connected client including a headless one." The interaction kinds are
versioned and may grow; one command that carries a `request_id` does not have
to grow with them. What happens to a stale one is already `docs/events.md`'s:
"a reply naming a request that is no longer pending is rejected and does
nothing, so a late approval can never authorise a different action."

**`steer_amend` is the atomic form of drop-then-steer.** Without it a client
changing queued text sends `steer_drop` then `steer`, and the loop can drain
the queue between them — the turn gets nothing when it should have got the new
text. One command closes that window. Both amend and drop are rejected
`stale_request` once `steering_applied` has landed, and neither can tear:
`docs/architecture.md` puts one inbox behind one thread draining at step
boundaries, so an amend lands wholly before a drain or wholly after it.

**`steer` and `reply` can name a delegate.** With an optional `session_id`
naming a delegate, the command goes to that delegate instead of this session.
Each parent forwards a command addressed to a descendant down the tree, so a
driver reaches any delegate in it (`docs/delegates.md`). The command is
rejected `stale_request` if no such delegate is running.

**`job_stop` names a running `job_id`.** It is rejected `stale_request` if the
job is not running. The terminal lists jobs with `/jobs` and can stop one from
there. The list is a fold of the log, so there is no driver list command.

**`background` frees the turn without a message.** It does what a steering
message does to running shell calls, with nothing sent to the model. The
terminal binds it to Ctrl+B. It never kills a command: the command becomes a
job and keeps its timeout.

**`shell` runs the person's own command, and the model sees it only if asked.**
It runs in the session's workspace with the shell and output cap of the shell
tool (`docs/tools.md`, "Shell"), and `cancel` stops it. It needs no approval:
the person typed it, and a client that can attach already controls the session
([#30](https://github.com/aakshintala/fiber/issues/30)).

- With `send` false, the output goes back in the command's
  `command_accepted` to the client that sent it and is never logged. The
  terminal's side panel reads git state this way.
- With `send` true, the output is logged as `shell_command`
  (`docs/events.md`) and joins the next turn's input, never the running turn.
  The terminal sends it for `!command`, and sends `!!command` with `send`
  false.

pi does the same with `!` and `!!` (`excludeFromContext`). Settled by
[#148](https://github.com/aakshintala/fiber/issues/148).

**`rewind` moves the process to the new session.** From then on `fiber serve`
drives the new session. It closes the old session with `rewound` while it
still holds that session's lock, releases the lock, takes the new one's, and
its lines carry the new `session_id`. A session no process holds is opened,
and its lock taken, before it is rewound.

**The set is a floor, not a proof.** It is what Fiber's settled semantics
require today. A later ticket may add one. Adding a command is additive and not
breaking, which is why `unknown_command` exists: an older Fiber tells a newer
client no, in words, instead of ignoring it.

A driver that needs a command Fiber does not define has found a hole in the
contract, not a reason for a private channel. Premise 5 gives the TUI "no
privilege a second GUI client would not have", so a command the terminal needs
is a command every driver gets.

### Replying

`reply`'s answer keys are the keys of the line it causes, so a client builds a
reply from the table it already reads.

An interaction (`interaction_requested`) is answered with `declined: true`, or
with the answer keys for its kind: `confirmed`, `labels`, `text`, or `answers`
with an optional `note` (`docs/events.md`, `interaction_resolved`).

An approval (`permission_requested`) is answered with these keys, which
`permission_resolved` records:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `decision` | string | yes | `allow` or `deny` |
| `feedback` | string | no | with `deny`, what the person typed; the model receives it |
| `remember` | object | no | with `allow`, on a request that offers a `rule`: `scope`, which is `session` for a session grant or `project` for a standing rule in the project's rules file, and `prefix`, which is the request's `rule.subject` or `rule.prefix` |

```json
{"id":"c_91be","command":"reply","args":{"request_id":"r_2c01","decision":"allow","remember":{"scope":"project","prefix":"npm test"}}}
```

A reply is rejected `stale_request` when its request is no longer pending, and
`invalid_arguments` when its keys do not fit the request: another kind's
answer keys, `feedback` with `allow`, or `remember` on a request with no `rule`
or with a prefix the request did not offer. A global standing rule is added by
editing the global rules file, never from an approval.

## Lifecycle

**First line is `fiber_started`**, carrying the Fiber version, the
`schema_version`, the `session_id`, and whether the session is new or resumed.
Both subcommands take the same resume selector, and so does `fiber`:
`--resume <id>` takes a full session id or any prefix of one that is unique
among the project's sessions. In the terminal, `fiber --resume` with no id
opens the session list. `fiber serve` and `fiber ask` have no list to show, so
there it is a usage error.

**A rewind changes the session, not the process.** The `session_id` on the
first line is the session the process started with. After a `rewind` the
stream goes on with the new session's lines, beginning with its
`session_started`, whose `forked_from` names the old session.

**A session exits when it is idle and has no client.** Idle means no turn
running and no jobs running. A client is a driver: stdin on `fiber serve`, or a
connection to the session's socket. A client leaves by stdin EOF, by closing
its connection, or by losing it. Leaving never cancels. When the last client
has left, Fiber finishes the turn in flight, gives the ending notice and waits
for any running jobs (`docs/tools.md`, "Background jobs"), then exits.

**A session that never got a prompt leaves nothing behind.** The terminal
starts its session at launch, so MCP servers connect while the person types. A
session that exits with no `turn_started` in its log deletes its own directory,
so opening `fiber` and then resuming another session leaves no empty session in
the list.

**`close` ends the session whoever else is attached.** It accepts no more
prompts, then follows the same path.

One rule covers every case that matters. A GUI frontend that dies mid-turn
closes the pipe, and Fiber finishes rather than orphaning itself. Closing the
terminal does the same, unless another client is still attached. A phone
that loses its connection mid-turn loses no work. A delegated run is spawned
with its prompt supplied and stdin already at EOF, so it runs until the
model's final answer — twenty minutes if it takes twenty minutes — on the same
path; its caller's turn ending changes nothing, because the caller's turn was
never holding the pipe.

There is no detach command. A client detaches by closing its connection, and
a session that exits is resumed from its log, so reattaching to one needs
nothing kept running.

The terminal offers two ways to leave. Detach (a second Ctrl+D on an empty
input box, or `/detach`) closes its connection, and the rule above applies.
Quit (a second Ctrl+C, or `/quit`) stops everything: it sends `cancel`, stops
each running job and delegate, then sends `close`. When the `clients` event
says another client is attached, quit asks first, and offers detach instead.

**A pending approval or question does not keep a session alive.** An
approval is raised before its tool call runs, and an `ask_user` question runs
nothing, so nothing is in flight. When the last client leaves with either
pending and nothing else running, the session exits,
and `fiber_exited` names the request it stopped on. Resuming the session
raises the request again and the turn goes on from there. The terminal and
`fiber remote` list sessions that are waiting on a person, from their logs.
Two cases have no one to wait for, and there escalation is a block as
`docs/permissions.md` ("Headless") describes: a session started by
`fiber ask`, and a session that has been sent `close`. In both, and in every
delegate, an `ask_user` question ends the turn instead, and the driver resumes
the session with the answers (`docs/tools.md`, "Asking the person").

**A pending elicitation waits within its call's timeout.** An MCP call is in
flight, so the session stays alive for it, and no longer than the call's
timeout (`docs/mcp.md`, "Calls").

**A prompt arriving mid-turn is rejected `busy` and starts nothing.** Steering
is the mid-turn channel. Fiber holds no prompt queue that no durable event
describes; the admission-ordered steering queue is the only queue.

**Cancellation targets the turn, not the process.** What it does is the
concurrency section of `docs/architecture.md` and adds nothing here.

**Exit codes: 0 success, 1 failure, 2 usage, 129 SIGHUP, 130 SIGINT, 143
SIGTERM.** Usage is Fiber called wrongly: a bad flag, two prompt sources, no
prompt, no tty. `fiber ask` exits 1 when its turn failed; `fiber serve` exits 1
only when the process itself failed (`docs/errors.md`, "What a caller gets").
What a signal guarantees before the process goes is "Shutdown".

## What a caller gets back

Nothing on this door is a second format. `docs/events.md`: "Filter a
non-interactive run's stdout to durable lines carrying its own `session_id` and
you have `events.jsonl`, byte for byte."

- **The verdict** is `fiber_exited`, the last line, which copies the final
  message's text so a one-shot caller reads one line and is done.
- **Questions** are `fiber_exited.questions`, when the model asked with
  `ask_user`. The run exits 0, and the caller resumes the session with the
  answers as its prompt (`docs/tools.md`, "Asking the person").
- **Finished or died** is whether `fiber_exited` is there at all. A
  `fiber_started` with no matching `fiber_exited` means the process died.
- **A failure before any session exists**, such as invalid configuration or a
  missing credential, still ends stdout with a `fiber_exited` line carrying the
  error, with no `session_id` (`docs/errors.md`, "Before a session exists").
- **Progress** is the ephemeral lines. They carry no `seq` and never reach the
  log.
- **After a rewind** the stream holds two sessions' lines. Filtering by each
  `session_id` gives each session's log, and `fiber_exited` carries the
  session the process ended on.

Two consequences for the door: **stdout carries no terminal escape codes and
no tty is required**, because either would break that byte-for-byte equality.

## Processes

Settled by
[Process architecture: core, TUI and shared services](https://github.com/aakshintala/fiber/issues/81);
the rationale and the rejected layouts are
[ADR 0009](adr/0009-each-session-is-one-process.md).

- **Every session is one `fiber serve` process.** A Fiber delegate is a
  child `fiber serve` of its parent, driven over the pipe it was spawned
  with, as a delegate on any other harness is (`docs/delegates.md`). Each
  session starts its own MCP servers and process extensions, so nothing is
  shared between sessions (`docs/mcp.md`, `docs/extensions.md`).
- **The terminal is its own process.** `fiber` starts a `fiber serve` session
  and is its client zero: commands down the pipe, events back up it. It draws
  what arrives and has no path to state the stream does not carry. It opens a
  session's socket only to attach to one already running. It starts that
  process in a new process session, so a closed window or a signal to the
  terminal reaches the terminal alone. A TUI crash, or an error in a TUI
  extension, cannot interrupt the session's work; the session then follows
  the lifecycle rule like any session whose client left.
- **Every running session listens on a local socket** at
  `~/.fiber/run/<session_id>` (`docs/state.md`), reachable only by the account
  that owns Fiber home. The socket carries the same driver commands and event
  stream as stdin and stdout. Being that account is the authentication. The
  process holding the session's lock owns the socket: it removes any old one
  before binding, and nothing else ever removes one.
- **Resuming a session that is still running attaches to it.** A session log
  has one writer (`docs/events.md`), so `fiber --resume` never opens a second
  one. A client attaching first folds the log by `seq`, then streams.
- **A client attaches across versions only when it can read the stream.** A
  session keeps the binary it started with through `fiber upgrade`. A client
  reads the session's `schema_version` from `fiber_started`; an additive
  difference is fine (`docs/events.md`, "Versioning"), and on a breaking one
  the client says which version the session runs and declines, so the person
  can close it or let it exit.

The pipe and the socket are the only paths into a running session, and they
carry the same bytes. The terminal, a script on stdin, a parent session and a
phone through `fiber remote` are the same kind of client, as map premise 5
requires.

## Shutdown

Settled by
[Shutdown: what SIGTERM has to guarantee](https://github.com/aakshintala/fiber/issues/34);
that ticket's resolution holds the rationale and the rejected alternatives.
What pi, codex and Claude Code do, and what a crash leaves behind, are
`research/shutdown/`.

A shutdown is Fiber stopping because a signal told it to. It is bounded, it
stops everything the session started, and it asks nobody anything. Exiting
because the work ran out ("Lifecycle") and `close` are not shutdowns: both
wait for jobs without a cap. A supervisor that wants the work finished sends
`close`, and SIGTERM when its patience runs out.

**Three signals, one path.** SIGTERM, SIGINT and SIGHUP each start a
shutdown. They differ only in the exit code: 143, 130 and 129. A second
SIGTERM or SIGINT during a shutdown skips the grace period below: every
process group still alive gets SIGKILL at once, and Fiber writes what it
knows and exits.

**What happens, all at once:**

- The model request's socket is closed.
- Every running tool call's process group, every job's and every delegate's
  gets SIGTERM. A group still alive 800 ms later gets SIGKILL, and Fiber
  reads its output for at most 2 s more: the sequence in `docs/tools.md`
  ("Stopping a command"). Groups are signalled together, never one after
  another. Six groups that ignore SIGTERM took 0.81 s in parallel and 4.85 s
  in sequence (macOS arm64, `research/shutdown/probe5_nested_kill_cost.py`).
- A delegate is a child `fiber serve`, and SIGTERM starts its own shutdown.
  Its parent waits for it to exit, up to the bound, rather than sending
  SIGKILL at 800 ms, so the delegate stops its own commands and writes its
  own `fiber_exited`. Depth is capped at 2 (`docs/delegates.md`) and every
  process forwards the signal before doing anything else, so every level of
  a tree counts down from almost the same moment.
- Each MCP call in flight gets `notifications/cancelled` and ends `failed`
  with code `mcp_cancel_requested`, as on a cancelled turn (`docs/mcp.md`,
  "Calls"); a pending elicitation goes with its call. Then each stdio
  server's stdin is closed and it gets SIGTERM, then SIGKILL 800 ms later.
- No model request is made, no ending notice is given, and no hook runs.
  Because no `after_tool` hook runs to redact it, a call cancelled by shutdown
  completes with no content and no artifact.

**What is written.** The turn in flight ends as the cancel key ends it
(`docs/architecture.md`, "Cancellation"): each tool call completes
`cancelled` once its group is empty, each job `cancelled`, a handoff in
flight `cancelled`, and the turn `turn_completed { outcome: interrupted }`. Two
things differ from a cancel: a pending approval or question stays pending, so
resuming raises it again (below), and queued steering messages start no turn. They
were never logged, so they are gone.
Nothing is written for a call before it has stopped. Then `fiber_exited`
with the exit code and no final message, the socket is unlinked, the lock is
released, and the process exits.

A session waiting on an approval or a question when the signal arrives has nothing running
(`docs/architecture.md`: "Permission decisions are made in order, before any
of them runs"). It stops its jobs and exits with `suspended_on` naming the
request, as it does when the last client leaves ("Lifecycle"), and resuming
raises the request again.

A signal that arrives before `fiber_started` is written exits with the code
and writes nothing. Once `fiber_started` is written, `fiber_exited` always
is, unless the process dies.

**The bound is 5 seconds** from the signal to exit, per process. The
command stage is at most 2.8 s (800 ms grace plus 2 s drain), the levels of
a tree run concurrently, and the rest is margin. It sits under the
supervisors Fiber runs under: Docker sends SIGKILL 10 s after SIGTERM,
Kubernetes 30 s, systemd 90 s (each one's documented default, not measured).
codex's headless server gives itself 45 s. Past the bound, every group still
alive gets SIGKILL; each call, job or delegate it belonged to ends `failed`
with code `indeterminate` (`docs/tools.md`: "never `completed`"), and the
exit code is unchanged. A delegate killed this way leaves a log with no
`fiber_exited`, like any process that died.

**What a crash leaves.** A crash, a SIGKILL, or a supervisor that gives up
before the bound stops nothing. A command whose output goes to the pipe
Fiber held dies at its next write; everything else keeps running, and a job
writes to a file, so every job survives (macOS arm64,
`research/shutdown/crash-cleanup.md`). Fiber does not go looking for them: a
resumed session marks each `orphaned` and "does not touch any process"
(`docs/events.md`). Linux's `PR_SET_PDEATHSIG` reaches only the shell Fiber
starts, not what the shell starts, and a recorded process group and start
time cannot prove a group is still the one recorded, so neither is used. The
gap is stated rather than half closed.

A Fiber delegate whose parent died sees its client leave, finishes its turn
and jobs, and exits ("Lifecycle"), so its own log is complete. Its parent's
log still marks the job `orphaned` on resume, because the parent cannot
know. An MCP call the delegate raises after that fails at once with
`mcp_server_unavailable`, since nothing can answer it.

**The terminal and the daemon.** The terminal starts its `fiber serve` in a
new process session, so a closed window or a signal to the terminal reaches
the terminal alone; the session is one client short and follows
"Lifecycle". Stopping `fiber remote` ends its relays, and each session sees
a client leave.

## Remote clients

A web or mobile client can attach to a running session and start a new one.
It reaches the host through `fiber remote`, an optional daemon.

- **`fiber remote` holds no session.** It lists sessions from their logs,
  starts `fiber serve` processes, resumes a session whose process has exited,
  and relays each remote client to a session's socket. A session is running
  when its socket accepts a connection; anything else is a log to resume. Its
  crash or its restart ends no session, and `fiber upgrade` restarts it.
- **A remote client reaches a delegate as the terminal does**: through the
  root, with `session_id` on `steer` and `reply`. It never opens a delegate's
  own socket.
- **Fiber ships no relay service.** `fiber remote` listens on an address the
  person chooses, and the person makes it reachable: tailscale, WireGuard, a
  LAN or `ssh -L`. Every remote connection presents a token.
- **A remote client has exactly the terminal's powers.** It receives the same
  event stream and sends the same driver commands.

What `fiber remote` speaks, how a client gets its token, and whether Fiber
installs it as a login service are
[Remote access: the remote endpoint](https://github.com/aakshintala/fiber/issues/86).

## Isolation

Fiber accepts a workspace path, runs in it, and records it on
`session_started`. Fiber creates a worktree only for a delegate that asks for
one (`docs/delegates.md`). A supervisor starting Fiber still makes the tree
and passes the path; it needs git anyway to report what changed. This follows
`docs/architecture.md`'s "`main` holds no feature logic" and keeps git out of
the binary.

## Fiber serves no MCP

Fiber ships no MCP server, and the supervisor that manages several
outstanding delegations to Fiber lives outside Fiber. A session's own
delegates are `docs/delegates.md`. The rationale is
[ADR 0005](adr/0005-no-mcp-server-the-supervisor-is-external.md). Fiber is an
MCP client, consuming MCP servers, and that is `docs/mcp.md`.

What Fiber owes the delegation slot instead is being cleanly wrappable, and
that is the whole of it:

- start non-interactively with a prompt, with no tty,
- put no escape codes on stdout,
- announce the session id on the first line,
- emit a documented, versioned event stream,
- exit with a stable code,
- run in a workspace path it is given rather than one it makes.

Everything a supervisor does beyond that — tracking several jobs, waiting on
any or all of them, running a verification gate afterwards, reporting a git
change set — is orchestration that is identical for any agent binary, and a
supervisor that knows only about Fiber is worth less than one that does not.

## Related

- The terminal's own shape: `docs/tui.md`
