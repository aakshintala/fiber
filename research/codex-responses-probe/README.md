# The ChatGPT codex endpoint, probed live

Findings for [#135](https://github.com/aakshintala/fiber/issues/135). Probed on 2026-09-29 (macOS) against
`https://chatgpt.com/backend-api/codex/responses` with model `gpt-6-luna` and the owner's ChatGPT login (plan: plus).
Facts below are for that model, that day and that platform.

## Method

`probe.py <group>` sends one streaming request per case (raw `urllib`, no zstd) and saves status, response headers,
request body, the full SSE event list and usage to `raw/<case>.json`. The access token, account id and refresh token are
read inside the script and never printed or saved; the account id header is not recorded. `raw/count.txt` holds the
request count: 49 of the 60 allowed, sent at least 3 seconds apart. The raw `sent_headers` field lists header names only; `probe.py` shows how each value was generated (stable within a pair or fresh per request); the actual values were not recorded. No 429 or other error except the deliberate 400s
below. Cost is plan quota, not dollars: the `x-codex-primary-used-percent` response header (5-hour window) moved from
30 to 57 and the secondary (weekly) from 8 to 13 across the run, almost all of it from the 13 distinct 9,000-token cache
prompts, each sent twice. Cache groups use random prompts, so no group could hit another's cache.

Baseline body: `model`, `instructions: "You are a helpful assistant."`, `store: false`, `stream: true`, `input`,
`include: ["reasoning.encrypted_content"]`. Headers: `Authorization`, `chatgpt-account-id`, `originator`, `User-Agent`,
`Accept: text/event-stream`, plus `OpenAI-Beta: responses=experimental` and a session id unless a case says otherwise.

## Does the SSE endpoint send `response.done`?

No. In 4 streams (2 text, 2 tool-call, one with two parallel calls), and in every other 200 stream, the last event is
`response.completed`; none contains `response.done`. Raw: `raw/ev-*.json` (field `events`). Event order:

- Text: `response.created`, `response.in_progress`, `response.output_item.added`, `response.content_part.added`,
  N x `response.output_text.delta`, `response.output_text.done`, `response.content_part.done`,
  `response.output_item.done`, `response.completed`.
- Tool call: `response.created`, `response.in_progress`, `response.output_item.added`, N x
  `response.function_call_arguments.delta`, `response.function_call_arguments.done`, `response.output_item.done`,
  `response.completed`. Two parallel calls repeat the added/delta/done/done group twice before `response.completed`.
- A reasoning reply (`f-verb-*.json`) adds reasoning items (`rs_` ids) before the message.

The wire is the plain Responses stream. The `response.completed` object carries no `end_turn` field. Scope: SSE only;
the WebSocket transport was not probed.

## Is `OpenAI-Beta` required?

No. Same request with `responses=experimental` (`raw/beta-with.json`), without the header (`raw/beta-without.json`),
without it and without any session id (`raw/beta-without-nosid.json`), and with pi's WebSocket value
`responses_websockets=2026-02-06` on the SSE endpoint (`raw/beta-other.json`): all four 200, same 13 events, same
22 input tokens. Status, event count and input tokens matched; the reply text varied between requests
("Hi there, friend!" and "Hi there, friend."). One request body only.

## Which fields does the endpoint accept, and do they change behaviour?

| Field | Result | Raw |
|---|---|---|
| `parallel_tool_calls: true` | 200. Two tool calls for a two-city prompt | `f-parallel-true.json` |
| `parallel_tool_calls: false` | 200. One tool call; the reported input grew from 61 to 139 tokens (`tools` attribution 33 to 111) | `f-parallel-false.json` |
| `parallel_tool_calls` omitted | 200. Two calls, same as `true`; the response object echoes `true` | `f-parallel-omitted.json` |
| `tool_choice: "auto"` | 200, model answered in text | `f-toolchoice-auto.json` |
| `tool_choice: "required"` | 200, tool called | `f-toolchoice-required.json` |
| `text.verbosity: "low"` / `"high"` / omitted | 200 all three. Output tokens for one prompt: 194 / 246 / 304 (one sample each, so the effect is not established; the response object echoes `medium` when omitted) | `f-verb-*.json` |
| `temperature: 0.5` and `0` | 400, body `{"detail":"Unsupported parameter: temperature"}` | `f-temp.json`, `f-temp0.json` |
| `service_tier: "priority"` | 200, no visible change (the completed object says `default`) | `f-tier-priority.json` |
| `service_tier: "default"` | 200, no visible change | `f-tier-default.json` |
| `service_tier: "flex"` | 400, `{"detail":"Unsupported service_tier: flex"}` | `f-tier-flex.json` |
| `service_tier: "auto"` | 400, `{"detail":"Unsupported service_tier: auto"}` | `f-tier-auto.json` |

Each case is one request. rig's clearing of `temperature` is right; its clearing of `parallel_tool_calls` and
`text.verbosity` is not needed (both accepted). pi passes `temperature` through if the caller sets one, which this
endpoint would reject. pi's `resolveCodexServiceTier` premise holds: a request for `priority` came back as `default`.

## Does a per-request id lose the cache?

The prompt was about 9,000 input tokens (random words, distinct per group). Each group sent the prompt twice, the
second 24 to 27 seconds after the first (`probe.py` sleeps 20 seconds plus 3 seconds of pacing; gaps are from the response `Date` headers). "Stable" means one uuid reused; "fresh" a new uuid each request.

| Group | Request 2 cached tokens | Raw |
|---|---|---|
| stable `session_id` + `x-client-request-id` + `prompt_cache_key` (round 2) | 7,936 | `cache-2-stable-*.json` |
| same, repeated (round 3) | 7,936 | `cache-3-stable-*.json` |
| fresh on all three, each request (rounds 2, 3) | 0, 0 | `cache-2-fresh-*.json`, `cache-3-fresh-*.json` |
| stable `prompt_cache_key`, fresh `session_id` header | 0 | `cachesplit-*.json` |
| stable `prompt_cache_key` only, no session headers | 0 | `cachekeyonly-*.json` |
| stable `session_id` + `x-client-request-id`, no `prompt_cache_key` | 7,936 | `cachehdronly-*.json` |
| stable `session_id` (underscore) header only | 7,936 | `cachehdr-sessionid-underscore-*.json` |
| stable `session-id` (hyphen) header only | 7,936 | `cachehdr-session-id-hyphen-*.json` |
| stable `x-client-request-id` only | 0 | `cachehdr-xreq-only-*.json` |
| no key, no headers | 0 | `cachenone-*.json` |

Answer: yes. On this endpoint the cache is routed by the `session_id` header (either spelling). `prompt_cache_key`
and `x-client-request-id` alone did not hit. A per-request session id loses the whole cache, as rig's does. Round 1
(`cache-1-*.json`) sent the second request 4 to 5 seconds after the first (script pacing 3 seconds) and got 0 cached tokens even with all three ids
stable; the gap was raised to 20 seconds of sleep for every later group. Whether the gap explains the miss is untested.
The hit is 7,936 tokens of about 9,000.

## The usage-limit error body

Not probed: no request reached a usage limit. The shape below comes from two reference implementations, read on
2026-10-07.

- pi (`@earendil-works/pi-ai` 1.0.0), `parseErrorResponse` in `dist/api/openai-codex-responses.js`:1240-1262. It reads
  `error.code`, falling back to `error.type`, and treats `usage_limit_reached`, `usage_not_included` or
  `rate_limit_exceeded` (or any 429) as the usage limit. It reads `error.plan_type` and `error.resets_at` (Unix
  seconds) for its message, "You have hit your ChatGPT usage limit (<plan> plan). Try again in ~N min."
- codex-cli 0.160.0: `strings` on the binary finds the error codes `usage_limit_reached` and `usage_not_included`
  beside `quota_exceeded`, `server_overloaded` and the other codes it reports.

Fiber matches only the two usage-limit codes as `quota_exceeded`. `rate_limit_exceeded` names a rate limit, so it stays
retryable. pi's free-text match (`isTerminalRateLimitError`, lines 52-54) is not adopted.

## Login

Not probed by Fiber: no login flow ran here. The constants below come from two reference implementations, read on
2026-10-07, and the package sends them unchanged.

- pi (`@earendil-works/pi-ai` 1.0.0), `dist/auth/oauth/openai-codex.js`:18-30: client id
  `app_EMoamEEZ73f0CkXaXp7hrann`, the authorize, token and redirect URLs (`http://localhost:1455/auth/callback`),
  the device user-code, token and page URLs, the device redirect (`https://auth.openai.com/deviceauth/callback`) and
  the scope `openid profile email offline_access`.
- pi's authorize parameters (`openai-codex.js`:225-240): `response_type=code`, `code_challenge_method=S256`,
  `id_token_add_organizations=true`, `codex_cli_simplified_flow=true` and `originator` (Fiber sends `fiber`).
- pi's token reply (`openai-codex.js`:99): the reply must carry `access_token`, `refresh_token` and a numeric
  `expires_in`; the account id is read from the access token's `https://api.openai.com/auth` claim
  `chatgpt_account_id`.
- pi's device flow (`openai-codex.js`:142-224): the user-code request sends `{"client_id": ...}` as JSON; a poll
  returning 403 or 404 is still pending; the completed poll returns `authorization_code` and `code_verifier`.
- codex-cli 0.160.0: `strings` on the binary holds the same client id.
- The owner's token claims (probed 2026-10-07, local decode only): the access token's `exp - iat` is 864000 s, and
  the `id_token` carries the email. Fiber takes `expires_at` from the access token's `exp`, never the id token's.

## Malformed replies

None. All 200 streams ended in `response.completed`; tool arguments parsed as JSON in the streams checked.

## For the owner

- Decide, system messages after the first: not probed (the ticket's probe list does not cover it).
- Decide, default instructions text: not probed. The endpoint accepted `"You are a helpful assistant."` (pi's text)
  on all 200 requests; rig's text was not sent, so nothing here favours either.
- Evidence for the `session_id` design: the session id header must be stable per root session and is the cache key on
  this endpoint; `prompt_cache_key` is not enough. docs/prompt-cache.md is corrected to say so.
- The run cost 27 points of the plus plan's 5-hour window. Large cache prompts are the expensive part.
