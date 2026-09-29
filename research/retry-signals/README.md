# Which retry signals providers send

Live probe for [#131](https://github.com/aakshintala/fiber/issues/131), run on
September 29, 2026 from macOS. `probe.py` reuses `redact()` and its secret-header list from
`../provider-errors/probe.py` and defines its own `send()` (which takes an HTTP
method and a raw body). It saves each response's status, headers and
body to `raw/<vendor>.<case>.json`. Keys, the codex token and account id are
never saved. Cookies, request ids (headers and the error body's `request_id`), organisation and workspace ids, the codex
turn-state header and Cloudflare reporting headers are redacted.

`probe.py errors <vendor>` sends the cheap failing requests. `probe.py burst
<name>` sends a capped burst and stops dispatching after the first 429.

## Models, prices and spend

| Vendor | Model | Price per million tokens | Source | Spend |
|---|---|---|---|---|
| Anthropic | `claude-haiku-4-5` | $1 in, $5 out | platform.claude.com/docs/en/about-claude/pricing | under $0.01: 1 model call at `max_tokens` 1; the bursts used the free `count_tokens` |
| OpenAI | `gpt-6-luna` | $0.10 in, $0.50 out | developers.openai.com/api/docs/pricing | under $0.01: 2 calls, 16 output tokens at most |
| Gemini | `gemini-3.1-flash-lite` | $0.25 in, $1.50 out | ai.google.dev/gemini-api/docs/pricing | under $0.01: 102 calls with a one-token prompt and `maxOutputTokens` 8, thinking budget 0 (`raw/gemini.ok.json` reports 1 prompt and 4 candidate tokens) |
| ChatGPT/codex | `gpt-6-luna` | plan, not per token | | 9 requests, one of them a model call |
| OpenRouter | `z-ai/glm-5.3-flash` | not looked up | | one 8-token call |
| muse | `muse-spark-1.3-contributor` | not looked up | | 1 normal 8-token call, 3 rejected oversized-limit requests |

Spend is an upper-bound estimate from the request sizes, not read from a
dashboard. No cap came close.

## Requests sent

Failing requests per vendor: bad key (401; Gemini returns 400), unknown model, malformed body
(missing field, and invalid JSON where it was sent), oversized output-token
limit, wrong method, wrong path. Every vendor also got one normal request.
Bursts:

- Anthropic `count_tokens`: 122 requests, concurrency 20. 102 returned 200, then 20 returned 429 (`raw/anthropic.count-tokens-burst*.json`).
- Gemini `countTokens`: 300 requests per run, concurrency 20, four runs. Only the last run's file is committed (`raw/gemini.count-tokens-burst.json`, 300 x 200). In the first run 298 returned 200 and 2 returned 503; that run and the two before the last have no raw file, so the 503s' headers and body were not saved.
- Gemini `generateContent`: 100 tiny requests, concurrency 20, all 200 (`raw/gemini.model-burst.json`).
- Anthropic and OpenAI model bursts skipped: their normal responses reported 1,000 (Anthropic messages) and 500 (OpenAI) requests a minute, above the 200 threshold.
- codex, OpenRouter and muse were never burst.

## Answers

### Does any provider return 409 or 425?

Not among the statuses reached. Statuses seen, all vendors: 200, 400, 401,
403, 404, 405 and 429 in the saved raw files. A 503 was also seen twice in a Gemini run that has no raw file. No 409, 425 or 501 in any raw file or in the terminal output of any run. Scope:
about 50 saved responses, cheap failure triggers only, macOS, one day, and
429s only at Anthropic `count_tokens` and muse. Databricks and OpenCode were
not reached: no keys.

### Which providers send `x-should-retry`, `retry-after-ms` or `retry-after`?

| Vendor | `x-should-retry` | `retry-after-ms` | `retry-after` | Rate-limit headers |
|---|---|---|---|---|
| Anthropic | `false` on 400 and 404 (`anthropic.malformed-*`, `unknown-model`, `max-tokens-huge`, `wrong-path`); `true` on the 429 (`anthropic.count-tokens-burst-first-429`); absent on 200, 401 and 405 | never | `1` on the 429 | `anthropic-ratelimit-*` on 200 (requests, input tokens, output tokens) and, for requests, on the 429 |
| OpenAI | never | never | never | `x-ratelimit-*` on 200 only |
| Gemini | never | never | never | none |
| ChatGPT/codex | never | never | never | `x-codex-*` usage-window headers on 200 and one 400 |
| OpenRouter | never | never | never | none |
| muse | never | never | `60` on a 429 (`muse.max-tokens-huge`) | `x-ratelimit-*` on 200 and the 429 |

"Never" means not present in any response reached at that vendor, and no
vendor was pushed into a 429 except the two shown. OpenAI, Gemini, codex and
OpenRouter 429 headers are unknown.

The Anthropic 429 came from the `count_tokens` limit (100 requests a minute),
which is separate from the messages limit (1,000 a minute). Its message says
"organization's rate limit of 100 requests per minute".

### Gemini: does a 429 carry `Retry-After` or only `RetryInfo`?

Not reached. About 100 model calls and 1,200 `countTokens` calls produced no 429. Gemini's 400, 404 and 200 responses carry
no retry header. The two 503s from the first `countTokens` run have no raw file, so their headers are unknown.

### ChatGPT/codex: does it return 501?

Not among 9 requests. Unsupported methods (GET, PUT, PATCH, TRACE) on
`/backend-api/codex/responses` returned 405 (`{"detail":"Method Not Allowed"}`;
TRACE returned a Cloudflare HTML page). An unknown sub-path returned 403 with a
Cloudflare challenge page (`cf-mitigated: challenge`). An unknown model
returned 400. Only those triggers were tried, so this says nothing about
whether the backend sends 501 in other states.

## Other facts seen

- An oversized output-token limit is rejected differently by each vendor: Anthropic 400 ("max_tokens: 50000000 > 64000"), OpenRouter 400 (context check), muse 429 with `retry-after: 60`, OpenAI and Gemini accepted 50,000,000 and returned 200. The muse case is the one in `research/provider-errors/`.
- Wrong method: Anthropic, OpenAI, muse and codex 405; Gemini and OpenRouter 404.
- OpenRouter answers a bad key with 401 "Missing Authentication header".

## For the owner

Decide item, a failure with no HTTP status. This probe produced no such
failure: every request got an HTTP status. It gives no evidence either way.

Evidence bearing on `x-should-retry`: among the vendors reached, only
Anthropic sends it, in the responses probed the override could only fire for Anthropic. Anthropic
marked its 429 `true` and its 400 and 404 responses `false`; its 401 and 405
responses carried no `x-should-retry`.

Evidence bearing on how a retrying client treats `retry-after`: muse answered
an oversized `max_tokens` with a 429 and `retry-after: 60`
(`raw/muse.max-tokens-huge.json`). Sent three times, it returned the same 429
and `retry-after: 60` each time (`raw/muse.max-tokens-huge-repeat2.json`,
`-repeat3.json`), with the rate-limit headers still showing full capacity. A
client that retries on 429 and honours the header would loop on it.

Evidence bearing on 409 and 425: neither appeared. Both statuses stay
unobserved, not ruled out.

## Malformed replies

None.
