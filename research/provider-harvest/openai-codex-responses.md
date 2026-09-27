# The openai-codex-responses wire protocol: pi and rig side by side

Read for Fiber's native implementation of the ChatGPT-subscription backend
(`chatgpt.com/backend-api/codex/responses`), used by the codex CLI. Two
reference implementations, read line by line:

- pi (TypeScript, compiled but unminified): `@earendil-works/pi-ai` version
  0.87.1, installed at
  `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/`.
  Main file `api/openai-codex-responses.js` (1,302 lines), compared against
  `api/openai-responses.js` (293 lines, plain OpenAI Responses) and the code
  both share, `api/openai-responses-shared.js` (689 lines).
- rig (Rust): `~/work/rig`, commit `42f4e06` on `main`, dated 2026-09-26.
  `crates/rig-core/src/providers/chatgpt/` (mod.rs, auth/mod.rs,
  auth/native.rs, auth/wasm.rs — 745 lines including a 132-line test file)
  and the three `ResponsesContract::Codex` branches inside
  `crates/rig-core/src/providers/openai/responses_api/wire.rs` (lines 56,
  165, 248).

Already-mined and read first, not repeated here except where it bears
directly on codex: `research/rig/README.md` (rig's wire protocols, SSE
parsing, errors and retries, vendor quirks) and
`research/provider-errors/README.md` (live error probe; notes
`openai-codex-responses` was never reached, no key available).

## What pi's codex module does

`api/openai-codex-responses.js` is 1,302 lines against plain Responses'
293. The size difference is not mostly wire protocol. Region by region,
with a class mark: (a) genuine wire difference from plain Responses, (b)
auth/transport that sits outside the protocol, or (c) duplicated code that
could be shared with `openai-responses.js` or its retry helper.

| Region | Lines | Count | Class | What it is |
|---|---|---:|---|---|
| Imports | 1-16 | 16 | c | Ordinary module imports |
| Config constants | 17-40 | 24 | a | Codex base URL, JWT claim path, zstd level, tool-call-provider set, websocket close/error codes, the closed set of response statuses |
| `assertSuccessfulOutput` | 41-48 | 8 | c | Same pending/error/aborted check plain Responses does inline (openai-responses.js:137-142), pulled into its own function here |
| `isTerminalRateLimitError` | 52-54 | 3 | a | ChatGPT-specific usage-limit text match (`GoUsageLimitError`, `FreeUsageLimitError`, "Monthly usage limit reached", "insufficient_quota", "quota exceeded", "billing") that turns a 429 non-retryable |
| `isRetryableError` | 55-63 | 9 | c | Status-code retry policy (429/500/502/503/504 plus a text regex fallback) reimplemented by hand |
| `getRetryAfterDelayMs` | 64-85 | 22 | c | `retry-after-ms` then `retry-after` parsing, duplicating `utils/provider-retry.js`'s `getRetryDelayMs` |
| `RetryDelayExceededError` / `validateRetryDelayMs` | 86-94 | 9 | c | Duplicates `validateServerRetryDelayMs` in `provider-retry.js` |
| `sleep` | 95-107 | 13 | c | Duplicates `abortableSleep` in `provider-retry.js` |
| `normalizeTimeoutMs` | 108-115 | 8 | b | Manual timeout arithmetic needed because codex uses raw `fetch`, not the OpenAI SDK's `timeout` option |
| `loadNodeZlib` / `compressRequestBodyZstd` | 116-139 | 24 | a | Compresses the SSE POST body with zstd (level 3) and sets `content-encoding: zstd`; the Codex backend accepts this, plain Responses (via the OpenAI SDK) never does this |
| `stream()`: output object literal | 143-163 | 21 | c | Same `AssistantMessage` shape as plain Responses, `api` field differs |
| `stream()`: accountId/session id resolution | 165-172 | 8 | a | Extracts `chatgpt-account-id` from the access token's JWT; resolves the cache/session id |
| `stream()`: `buildRequestBody` + `onPayload` hook | 173-177 | 5 | c | Same hook pattern as plain Responses |
| `stream()`: websocket request id + header builders | 178-180 | 3 | a | Distinct SSE vs websocket header sets |
| `stream()`: transport/timeout option resolution | 181-184 | 4 | b | Plumbing |
| `stream()`: websocket dispatch loop | 185-246 | 62 | a | Tries the websocket transport first (unless disabled or already fallen back for this session); retries once on `previous_response_not_found` or `websocket_connection_limit_reached`; records diagnostics and falls back to SSE on any other transport failure |
| SSE: zstd body + `content-encoding` header | 247-254 | 8 | a | See above |
| SSE: fetch-with-retry loop | 255-321 | 67 | c | Reimplements `retryProviderRequest`'s loop (fresh request per attempt, exponential backoff, retry-after) by hand because codex uses raw `fetch` |
| SSE: response validation, `start`/`done`/`error` emission | 322-353 | 32 | c | Same shape as plain Responses' tail |
| `streamSimple` | 354-369 | 16 | c | Exact same wrapper pattern as plain Responses' `streamSimple` |
| `buildRequestBody` | 370-431 | 62 | a | The single largest concentration of genuine request-body differences — see the fact table below |
| `getServiceTierCostMultiplier` / `applyServiceTierPricing` | 432-451 | 20 | c | Byte-for-byte duplicate of the same two functions in `openai-responses.js:273-292` |
| `resolveCodexServiceTier` | 452-457 | 6 | a | Codex-specific quirk: if the response echoes `service_tier: "default"` but the request asked for `flex`/`priority`, trust the request's value for pricing |
| `resolveCodexUrl` / `resolveCodexWebSocketUrl` | 458-474 | 17 | b | URL path resolution (`/codex/responses`) and `https:`→`wss:` scheme swap |
| `processStream` | 478-485 | 8 | c | Thin glue composing `parseSSE` + `mapCodexEvents` + the shared `processResponsesStream` |
| `CodexApiError` / `CodexProtocolError` and predicates | 486-514 | 29 | a | Typed errors keyed to codex-only event/error codes, used to decide the two one-shot retries above |
| `extractCodexEventError` / `mapCodexEvents` / `normalizeCodexStatus` | 515-562 | 48 | a | Event-vocabulary adapter: throws on a top-level `error` event and on `response.failed`; folds `response.done`, `response.completed` and `response.incomplete` into one re-tagged `response.completed` event, and reads a boolean `end_turn` off the response |
| `parseSSE` | 563-628 | 66 | c | Hand-rolled SSE reader, needed because codex uses raw `fetch` rather than the OpenAI SDK's built-in stream parsing; the framing itself (`data:` lines, blank-line separated, `[DONE]` sentinel) is standard SSE, not a codex quirk |
| Websocket: debug stats + session cache scaffolding | 629-707 | 79 | b | Bookkeeping and a `Map` of live sockets keyed by session/account id |
| Websocket: constructor + bun proxy workaround | 708-737 | 30 | b | Runtime workaround, not wire |
| Websocket: `connectWebSocket` / `acquireWebSocket` pooling | 738-939 | 202 | b | Connection lifecycle: open, reuse if idle and not expired, close and evict otherwise |
| Websocket: error/close/data decoding | 940-993 | 54 | b | Generic websocket event-to-error/text decoding |
| Websocket: `parseWebSocket` event loop | 994-1104 | 111 | b | Turns websocket messages into the same JSON event stream `mapCodexEvents` consumes; detects completion from message content, not from framing |
| Websocket: delta-continuation mechanic | 1105-1145 | 41 | a | `requestBodyWithoutInput`/`getCachedWebSocketInputDelta`/`buildCachedWebSocketRequestBody`: on a reused connection, diffs the new input against the last request+response and sends only the delta plus `previous_response_id`, instead of resending full history |
| Websocket: `startWebSocketOutputOnFirstEvent` | 1146-1155 | 10 | b | Glue: defers emitting `start` until the first event arrives |
| Websocket: `processWebSocketStream` | 1156-1220 | 65 | a | Sends the `{type: "response.create", ...body}` envelope, runs the stream, saves the continuation entry (response id + response items) for the next turn on this connection |
| `parseErrorResponse` | 1224-1246 | 23 | a | ChatGPT-specific error body: `usage_limit_reached`/`usage_not_included`/`rate_limit_exceeded` plus `plan_type`/`resets_at` produce "You have hit your ChatGPT usage limit ... Try again in ~N min." |
| Auth & Headers: `extractAccountId`, `buildBaseCodexHeaders`, `buildSSEHeaders`, `buildWebSocketHeaders` | 1250-1302 | 53 | a | `chatgpt-account-id`, `originator`, `User-Agent`, `OpenAI-Beta` (two different values for SSE vs websocket), `session-id`/`x-client-request-id` |

Totals (approximate; roughly 16 lines of section-header comments and blank
separators are not attributed to any region):

- (a) genuine wire difference: ≈459 lines
- (b) auth/transport outside the protocol: ≈515 lines
- (c) duplicated, could be shared: ≈312 lines

The single largest region by line count is the websocket transport
(constants, dispatch, connection pooling, event decoding, delta
continuation, envelope send — lines 629 through 1220, roughly 592 lines,
split above into (a) and (b) pieces). Strip the websocket transport out
entirely and the remaining SSE-only codex module is closer to 700 lines,
of which the genuinely codex-specific wire facts (`buildRequestBody`,
header building, event-vocabulary adapter, ChatGPT error body, zstd
compression) total under 250 lines — comparable in size to what rig
expresses as three `if` branches plus the 613-line `chatgpt/` auth module.

## Protocol or variant

Read: codex is a Responses variant with flags, not a distinct wire
protocol — but pi's websocket transport and its always-forced request
flags are real behavior Fiber has to decide whether to replicate, not
just declare away.

Evidence for "variant, not protocol":

- Every event type codex emits over SSE (`response.created`,
  `response.output_item.added/done`, `response.output_text.delta`,
  `response.function_call_arguments.delta/done`, `response.reasoning_*`,
  `response.completed`/`incomplete`/`failed`, `error`) is consumed by the
  exact same shared decoder pi uses for plain Responses
  (`openai-responses-shared.js:320-659`, imported and called unchanged from
  `openai-codex-responses.js:479,1189`). Tool-call assembly and reasoning-item
  handling are byte-identical code paths, not reimplemented.
- rig encodes the whole difference as three `if quirks.contract ==
  ResponsesContract::Codex` guards in one shared encoder/decoder
  (`responses_api/wire.rs:56,165,248`) plus a 75-line `Dialect` constant
  (`chatgpt/mod.rs:44-67`) that turns on always-streaming, an
  `AllInstructions` system-message placement, and one decoder flag
  (`with_envelope_repair()`). No separate parser exists on rig's side at
  all.
- The websocket transport itself is not evidence of a distinct protocol:
  rig's `responses_api/websocket.rs` (909 lines) implements the identical
  `response.create` / `response.done` / delta-continuation mechanic
  generically for *any* Responses-capable dialect, gated behind a Cargo
  feature, not behind `ResponsesContract::Codex`. OpenAI's own websocket
  mode for Responses is a documented feature of the plain API; pi simply
  never wired it up for its plain `openai-responses.js`, only for codex.
  That is a pi implementation choice, not a fact about the wire.

Evidence that keeps this from being "just a dialect flag" the way rig
treats it:

- pi's codex request body forces several fields plain Responses leaves to
  the caller: `parallel_tool_calls: true` always, `tool_choice: "auto"`
  default, `text.verbosity` always present, `stream: true` always (even
  conceptually for a "non-streaming" call). rig's Codex contract goes the
  *opposite* direction on some of the same fields — it clears
  `parallel_tool_calls`, `service_tier` and `temperature` to `None` rather
  than forcing or passing them through (`wire.rs` lines in the Codex branch
  starting at 165). Two independent readings of the same backend disagree
  on what it wants sent; see Disagreements below.
- pi's websocket path has a stateful continuation mechanic keyed to a
  session id that also serves as the SSE `prompt_cache_key` and the
  `session-id`/`x-client-request-id` headers — one identifier doing three
  jobs. rig's equivalent session id (`chatgpt::session_id()`,
  `chatgpt/mod.rs:69-71`) is explicitly "a per-request session correlator
  for transport headers only," freshly generated every call, disconnected
  from `prompt_cache_key`. If Fiber wants pi's cross-purpose reuse of one
  id, that has to be designed in, not inherited from treating codex as a
  flag.
- The ChatGPT usage-limit error body (`usage_limit_reached`, `plan_type`,
  `resets_at`) and the free-text `GoUsageLimitError`/`FreeUsageLimitError`
  retry-suppression regex are wire facts about this one backend that
  neither rig nor Fiber's error taxonomy (`docs/errors.md`) currently name.

Net: the request/response JSON shape is close enough to plain Responses
that rig's three-guards-in-one-encoder design is defensible and Fiber's
ADR 0007 (treating codex as a fifth native protocol) is not required by
the wire shape alone. What is not optional is deciding, as explicit design
choices rather than inherited defaults: whether Fiber forces or omits
`parallel_tool_calls`/`tool_choice`/`service_tier`/`temperature` the way
pi does, whether Fiber implements the websocket transport and its delta
continuation at all, and whether one id serves prompt-cache and transport
correlation or two separate ones do. The owner decides; this is evidence,
not the ruling.

## Request fields and headers

| Fact | pi | rig | Mark |
|---|---|---|---|
| `store: false` forced | `openai-codex-responses.js:391` | `wire.rs:165` region, `additional_parameters.store = Some(false)` | agree |
| `stream: true` forced regardless of caller's streaming/non-streaming intent | `openai-codex-responses.js:392` | `wire.rs:56`, `let streaming = matches!(mode, Mode::Streaming) \|\| codex;` | agree |
| System messages lifted into top-level `instructions` | Only the *leading* system message; mid-conversation system messages stay in `input` as system/developer-role items (`openai-codex-responses.js:387-388`, `openai-responses-shared.js:130-139`) | *Every* system item, wherever it appears, lifted into `instructions` (`SystemInstructionsPlacement::AllInstructions`, `responses_api/mod.rs:1093-1104,1191-1202`; `chatgpt/mod.rs:59`) | disagree |
| Default instructions text when none given | `"You are a helpful assistant."` (`openai-codex-responses.js:393`) | `"You are ChatGPT, a helpful AI assistant."` (`chatgpt/mod.rs:26`) | disagree |
| `text.verbosity` field | Always sent, default `"low"` (`openai-codex-responses.js:395`) | Cleared to `None` for Codex (`wire.rs:165` region, `additional_parameters.text = None`) | disagree |
| `include: ["reasoning.encrypted_content"]` | Always sent unconditionally (`openai-codex-responses.js:396`) | Always ensured present (pushed if missing) regardless of reasoning state (`wire.rs:165` region) | agree |
| `tool_choice` default | Always sent, defaults to `"auto"` if caller gave none (`openai-codex-responses.js:398`) | Not touched by the Codex branch; only sent if the caller set one, same as plain Responses | disagree |
| `parallel_tool_calls` | Always sent as `true` (`openai-codex-responses.js:399`) | Cleared to `None` for Codex (`wire.rs:165` region) | disagree |
| `max_output_tokens` | Never set in the codex body builder at all (no line sets it) | Explicitly cleared to `None` for Codex (`wire.rs:165` region) | agree (field absent both ways) |
| `temperature` | Passed through if caller supplied one (`openai-codex-responses.js:401-403`) | Always cleared to `None` for Codex (`wire.rs:165` region) | disagree |
| `service_tier` | Passed through if caller supplied one (`openai-codex-responses.js:404-406`); `resolveCodexServiceTier` corrects pricing if the response echoes `"default"` (`openai-codex-responses.js:452-457`) | Always cleared to `None` for Codex (`wire.rs:165` region) | disagree |
| `background`, `metadata`, `top_p`, `user` | Not referenced anywhere in the codex module | Explicitly cleared for Codex (`wire.rs:165` region) | rig only |
| Reasoning effort mapping through `model.thinkingLevelMap` (an "off" value maps to a model-specific string, or omits `reasoning` if that maps to null) | `openai-codex-responses.js:414-429` | No equivalent — rig's Codex branch does not touch `reasoning`, it passes through whatever the generic Responses conversion set | pi only |
| `chatgpt-account-id` header | `openai-codex-responses.js:1276` | `"ChatGPT-Account-Id"` (`wire.rs:1169`) | agree |
| `originator` header | `"pi"` (`openai-codex-responses.js:1277`) | Configurable, default `"rig"` (`chatgpt/mod.rs:51`, `wire.rs:1156`) | agree (mechanism), value differs by design |
| `User-Agent` header | `getPiUserAgent()` (`openai-codex-responses.js:1278`) | `default_user_agent()` via `Identity` (`wire.rs:1157,405-408`) | agree |
| `OpenAI-Beta` header | `"responses=experimental"` for SSE, `"responses_websockets=2026-02-06"` for websocket (`openai-codex-responses.js:1283,1298`) | Not sent anywhere for chatgpt/codex (no match in `chatgpt/` or the Codex branch of `wire.rs`) | pi only |
| `session-id` / `x-client-request-id` headers | Both set to the same value: the caller's stable session id, clamped as the cache key (`openai-codex-responses.js:1286-1289` for SSE; `1299-1300` for websocket, using a websocket-specific request id) | `session_id` header only (underscore, not hyphen), a fresh random value generated every request via `crate::id::generate()`, explicitly documented as "transport headers only," unrelated to `prompt_cache_key` (`chatgpt/mod.rs:69-71`, `wire.rs:1166`); no `x-client-request-id` sent | disagree |
| `prompt_cache_key` | The caller's session id, clamped (`clampOpenAIPromptCacheKey`), `undefined` if `cacheRetention === "none"` (`openai-codex-responses.js:171-172,397`) | Field exists on the wire type (`responses_api/mod.rs:1507`) and is not cleared by the Codex branch, but its source for chatgpt requests was not traced past the generic Responses conversion in this pass | pi only (confirmed mechanism) |
| Request-body compression (`content-encoding: zstd`) | Compresses the SSE POST body with zstd level 3 when Node's zlib supports it (`openai-codex-responses.js:26-28,116-139,250-253`) | No equivalent found anywhere in `chatgpt/` or `openai/` | pi only |
| Declared response header for extracting a request id on error | No per-provider declared header name in the codex module | `request_id_header: Some("x-request-id")` declared on the `DIALECT` const (`chatgpt/mod.rs:49`) | rig only |
| Codex-specific URL path (`/codex/responses`) | `resolveCodexUrl` appends `/codex/responses` if not already present (`openai-codex-responses.js:458-466`) | `CHATGPT_API_BASE_URL` already ends `.../codex` and `quirks.responses.path` supplies the rest (`chatgpt/mod.rs:23`) | agree |

## Stream events and terminal event

| Fact | pi | rig | Mark |
|---|---|---|---|
| `response.done` as an alternate terminal event | Explicitly matched alongside `response.completed`/`response.incomplete`, re-tagged to `response.completed` before it reaches the shared handler (`mapCodexEvents`, `openai-codex-responses.js:544-554`) | Not a recognized SSE event name for any dialect (`streaming.rs:173-214` has no `response.done` case); `response.done` is only handled in the separate `websocket.rs` decoder (`websocket.rs:159`) | disagree |
| `end_turn` boolean read off the terminal response | Read and stored on `output.endTurn` (`openai-codex-responses.js:546-547`) | No equivalent found in `responses_api/` | pi only |
| Codex-only decoder tolerance for frames missing envelope bookkeeping (ids/index) on replay | No named equivalent; the shared handler's `getSlot`/`getOrCreateSlot` silently returns `undefined` and skips (`openai-responses-shared.js:329-332,410-412`), an implicit tolerance rather than a named repair mode | Explicit `with_envelope_repair()` decoder mode, enabled only for `ResponsesContract::Codex` (`wire.rs:252`, `streaming.rs:905`) | disagree (same goal, different mechanism) |
| Bare `error`-typed event mid-stream | Thrown as `CodexApiError` before reaching the shared handler (`mapCodexEvents`, `openai-codex-responses.js:531-537`); the shared handler has the identical check as a fallback (`openai-responses-shared.js:640-642`) | Classified via `classify_marker_keyed_frame::<ErrorEnvelope>`, not Codex-gated — every dialect's SSE decoder does this (`streaming.rs:851,856,939`) | agree |
| `response.failed` | Extracts `error.code`/`error.message` (`mapCodexEvents`, `openai-codex-responses.js:538-543`) | Recognized for every dialect, not Codex-gated (`streaming.rs:179`) | agree |
| Response status vocabulary is clamped to a closed set before reaching the shared handler | `CODEX_RESPONSE_STATUSES` = completed/incomplete/failed/cancelled/queued/in_progress; anything else becomes `undefined` (`openai-codex-responses.js:33-40,558-561`) | No equivalent status allow-list found; `ResponseStatus` presumably comes from the same enum used for every Responses dialect | pi only |
| Tool-call argument delta assembly (`response.function_call_arguments.delta/done`) | Identical shared code path used for both codex and plain Responses (`openai-responses-shared.js:544-564`) | Identical decoder/fold path used for every `ResponsesContract` (`streaming.rs`, no Codex branch) | agree |
| Reasoning item / encrypted content round-trip | Same shared code (`openai-responses-shared.js:417-432,581-593`); codex forces `include: ["reasoning.encrypted_content"]` on every request so a signature is always available to persist | Same decoder path; Codex branch forces the same `Include::ReasoningEncryptedContent` (`wire.rs:165` region) | agree |

## Tool-call assembly and reasoning items

No codex-specific tool-call or reasoning-item assembly code exists on
either side. Both pi and rig route codex through the exact same
streaming-assembly code as plain OpenAI Responses; the only Codex-specific
touch is forcing `reasoning.encrypted_content` into `include` (both sides,
see above) and rig's `with_envelope_repair()` decoder tolerance (pi has no
named equivalent, see above).

## Prompt-cache keys and session ids

| Fact | pi | rig | Mark |
|---|---|---|---|
| Session id doubles as `prompt_cache_key`, transport header, and websocket connection-pool key | One value (the caller's stable session id, clamped) serves all three purposes (`openai-codex-responses.js:171-172,178,397,860-861,916-918`) | The transport-header session id (`chatgpt::session_id()`) is documented as unrelated to `prompt_cache_key`, freshly generated per request (`chatgpt/mod.rs:69-71`) | disagree |
| Websocket connection reuse keyed by (session id, account id), with idle/age TTLs | 5-minute idle TTL, 55-minute max connection age, reused while not busy and not expired (`SESSION_WEBSOCKET_CACHE_TTL_MS`/`SESSION_WEBSOCKET_MAX_AGE_MS`, `openai-codex-responses.js:633-634`; pooling logic `851-939`) | Not found — rig's generic `websocket.rs` defines the wire messages sent on a connection the *caller* already opened and holds open; connection pooling by session id is left to the embedding application | pi only |
| Delta continuation: on a reused connection, send only new input plus `previous_response_id` instead of full history | `getCachedWebSocketInputDelta`/`buildCachedWebSocketRequestBody` (`openai-codex-responses.js:1105-1145`), only over websocket | Same mechanic, implemented generically for any Responses dialect over websocket, not Codex-gated (`websocket.rs:1-2` doc comment: "chain completed or incomplete response IDs") | agree (mechanism), scoped differently (pi: codex-only; rig: any dialect) |
| Websocket send envelope shape | `{type: "response.create", ...body}` (`openai-codex-responses.js:1188,1156-1220`) | Same `type: "response.create"` envelope, generic (`websocket.rs:66-73`) | agree |

## Errors, retry signals, context overflow

| Fact | pi | rig | Mark |
|---|---|---|---|
| Retry loop exists at all for codex | Yes, hand-rolled around raw `fetch` (`openai-codex-responses.js:255-321`), bypassing the shared `utils/provider-retry.js` that plain Responses uses (`openai-responses.js:122`) | No retry loop anywhere in rig, for any dialect (confirmed in the already-mined `research/rig/README.md`, §5) | pi only (not codex-specific: rig has none for anything) |
| Retryable HTTP statuses | 429 (unless a terminal usage-limit match), 500, 502, 503, 504, plus a text-regex fallback (`rate.?limit\|overloaded\|service.?unavailable\|upstream.?connect\|connection.?refused`) (`isRetryableError`, `openai-codex-responses.js:55-63`) | `retryable_status`: 408, 425, 429, any 5xx (`error.rs:278-284`, per already-mined README) — generic, not codex-specific | disagree |
| `retry-after-ms` / `retry-after` header precedence | Checked in that order (`getRetryAfterDelayMs`, `openai-codex-responses.js:64-85`) | Rig has no retry loop; `Retry-After` is documented only as something a caller could read off `non_success_headers()` (`http_client/mod.rs:88-100`, per already-mined README) | pi only |
| `x-should-retry` header | Checked by plain Responses' shared retry helper (`provider-retry.js:10-13`) but not checked anywhere in the codex module's own `isRetryableError`/`getRetryAfterDelayMs` — an inconsistency between pi's two Responses paths, not a codex-vs-rig fact | not applicable (rig has no retry loop) | pi only, and inconsistent within pi itself |
| ChatGPT usage-limit error body (`usage_limit_reached`, `usage_not_included`, `rate_limit_exceeded`, `plan_type`, `resets_at`) | Recognized and turned into "You have hit your ChatGPT usage limit ... Try again in ~N min." (`parseErrorResponse`, `openai-codex-responses.js:1224-1246`) | No equivalent anywhere in `chatgpt/` or `openai/` (confirmed by grep; matches the already-mined `research/provider-errors/README.md`, which notes `openai-codex-responses` was never reached by the live probe) | pi only |
| Free-text ChatGPT usage-limit strings that suppress retry on a 429 | `GoUsageLimitError`, `FreeUsageLimitError`, `Monthly usage limit reached`, `available balance`, `insufficient_quota`, `out of budget`, `quota exceeded`, `billing` (`isTerminalRateLimitError`, `openai-codex-responses.js:52-54`) | No equivalent | pi only |
| Websocket-specific retryable error codes | `websocket_connection_limit_reached` and `previous_response_not_found`, each triggering exactly one silent reconnect-and-retry (`openai-codex-responses.js:31-32,218-227,509-513`) | No equivalent found anywhere in rig (grepped the whole `rig-core` tree) | pi only |
| Context-overflow detection for codex specifically | None — codex falls through to the same generic `OVERFLOW_PATTERNS` list every OpenAI-family provider uses (`utils/overflow.js`, no ChatGPT-specific string) | None (already-mined README: "rig has no context-overflow ... semantic codes anywhere in the codebase") | agree (neither side treats codex as needing its own signature) |
| Codex was ever independently probed against a live endpoint | No | No | agree — the provider-errors probe (`research/provider-errors/README.md`) explicitly could not reach `openai-codex-responses`; everything above is read from source, not observed on the wire |

## Disagreements

- System-instructions placement: pi lifts only the leading system message
  into `instructions` and leaves mid-conversation system messages in
  `input`; rig's `AllInstructions` placement lifts every system message,
  wherever it sits in the conversation, into `instructions`.
- Default instructions text differs by wording ("You are a helpful
  assistant." vs "You are ChatGPT, a helpful AI assistant.").
- `text.verbosity`: pi always sends it; rig always clears it to absent for
  Codex.
- `tool_choice` default: pi always forces `"auto"` if the caller gave none;
  rig never forces one for Codex, matching plain Responses' caller-driven
  behavior.
- `parallel_tool_calls`: pi always forces `true`; rig always clears it to
  absent for Codex.
- `temperature` and `service_tier`: pi passes these through if the caller
  supplied them; rig always strips both for Codex, unconditionally.
- `session-id`/`x-client-request-id` header semantics: pi's session id is
  one stable value reused for the cache key, the header, and the
  websocket connection-pool key; rig's `session_id` header is documented
  as a disposable per-request correlator, unrelated to caching, and rig
  sends no `x-client-request-id` at all.
- `OpenAI-Beta` header: pi sends it (two different values for SSE vs
  websocket transport); rig sends none.
- `response.done` as an SSE event: pi treats it as a possible terminal SSE
  event for codex; rig's SSE decoder does not recognize the name at all
  (only its separate, generic websocket decoder does). If the Codex
  backend's SSE endpoint genuinely emits `response.done`, rig's SSE path
  as read would miss it.
- Retryable HTTP statuses: pi's codex retry policy is 429/500/502/503/504
  plus a text-regex fallback; rig's generic policy is 408/425/429 plus any
  5xx. Neither list is a subset of the other (pi retries 501, rig doesn't;
  rig retries 408/425, pi doesn't).
- Envelope-repair tolerance for replayed frames missing bookkeeping: rig
  names this as an explicit decoder mode gated to `ResponsesContract::
  Codex`; pi has no named equivalent, relying on the shared handler's
  implicit "skip if slot missing" behavior.

## Sources

- pi: `@earendil-works/pi-ai` 0.87.1, installed at
  `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/`.
  Files read in full: `api/openai-codex-responses.js` (1,302 lines),
  `api/openai-responses.js` (293 lines), `api/openai-responses-shared.js`
  (689 lines), `utils/provider-retry.js`, `utils/overflow.js`,
  `providers/openai-codex.js`, `auth/oauth/openai-codex.js`.
- rig: `~/work/rig`, commit `42f4e06`, `main`, dated 2026-09-26. Files
  read: `crates/rig-core/src/providers/chatgpt/mod.rs`,
  `crates/rig-core/src/providers/chatgpt/auth/mod.rs`,
  `crates/rig-core/src/providers/chatgpt/auth/native.rs`,
  `crates/rig-core/src/providers/openai/wire.rs` (identity/header sections),
  `crates/rig-core/src/providers/openai/responses_api/wire.rs` (all three
  `ResponsesContract::Codex` branches, lines 56, 165, 248),
  `crates/rig-core/src/providers/openai/responses_api/mod.rs`
  (`AdditionalParameters`, `SystemInstructionsPlacement`),
  `crates/rig-core/src/providers/openai/responses_api/streaming.rs`
  (event vocabulary, `with_envelope_repair`),
  `crates/rig-core/src/providers/openai/responses_api/websocket.rs`
  (generic Responses-over-websocket transport).
- Already mined, read first:
  `/Users/aakshintala/work/fiber/.claude/worktrees/fiber-harvest/research/rig/README.md`,
  `/Users/aakshintala/work/fiber/.claude/worktrees/fiber-harvest/research/provider-errors/README.md`.
- Fiber context read for framing, not modified:
  `docs/model-routing.md`, `docs/adr/0007-protocols-are-native-providers-are-extensions.md`,
  `docs/errors.md`, `docs/prompt-cache.md`.
