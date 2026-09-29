# 8. The MCP client is built in

Date: 2026-09-23

## Status

Accepted. Settled by
[MCP client](https://github.com/aakshintala/fiber/issues/22). The contract is
`docs/mcp.md`. What pi, codex and Claude Code do is
`research/mcp-client/README.md`.

## Context

MCP is the industry standard for giving an agent tools from outside. The owner
uses many MCP servers on a work machine. Fiber serves no MCP
([ADR 0005](0005-no-mcp-server-the-supervisor-is-external.md)); this decision is
about consuming servers.

The owner considers extensions better than MCP. An extension registers through
the same seams a built-in does, carries its own effects function, and runs in
Fiber's process with no protocol in between (`docs/extensions.md`). That is why
building an MCP client into the binary is the surprising choice.

The three references differ. codex and Claude Code both build the client in,
name each tool `mcp__<server>__<tool>`, and read the server's tool hints. pi's
core has no MCP. The third-party `pi-mcp-adapter` package adds it and exposes
one proxy tool, `mcp`, in place of every server tool.

## Decision

Fiber's MCP client is native Rust in the binary, like the wire protocols
([ADR 0007](0007-protocols-are-native-providers-are-extensions.md)). It
registers each server tool through the tool seam, so an extension can replace
any of them by name, as it can any built-in. Each server tool is its own Fiber
tool, deferred by default where the protocol supports deferral.

## Consequences

- The binary carries MCP's protocol, its two transports and its OAuth flow. A
  fix to any of them needs a Fiber release.
- A server can start at session start, keep a child process with pipes open,
  hold a streaming HTTP connection and ask the person a question during a call.
  A process extension can do all of this too, but it speaks Fiber's own
  protocol, not MCP (`docs/extensions.md`). MCP stays the way to bring in
  tools from the ecosystem.
- Where servers run is [ADR 0009](0009-each-session-is-one-process.md)'s:
  every session starts its own.
- MCP tools declare effects per tool, from hints, which is weaker than an
  extension's per-call effects function (`docs/mcp.md`, "Effects").

## Rejected

A first-party Lua extension, as pi does it. It gained two things: MCP fixes
arrive as extension updates without a binary release, and the binary holds no
protocol code. It needed four capabilities a Lua extension lacked:

- a long-running child process with pipes
- a streaming HTTP connection
- asking the person from inside a tool call
- starting at session start, where `docs/extensions.md` has a VM "created the
  first time the extension is invoked, not at startup"

A process extension now has all four (`docs/extensions.md`), so this is
possible. It is still not how Fiber does it: every session would run the
extension as one more process, for a protocol every MCP server shares.

No MCP client at all. MCP is the industry standard and the owner uses many MCP
servers at work. Leaving it out invites someone to build an adapter, which is
this decision's rejected extension built by someone else.

One proxy tool for every server, as `pi-mcp-adapter` exposes. It keeps the
model's tool list to one entry however many servers are configured. It loses
discoverability, the argument ADR 0005 makes for MCP itself: "An MCP tool
arrives in a calling agent's catalog with a name, a schema and a description,
and is callable without anyone having explained it." Behind a proxy, a tool is
named only inside another tool's results. It also leaves the permission check
one tool wide. Per-tool deferral keeps the list small where a protocol supports
it, and keeps every tool's name in front of the model.
