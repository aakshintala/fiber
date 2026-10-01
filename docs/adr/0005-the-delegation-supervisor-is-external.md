# 5. The delegation supervisor is external

Date: 2026-09-21

## Status

Accepted. Settled by
[Front doors: which invocation modes does v0.0.1 have?](https://github.com/aakshintala/fiber/issues/10).
The contract is `docs/invocation.md`. The one MCP server Fiber ships, for
session messages, was added by
[Intercom: messaging a session you did not start](https://github.com/aakshintala/fiber/issues/78).

## Context

Map premise 2 reads: "v0.0.1
must support both interactive sessions, as pi does, and non-interactive
sessions for delegated work, as pi, codex and cursor-agent each offer a
headless mode... A non-interactive caller may have several delegations
outstanding at once."

That makes being delegated to a thing v0.0.1 owes, which raises the question
of how much of the delegation machinery belongs inside Fiber. The test
for it: "if fiber can do everything that we
built cursor-delegate around the cursor-agent CLI, then just the CLI is enough.
if not, include MCP."

[What does cursor-delegate add around the cursor-agent CLI?](https://github.com/aakshintala/fiber/issues/2)
answered the capability half: of roughly 25 capabilities, "all but four are
orchestration and policy around a single spawn", and the four that remain "do
not apply when the caller is shell-capable".

Two facts outweigh that finding.

**That finding was about capability, not discoverability.** An MCP tool arrives
in a calling agent's catalog with a name, a schema and a description, and is
callable without anyone having explained it. A CLI arrives as nothing: it needs
a skill whose description happens to match, or an injection at session start,
and if neither fires the tool may as well not be installed. Premise 2
makes that gap matter, because a slot Fiber must serve is one a
caller has to be able to find.

**cursor-agent keeps a durable record too.** It is `~/.cursor/projects/<project>/agent-transcripts/<id>/<id>.jsonl`, one
JSON object per line, ending in `{"type":"turn_ended","status":"success"}` —
143 of them for this project alone. Both agents write a log. As the owner put it: "Having to scrape that log ad-hoc each time is stupid when
it can be done by the tool instead." A log is not an interface, and the
existence of one is no argument against shipping an API over it.

## Decision

Fiber ships no MCP server for delegating to Fiber. The supervisor that spawns delegations, tracks
several at once, waits on them, cancels them and runs verification gates lives
outside Fiber, in a tool that wraps both `fiber` and `cursor-agent`.

Fiber's entire obligation to the delegation slot is being cleanly wrappable:
no tty required, no terminal escape codes on stdout, the session id on the
first line, a documented and versioned event stream, stable exit codes, and a
workspace path it runs in rather than creates.

Fiber is an MCP client, consuming tool servers, and that client is built in
([ADR 0008](0008-the-mcp-client-is-built-in.md)). This decision is only about
serving.

Fiber ships one MCP server, for a different job: `fiber mcp serve`, which
gives a delegate running on another harness `session_list` and
`session_message` (`docs/delegates.md`, "Delegates on another harness"). It is
the stateless kind rejected below for delegation. It reads `~/.fiber/run/` and
writes to one socket, and it lists no tools unless a Fiber parent started it.
This ADR's own discoverability argument is why it is a server and not a
command. A delegate on Claude Code already has `SendMessage` and `ListAgents`,
which reach Claude Code's subagents and sessions, never Fiber's. A command named
only in a prompt would lose to those tools. Two tools in the catalog, each
saying what it reaches, do not.

## Why

**Four of the five things a supervisor does are not Fiber-shaped.** Spawning
with a model, a permission mode and a workspace is Fiber's own flags. Tracking
N outstanding jobs, listing them, waiting on any of them, running a gate,
reporting a git change set, and presenting tools to a calling agent are all
identical for any agent binary. Only the spawn row is about Fiber.

**Two supervisors is worse than one.** cursor-delegate keeps supervising
cursor-agent either way. A `fiber job` command group that knew only about Fiber
would leave two supervisors with different semantics for the single job of
"I have four delegations outstanding". One wrapper over both is fewer moving
parts, and the wrapper is where that logic was already proven.

**A stateful MCP server collides with premise 4.** cursor-delegate holds its
job registry in the memory of a long-lived process, and
[#2](https://github.com/aakshintala/fiber/issues/2) records that it loses jobs
when that process restarts. A Fiber equivalent would be a long-lived process
holding session state nothing else can see — a daemon that holds sessions,
against [ADR 0009](0009-each-session-is-one-process.md), where the hub holds
none — or a second authority beside the session log, against ADR 0001.

**The discoverability gap is real but is not Fiber's to close.** It is a
property of the calling host, not of the agent being called. One wrapper
exposing both agents closes it once for both; a Fiber-only MCP server would
close it once for one, and add a second protocol to v0.0.1 for a host the
owner does not use.

## Consequences

- A calling agent cannot discover Fiber without a wrapper or a skill. This is
  accepted, and it is the cost being paid.
- Nothing in Fiber tracks a delegation made to it from outside. The caller's
  handle is the session id. There is no status file, no heartbeat, and nothing
  that can drift from the log. Fiber's own background jobs and delegates
  (`docs/tools.md`, `docs/delegates.md`) are a different thing.
- `cancel` across several runs, `wait-any`, `list` and gate execution are the
  wrapper's, and Fiber publishes no API for them.
- Nothing kills a wedged Fiber from inside. `timeout(1)`, or the wrapper's own
  watchdog, covers it.
- If the wrapper is later found to need something Fiber does not expose, that
  is an addition to `docs/invocation.md`, not a reason to revisit the server.

## Rejected

**A `fiber job` command group** — `start --background --gate`, plus
`poll`, `wait`, `cancel`, `list`. It would be cheap, because the session log
makes every one of those a pure fold with no state of its own, and it would
put the gate somewhere it cannot be forgotten, which convention alone has not
achieved. Rejected on the two-supervisors argument: the same logic has to exist
in the wrapper anyway for cursor-agent, and having it in both places is the
cost.

**A stateless `fiber mcp`** answering every call by reading the same session
directories the CLI reads. This does not violate premise 4 the way a registry
would, and the MCP client is built in
([ADR 0008](0008-the-mcp-client-is-built-in.md)), so the protocol and framing
are already in the binary, making the server half small. Rejected because it closes the discoverability gap for one
agent where the wrapper closes it for both, and because premise 1 rules out
what is wanted eventually but not needed to start using Fiber daily.

**Extending cursor-delegate as-is.** Not rejected — it is the accepted path,
renamed to reflect that it wraps more than one agent. It is outside this
repository and outside this map.
