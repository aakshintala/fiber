---
name: using-fiber
description: Answer a question about Fiber itself, such as how to configure it, what a tool or permission does, how extensions, skills or events work, or what an error means, from the documentation installed with Fiber and not from memory.
---

# Using Fiber

Fiber's documentation is installed with the binary, in `docs/` under Fiber home. Fiber home is `~/.fiber` unless the person has moved it (`docs/state.md`). The installed docs match the installed version, so they are the authority on what this Fiber does. Read them with your file tools; do not answer a question about Fiber from memory.

## Where to look

- `docs/user/` is for using Fiber: installing, `fiber doctor`, logging in, choosing a model, a first prompt, permissions, and where to get help. Start here for a how-do-I question.
- The other files in `docs/` say how Fiber works. Pick the one that matches the question:
  - settings and what a key does: `docs/configuration.md`
  - the tools the model has, and their arguments: `docs/tools.md`
  - what is allowed, asked or refused: `docs/permissions.md`
  - extensions and the extension API: `docs/extensions.md`
  - the session log and every event: `docs/events.md`
  - an error code, or what to do about it: `docs/errors.md`
  - skills, the system prompt and the opening message: `docs/system-prompt.md`
  - the prompt cache and warming: `docs/prompt-cache.md`
  - what Fiber writes to disk: `docs/state.md`
  - commands and flags: `docs/invocation.md`
  - MCP servers: `docs/mcp.md`
  - delegates and forks: `docs/delegates.md`

## How to answer

1. List `docs/` in Fiber home, open the page that fits, and read the section the question is about.
2. Quote the doc's rule or name the key, command or code it gives. Say which file it came from.
3. A doc can describe behaviour the binary does not have yet, because the docs are written before the code. When a doc and what you observe disagree, say so.
4. When `docs/` is missing from Fiber home, as for a binary copied without the installer, the docs are not installed. Say so, and send the person to the Fiber website, where the same pages are published.
