# Anthropic Messages facts, probed

Live probe for [#132](https://github.com/aakshintala/fiber/issues/132). It
settles four facts that pi and rig disagree on
(`research/provider-harvest/anthropic-messages.md`, "Disagreements"). Run on
September 29, 2026 from macOS, against `https://api.anthropic.com/v1/messages`
with the direct Anthropic key.

## Method

`probe.py <stream|beta|choice|caller|ptc|cache>` sends each request, saves the
status, response headers, body (or the whole SSE text plus a parsed event list)
and the request to `raw/<case>.json`, and stops before a running cost estimate
passes $1.00. Request headers are never saved. The organisation and workspace
ids, cookies and `cf-ray` are dropped from the response headers.

- Model: `claude-sonnet-5-5`, `anthropic-version: 2023-06-01`.
- Price: $2 per million input tokens, $10 per million output tokens, cache
  writes 1.25x, cache reads 0.1x
  (https://platform.claude.com/docs/en/about-claude/pricing, fetched
  2026-09-29).
- Spend: about $0.08 on Anthropic, summed from the per-run estimates (each run
  starts its own counter).
- `thinking: {type: enabled, budget_tokens}` is rejected on this model with
  400 ("Use thinking.type.adaptive and output_config.effort"). The thinking
  cases use `adaptive`.

## Which event ends a stream

In every stream that reached HTTP 200, `message_delta` carrying the
`stop_reason` was followed by `message_stop`, and `message_stop` was the last
event. `message_delta` without a following `message_stop` did not occur.

`raw/stream.json` holds 8 streaming requests. Seven returned 200 and every one
ended `message_delta`, `message_stop`; the eighth (`max_tokens: -1`) was a 400.

| Case | `stop_reason` | Blocks |
|---|---|---|
| plain text | `end_turn` | text |
| tool use | `tool_use` | tool_use |
| `max_tokens: 5` | `max_tokens` | text |
| `stop_sequences: ["5"]` | `stop_sequence` | text |
| adaptive thinking, easy question | `end_turn` | text (no thinking block) |
| adaptive thinking + tool | `tool_use` | tool_use |
| adaptive thinking, hard question | `end_turn` | thinking, text |

Every stream began `message_start`, `content_block_start`, `ping`.
`message_start` carried `stop_reason: null`; `message_delta` carried the final
`stop_reason` and `stop_sequence`. The 400 arrived as a plain JSON error with no
SSE events. Scope: 7 successful streams on one model. A failure after the stream
starts, and a dropped connection, were not triggered, so what ends a stream that
fails part-way is not measured.

## The `?beta=true` query

No difference seen. `raw/beta.json` sends the same body to `/v1/messages` and
`/v1/messages?beta=true`, plain and with a tool, non-streaming and streaming.
All returned 200. Response header names match, the body has the same keys
(`container`, `content`, `diagnostics`, `id`, `model`, `role`, `stop_details`,
`stop_reason`, `stop_sequence`, `type`, `usage`), the tool call carries the same
`caller`, and the streaming event names match. `?beta=false` also returned 200.
Scope: this model and these requests. Beta-only features were tried only with
the `anthropic-beta` header (see `caller` below), not with the query.

## `tool_choice` with no `tools`

The answer depends on the value. `raw/choice.json`, requests without `tools`:

| `tool_choice` | Result |
|---|---|
| `{"type":"auto"}` | 200 |
| `{"type":"none"}` | 200 |
| `{"type":"any"}` | 400 "tool_choice.any may only be specified while providing tools" |
| `{"type":"tool","name":"get_weather"}` | 400 "Tool 'get_weather' not found in provided tools" |

`tools: []` with `auto` also returned 200. pi's habit of always sending it is
safe for `auto` and `none`, and fails for `any` and `tool`.

## `caller` on `tool_use`

From `raw/caller.json`, `raw/ptc.json` and `raw/stream.json`:

- A normal tool call has `caller: {"type": "direct"}`, in the non-streaming body
  and in the stream's `content_block_start` (where `input` is `{}`).
- Server-side code execution changes it. With the `code_execution_20250825`
  tool, a client tool listing `allowed_callers: ["code_execution_20250825"]` and
  the header `anthropic-beta: advanced-tool-use-2025-11-20`, the model wrote
  code that called the client tool. Each resulting `tool_use` carried
  `caller: {"type": "code_execution_20250825", "tool_id": "srvtoolu_..."}`,
  naming the `server_tool_use` block that ran the code. The response also had a
  `container` object.
- The validator's rejection of a bad tag lists three: `direct`,
  `code_execution_20250825`, `code_execution_20260120`. The last was not
  triggered; it needs the newer code execution tool version.
- Replaying a `tool_use` without `caller` returned 200, for a direct call and
  for the code execution calls. Replaying it as received also returned 200.
  `caller.type` of `bogus` returned 400. A `code_execution` caller whose
  `tool_id` matched no block returned 400 ("source tool ... not found").

So `caller` need not be sent back. If it is, it must be valid and point at a
real block.

## More than 4 cache markers (Decide, evidence only)

`raw/cache.json`: 4 `cache_control` blocks returned 200. 5 returned 400 "A
maximum of 4 blocks with cache_control may be provided. Found 5." 6 (one tool,
4 system blocks, 1 message block) returned 400 "Found 6." The count spans
`tools`, `system` and `messages`.

## Malformed replies

None. Every 200 parsed as documented. Two things to know about
`claude-sonnet-5-5`: `thinking.type=enabled` is rejected, and adaptive thinking
on an easy question produced no thinking block.

## For the owner

- More than 4 cache markers: the server returns a 400 that names the limit and
  the count, so pi's never counting fails too, one round trip later. Counting in
  Fiber makes it an encode-time error. Fiber's own design
  (`docs/prompt-cache.md`) uses up to three markers.
- Unknown `stop_reason` and strict tool schemas: not probed; no cheap trigger.
