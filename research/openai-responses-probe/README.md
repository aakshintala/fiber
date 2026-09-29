# OpenAI Responses probe (#134)

Questions: does OpenAI answer and cache the same for top-level `instructions` and a system message in `input`, and what does OpenAI do with a function tool that has no `strict` key.

## Method

`probe.py` sends 13 requests to `https://api.openai.com/v1/responses` with model `gpt-6-luna`, reasoning effort `none`, on 2026-09-29, run from macOS. Price from https://developers.openai.com/api/docs/pricing: $0.10 per million input tokens, $0.01 cached input, $0.50 output. Estimated spend: $0.0009 (cap $0.75). Raw requests and responses: `raw/probe.json`; console output: `raw/out.txt`. Only OpenAI was used.

The minimum cacheable prefix for GPT-5.6 and later is 1,024 visible input tokens (https://developers.openai.com/api/docs/guides/prompt-caching). The probe prompt is 2,346 to 2,348 tokens, well past the minimum, not just past it.

## Instructions against a system message in `input`

Both forms hold the same 110-rule text and the same question ("What is the number in Rule 7?").

- Answer: `7` for every request in both forms.
- Cache, `instructions` sent twice: 2347 input tokens, cached 0 then 2344.
- Cache, system message in `input` sent twice: 2348 input tokens, cached 0 then 2345.
- Switching forms with identical text (order: instructions, instructions, input, input, instructions): cached 0, 2343, 2343, 2343, 2343. The first request in the other form hit the cache the first form had warmed, and the switch back hit it again.

The two forms cache and answer the same, and they share a cache entry (`raw/probe.json`, labels `instructions #n`, `input-system #n`, `switch ...`). Scope: one prompt, one model, one short question.

## Tool with no `strict` key

The schema has properties `a` and `b`, with only `a` required and no `additionalProperties` key. Strict mode rejects it.

- No `strict` key: the response has `status: "completed"` (the raw file records no HTTP status), and the output holds a `function_call` to `f`. The response object's `tools` echo shows `"strict": true`, and OpenAI rewrote the schema to `"required": ["a","b"]` and `"additionalProperties": false`. So the default is strict, and OpenAI normalises the schema instead of rejecting it.
- `strict: true` sent explicitly with the same schema: 400 `invalid_function_parameters`, "'additionalProperties' is required to be supplied and to be false."
- `strict: false`: accepted; echo shows `"strict": false` and the schema unchanged.

What shows the default: the `tools[0].strict` echo in the response to the request that had no key (`raw/probe.json`, label `tool no strict`). The normalised `required` list means an omitted key makes optional parameters required in the model's view.

## Decide: `store`

A request with no `store` key returns `"store": true` in the response object (`raw/probe.json`, label `store default`). This matches rig's reliance on the default; pi sends `false`. The ruling is the owner's.

## For the owner

- `store`: evidence above; the default is `true` on api.openai.com for `gpt-6-luna`.
- The other Decide items are stream and parser behaviour, not probed.

## Malformed replies

None.
