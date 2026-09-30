# 9. Each session is one process; the daemon holds none

Date: 2026-09-24

## Status

Accepted. Settled by
[Process architecture: core, TUI and shared services](https://github.com/aakshintala/fiber/issues/81).
The contract is `docs/invocation.md` ("Processes"). What pi, codex, opencode
and Claude Code do is `research/process-architecture/README.md`. The memory
figures are `research/delegate-memory/README.md`.

## Context

A web or mobile client must be able to attach to a session on the host and
start a new one, with no terminal open. Something must therefore listen while
no session runs.

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

- Every session is one `fiber serve` process, a delegate included. A Fiber
  delegate is a child `fiber serve` of its parent, driven over the pipe it was
  spawned with, exactly as a delegate running another harness is.
- Every session starts its own MCP servers and process extensions, a
  delegate included. Nothing is shared between sessions (`docs/mcp.md`,
  "Where servers run"; `docs/extensions.md`).
- An image child is a short-lived process, one per image. The session
  starts it by running `fiber` again with an internal command, not a door.
  The session process runs no image code (`docs/invocation.md`,
  "Processes").
- The terminal UI is its own process, client zero of the `fiber serve` it
  spawns, over that process's stdin and stdout.
- Every running session listens on a local socket for further clients.
- `fiber remote` is an optional daemon for remote clients. It starts and
  resumes `fiber serve` processes and relays clients to them. It holds no
  session.
- Fiber ships no relay service. The person brings the network.

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
  that child, not the session.
- `fiber upgrade` can restart `fiber remote` without stopping any session.
  A running session keeps its binary until it exits.
- A TUI crash, or a TUI extension's error, cannot interrupt a session's work.
  The session then follows the lifecycle rule like any session without a
  client.
- The TUI can use only what the pipe and the socket carry, so premise 5 holds
  by construction.
- No two sessions share anything in memory. Each pays for its own MCP
  servers, process extensions and model catalog: with two Node MCP servers a
  delegate adds about 100 MiB, about 1 GiB at the measured peak of 8
  delegates on macOS; with one Rust server, about 10 MiB. A server or extension too heavy to run once
  per session is its author's to make smaller.
- A stdio MCP server's elicitation carries no link to the call that raised
  it, but since only one session uses a server, the elicitation always
  belongs to that session (`docs/mcp.md`, "Elicitation, sampling and
  roots").

## Rejected

A delegate as threads in its root's process. It saves about 7 MiB per
delegate, less than one Node MCP server at the measured peak. It makes
`fiber serve` a multi-session process: several loops, logs, locks and sockets
in one process, a model catalog and an MCP client called from several loops at
once, a Lua VM per extension per session in one process, and a stop that is a
flag native code need never check. A crash in C code or an out-of-memory kill
would end every session in the tree, and a delegate stuck in native code could
not be stopped without ending the tree, which
[Shutdown](https://github.com/aakshintala/fiber/issues/34) forbids. It would
also make the Fiber harness a different shape from every other harness.

A daemon that runs every session, as codex does. It gives one address for
every client, cheap sessions as threads, and MCP sharing across the whole
host. Restarting it for an upgrade stops every model stream and child process
in every session, and one crash ends every session on the host.

Sharing MCP servers across session trees through the daemon. It would save
about 80 to 100 MiB per extra concurrent session with two Node servers, but
only in about 1 active window in 8. It adds five costs: elicitations that cannot be attributed across
sessions, a reload that needs a private instance, a daemon restart that
restarts servers under running sessions, a daemon crash that removes MCP from
every session, and a second code path for sessions started without the daemon.
Sharing across trees would need the relay that sharing within a tree was
rejected for, below.

MCP servers owned by the root and shared by its delegates. The root started
each server once, and a delegate sent its calls up through its parent. With two
Node servers it used 187 MiB against 1.0 GiB with servers per session at the
peak of 8 delegates (macOS arm64, `research/delegate-memory/README.md`). It was wrong for
two of the owner's three local servers: cursor-delegate works in the folder it
started in, so a delegate in a worktree got the root's folder, and node_repl
keeps a JavaScript kernel whose variables every sharing session would see. It
also needed three relay messages, a delegate's tool set given to it by its
parent, and a rule for an elicitation no one could attribute. Process
extensions follow the same rule for the same reasons.

A hosted relay, as Claude Code's Remote Control uses. It works from any
network with nothing installed on the phone. Fiber would have to run the
service, and without end-to-end encryption it would see every event and
command.

The TUI in the session's process, as Claude Code and pi do. It is one process
with fewer parts. A TUI crash or a TUI extension's error would end the session.

The TUI attaching to its own session over the socket rather than the pipe. It
would need the TUI to wait for the socket to appear, and the session to know
it was started without a stdin driver. The pipe is there at spawn and carries
the same bytes.
