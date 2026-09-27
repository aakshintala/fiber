# What reasoning state a resume must send back

Findings for [#95](https://github.com/aakshintala/fiber/issues/95), with the reasoning probes of
[#132](https://github.com/aakshintala/fiber/issues/132) and
[#134](https://github.com/aakshintala/fiber/issues/134). Probed live on 2026-09-27 (UTC) through
OpenRouter: Sonnet 5 and Haiku 4.5 on its native Messages endpoint (`/api/v1/messages`, pinned to
Anthropic unless a row names another host), and GPT-6 Luna on its Responses endpoint
(`/api/v1/responses`, `store: false`, `include: ["reasoning.encrypted_content"]`).

Each probe runs one turn that reasons and calls a tool, then sends the follow-up request with the
tool result in several variants. Scripts: `anth.py`, `resp.py`. Printed results: `out-*.txt`. Every
request and response: `raw-*.json`, with the 230-rule system-prompt filler replaced by a
placeholder.

## Answer

- Log each reasoning item exactly as the provider returned it. The load-bearing bytes are opaque:
  Anthropic's `signature` (or a redacted block's `data`) and OpenAI's `encrypted_content`. Both
  providers check them and refuse an altered one.
- The readable reasoning text is not what the provider uses. Changing or emptying Anthropic's
  `thinking` text, or clearing OpenAI's `summary`, was accepted and still read the whole cache.
- Dropping reasoning state is accepted on both providers, even mid-turn with a tool result. It
  costs the cache from the dropped item onwards, and OpenAI spends the reasoning tokens again.
- A switch to another Anthropic model, or to Sonnet 5 on Bedrock or Vertex, accepted a genuine
  signature unchanged. A switch loses the cache anyway, because the cache is per model and per
  host.
- A switch to another protocol has no field to carry the state in. What a fork or switch strips is
  an owner ruling, below.

## Anthropic Messages

Turn 1 returned `thinking` (text plus `signature`) and `tool_use`. Turn 2 sent the tool result.
`read` and `write` are cached input tokens read and written. The unchanged request read 7,013.

| Turn 2 variant | Result | Cache read |
|---|---|---|
| Unchanged | accepted | 7,013 of 7,015 |
| Thinking block dropped | accepted | 6,917, up to the first user message |
| Signature changed by one character | 400 ``Invalid `signature` in `thinking` block`` | none |
| Thinking text changed, signature kept | accepted | 7,013 |
| Thinking text emptied, signature kept | accepted | 7,013 |
| Thinking sent as a `text` block | accepted | 6,917 |
| Thinking turned off, block kept | accepted | 7,013 |
| Thinking turned off, block dropped | accepted | 6,987, the entry the dropped variant wrote |
| Haiku 4.5, block unchanged | accepted | 0, first request to that model |
| Haiku 4.5, block dropped or as text | accepted | 5,079, Haiku's own earlier write |
| Sonnet 5 on Bedrock, block unchanged | accepted | 0, first request to that host |
| Sonnet 5 on Vertex, block unchanged | accepted | 0, first request to that host |

On turn 3, after a text reply, dropping turn 1's thinking read 6,987 instead of 7,167: the cache
survives only up to the first dropped block.

The changed-text rows read the same 7,013 tokens as the unchanged request, so the text is neither
counted nor part of the cache key. This suggests Anthropic rebuilds the thinking from the
signature and ignores the text sent with it (INFERENCE).

Redacted thinking:

- Anthropic's documented test string (`ANTHROPIC_MAGIC_STRING_TRIGGER_REDACTED_THINKING_…`) no
  longer produces a `redacted_thinking` block on Sonnet 5 or Haiku 4.5. Both returned ordinary
  `thinking` (`out-redacted.txt`), so a genuine redacted block could not be probed.
- A `redacted_thinking` block with made-up `data` is refused on both models with
  ``Invalid `data` in `redacted_thinking` block``.
- The same block carrying a genuine thinking `signature` as its `data` is accepted on both, so the
  two are the same kind of signed blob. That is also true when Sonnet's signature goes to Haiku.
  By that equivalence a redacted block resent after a model switch is accepted too (INFERENCE,
  from `out-fake-redacted.txt`).

Also seen: each `tool_use` block now carries `"caller": {"type": "direct"}`, a field neither pi
nor rig models.

## OpenAI Responses

Turn 1 returned a `reasoning` item and a `function_call`. The reasoning item's keys were `id`,
`type`, `status`, `summary`, `encrypted_content` and `format: "openai-responses-v1"`. The
unchanged request read 4,844 cached tokens of 4,847.

| Turn 2 variant | Result | Cache read | Reasoning tokens |
|---|---|---|---|
| Unchanged | accepted | 4,844 | 49 to 63 |
| Reasoning item dropped | accepted | 4,699, the system prompt and first message | 135 |
| `encrypted_content` changed by one character | 400 `invalid_encrypted_content`, from OpenAI | none | |
| Only rig's fields (`format` removed) | accepted | 4,844 | 59 |
| Only `type`, `id`, `encrypted_content`, empty `summary` | accepted | 4,844 | 35 |
| No `id` | accepted | 4,844 | 41 |
| No `encrypted_content` | accepted, the item silently ignored | 4,736 | 173 |
| Sonnet 5, unchanged or dropped | accepted, same input size either way | 0 | |

On turn 3 dropping every reasoning item read 4,736 instead of 4,961.

`include: ["reasoning.encrypted_content"]` was accepted with `reasoning.effort: "none"`, with
`"minimal"` and with no `reasoning` key. With `none` no reasoning item came back; the other two
returned one with `encrypted_content`.

Limits of going through OpenRouter:

- OpenRouter rewrites items. Function-call ids came back as `fc_tmp_…`, and `format` is its field,
  not OpenAI's. So "no id" and "no `encrypted_content`" show only what OpenRouter forwards, not
  what OpenAI does with them.
- OpenRouter did not return OpenAI's raw reasoning item, so the probe cannot show whether OpenAI
  sends fields rig does not model. Removing every field outside rig's set kept the whole cache.
- "Sonnet 5 through Responses" tests OpenRouter's translation, which strips foreign reasoning. It
  says nothing about Anthropic.

## ChatGPT/codex

No live request was made. Codex's own session log and source (openai/codex at 8f195c9) show what
OpenAI's client keeps:

- Every `response_item` is logged whole, reasoning included, with `id` and `encrypted_content`
  (`~/.codex/sessions/…/rollout-*.jsonl`).
- Codex rebuilds items from typed fields, not raw JSON. Its `Reasoning` item has `id`, `summary`,
  `content`, `encrypted_content` and `internal_chat_message_metadata_passthrough`
  (`codex-rs/protocol/src/models.rs:1048-1060`). Codex sets the passthrough field itself, which
  holds a turn id, and sends it only to OpenAI (`core/src/client.rs:940-950`).
- A `FunctionCall` can carry `encrypted_function_args`, which codex also strips for any provider
  other than OpenAI (`models.rs:1088`, `client.rs:944-948`). Opaque state is not confined to
  reasoning items.

## What this settles on the tickets

- #95: log the item verbatim; the opaque field is what is checked; dropping is accepted at a cache
  cost; another model on the same protocol accepts it.
- #132, redacted thinking after a model switch: not directly probed, because no redacted block can
  be produced. Its signed-blob equivalence points to "accepted if resent". The rule is the owner's.
- #134, fields outside rig's set: not observable through OpenRouter. Codex shows OpenAI's own
  client uses a wider typed schema than rig, and that its extra fields vary by endpoint. Verbatim
  logging, which is pi's approach, covers fields no one has modelled yet.
- #134, `include` with reasoning off: accepted, through OpenRouter.
- #135: no codex request made; nothing to tick.
