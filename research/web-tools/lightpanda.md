# Lightpanda as a JavaScript-rendering MCP server

Probed 2026-10-07 on macOS arm64, at the owner's request. Sources are
Lightpanda's README and `lightpanda fetch --help` at the version below, and
live runs. Every memory and time figure is macOS arm64 and is not a Linux
figure.

## What it is

Lightpanda (https://github.com/lightpanda-io/browser) is a headless browser
for agents and automation, written in Zig, with V8 for JavaScript and
html5ever for parsing. It is not built on Chromium, Blink or WebKit. Its
licence is AGPL-3.0. Builds are nightly. The version probed was
`1.1.0-nightly.10207+92184a50d`, installed with
`brew install lightpanda-io/browser/lightpanda` (91.3 MB on disk). Builds
exist for Linux and macOS, x86_64 and aarch64, but not natively for Windows.

It runs in four ways: `fetch` (dump one page as HTML, Markdown, PNG, PDF or a
semantic tree), `serve` (a Chrome DevTools Protocol or WebDriver BiDi server),
`agent`, and `mcp`. It sends usage telemetry by default, which
`LIGHTPANDA_DISABLE_TELEMETRY=true` turns off. Every run below set it.

## MCP transports

`lightpanda mcp` speaks MCP over stdio, or over streamable HTTP with
`--port`. The server reports `lightpanda 0.1.0` and protocol `2025-06-18`. It
offers 32 tools. They include `goto`, `markdown`, `html`, `links`,
`evaluate`, `click`, `fill`, `screenshot`, `search`, and `session_new`,
`session_list` and `session_close` for separate browsing contexts.

Fiber on origin/main (`7720b0fb`) starts stdio servers from `mcp.servers` in
the configuration (`crates/main/src/mcp_servers.rs`). It skips servers with a
`url`, because the HTTP transport is #593. `fiber mcp add` is not built yet,
so the server was declared by writing `config.json`:

```json
{"mcp":{"servers":{"lightpanda":{"command":"lightpanda","args":["mcp"],
  "env":{"LIGHTPANDA_DISABLE_TELEMETRY":"true"},"declare_in_full":true,
  "startup_timeout_ms":10000}}}}
```

A `fiber ask` turn on `anthropic/claude-haiku-4-5` saw the tools as
`mcp__lightpanda__*` and called `goto`, then `markdown`, successfully.

## Fetching a static page and a JavaScript page

Two pages:

- https://example.com/, which is static
- https://hn.algolia.com/, whose HTML holds 37 characters of visible text and
  whose stories load with JavaScript

| Page | Fiber `web_fetch` result | Lightpanda Markdown | Stories present |
|---|---|---|---|
| example.com | 430 characters, with Fiber's header line | 1,344 bytes (`fetch`); 960 characters (MCP) | not applicable |
| hn.algolia.com | 298 characters: the header line and "Hacker News Search powered by Algolia" | 19,884 bytes (`fetch`, `networkidle`); 16,602 to 19,824 characters (MCP `goto` with `networkidle`, then `markdown`) | `web_fetch`: none. Lightpanda: 25 |

On example.com, Lightpanda's Markdown also holds Arabic and Chinese versions
of the paragraph, which the raw HTML does not contain.

The MCP `markdown` tool with a `url` argument waits only for `load`. On
hn.algolia.com that returned 810 characters and no stories. The stories
appear only after `goto` with `waitUntil: "networkidle"` followed by
`markdown`.

## Memory and time

Peak memory footprint (`/usr/bin/time -l`), macOS arm64, three runs each:

| Run | example.com | hn.algolia.com |
|---|---|---|
| `lightpanda fetch --dump markdown` | 9.8 MiB, 0.17–0.18 s | 45.7–47.4 MiB, 1.64–1.75 s |
| `lightpanda mcp`, one `goto` (`networkidle`) and one `markdown` | 9.8 MiB | 46.3–46.9 MiB |

This is the Lightpanda process alone, a separate process from the Fiber
session.

## Bearing on Fiber

- Licence: `docs/dependencies.md` excludes copyleft licences that reach
  beyond the file, AGPL included, so Lightpanda cannot be a dependency of
  Fiber. Running it as a separate program that a person installs and declares
  as an MCP server does not link it.
- Memory: one JavaScript page cost about 46 MiB in Lightpanda's own process on
  macOS arm64. `docs/performance.md` budgets 137 MiB peak RSS (Linux x86_64)
  for a busy session, and a stdio MCP server's memory is its own process's.
- Today: a person can declare it under `mcp.servers` on origin/main and get
  JavaScript-rendered pages, which `web_fetch` does not render.
- Its `click`, `fill`, `press` and `evaluate` tools are browser control. They
  bear on any later computer-use work.

## Found in passing

Two Fiber behaviours seen during the probe, unrelated to Lightpanda:

- The reviewer's first stage allows one output token
  (`crates/loop/src/reviewer.rs`, `Some(1)`; `docs/permissions.md`, "The
  reviewer"). With `anthropic/claude-sonnet-5-5` as the reviewer, both
  reviewed calls were denied with "expected one word, `check` or `allow`, but
  got \"all\"".
- A tool call streamed with empty arguments, here `markdown` with no
  arguments, was logged with `arguments: ""` and sent back to Anthropic as
  `tool_use.input: ""`. Anthropic answered HTTP 400 ("Input should be an
  object") and the turn failed.

The comparison runs above used standing allow rules in a throwaway Fiber home
to get past the first.
