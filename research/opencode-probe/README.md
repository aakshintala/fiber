# OpenCode Go and Zen probe

Questions: does one key cover OpenCode Go and OpenCode Zen; which protocols Go
serves, and over which `muse-spark-1.3-contributor` answers; what a response
says about cost; whether the model lists tell Go from Zen. Also: one streamed
tool-calling exchange with `muse-spark-1.3-contributor` on Go, as raw bytes for
the provider tests.

## Method

`probe.py` on October 1, 2026, from macOS, with one key read from
`/tmp/opencode-key` and never written out (each raw file is checked for it).
Steps: `models`, `go_protocols`, `go_stream_tools`, `zen`, `usage`. Raw requests
and responses are in `raw/<step>.json`; the two tool-exchange streams are
`raw/go_stream_tools_0.sse` (the tool call) and `raw/go_stream_tools_1.sse` (the
answer after the tool result).

Spend: Go is the owner's subscription; 10 requests. Zen is billed per token: two
model-list GETs and 2 requests, one of which failed, and the other used 12 input and 5 output tokens on
`gpt-6-luna` ($0.10 and $0.50 per million), under $0.00001.

## pi's reference

pi (`@earendil-works/pi-ai`, under
`/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/node_modules/`)
has two providers on one key, `OPENCODE_API_KEY` (`dist/env-api-keys.js:106-107`):

- `opencode-go`, "OpenCode Go" (`dist/providers/opencode-go.js:8-19`):
  `anthropic-messages` at `https://opencode.ai/zen/go`, `openai-completions` and
  `openai-responses` at `https://opencode.ai/zen/go/v1`
  (`dist/providers/data/opencode-go.json`). `muse-spark-1.3-contributor` is
  listed under `openai-responses`, at $0.10 and $0.20 per million.
- `opencode`, "OpenCode Zen" (`dist/providers/opencode.js:10-24`): the same three
  plus `google-generative-ai`, at `https://opencode.ai/zen` and
  `https://opencode.ai/zen/v1`, and a `typesafe-system-one` classifier protocol.
- Both add `x-opencode-session` with the session id
  (`dist/providers/opencode-headers.js:1-21`).
- pi gives Go models API prices and no subscription marker. It treats
  `GoUsageLimitError`, `FreeUsageLimitError` and "Monthly usage limit reached" as
  limits not to retry (`dist/utils/retry.js:4-21`).

## Findings

- **One key, two base URLs.** The key answered on Go
  (`https://opencode.ai/zen/go/v1/...`) and on Zen (`https://opencode.ai/zen/v1/...`).
- **Requests need a `User-Agent`.** With Python's default, both model lists
  answered 403 "error code: 1010" (Cloudflare). `fiber-probe/0.1` passed.
- **Go serves three protocols, and a model speaks one.**
  `muse-spark-1.3-contributor` answered 200 on `/zen/go/v1/responses`, and 400
  `{"type":"ModelProtocolUnsupported","message":"Model does not support this
  protocol."}` on `/v1/chat/completions` and `/v1/messages`. `glm-5.3-flash`
  answered on `/v1/chat/completions` and `qwen3.8-flash` on `/v1/messages`, as pi
  lists them (`raw/go_protocols.json`).
- **The model lists differ.** `GET /zen/go/v1/models` returned 30 ids,
  including `muse-spark-1.2-contributor` and `muse-spark-1.3-contributor`.
  `GET /zen/v1/models` returned 18, including `muse-spark-1.3` and
  `muse-spark-1.3-contributor-free`, but not the Go model. Entries hold `id`,
  `object`, `created` and `owned_by` only: no price, protocol or plan
  (`raw/models.json`). `muse-spark-1.3-contributor` on Zen's URL answered 400
  `{"error":{"type":"server_error","message":"Upstream request failed: Model is
  unavailable."}}` (`raw/zen.json`).
- **No cost on either plan.** No response body or header carried a cost or a
  subscription marker. Both carry `usage` in the protocol's own shape. Headers
  name the routing: `x-opencode-endpoint-id` (for example `meta` for muse),
  `x-opencode-upstream-model-id`, `x-zen-model` and `x-opencode-log-id`.
- **The tool exchange worked.** On Go, streamed, with one strict function tool:
  a `function_call` item with `response.function_call_arguments.delta` and
  `.done`, then `response.completed`. The follow-up with the
  `function_call_output` answered "The weather in Paris is 18°C and clear." The
  second request read 497 of 616 input tokens from the cache with the same
  `x-opencode-session`.
- **Go reports quota.** `GET /zen/go/v1/usage` answered `rolling`, `weekly` and
  `monthly`, each with `status`, `percent` and `resetsAt` (`raw/usage.json`).
  `percent` stayed 0 after these requests.

## Scope

One key, one day, one machine. The Go limit errors pi names were not reached, so
their shape is unmeasured. The Zen-only `google-generative-ai` route was not
called.
