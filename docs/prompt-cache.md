# Prompt cache

Every provider Fiber talks to caches the start of a request. A later request
whose leading bytes match reads that start from the cache at a tenth of the
input price or less. A request whose bytes differ from some point on pays full
price, or more, for everything after that point. Fiber keeps those bytes stable
by design, and every change to them is a named event in the session log.

Vocabulary is `GLOSSARY.md`: prompt cache, preamble, session, turn, step.

## What a request is built from

A request is the preamble, then the conversation.

The preamble is the system prompt, the tool definitions and the request
settings: model, thinking level, `tool_choice`, cache lifetime and credential
label. The
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
| Switch | Built again with the new model, thinking level or credential label ("Switching model"). |

Each build is logged as `preamble_built` (`docs/events.md`), carrying the
reason, the request settings, the system prompt text and the full tool
definitions as sent. A fork or a rewind sends the latest build before its
point, so its first request matches its parent's byte for byte.

Between builds the preamble does not change. What varies by project or by day,
such as instruction files, the environment and the skills listing, is in the
opening message, which is logged once. The `skill` tool's definition never
names a skill, so adding or removing one never changes the tool set. A change to any of it reaches the model
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
- An extension's inner calls add no tool definitions and change no message
  the model was sent: the model sees only the outer call's result
  (`docs/extensions.md`, "Running a tool").

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

Fiber places at most these three markers, and model data adds none (`docs/model-routing.md`, "Extra request body fields"). Anthropic refuses a request with more than 4 markers across `tools`, `system` and `messages` (`research/anthropic-messages-probe`).

Providers that route by key are given the root session's id:

| Provider | Field |
|---|---|
| OpenAI | `prompt_cache_key` |
| ChatGPT/codex | the `session_id` header (`session-id` and `session_id` both work) |
| OpenRouter | `session_id` |
| OpenCode | the `x-opencode-session` header |

A fork and every session in its lineage use the root's id. Probed: a different
`prompt_cache_key` missed the whole cache on GPT-6 Luna.

With `gpt-6-luna`, ChatGPT/codex hit the cache on the second request in each
of 5 pairs sent 24 to 27 seconds apart with a stable `session_id` header (7,936 of
about 9,000 tokens). A stable `prompt_cache_key` with a new header each time
missed in its pair, as did a stable `prompt_cache_key` alone and a stable
`x-client-request-id` alone (one pair each). A new id each request missed in 2
pairs. Pairs 4 to 5 seconds apart missed even with every id stable.

An extension that sets OpenRouter's `provider.order` loses sticky routing, and
with it the cache.

Gemini uses implicit caching only. Fiber does not create explicit caches
through `cachedContents`, for the same reason it sends `store: false`: the
vendor holds no session state (`docs/model-routing.md`).

## Cache lifetime

The cache lifetime is 1 hour by default, in every session, delegates included.
It is configurable per model and per session (`docs/configuration.md`). It is part of the preamble, so it
changes only at a build.

Anthropic stores 5-minute and 1-hour entries separately. Moving a session from
one to the other rewrites the whole cache, and a request that puts a 1-hour
marker after a 5-minute one is refused. So the lifetime cannot vary by turn.

A 1-hour write costs 2 times base input against 1.25 times for 5 minutes. A
session that pauses longer than 5 minutes even once in about 100 requests is
cheaper on 1 hour. The replay is
[research/prompt-cache/ttl.py](../research/prompt-cache/ttl.py).

OpenAI offers one lifetime, 30 minutes, on GPT-5.6 and later, so the setting
applies only where a protocol offers a choice.

### Warming while idle

By default Fiber sends no request only to keep a cache warm, so an idle Fiber
does no work. With `cache.warm_idle` set, an idle session refreshes its cache
shortly before the lifetime ends, with a request that reads the cached prefix
and asks for one token of output, so a person who returns after a long pause finds the
cache still warm. The refresh is logged as `usage_recorded` like any request,
so its cost shows in the session's spend.

A refresh is the session's last request with its output capped at one token,
so it reads the cache only when every other byte is the same. A session sends
no refresh when lowering the output cap would change anything else in the
request. On Anthropic, a thinking level sent as a token budget is that case:
`budget_tokens` must stay below `max_tokens`, so a one-token cap forces a
different budget, and a change of thinking parameters invalidates Anthropic's
cached messages ([research/prompt-cache/README.md](../research/prompt-cache/README.md)).
The refresh would miss and pay for a full rebuild. A level sent as
adaptive thinking or an effort parameter does not depend on the cap, and
warms.

Warming stops at `cache.warm_cap` after the last turn, whether or not a client
is connected. A connected client is not a signal: a terminal left open is
connected all weekend (`docs/invocation.md`, "Lifecycle"). While warming, the
session is not idle; when warming stops, the idle clock starts.

The cap has a ceiling. A refresh costs 0.05 to 0.1 times the prompt's input
price and a 1-hour rebuild 2 times, so warming through N lifetimes on a session
nobody returns to wastes at most 0.1 × N. Below 19 lifetimes, that is always
less than the one rebuild warming guards against. The default cap is 2
lifetimes, `"2h"`. Fiber has no savings threshold. The replay is
[research/prompt-cache/warm-cap.md](../research/prompt-cache/warm-cap.md).

A `fiber ask` session and a delegate never warm: each exits when its run ends.
A repository may set both keys, as a company's policy for its repository:
the worst a hostile value can do is spend money, bounded by the cap's ceiling
(`docs/configuration.md`, "What a repository may set"). The person's
per-project file overrides them.

## Switching model

A person switches model or thinking level with `/model` or `/thinking`, and a driver with
the `model` command (`docs/invocation.md`). A credential label switches the
same way, with `/credential` or the `credential` command: a vendor holds its
cache per account or workspace, so the new label starts with a cold cache. The switch applies at the next turn
boundary. Before it applies, the person sees one line saying the switch
rebuilds the cache, with its size from the last `usage_recorded`, for example
"switching rebuilds the cache: about 180,000 tokens". No confirmation is asked.

The log records `model_changed`, with the settings before and after, then
`preamble_built`. The new model is sent no reasoning state another model
produced (`docs/loop.md`, "What the model is sent"). Probed: a model switch
missed the whole cache on Anthropic and
on Muse Spark, and a change of thinking level, sent as each vendor's effort
parameter, missed it on Anthropic and GPT-6 Luna.

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
- The reviewer has its own cache. Its request is the shared instructions and
  the person's `reviewer.context`, global then per-project, fixed for the
  session, then the person's messages and the tool calls in log order, then
  the call under review with its declared effects, then the stage's
  instruction. Both stages send byte-identical bytes up to the stage
  instruction, so every reviewer
  pass of either stage extends one cache chain as the session grows. A call
  must render identically when it later appears in history, because the chain
  depends on it. Where a provider routes by key, its key is the reviewed
  session's own id plus `reviewer`, a delegate's included.
- The system prompt holds only what is fixed for the session. What it holds
  and what feeds it is `docs/system-prompt.md`.

## Sources

- Provider rules, with quotes: [research/prompt-cache/README.md](../research/prompt-cache/README.md)
- Live probes and results: [research/prompt-cache/probes.md](../research/prompt-cache/probes.md)
- The audit of settled decisions:
  [#33 comment](https://github.com/aakshintala/fiber/issues/33#issuecomment-5802809634)
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
