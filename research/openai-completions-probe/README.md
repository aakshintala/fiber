# openai-completions probe (#133)

Question: which of the facts pi and rig disagree on for `openai-completions`
(`research/provider-harvest/openai-completions.md`, "Disagreements") can a live
request settle?

## Method

`probe.py openai` and `probe.py openrouter` send 8 and 23 small requests and save
each request and reply to `raw/<vendor>-<name>.json`. No key or `Authorization`
header is saved; a grep for each key over this folder matched nothing.
Both ran on macOS, 2026-09-29.

- OpenAI Chat Completions direct, `gpt-6-luna`, `reasoning_effort: none` unless a row says otherwise.
  Price $0.10 in, $0.50 out per million tokens
  (https://developers.openai.com/api/docs/models/gpt-6-luna). Estimated spend under $0.01 of the $0.50 cap.
- OpenRouter, `z-ai/glm-5.3-flash`. Price $0.15 in, $0.50 out, $0.03 cached per
  million (https://openrouter.ai/api/v1/models). Estimated spend under $0.01 of the $1.00 cap.
  Reasoning is mandatory on this model: `effort: none` returned 400 once (`raw/openrouter-effort-none-400.json`), so the other runs use `low` or `medium`. OpenRouter routed to different upstream providers between requests (`provider` field in each body).
- muse: not run. `probe_muse.py` targets `https://api.meta.ai/v1/responses`, so
  muse speaks `openai-responses` in our scripts, not this wire.
- No key for Groq, Mistral, Perplexity, Hugging Face, Venice, DeepSeek or Moonshot.

## Probe list

1. Output-token field per vendor.
   - OpenAI, `gpt-6-luna`: `max_tokens` returns 400 `unsupported_parameter` ("Use 'max_completion_tokens' instead"); `max_completion_tokens` returns 200. Files: `raw/openai-mt-max_tokens.json`, `raw/openai-mt-max_completion_tokens.json`.
   - OpenRouter, `glm-5.3-flash`: both fields return 200. Files: `raw/openrouter-max_tokens.json`, `raw/openrouter-max_completion_tokens.json`. Acceptance only; whether the cap applied is not measured.
   - Groq, Mistral, Perplexity, Hugging Face, Venice: unreached, no key.
2. `stream_options.include_usage`.
   - OpenAI: accepted (200). In the one stream with it, a final chunk has `choices: []` and `usage`; in the one stream without it, no usage chunk appeared (`raw/openai-stream-include_usage.json`, `raw/openai-stream-no_include_usage.json`).
   - OpenRouter: accepted (200), and a usage chunk arrives whether or not it is sent (`raw/openrouter-stream-no_include_usage.json`). The usage chunk carries `choices` with one entry, `finish_reason: stop` and empty content, the same as the chunk before it. So `finish_reason` arrives twice.
   - Mistral, Perplexity, Mira and every other vendor: unreached. No rejection seen among the two vendors reached.
3. `reasoning_text` in the stream: OpenAI (Chat Completions, `reasoning_effort: low`) streams no reasoning field at all; OpenRouter streams `reasoning` and `reasoning_details`, never `reasoning_text` or `reasoning_content` (`raw/openai-stream-reasoning-low.json`, `raw/openrouter-stream-reasoning.json`). Other vendors unreached: which one sends `reasoning_text` is unanswered.
4. Reasoning field on the replayed assistant message. DeepSeek and Groq unreached. OpenRouter, `glm-5.3-flash`, pinned to one upstream with `provider: {only: [OpenInference], allow_fallbacks: false}`: the no-field control gave `prompt_tokens` 22, and `reasoning`, `reasoning_content` and `reasoning_details` each gave 383, so all three were read into the prompt (`raw/openrouter-pinned-replay-*.json`). The earlier unpinned runs (`raw/openrouter-replay-*.json`) gave the same numbers but the control ran on a different upstream, so they are not the evidence. OpenRouter does not require one name on this model. Scoped to one model, one upstream, one request each.
5. `reasoning_details` types in an OpenRouter stream: only `reasoning.text` (fields `type`, `text`, `format: "unknown"`, `index`), each chunk beside the same text in `reasoning` (`raw/openrouter-stream-reasoning.json`). No `reasoning.summary` or `reasoning.encrypted` among the three streams with reasoning deltas on this model (`stream-no_include_usage`, `stream-reasoning`, `stream-length`). Those types come from other models, which are not permitted here.
6. `finish_reason: network_error`: not seen. OpenAI gave `stop`, `tool_calls`, `length`; OpenRouter gave `stop`, `tool_calls`, `length`, with `native_finish_reason` equal in each. Other vendors unreached.
7. Tool-call `index`: present on every tool-call delta from OpenAI and from OpenRouter (`raw/openai-stream-tool.json`, `raw/openrouter-stream-tool.json`). Other vendors unreached. OpenRouter interleaves `: OPENROUTER PROCESSING` comment lines in the stream.
8. `cache_control` on OpenRouter, with a non-Anthropic model. Placed on a system text part (no `ttl`, `ttl: 1h`, `ttl: 5m`; `ttl` was sent only on the system part), on the last message's text part, on the last tool inside `function`, on the last tool at the top level: all 200. One request with `{"type":"bogus"}` also returned 200, so that value was accepted on that request. Whether it passes any placement or `ttl` upstream cannot be seen: GLM has no cache-write price, and `cache_write_tokens` was 0 throughout (`raw/openrouter-cc-*.json`). That needs an Anthropic model, which was outside the permitted list. OpenAI direct also returned 200 for `cache_control` on a text part (`raw/openai-cache_control-on-openai.json`); it was accepted; whether it has any effect is not measured.

## Malformed replies

None seen.

## For the owner

Decide items this evidence bears on:

- Input-token count: OpenRouter's `prompt_tokens` already includes `cached_tokens` (for example 15 with 14 cached). That is the raw figure rig reports; pi subtracts.
- Reasoning sent back: `reasoning`, `reasoning_content` and `reasoning_details` each changed the prompt token count on OpenRouter/`glm-5.3-flash`, so replay under any of the three names is read there.
- Strict tool schemas, `content_filter`, tool-argument parse failures, per-vendor quirks: no evidence from the vendors reached.
