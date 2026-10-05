# 10. Fiber never phones home

Date: 2026-10-04

## Status

Accepted. Settled by
[Observability: what Fiber records about itself outside a session](https://github.com/aakshintala/fiber/issues/60).

## Context

Many tools send usage data, crash reports or analytics to their developers,
often unless the person opts out. Claude Code and codex both ship telemetry
export, though to the operator's own collector. Fiber also runs as the core of
a personal assistant whose promise is that a person's data is not handed to
any one company, and as a worker in a software factory run by someone other
than Fiber's developers.

## Decision

No usage data leaves the machine except what the person's own configuration
sends. What does leave is all of the person's choosing:

- model requests, to the providers they configured, and the model lists and
  quota those providers' extensions fetch (`docs/model-routing.md`)
- tools that reach the network, such as `web_fetch`, when the model calls them
  and the permission order allows it (`docs/permissions.md`)
- MCP servers and extensions the person installed or approved, which run with
  the account's rights (`docs/mcp.md`, `docs/extensions.md`)
- `fiber update` and extension installs, when the person runs them
  (`docs/releasing.md`)
- clients and exporters the person connects to their hub

Fiber sends nothing to its developers: no analytics, no usage counts, no crash
reports. A crash file and the diagnostic logs stay in Fiber home
(`docs/state.md`, "What each part holds"). Nothing checks for updates on a
timer.

## Consequences

- Fiber's developers learn how it is used only from what people choose to tell
  them, such as a bug report with a crash file attached.
- Exporting metrics or traces to the operator's own monitoring is not phoning
  home, since the operator configures where it goes. It belongs outside the
  core, built from the session event stream and the diagnostic log.
- A feature that would send data anywhere the person did not configure is
  refused, whatever it would teach Fiber's developers.
