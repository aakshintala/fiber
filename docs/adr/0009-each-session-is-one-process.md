# 9. Each session is one process; the hub holds none

Date: 2026-09-30

## Status

Accepted. Settled by
[Process architecture: core, TUI and shared services](https://github.com/aakshintala/fiber/issues/81)
and [Control center: one hub, headless sessions, clients over it](https://github.com/aakshintala/fiber/issues/256).
The contract is `docs/invocation.md` ("Processes", "The hub"). What pi, codex,
opencode and Claude Code do is `research/process-architecture/README.md`. The
memory figures are `research/delegate-memory/README.md`.

## Context

Fiber is a set of headless sessions, many on one machine, and a set of
clients that act as a control center over several at once: the terminal, a
GUI, a phone or a web page. A client must be able to list sessions, start
one, attach to one and see which are waiting on the person, with no terminal
open. Something must therefore listen while no session runs.

The owner runs several top-level sessions at once. Across their Claude Code
and pi logs, top-level sessions active in the same 10 minutes are p50 1, p90 3,
p99 4, max 8, and 12.7% of active windows have 3 or more.

codex runs every session inside one host-wide daemon that its TUI starts
automatically. Its `codex app-server daemon update` "may interrupt work".
Claude Code runs one process per session and a separate `remote-control`
server started by hand. opencode runs its server and TUI as two threads of one
process. None of the four runs a sub-agent as its own process.

A delegate costs about 2 MiB as threads and 8 to 9.5 MiB as its own process:
about 7 MiB more per delegate, or about 56 MiB at the owner's measured peak of
8. An MCP server costs 43 to 93 MiB written in Node and about 1.2 MiB written
in Rust (macOS arm64). Fiber does not choose its users' servers, so the design
assumes the heavy end.

## Decision

- Every session is one process, running the internal session command, a
  delegate included. The hub starts one for a client, `fiber ask` is one, and a
  parent starts one for each Fiber delegate.
- No session has a pipe driver. Every driver is a client over the session's
  socket. `fiber ask`'s stdout is a watcher.
- A Fiber delegate is a child process of its parent, as a delegate on any
  other harness is. The parent is an ordinary client over the delegate's
  socket, and holds a pipe to the delegate's stdin that it never writes to.
  End of file on that pipe starts the delegate's shutdown.
- Every session starts its own MCP servers and process extensions, a
  delegate included. Nothing is shared between sessions (`docs/mcp.md`,
  "Where servers run"; `docs/extensions.md`).
- An image child is a short-lived process, one per image. The session
  starts it by running `fiber` again with an internal command, not a door.
  The session process runs no image code (`docs/invocation.md`,
  "Processes").
- Every running session listens on a local socket.
- One hub is the process every client talks to, the local terminal included.
  It holds no session. It lists sessions, starts and resumes them, relays
  every client connection to a session's socket, and serves one feed of
  every live session's status.
- A client starts the hub when none is running; that hub never listens
  remotely and exits once no client has been connected for a while.
  `fiber hub install` registers it as a login service that listens on an
  address the person chose.
- The terminal UI is its own process, a client of the hub.
- Fiber ships no hosted relay service. The person brings the network.

## Consequences

- One process is one session: one log, one lock, one socket, one set of
  Lua VMs, its own MCP servers and process extensions. Nothing in the process
  has to ask which session it is in, and no MCP server or extension serves two
  sessions.
- The Fiber harness has the same shape as every other harness: a child
  process with a prompt in and an event stream out.
- Stopping a delegate is a signal to its process group, which always works.
  A crash in C code (Lua, ring), an out-of-memory kill or a panic ends one
  delegate and nothing else. An out-of-memory kill in an image child ends
  that child, not the session. A delegate whose parent died stops within
  the shutdown bound, because the kernel closes its lifeline.
- Every client, local or remote, has one address, one protocol and one place
  where it is authenticated. Locally, being the account that owns Fiber home
  is the authentication; remotely, a token.
- A hub crash or restart drops client connections and ends no session.
  `fiber update` restarts it without stopping any session. A running session
  keeps its binary until it exits.
- A TUI crash, or a TUI extension's error, cannot interrupt a session's work.
- The TUI can use only what the hub relays from a session's socket, so
  premise 5 holds by construction.
- Every line a client receives crosses one extra hop, measured at about
  0.5 µs of CPU a line (Apple M3 Pro, macOS 26.6,
  [#226](https://github.com/aakshintala/fiber/issues/226#issuecomment-5906071958)).
- No two sessions share anything in memory. Each pays for its own MCP
  servers, process extensions and model catalog: with two Node MCP servers a
  delegate adds about 100 MiB, about 1 GiB at the measured peak of 8
  delegates on macOS; with one Rust server, about 10 MiB. Servers start on
  their first call (`docs/mcp.md`), so a session pays only for the servers it
  uses. A server or extension too heavy to run once per session is its
  author's to make smaller.
- A stdio MCP server's elicitation carries no link to the call that raised
  it, but since only one session uses a server, the elicitation always
  belongs to that session (`docs/mcp.md`, "Elicitation, sampling and
  roots").

## Rejected

A delegate as threads in its root's process. It saves about 7 MiB per
delegate, less than one Node MCP server at the measured peak, and it does not
reduce the MCP cost, since servers stay per session. It makes the session
process a multi-session process: several loops, logs, locks and sockets in one
process, a model catalog and an MCP client called from several loops at once,
a Lua VM per extension per session in one process, and a stop that is a flag
native code need never check. A crash in C code or an out-of-memory kill
would end every session in the tree, and a delegate stuck in native code could
not be stopped without ending the tree, which
[Shutdown](https://github.com/aakshintala/fiber/issues/34) forbids. It would
also make the Fiber harness a different shape from every other harness. The
costs a tree of separate processes adds, the spend copies and the lifeline,
are a tree of logs' costs or are needed for other harnesses anyway.

A hub that runs every session, as codex's daemon does. It gives cheap
sessions as threads and MCP sharing across the whole host. Restarting it for
an upgrade stops every model stream and child process in every session, and
one crash ends every session on the host.

A hub that only finds sessions and hands back their socket paths, with
local clients connecting straight to a session. It survives a hub restart
without dropping anyone, but remote clients still need relaying, which leaves
two transports and two ways of authenticating a client.

No hub, with each client listing `~/.fiber/run/` and spawning sessions
itself, and a daemon only for remote clients. It is the design this ADR
replaces. It gave three different things that start sessions, two transports,
per-project session lists, and a terminal that was the special client of the
session it spawned.

A parent relaying its delegates' streams onto its own, with commands
forwarded down the tree by `session_id`. Deltas are 4 to 14 times the bytes of
the durable lines in the owner's sessions, every root client received them
for content it did not show, and a late client still learned a delegate's
history only from the delegate's log
([#226](https://github.com/aakshintala/fiber/issues/226)).

Sharing MCP servers across sessions, in the hub or in a root shared by its
delegates. It would save about 80 to 100 MiB per extra concurrent session with
two Node servers, but only in about 1 active window in 8. It was wrong for two
of the owner's three local servers: cursor-delegate works in the folder it
started in, so a delegate in a worktree got the root's folder, and node_repl
keeps a JavaScript kernel whose variables every sharing session would see. It
also needs a rule for an elicitation no one can attribute, and a hub restart
would restart servers under running sessions. Process extensions follow the
same rule for the same reasons.

A hosted relay, as Claude Code's Remote Control uses. It works from any
network with nothing installed on the phone. Fiber would have to run the
service, and without end-to-end encryption it would see every event and
command. The hub is not this: it runs on the person's machine, as their
account.

The TUI in the session's process, as Claude Code and pi do. It is one process
with fewer parts. A TUI crash or a TUI extension's error would end the
session, and one terminal could not show several sessions.
