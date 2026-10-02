# MCP

How Fiber uses MCP servers: what it starts, what the model sees, how a call is
judged, and what happens when a server fails. It is settled by
[MCP client](https://github.com/aakshintala/fiber/issues/22); the reasoning and
the rejected alternatives are
[ADR 0008](adr/0008-the-mcp-client-is-built-in.md). What pi, codex and Claude
Code do is in `research/mcp-client/`.

Vocabulary is `GLOSSARY.md`. MCP server, tool call, effect, workspace, driver,
driver command, session and event mean what it says there and nothing else.

## What Fiber does with MCP

Fiber is an MCP client. The client is built into the binary, as the wire
protocols in `docs/model-routing.md` are. Fiber serves no MCP
for delegating to Fiber
([ADR 0005](adr/0005-the-delegation-supervisor-is-external.md)).

Fiber speaks two transports:

- stdio, for a server Fiber starts as a child process
- streamable HTTP, for a remote server

It has no legacy HTTP with server-sent events (HTTP+SSE). codex has none
either.

A remote server that needs OAuth uses MCP's OAuth flow, which the client
carries ([ADR 0008](adr/0008-the-mcp-client-is-built-in.md)). A person logs in
to a server with `fiber mcp login <server>`. Token refresh follows the provider
rule in `docs/model-routing.md`, "Credentials": lock the credential file,
re-read it, refresh once. A headless start with no valid token counts as
the server failing to start (see [Starting servers](#starting-servers)).

Fiber is an MCP server in one place: `fiber mcp serve`, which a delegate on
another harness uses to send session messages (`docs/delegates.md`,
"Delegates on another harness").

## Tools and their names

Each tool a server offers becomes a Fiber tool named `mcp__<server>__<tool>`,
the convention codex and Claude Code both use. A name longer than a protocol
allows is cut short and given a short hash suffix, so two long names stay
distinct.

MCP tools register through the tool seam like any built-in
(`docs/architecture.md`, "Tool seam"). The seam's rule applies unchanged:
registering an existing name replaces it, and the replacement is recorded in the
session log. An extension can replace any MCP tool, or `mcp_resources`, by
registering its name.

The tool set is fixed at the session's first request. A resumed session lists
its servers' tools again and fixes its tool set at its own first request. Tools
are sorted by name, with every schema's keys sorted (`docs/prompt-cache.md`,
"Bytes"), and only [reload](#reload) changes the set. Changing tool definitions
mid-session misses the whole prompt cache.

### Deferred tools

On a model that supports deferral, MCP tools are declared deferred by default:
the model sees each tool's name and loads its full definition with
`tool_search` when it needs it. On any other model every tool is declared in
full; OpenAI's `allowed_tools` restricts calls but still sends each definition.
A server can be marked `declare_in_full`, so its tools are always declared in full.

Why deferral keeps the cache is `docs/prompt-cache.md`. Which models defer,
`tool_search`, and which tools the model sees in general are
`docs/tools.md`, "Which tools the model sees".

## Effects

An MCP tool's effects come from the hints its server declares for it, in the
vocabulary of `docs/permissions.md`:

| Hint | Effect |
|---|---|
| `readOnlyHint` true | `reads`, reversible |
| `destructiveHint` true | `writes`, irreversible |
| `destructiveHint` false | `writes`, reversible |
| `openWorldHint` true, or absent | adds `network` |
| any remote (HTTP) server | adds `network` |
| neither `readOnlyHint` nor `destructiveHint` | `executes`, irreversible |

A tool with no hints at all therefore declares `executes` and `network`,
irreversible, so `auto` sends every call to it to the reviewer. Configuration
can override any tool's hints.

The hints are believed, on the same grounds as an extension's declarations. The
person installed the server, and `docs/permissions.md` says of extensions: "An
extension runs with the account's full rights, so misdeclaring buys it nothing
it could not do directly."

MCP hints are per tool, not per call. That is weaker than `docs/permissions.md`
asks: "Classification is per call, not per tool." MCP offers nothing per call,
so Fiber cannot do better. Every call to one MCP tool declares the same effects,
whatever its arguments.

An MCP tool declares no paths. A `reads` call takes the permission fast path. A
`writes` call never does, because the workspace fast path needs paths inside
the workspace. The credential deny (`docs/permissions.md`, "Credentials") cannot
see what an MCP tool touches, as it cannot for a shell command it does not
recognize.

## Prompts and resources

A server's prompts become prompt templates. How a person invokes a prompt
template is not yet specified: `docs/extensions.md` says "What a skill, a prompt
template and a theme are to Fiber is not yet specified."

A server's resources are reached through one tool, `mcp_resources`, with two
actions:

- `list`, which lists the resources a server offers
- `read`, which reads one resource

`mcp_resources` is declared only when at least one configured server offers
resources. Its effect is `reads`, plus `network` for a remote server.

## Calls

Each call has a timeout, 10 minutes by default, the same as the shell's
`timeout_ms` (`docs/tools.md`, "Timeout"). Configuration can change it per
server. A call that times out ends `failed` with code `timeout`.

Cancelling a turn sends MCP's `notifications/cancelled` for each call in
flight and stops waiting. The call ends `failed` with code
`mcp_cancel_requested`: Fiber asked the server to stop, and the server may still
act. MCP says a server receiving that notice should not respond, so Fiber never
learns whether the server stopped, and the call is never `cancelled`
(`docs/tools.md`, "Cancellation").

A result goes through the tool contract in `docs/tools.md`:

- text goes to `content`
- an image is processed once (`docs/model-routing.md`, "Image limits") and
  goes to `content` as an image part, written to `artifacts/`
- an image that cannot be read, or is over 50 megapixels, ends the call
  `failed` with code `unsupported_file` and the decoder's message (or the
  pixel count)
- the 16 KiB cap applies, and a cut result keeps its full bytes in an artifact
- a result the server marks as an error ends `failed` with code `tool_error`

## Starting servers

Settled by [MCP servers start on first call](https://github.com/aakshintala/fiber/issues/257).

**A server starts on the first call to one of its tools.** The model sees its
tools from the session's first request all the same: they are declared from
the server's last tool list, cached in Fiber home at
`cache/mcp/<server>.json` (`docs/state.md`). The cache is keyed by a hash of
the server's declaration: its command, arguments, environment, or URL. The
first call waits for the server to start and list its tools, up to its startup
deadline. The deadline is 5 seconds by default, and configuration can change
it per server.

**Some servers start with the session:**

- a server marked `required`. A `required` server that misses its deadline,
  or fails to start, stops the session starting, and a headless run exits
  with code 1 and error code `mcp_required_server_failed`.
- a server with no cached list for its current declaration: the first time
  it is used, and after its declaration changes. It starts to get the list,
  and stays running.

The first request to the model waits for these, and a person can type while
they connect.

**A server that fails to start** is recorded as the durable event
`mcp_server_failed` (`docs/events.md`), whose `error.message` is Fiber's own
sentence and says what to do, such as how to log in, so a person and a
headless caller read the same advice. For a server started on a call, the
call fails with that error and the server is treated as one that died ("When
a server dies"). For a server started with the session for want of a cached
list, its tools are left out for the whole session.

**When a started server lists different tools from the cached list,** the
session keeps the tools it declared, because a tool set that changes
mid-session misses the whole prompt cache. The cache is updated, so the next
session declares the new list. A call to a declared tool the server no longer
has fails with `mcp_tool_removed`.

The 5-second deadline:

- The MCP specification sets no number. It says only "Implementations SHOULD
  establish timeouts for all sent requests".
- codex and Claude Code wait 30 seconds.
- The owner's three local servers took 82 to 105 ms median, 143 ms at most,
  from launch to their tool list (macOS arm64, `research/mcp-client/`).
- 5 seconds is Claude Code's connect timeout.
- A cold `npx -y` start or a slow OAuth exchange can miss it. That server's
  configuration raises it.

A stdio server starts in the session's workspace.

Each server pipe takes one parked thread, as `docs/architecture.md` ("The
threads") already budgets.

## When a server dies

A server that dies mid-session is restarted once, on the next call to one of its
tools. Each death is recorded as `mcp_server_failed`, and a restart that
brings it back as `mcp_server_ready`. If it dies again, it
stays dead for the rest of the session. Its tools
stay declared, so the prompt cache holds, and every call to them fails with code
`mcp_server_unavailable`.

A server's notice that its tool list changed is ignored until reload. A tool the
server removed fails with code `mcp_tool_removed`. A tool the server added is
not declared.

## Reload

`reload` is a driver command (`docs/invocation.md`). The terminal and every
driver have it, because `docs/architecture.md` gives the terminal no privilege
a second client lacks.

Reload:

1. Re-reads configuration.
2. Keeps unchanged, healthy servers connected, and asks each for its tools
   again.
3. Restarts changed servers and servers that died.
4. Starts every server not yet started this session, to get its tools.
5. Updates each server's cached tool list.
6. Reloads extensions (`docs/extensions.md`).
7. Declares the new tool set.

It costs one prompt-cache miss. The log records it as the durable event
`reloaded` (`docs/events.md`).

## Elicitation, sampling and roots

A server can ask the person a question in the middle of a call. This is MCP
elicitation, and Fiber answers it. An elicitation is an interaction like any
other: Fiber raises it on the event stream as one of the interactions
`docs/architecture.md` already names, logged as `interaction_requested` and
`interaction_resolved` (`docs/events.md`, "Interactions"), and any driver
answers it with `reply`.

Fiber advertises form elicitation. The server sends a form of typed fields.
Fiber asks each field in turn:

| Field type | Interaction |
|---|---|
| boolean | confirm |
| enum | select |
| string, number or integer | text input |

When no answer is possible, Fiber declines, which is a response MCP defines.
That is the same case as an escalation's block (`docs/permissions.md`,
"Headless"): a session started by `fiber ask`, or one that has been sent
`close`. Otherwise a pending elicitation waits for a client within the call's
timeout ("Calls"); an elicitation does not extend it. An elicitation in a
delegate reaches a person the way the delegate's escalations do
(`docs/permissions.md`, "Delegates").

On stdio, an elicitation carries nothing that links it to the call that raised
it: the MCP TypeScript SDK 1.29.0 passes `relatedRequestId` to its transport
(`shared/protocol.js:337`), and the stdio transport's `send(message)` drops it
(`server/stdio.js:63`). Each session has its own servers ("Where servers
run"), so an elicitation always belongs to the session that started the
server. When that session has more than one call in flight on the server, its
log records the elicitation with the `action_id` of each.

Fiber does not advertise sampling and does not answer a sampling request.
Neither codex nor Claude Code advertises it.

Fiber does not advertise roots. codex advertises none; Claude Code does.

## A repository's servers

A server declared in a repository's configuration runs a program, so it needs
a person's approval before it starts. The session that would start it raises
the approval as an interaction before its first model request, showing the
server's declaration, so any client can answer it (`docs/tui.md`, "Approving
a repository's MCP servers"). The approval covers the server's exact
declaration: a changed declaration needs a new approval, and approvals are
recorded per machine in Fiber home (`docs/state.md`). A headless run fails
with `mcp_server_unapproved` if a repository declares a server nobody has
approved. `fiber approve`, run in the repository from a terminal, records the
approvals.

A repository's servers load per repository, unlike extensions, which never
load from one (`docs/extensions.md`, "Extensions in a repository").

The approval covers what the declaration says to run, not the program itself. A
declaration that fetches its program at start, such as `npx -y`, can run
different code later under the same approval.

## Error codes

The `mcp_*` codes are listed with every other code in `docs/errors.md`.

## Configuration

Each server has:

- a command, arguments and environment, for a stdio server
- a URL, for a remote server
- `required`
- a startup deadline
- a call timeout
- whether its tools are declared in full (`declare_in_full`)
- per-tool hint overrides
- which tools are enabled and which are disabled

The keys are `docs/configuration.md` ("MCP servers").

## Where servers run

Every session is one process ([ADR 0009](adr/0009-each-session-is-one-process.md)),
and every session starts its own servers, a delegate included. Nothing is
shared between sessions.

- A server starts in the workspace of the session that started it, so a
  delegate in a worktree gets servers that work in that worktree. The owner's
  cursor-delegate server works in the folder it started in unless a call names
  another, and node_repl keeps a JavaScript kernel whose variables persist
  between calls. Neither would be correct shared between sessions.
- A server takes 43 to 93 MiB written in Node and about 1.2 MiB written in
  Rust (macOS arm64, `research/delegate-memory/README.md`, "MCP servers"),
  once per session that calls it. Fiber assumes the heavy end: a delegate
  that calls two Node servers adds about 100 MiB. A server a session never
  calls costs it nothing, because servers start on their first call.
  A server that costs too much to run once per session is its author's to make
  smaller; Fiber does not share servers to hide the cost.
- A delegate starts its own servers as any session does ("Starting
  servers"). Servers are not moved into the hub: a hub restart would restart
  them under running sessions, and the two above would be wrong shared.

## Not settled here

- How a person invokes a prompt template
- Where a server's OAuth token is stored
- How MCP content other than text and images, such as audio and resource
  links, reaches the model
