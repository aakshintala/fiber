# Quota and billing error shapes vendors document

For [#871](https://github.com/aakshintala/fiber/issues/871). Read 2026-10-07.
Fiber maps each shape below to `quota_exceeded`, never retried; a shape the
vendor's documentation does not tell apart from a rate limit stays
`rate_limited`. ChatGPT/codex's usage-limit body stays with #311.

Fixtures live in `documented/`, not `raw/`: `raw/` holds live recordings and
these are not recordings. Each fixture's `note` says which fields are
documented and which are illustrative. Where the docs give no message text,
the message is the docs' own description sentence. The status, type, code,
reason, `@type` and prefix are always the documented values. Each stream
fixture ends where the decoder returns: at the error event or chunk.

## Mapped to `quota_exceeded`: HTTP status

### Anthropic

- A1: 402 `billing_error` ("There's an issue with your billing or payment
  information."). The status alone identifies it.
  Source: <https://platform.claude.com/docs/en/api/errors>
- A2: 429 `error.type` `rate_limit_error` with
  `error.details.error_code` `enforced_spend_limit_reached` and no
  `retry-after`. The docs say: "Use it to tell this response apart from a
  rate limit."
  Source: <https://platform.claude.com/docs/en/api/rate-limits>,
  "Reaching your spend cap"
- A3: 400 `error.type` `invalid_request_error` with a message beginning
  `You have reached your specified API usage limits`. Same page,
  "Setting your own spend limit".
- A4: 400, the same, with a message beginning
  `You have reached your specified workspace API usage limits`. Same page.

### OpenAI (`openai-completions`, `openai-responses`)

- O1-O4: 429 with `error.code` `credit_balance_exhausted`,
  `organization_spend_limit_exceeded`, `project_spend_limit_exceeded` or
  `organization_usage_limit_exceeded`.
- O5: 429 with `error.type` `insufficient_quota`, or `error.code`
  `insufficient_quota` (the code form #871 names). The docs say: "The
  broader `error.type` can still be `insufficient_quota`. Retrying billing,
  spend, or quota errors won't restore API access." Every billing row on
  that page is a 429.
  Source: <https://developers.openai.com/api/docs/guides/error-codes>

### Gemini API and Vertex

- G1: 402 on depleted Prepay credits: "Requests then fail with an HTTP 402
  Payment Required error until you add credits." The status alone
  identifies it.
  Sources: <https://ai.google.dev/gemini-api/docs/billing> ("Prepay"),
  <https://ai.google.dev/gemini-api/docs/troubleshooting>, and Google's
  2026-09-18 announcement:
  <https://discuss.ai.google.dev/t/api-update-depleted-prepay-credits-now-return-http-402-instead-of-429/183654>
- G2, G3: any status, with an `error.details[]` entry whose `@type` is
  `type.googleapis.com/google.rpc.ErrorInfo` and whose `reason` is
  `BILLING_DISABLED` or `RESOURCE_QUOTA_EXCEEDED`.
  Source: <https://github.com/googleapis/googleapis/blob/master/google/api/error_reason.proto>

### OpenRouter

- R1: 402 "Your account or API key has insufficient credits", with
  `metadata.error_type` `payment_required`. The status identifies it,
  except for N6's case.
  Source: <https://openrouter.ai/docs/api/reference/errors-and-debugging>

## Mapped to `quota_exceeded`: inside a 200 stream

- S1: Anthropic `error` event whose `error.type` is `billing_error`.
  Source: <https://platform.claude.com/docs/en/build-with-claude/streaming>,
  "Error events": an error event "would normally correspond to" the HTTP
  error of the same type.
- S2: Anthropic `error` event with A2's `type` and `details.error_code`.
  Same page, applied to A2.
- S3: OpenRouter mid-stream chunk whose `error.code` is the number 402. The
  documented mid-stream shape is
  `{"error":{"code":<number>,"message",...,"metadata":{"error_type"}},"choices":[{"finish_reason":"error"}]}`.
  No `Retry-After` can arrive mid-stream, and the docs say "A 402 without
  the header is not a wait-and-retry case." Same errors page, "Streaming
  Errors".
- S4: Gemini in-stream `error` (a `google.rpc.Status`) whose `code` is the
  number 402, or whose `details` hold G2's or G3's `ErrorInfo`. G1-G3
  applied to the stream; Gemini documents no separate in-stream error
  shape. The 402 code is inferred from G1, not documented.

## Not mapped

- N1: Anthropic 429 on the Claude Code workspace's spend limit, which
  carries `retry-after`. The docs give no field that tells it apart from a
  rate limit. Stays `rate_limited`.
- N2: Anthropic 429 acceleration limit. The docs describe it as a rate
  limit. Stays `rate_limited`.
- N3: OpenAI 429 `rate_limit_error` with code `slow_down`, and "Rate limit
  reached for requests". Rate limits; the docs say to follow `Retry-After`.
  Stays `rate_limited`.
- N4: Gemini 429 `RESOURCE_EXHAUSTED`, bare or with only a
  `google.rpc.QuotaFailure` detail; the same in the stream. Gemini answers
  every RPM, TPM and RPD overrun, and a spend-based rate limit, with "a
  `429 RESOURCE_EXHAUSTED` error"
  (<https://ai.google.dev/gemini-api/docs/rate-limits>). Stays
  `rate_limited`.
- N5: Google `ErrorInfo` reason `RATE_LIMIT_EXCEEDED`. The proto defines it
  as a rate quota. Stays `rate_limited`.
- N6: OpenRouter 402 whose `metadata.limit_source` is
  `openrouter_in_flight_budget` and that carries a `Retry-After` Fiber
  parses as seconds. The docs make it a wait-and-retry case only when the
  header is present: "may include a standard HTTP `Retry-After` ... A 402
  without the header is not a wait-and-retry case." OpenRouter's docs give
  `Retry-After` as "seconds to wait". Without the header, or with a value
  Fiber cannot parse as seconds (such as an HTTP date), it is R1. Stays
  `rate_limited` only with a parsed-seconds header.
- N7: OpenRouter 400 `token_limit_exceeded` ("e.g. credit-based cap"). The
  docs' "e.g." leaves it open whether this is billing. It is never retried
  either way; stays `invalid_request`.
- N8: Amazon Bedrock `ServiceQuotaExceededException` (400),
  `ThrottlingException` (429), Marketplace payment failures (403). No Fiber
  request reaches Bedrock's Invoke API today: `anthropic-messages` posts to
  `<base_url>/messages` and reads SSE, and the AWS framing that
  `docs/model-routing.md` names for Bedrock is not built (probe #221 is
  open), so a Bedrock error cannot reach `status_code`. Bedrock names the
  exception only in the `X-Amzn-ErrorType` header, which Fiber does not
  keep. The quota exception's own docs say "You can resubmit your request
  later", so whether it is `quota_exceeded` or `rate_limited` is itself
  unsettled. Classify Bedrock errors in the ticket that builds its framing.
- N9: ChatGPT/codex `usage_limit_reached` and `usage_not_included`. They
  belong to #311.
- N10: OpenAI Responses in-stream failures. The `response.failed`
  `error.code` enum (server_error, rate_limit_exceeded, invalid_prompt,
  vector_store_timeout, bio_policy, image codes) has no quota code.
