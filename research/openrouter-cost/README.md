# When OpenRouter's cost settles

Findings for [#97](https://github.com/aakshintala/fiber/issues/97). Probed live on 2026-09-29 (UTC)
from macOS through OpenRouter, model `z-ai/glm-5.3-flash` only. The owner chose this model for this round on 2026-09-29, overriding the model list in the ticket. Providers
OpenRouter routed to: Relace, Together, CoreWeave, Fireworks and, for the aborted stream,
OpenInference.

Question: does Fiber ever need the late-cost path for OpenRouter (`docs/events.md`,
`usage_recorded`)?

## Method

`probe.py` sends 21 chat completions with a hard count: 10 non-streaming and 10 streaming (kinds:
plain, forced tool call, long reply, fixed-prefix repeats for caching), plus one streaming request
whose connection is closed after three content deltas. For each it records the inline `usage`
(non-streaming: the body; streaming: the last `usage` in the stream) and the generation id, then
polls `GET /api/v1/generation?id=` at 0, 1, 2, 5, 10, 30, 60 and 120 seconds after the response
finished, saving every reply. Raw: `raw/results.json` (each request's kind and stream flag, the responses, stream chunks and
polls; it holds no request bodies; workspace id redacted). All requests went to
`/api/v1/chat/completions`, and `probe.py` leaves `stream_options` unset.

Price, from `GET https://openrouter.ai/api/v1/models` on 2026-09-29: input $0.15 per million
tokens, output $0.50 per million, cache read $0.03 per million. Cap $1.00.

Key balance, `GET /api/v1/key`, before: usage $0, limit remaining $10. After: usage $0.005596,
limit remaining $9.994404. The inline costs sum to $0.005186 for the 20 completed requests, plus
$0.000077 for the aborted one from its lookup: $0.005263. The balance is $0.000333 higher; the
probe does not explain the gap.

## Is the inline cost ever missing or different from the lookup?

- Missing: on all 20 completed requests, streaming and not, `usage.cost` is present, with
  `cost_details` (`upstream_inference_cost`, prompt and completion parts). Only the aborted stream
  had none (below). `raw/results.json`, field `inline_usage`.
- Different: on all 20, the inline `cost` equals the lookup's `data.total_cost` and `data.usage`
  exactly, at every poll that returned 200. No later poll changed a value.
- Cost varies with the provider OpenRouter routes to. The same prompt cost
  about $0.00009 on Relace and about $0.00037 on CoreWeave, Fireworks or Together.

## How long until the lookup returns a cost?

The lookup returns 404 (`Generation ... not found`) at first, not a partial cost. Time after the
response finished until the first 200, among the 21 requests: at the 5 s poll (1 request), at
the 10 s poll (11), at the 30 s poll (9). Polls were at fixed delays, so each figure is an upper bound within one step: the
lookup was ready at or before that poll. Every request had a 200 by 30.3 s. Once it returned 200,
the cost was final.

## Does streaming change either answer?

No. The 10 streaming and 10 non-streaming completions match on both counts: inline cost present
and equal to the lookup, lookup ready within 5 to 30 s.

The stream's last chunk before `[DONE]` carried `usage` (with `cost`) on all 10 completed
streams. It is the second of two consecutive chunks that carry `finish_reason`; the first has no
`usage`.

## Does an aborted stream still report a cost?

The one aborted stream (closed after 12 chunks, three content deltas) carried no `usage` and so no
inline cost. The lookup was 404 through 10 s and returned 200 at 30 s with `total_cost`
7.744e-05 and 91 completion tokens (more than the three deltas read). At every successful poll it
had `cancelled: true`, `finish_reason: null` and provider status 499. OpenRouter billed the
cancelled generation for the tokens it had produced. Scope: one request, one provider
(OpenInference).

## Other facts

- Prompt caching: request 7 (streaming repeat of a fixed prefix) reported `cached_tokens` 2240.
  Request 2, the same prefix and token counts on the same provider (Relace), reported 0 cached, and
  both cost 8.895e-05. The probe does not show a cost difference from the cache hit.
- The lookup reports `streamed: true` for every request including non-streaming ones, so that
  field does not tell the two apart.

## Malformed replies

None.

## For the owner

No Decide item rules on this ticket. The evidence bears on the late-cost path: for a completed
call, OpenRouter's inline cost was always present and never changed, so the path is not needed
for it. For the one stream the client abandoned, the cost was not inline and only the lookup,
ready by 30 s, supplied it.
