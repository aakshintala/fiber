# Provider errors on the wire

Live probe for [#96](https://github.com/aakshintala/fiber/issues/96), run on
September 26, 2026 from macOS. Evidence for
[Error taxonomy](https://github.com/aakshintala/fiber/issues/113).

`probe.py` sends each failing request and saves the status, response headers
and body (or each stream line with its arrival time) to `raw/<case>.json`.
Request headers are never saved. Cookies, request ids, the account's
`user_id` and muse's `Proxy-Status` header, which carries an encoded client
address, are redacted.

## What the probe found

- The HTTP status alone cannot classify an error. Through OpenRouter, an
  OpenAI context overflow arrives as HTTP 200 with the error in the body or
  the stream.
- No two upstreams report a context overflow the same way. Each needs its own
  match, and muse's cannot be told apart from any other bad parameter.
- muse answers an oversized `max_tokens` with a 429 rate limit and
  `Retry-After: 60`. Retrying it can never succeed.
- Every other failure, streamed or not, arrived as a non-200 status before any
  stream event.
- A rate limit was not reached: 170 requests at muse, 32 at a time, all
  returned 200.

## Endpoints

| Name | URL | Model | Protocol |
|---|---|---|---|
| muse | `api.meta.ai/v1/chat/completions` | `muse-spark-1.3-contributor` | `openai-completions` |
| muse | `api.meta.ai/v1/responses` | same | `openai-responses` |
| muse | `api.meta.ai/v1/messages` | same | `anthropic-messages` |
| OpenRouter | `openrouter.ai/api/v1/chat/completions` | `anthropic/claude-haiku-4.5`, `openai/gpt-6-luna` | `openai-completions` |
| OpenRouter | `openrouter.ai/api/v1/responses` | `openai/gpt-6-luna` | `openai-responses` |
| OpenRouter | `openrouter.ai/api/v1/messages` | `anthropic/claude-haiku-4.5` | `anthropic-messages` |

## Error envelopes

Three body shapes cover every error seen:

- OpenAI shape, muse's completions and responses endpoints:
  `{"error":{"code","message","param","type"}}`. `code` is often `null`.
- Anthropic shape, muse's and OpenRouter's messages endpoints:
  `{"type":"error","error":{"type","message"}}`. OpenRouter adds
  `error.error_type` and `request_id`.
- OpenRouter shape, its completions and responses endpoints:
  `{"error":{"message","code","metadata"}}`. `code` repeats the HTTP status as
  a number. When an upstream failed, `message` is "Provider returned error"
  and `metadata.raw` holds the upstream's body as a JSON string, with
  `metadata.provider_name`. OpenAI upstreams also get
  `metadata.provider_error_code`.

## Results by failure

### Context overflow

| Path | Status | What identifies it |
|---|---|---|
| OpenRouter's own check, any protocol | 400 | message "This endpoint's maximum context length is 200000 tokens. However, you requested about 390016 tokens (390000 of text input, 16 in the output)." No code. |
| OpenRouter to Anthropic | 400 | `metadata.raw` holds Anthropic's `invalid_request_error`, "prompt is too long: 602006 tokens > 200000 maximum" |
| OpenRouter to Amazon Bedrock | 400 | `metadata.raw` is `{"message":"prompt is too long: 602006 tokens > 200000 maximum"}`, with no type |
| OpenRouter to OpenAI, completions | 200 | `error.metadata.provider_code` is `context_length_exceeded`; message "Your input exceeds the context window of this model." |
| OpenRouter to OpenAI, responses | 200 | `status: "failed"`, `error.code` is `invalid_prompt`; same message |
| muse, all three protocols | 400 | message "The request contains invalid parameters. Check the request body for any errors or inconsistencies.", `code: null` |

On the messages endpoint, OpenRouter unwraps the upstream's error into the
Anthropic shape, so the message is in `error.message` directly.

OpenRouter checks the size itself before forwarding. Its estimate counted
`hello ` at about 1.5 tokens a word and CJK text at one token a character, so
ordinary text is rejected by OpenRouter and never reaches the vendor. Random
alphanumeric text passed its check and reached the vendor: 700,000 characters
were 602,006 tokens to Anthropic. A real session near the limit can hit
either check.

The same OpenRouter message also rejects an oversized `max_tokens`:
"you requested about 50000002 tokens (2 of text input, 50000000 in the
output)". The message's own numbers tell the two cases apart.

The muse cases sent about 1.4 million tokens. An 8 MB body padded with JSON
whitespace, and so few tokens, succeeded, so the 400 is not a request-size
limit. muse's context window was not bracketed.

### How an error arrives after HTTP 200

OpenRouter sends `: OPENROUTER PROCESSING` comments on a stream, and blank
lines on a non-streamed response, while the upstream works. The status is
already 200 when the upstream fails. On the 4 million character GPT-6 Luna
requests this took 9 to 16 seconds:

- non-streamed completions: whitespace, then `{"id":...,"error":{...}}` as
  `application/json`
- streamed completions: one `data:` chunk with `"choices":[]` and an `error`
  object, then the stream ends with no `[DONE]` and no `finish_reason`
- non-streamed responses: whitespace, then a response object with
  `"status":"failed"` and an `error`
- streamed responses: `response.created`, `response.in_progress`,
  `response.failed`, then `data: [DONE]`

This is the only way the probe saw a stream fail after it started. A stream
that dies midway for other reasons, such as an overload during generation,
was not triggered.

### Authentication

Every endpoint returned 401 for a bad key and for no key.

- muse: `type: authentication_error`, `code: invalid_api_key`, message
  "Unauthorized".
- OpenRouter: a bad key says "Missing Authentication header" and no key says
  "No cookie auth credentials found". Neither message describes the fault.

### Invalid tool schema

A parameter typed `"banana"` got 400 on every endpoint, streamed or not,
before any event.

- muse: `param: "parameters"`, message "Invalid JSON schema: ...".
- OpenRouter to OpenAI: `provider_error_code: invalid_function_parameters`.
- OpenRouter to Anthropic: "tools.0.custom.input_schema: JSON schema is
  invalid."

OpenRouter retried this deterministic 400 on four upstreams, Amazon Bedrock,
Anthropic, Google and Azure, and listed them in
`metadata.previous_errors`.

### Unsupported `tool_choice`

- muse: 400 on all three protocols, `param: "tool_choice"`, message "only
  `\"auto\"` is supported for `tool_choice`".
- OpenRouter: `required` (or `{"type":"any"}` on messages) succeeded on every
  model.

### Unknown model

- muse: 404, `code: model_not_found` on completions and responses,
  `type: not_found_error` on messages.
- OpenRouter: 400, "anthropic/no-such-model is not a valid model ID".

### Wrong path

Probed 2026-10-07 with a misspelt endpoint path, for comparison with an
unknown model:

- Anthropic, `POST /v1/messagez`: 404,
  `{"type":"error","error":{"type":"not_found_error","message":"Not found"}}`.
  An unknown model on `/v1/messages` is also 404 `not_found_error`, with the
  message "model: claude-no-such-model".
- OpenAI, `POST /v1/chat/completionz`: 404, empty body.
- OpenRouter, `POST /api/v1/chat/completionz`: 404,
  `{"error":{"message":"Not Found","code":404}}`.

### Rate limit

Not reached. A burst of 170 one-token requests at muse's completions endpoint,
32 at a time, all returned 200, although muse's headers advertise
`x-ratelimit-limit-requests: 150`. OpenRouter was not tried.

The 429 muse returned for `max_tokens: 50000000` shows its rate-limit shape:

- headers `Retry-After: 60`, `x-ratelimit-limit-requests`,
  `x-ratelimit-remaining-requests`, `x-ratelimit-limit-tokens`,
  `x-ratelimit-remaining-tokens`
- `type: rate_limit_error`, and `code: rate_limit_exceeded` outside the
  messages protocol
- message "Output token rate limit exceeded. Try lowering max_completion_tokens
  to reduce reserved capacity."

OpenRouter returned no rate-limit headers on any response.

## How a successful stream ends

Recorded so a decoder can tell a complete stream from a truncated one:

| Endpoint | Last events |
|---|---|
| muse completions | a chunk with `finish_reason`, then `data: [DONE]` |
| muse responses | `event: response.incomplete` (the 16-token cap was hit), then `data: [DONE]` |
| muse messages | `message_delta` with `stop_reason`, then `event: message_stop` |
| OpenRouter completions | a chunk with `finish_reason` and `native_finish_reason`, a usage chunk, then `data: [DONE]` |
| OpenRouter responses | `response.completed`, then `data: [DONE]` |
| OpenRouter messages | `message_stop`, then `event: data` and `data: [DONE]`, which Anthropic's protocol does not have |

## Weak points

- One run, one day. Messages are vendor text and can change without notice.
- muse's context window is unknown, so its generic 400 was only seen far past
  the limit.
- No rate limit, overload (529 or 503), quota or billing error, or refusal was
  triggered. Their shapes are not known from this probe.
- No stream failed partway through generation. The only error after HTTP 200
  came before any content.
- Anthropic and OpenAI were reached only through OpenRouter, never directly.
  The Anthropic and OpenAI bodies seen are the ones inside `metadata.raw`.
- `openai-codex-responses` and `google-generative-ai` were not probed; no key
  reaches them.
