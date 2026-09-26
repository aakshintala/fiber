# Prompt cache

Every provider Fiber talks to caches the start of a request. A later request
whose leading bytes match reads that start from the cache at a tenth of the
input price or less. A request whose bytes differ from some point on pays full
price, or more, for everything after that point. Fiber keeps those bytes stable
by design, and every change to them is a named event in the session log.

Vocabulary is `CONTEXT.md`: prompt cache, preamble, session, turn, step.

## What a request is built from

A request is the preamble, then the conversation.

The preamble is the system prompt, the tool definitions and the request
settings: model, effort, thinking, `tool_choice` and cache lifetime. The
conversation is everything the session log adds after it, rendered in log
order.

The conversation is a projection of the log. It is byte-stable only over what
the log holds. Anything a request reads from outside the log, such as a file on
disk, the clock or a server's tool list, can change the bytes on resume, on a
fork or when a server reconnects. So:

- the preamble is logged whenever it is built
- nothing in the conversation is rendered from the clock or from state outside
  the log: wake inputs, notices, monitor batches, the jobs line and the nudge are
  built from logged fields, and the envelope `ts` never reaches the model
- the conversation only grows at its end, apart from a handoff
  (`docs/handoff.md`)

## The preamble

The preamble is built at four points, and only these:

| Point | What happens |
|---|---|
| Session start | Built before the first request. |
| Resume | Built again from current inputs: the person's system prompt files, extension prompt texts, the model's addendum, the servers' tool lists. Instruction files and the date are not inputs: they live in the logged opening message (`docs/system-prompt.md`). If nothing changed, the bytes match and the cache still hits. |
| `reload` | Built again with the new tool set (`docs/mcp.md`, "Reload"). |
| Switch | Built again with the new model, effort or thinking ("Switching model"). |

Each build is logged as `preamble_built` (`docs/events.md`), carrying the
reason, the request settings, the system prompt text and the full tool
definitions as sent. A fork or a rewind sends the latest build before its
point, so its first request matches its parent's byte for byte.

Between builds the preamble does not change. What varies by project or by day,
such as instruction files, the environment and the skills listing, is in the
opening message, which is logged once. A change to any of it reaches the model
as a message appended at the end, never as an edit (`docs/system-prompt.md`).

## Bytes

Two requests built from the same inputs are the same bytes:

- tools go in one list sorted by name, built-ins and MCP tools together
- every JSON object in a tool definition is serialised with its keys sorted,
  including a schema an MCP server supplied
- no value reaches a request through a hash map's iteration order
- serde_json's `preserve_order` feature is never enabled; a CI check fails the
  build if any dependency turns it on, because Cargo features apply to the whole
  build

Probed ([research/prompt-cache/probes.md](../research/prompt-cache/probes.md)):
a Rust `HashMap` serialised the same eight names in a different order in each
of three runs. The same schema with its keys in another order missed the cache
on GPT-6 Luna and on Muse Spark.

## Tools

The tool set does not change between builds. Changing, adding, removing or
reordering a definition misses the whole cache on every provider probed.

- Every extension that registers a tool runs that registration before the
  session's first request. For such an extension, first use is session start
  (`docs/extensions.md`).
- A tool that stops working stays declared and fails its calls with a stable
  code, as MCP tools do when their server dies (`docs/mcp.md`).
- `tool_choice` does not change during a session. Changing it misses the cache
  for every message after the preamble.
- A list that can change during a session, such as model references, roles or
  quotas, goes in a tool result, never in a tool definition
  (`docs/delegates.md`, "Choosing a model").

### Deferred tools

On a model that supports deferral, deferred tools are declared with
`defer_loading`. A deferred tool sits outside the cached prefix, and the model
loading one appends it to the conversation, so the cache holds. Anthropic and
OpenAI Responses (gpt-5.4 and later) defer natively; OpenAI documents that
"Tool search is designed to preserve the model's cache". Probed: on Muse, a
request that loaded a deferred tool read 6,385 of its 6,980 input tokens from
the cache. Which models defer and which tools are deferred is
`docs/tools.md`, "Which tools the model sees". OpenAI's `allowed_tools`
restricts which tools may be called but still sends their definitions; it is
not deferral.

## Cache markers and keys

Anthropic caches up to a marked block and looks back at most 20 positions for an
earlier cache entry. A run of consecutive tool calls counts as one position, and
so does a run of results. Each Anthropic request carries up to three markers:

- the end of the system prompt
- the point where the previous request ended
- the new end

The second marker keeps a long stretch of appended messages within the
lookback. Probed: 12 appended exchanges with only an end marker lost the cache
for every message.

Providers that route by key are given the root session's id:

| Provider | Field |
|---|---|
| OpenAI and ChatGPT/codex | `prompt_cache_key` |
| OpenRouter | `session_id` |
| OpenCode Go | the `x-opencode-session` header |

A fork and every session in its lineage use the root's id. Probed: a different
`prompt_cache_key` missed the whole cache on GPT-6 Luna.

An extension that sets OpenRouter's `provider.order` loses sticky routing, and
with it the cache.

## Cache lifetime

The cache lifetime is 1 hour by default, in every session, delegates included.
It is configurable per model and per session (`docs/configuration.md`). It is part of the preamble, so it
changes only at a build.

Anthropic stores 5-minute and 1-hour entries separately. Moving a session from
one to the other rewrites the whole cache, and a request that puts a 1-hour
marker after a 5-minute one is refused. So the lifetime cannot vary by turn.

A 1-hour write costs 2 times base input against 1.25 times for 5 minutes. A
session that pauses longer than 5 minutes even once in about 100 requests is
cheaper on 1 hour. Replayed on the owner's sessions with their real gaps, on
Opus 5.5, 1 hour costs 0.88 of 5 minutes across 641 pi sessions and 0.57 across
169 Claude Code sessions. It costs 1.01 across 143 Claude Code subagent
sessions, and implementation delegates run longer tools than those did
([research/prompt-cache/ttl.py](../research/prompt-cache/ttl.py)).

OpenAI offers one lifetime, 30 minutes, on GPT-5.6 and later, so the setting
applies only where a protocol offers a choice.

Fiber sends no requests only to keep a cache warm. A session with no client
exits when idle (`docs/invocation.md`, "Lifecycle"), and an idle Fiber does no
work.

## Switching model

A person switches model, effort or thinking with `/model`, and a driver with
the `model` command (`docs/invocation.md`). The switch applies at the next turn
boundary. Before it applies, the person sees one line saying the switch
rebuilds the cache, with its size from the last `usage_recorded`, for example
"switching rebuilds the cache: about 180,000 tokens". No confirmation is asked.

The log records `model_changed`, with the settings before and after, then
`preamble_built`. Probed: a model switch missed the whole cache on Anthropic and
on Muse Spark, and a change of reasoning effort missed it on Anthropic and
GPT-6 Luna.

## Usage

`usage_recorded` splits its tokens into uncached input, input read from the
cache, input written to the cache by lifetime, and output. Every provider
prices these differently, so no cost can be computed without the split.

## What misses the cache on purpose

Each of these costs one rebuild and is logged:

| Cause | Event | Rebuilt from |
|---|---|---|
| Handoff | `handoff_completed` | after the preamble (`docs/handoff.md`) |
| `reload` | `reloaded`, `preamble_built` | the start |
| Switch | `model_changed`, `preamble_built` | the start |
| Resume with changed inputs | `fiber_started`, `preamble_built` | the first changed byte |

A cache entry also expires after its lifetime with no request.

## Rules for other areas

- Hooks: no hook rewrites a message the model has already been sent. Every
  hook point changes content before it is logged (`docs/extensions.md`,
  "Hooks").
- The reviewer has its own cache. Its prompt is fixed instructions, then the
  person's messages and the tool calls in log order, then the call under review.
- The system prompt holds only what is fixed for the session. What it holds
  and what feeds it is `docs/system-prompt.md`.

## Sources

- Provider rules, with quotes: [research/prompt-cache/README.md](../research/prompt-cache/README.md)
- Live probes and results: [research/prompt-cache/probes.md](../research/prompt-cache/probes.md)
- The audit of settled decisions:
  [#33 comment](https://github.com/aakshintala/fiber/issues/33#issuecomment-5802809634)
