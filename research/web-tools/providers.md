# Web fetch and web search: provider survey

Research for issue #57. All facts below are from primary vendor docs (WebFetch'd
2026-09-27) or from reading the vendor's own open-source code on GitHub. No live
API probes were run. "Not found" means the primary source was checked and did
not say.

## 1. Anthropic Messages: web_search and web_fetch server tools

Source: https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool
and https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool
(docs.claude.com redirects here), and https://platform.claude.com/docs/en/build-with-claude/prompt-caching

**web_search tool versions:**
- `web_search_20250305`: basic web search.
- `web_search_20260209`: adds dynamic filtering (Claude writes/runs code to filter results before they hit context; runs inside code execution, `allowed_callers` defaults to `["code_execution_20260120"]`).
- `web_search_20260318`: adds `response_inclusion` control (`"excluded"` drops nested server_tool_use/result pairs consumed by a completed code-execution call, `"full"` is default).

**web_fetch tool versions:**
- `web_fetch_20250910`: basic fetch.
- `web_fetch_20260209`: adds dynamic filtering.
- `web_fetch_20260309`: adds dynamic filtering + `use_cache` (cache bypass, default `true`).
- `web_fetch_20260318`: latest; adds `response_inclusion` on top of the above.

**Parameters (web_search):** `max_uses` (int, caps searches per request; over-limit -> `web_search_tool_result_error` with `max_uses_exceeded`), `allowed_domains` XOR `blocked_domains` (bare domains, optional path, no scheme; both together -> 400), `user_location` (`type: "approximate"`, plus any of `city`/`region`/`country` [ISO 3166-1 alpha-2] /`timezone` [IANA]), `allowed_callers` (`["direct"]` or `["code_execution_20260120"]`), `response_inclusion` (20260318+). No `max_content_tokens` on web_search (that's web_fetch only).

**Parameters (web_fetch):** `max_uses` (no default limit; failed fetches count), `allowed_domains` XOR `blocked_domains`, `citations: {enabled: true|false}` (default false, unlike web_search where citations are always on), `max_content_tokens` (approximate cap on fetched text, doesn't apply to PDFs/binary), `use_cache` (20260309+, default true), `response_inclusion` (20260318+).

**Pricing:**
- web_search: **$10 per 1,000 searches**, plus standard token costs for the content it returns. Each search = one use regardless of result count. Failed/errored searches are not billed.
- web_fetch: **no additional charge** beyond standard token costs for the fetched content. Rough token costs given in the doc: ~2,500 tokens per 10 KB page, ~25,000 tokens per 100 KB doc page, ~125,000 tokens per 500 KB PDF.

**Response block shapes:**
- `server_tool_use` block: `{type, id: "srvtoolu_...", name: "web_search"|"web_fetch", input}`.
- `web_search_tool_result`: `{type, tool_use_id, content: [{type: "web_search_result", url, title, encrypted_content, page_age}] }` (or, on error, `content` is a single `{type: "web_search_tool_result_error", error_code}` object, not a list). Error codes: `too_many_requests`, `invalid_tool_input`, `max_uses_exceeded`, `query_too_long`, `request_too_large`, `unavailable`.
- `web_fetch_tool_result`: `{type, tool_use_id, content: {type: "web_fetch_result", url, content: {type: "document", source: {...}, title, citations}, retrieved_at}}`. PDFs come back as `source: {type: "base64", media_type: "application/pdf", data}`. Error codes add `url_too_long` (>250 chars), `url_not_allowed`, `url_not_in_prior_context`, `url_not_accessible`, `unsupported_content_type`.
- Citations: web_search citations are `web_search_result_location` (`url`, `title`, `encrypted_index`, `cited_text` up to 150 chars — these three fields don't count toward token usage). web_fetch citations (when enabled) are `char_location` (`document_index`, `document_title`, `start_char_index`, `end_char_index`, `cited_text`).

**encrypted_content must be sent back verbatim:** Yes. Quote: "To continue a conversation that contains search results, send the assistant's content blocks back exactly as you received them, including each result's `encrypted_content`. The API decrypts that content on later turns to restore the search results in Claude's context. If `encrypted_content` is missing or modified, the request fails with a 400 validation error." Citations carry an analogous `encrypted_index` that must also round-trip.

**pause_turn:** "The API can pause a long-running search turn and return `stop_reason: 'pause_turn'`. To continue, send the paused assistant message back unchanged in a new request." If Claude calls a server tool and a client tool in the same parallel batch, the API instead returns `stop_reason: "tool_use"` and defers the server tool call until you return the client tool_result.

**Prompt caching / system prompt:** The prompt-caching doc's cache-invalidation table states verbatim: **"Web search toggle | Enabling/disabling web search modifies the system prompt"** — turning web search on or off invalidates the system-prompt and message caches (tools cache stays valid). Separately, server tool results (e.g. web_search results) get their own automatic `ephemeral_5m` cache write behavior, documented under Tool use with prompt caching.

Web search is unavailable on Amazon Bedrock; web fetch is unavailable on Bedrock and Google Cloud. Both are on the Claude API, Claude Platform on AWS, and Microsoft Foundry (Azure-hosted Foundry deployments get only the basic, non-dynamic-filtering versions).

## 2. OpenAI Responses hosted web_search / web_search_preview, and codex

Sources: https://developers.openai.com/api/docs/guides/tools-web-search (platform.openai.com/docs/guides/tools-web-search redirects here), https://developers.openai.com/api/docs/pricing, and reading github.com/openai/codex (codex-rs) directly.

**Parameters:** `search_context_size` (`low`/`medium`/`high`), `return_token_budget` (`default`/`unlimited`), `filters` (up to 100 `allowed_domains` or `blocked_domains`), `search_content_types` (`image` and/or `text`), `image_settings` (`max_results`, `caption`), `user_location` (`country`, `city`, `region`, `timezone`), `external_web_access` (bool — live internet on/off), `tool_choice` (`auto`/`required`/explicit).

**Models:** Responses API — `gpt-6-astra`, `gpt-5.5`, `gpt-4.1`/`gpt-4.1-mini` (128k search-context cap). Chat Completions (legacy) — `gpt-5-search-api`, `gpt-4o-search-preview` (deprecated, shuts down 2026-07-23).

**Output shape:** two output items — `web_search_call` (has `action` of type `search`, `open_page`, or `find_in_page`) and a `message` item whose text carries `url_citation` annotations (url, title, source span).

**Pricing** (from the OpenAI pricing page): web search (all models, and image web search) — **$10.00 / 1k calls** + search-content tokens billed at model rates. `web_search_preview` on reasoning models (gpt-5/o-series) — same **$10.00/1k calls**. `web_search_preview` on non-reasoning models — **$25.00/1k calls**, but search content tokens are free.

**Does the ChatGPT/codex backend accept it?** Reading codex-rs directly shows codex has *two different* code paths, not a single passthrough of the public `web_search` tool:
1. A **hosted `web_search` ToolSpec** built by `create_web_search_tool()` (`codex-rs/core/src/tools/hosted_spec.rs`, `spec_plan.rs`) with a custom shape:
   ```rust
   #[serde(rename = "web_search")]
   WebSearch { external_web_access: Option<bool>, indexed_web_access: Option<bool> }
   ```
   (`codex-rs/tools/src/tool_spec.rs`, lines 33-44). `external_web_access`/`indexed_web_access` are codex's own fields, driven by a `WebSearchMode` enum (`Cached` -> `(false, None)`, `Indexed` -> `(true, Some(true))`, `Live` -> `(true, None)`, `Disabled` -> tool omitted). The source comment literally flags the mismatch with OpenAI's public docs: `// TODO: Understand why we get an error on web_search although the API docs say it's supported. https://platform.openai.com/docs/guides/tools-web-search?api-mode=responses`. This path is what non-OpenAI-authenticated/ChatGPT-style providers get.
2. A **"standalone web search" extension** (`codex-rs/ext/web-search/`) that calls the real public OpenAI `web_search` Responses tool shape (via `codex_api::ExternalWebAccess`, `SearchContextSize`, `SearchFilters`, etc.). It's only wired up `available: (config.model_provider.is_openai() || config.model_provider.uses_openai_actor_authorization() || config.model_provider.supports_standalone_web_search) && web_search_mode != Disabled` — i.e., for providers flagged as OpenAI or OpenAI-actor-authorized. `supports_standalone_web_search` defaults to `false` for at least one auth-env provider profile (`codex-rs/login/src/auth_env_telemetry.rs`) and is explicitly `true` for a local test provider profile (`codex-rs/config/src/thread_config.rs`).

Net: codex does not treat "web_search" as a single pass-through tool string; it swaps schema/behavior depending on which backend/auth path the session uses, and its own TODO comment says the mapping onto OpenAI's documented tool has open questions. I did not find a definitive statement of which mode `chatgpt.com/backend-api/codex/responses` specifically gets — that would need a live probe or reading the ChatGPT-specific provider profile, which wasn't in the files grepped.

## 3. OpenRouter web search

Source: https://openrouter.ai/docs/features/web-search

- `:online` suffix (e.g. `openai/gpt-5.2:online`) is shorthand for `plugins: [{id: "web"}]`.
- Two engines: **native** (default for Anthropic, Google, OpenAI, Perplexity, SpaceXAI — uses the provider's own built-in search, and pricing passes through directly from that provider) and **Exa** (fallback for everything else; modes `instant`, `fast`, `auto` [default], `deep-lite`, `deep`, `deep-reasoning`; priced **$0.007/request** for most modes, **$0.012–0.015/request** for deep modes). Engine can be forced with `"engine": "native"|"exa"`.
- OpenRouter also documents a **server tool** form (`openrouter:web_search`) that lets the model itself decide when/how often to search, distinct from the always-on plugin — described as giving better results because the model controls search timing.
- Native engine = passthrough of the underlying provider's server tool (so for an Anthropic model this is effectively Anthropic's `web_search` tool via OpenRouter); Exa engine is OpenRouter's own layer, not a provider server tool.

## 4. Databricks Model Serving / Foundation Model APIs

Source: https://learn.microsoft.com/en-us/azure/databricks/machine-learning/foundation-model-apis/api-reference

Direct quote from the Responses API `tools` field row: **"An array of tools the model may call while generating a response. Note: Code interpreter and web search tools are not supported by Databricks."** The Responses API's supported tool types are limited to `function`, `custom`, `mcp`, `image_generation`, `shell` (custom tools/grammar only on GPT-5 series). No Anthropic server-tool passthrough (web_search/web_fetch) or OpenAI hosted web_search is documented anywhere in the API reference — Databricks re-implements its own Responses/Chat Completions surface in front of external models and simply omits these hosted tools. Prompt caching and cache_control are supported for Databricks-hosted Claude endpoints, but that's unrelated to web tools.

## 5. OpenCode Zen / OpenCode Go / opencode CLI

Sources: https://opencode.ai/docs/zen/, https://opencode.ai/docs/go/ (no mention of web search either way — not found), and reading github.com/anomalyco/opencode (formerly sst/opencode; `sst/opencode` now redirects there) directly.

Zen and Go docs say nothing about web search passthrough — not found in either primary doc.

The opencode CLI itself ships **two separate things** under "web search," confirmed by `packages/llm/AGENTS.md` (quoted verbatim):

> "Provider-defined / hosted tools (Anthropic `web_search` / `code_execution` / `web_fetch`, OpenAI Responses `web_search_call` / `file_search_call` / `code_interpreter_call` / `mcp_call` / `local_shell_call` / `image_generation_call` / `computer_use_call`) pass through the runtime untouched: Routes surface the model's call as a `tool-call` event with `providerExecuted: true`, and the provider's result as a matching `tool-result` event with `providerExecuted: true`. Callers detect `providerExecuted` on `tool-call` and skip local dispatch... Anthropic encodes them back as `server_tool_use` + `web_search_tool_result` ... blocks; OpenAI Responses callers typically use `previous_response_id` instead of resending hosted-tool items."

So opencode passes Anthropic's and OpenAI's hosted server tools straight through when the underlying model/provider supports them (`packages/llm/src/protocols/anthropic-messages.ts`, `openai-responses.ts`).

Separately, opencode ships its own **provider-independent `websearch` tool** (`packages/core/src/tool/websearch.ts`, name `"websearch"`) that is NOT a passthrough — it calls out to Exa or Parallel.ai as MCP backends (`EXA_URL = "https://mcp.exa.ai/mcp"`, `PARALLEL_URL = "https://search.parallel.ai/mcp"`, invoking method `web_search_exa` via MCP over HTTP). A source comment states: "Provider-independent local web search retained in V2 core for launch parity... distinct from provider-hosted web search tools, which remain route-owned and execute at the model provider." Provider is chosen by `selectProvider()`: explicit override > `enableParallel` flag > `enableExa` flag > a deterministic 50/50 hash-of-session-id split between exa/parallel. Exa is gated behind `OPENCODE_EXPERIMENTAL`/`OPENCODE_ENABLE_EXA` env flags; a `websearch.txt` prompt/description file backs the tool description. There's also a separate `webfetch` tool (`packages/core/src/tool/webfetch.ts`, `packages/opencode/src/tool/webfetch.ts`) that is a plain local URL-fetch tool, not a provider passthrough.

## 6. muse / Meta Model API

Primary source found (secondary sources led me to it): https://dev.meta.ai/docs/overview and https://dev.meta.ai/docs/search-grounding

Meta Model API exposes three request formats over one backend: `/v1/responses` (OpenAI Responses-shaped, agentic/multi-step with server-managed state), Chat Completions (OpenAI-compatible), and an Anthropic Messages-compatible endpoint. Model is `muse-spark-1.1`, base URL `https://api.meta.ai/v1`.

Per `dev.meta.ai/docs/search-grounding`: **the `web_search` tool is only available through the Responses API — it cannot be used with Chat Completions or the Messages (Anthropic-compatible) endpoint.** Parameters: `type: "web_search"`, `search_context_size` (`low`/`medium`[default]/`high`), `user_location` (`country`/`region`/`city`/`timezone`), `include: ["web_search_call.results"]` to get raw results. Response has a `web_search_call` item, a `message` item with `output_text` and `url_citation` annotations, and an optional `results` array. Pricing/rate limits: the search-grounding page pointed to a separate pricing page that wasn't fetched directly, but a secondary source (layer3labs.io, cross-checked against the vendor blog) states web search on Meta Model API bills at **$2.50 per 1,000 queries** — flag this figure as secondary-sourced, not confirmed on a primary pricing page.

## 7. Google Gemini google_search grounding

Source: https://ai.google.dev/gemini-api/docs/google-search

Request: `tools: [{"type": "google_search"}]` (generative-ai API shape; note the fetched page used Responses-style terminology in places, so treat the exact request field names as approximate pending a direct SDK check). Response carries a `google_search_call` step (search `queries` executed), a `google_search_result` step (`search_suggestions` HTML snippet for the required rendered search suggestions), and the model's text output with inline `url_citation` annotations (`url`, `title`, `start_index`, `end_index`). Pricing: for Gemini 3 models, billed per search query the model actually executes (each query in a multi-query call counts separately; empty queries aren't billed). Gemini 2.5 and older bill per prompt rather than per query. Supported on current Gemini 3.x/2.5 models.

## 8. Client-side search/fetch APIs a harness could call itself

| Service | Endpoint | Auth | Free tier | Price | Response shape (brief) |
|---|---|---|---|---|---|
| Brave Search | `GET https://api.search.brave.com/res/v1/web/search` | header `X-Subscription-Token` | $5 free credit/month | $5 / 1,000 requests (Search plan) | `{web: {results: [{title, url, description, ...}]}, news, images, ...}` |
| Exa | `POST https://api.exa.ai/search` | header `x-api-key` (or `Authorization: Bearer`) | new accounts get free credits (amount not stated) | ~$0.007/neural search request + optional summary (~$0.005) and contents extraction charges, billed via usage counters | `{requestId, results: [{title, url, publishedDate, author, text, highlights, summary}], costDollars, searchTime}` |
| Tavily | `POST https://api.tavily.com/search` | `Authorization: Bearer tvly-<key>` | not stated on the reference page (not found) | 1 credit/request (basic/fast/ultra-fast), 2 credits/request (advanced) | `{query, answer?, results: [{title, url, content, score, published_date?}], response_time, usage: {credits}}` |
| Kagi | `GET/POST https://kagi.com/api/v1/search` | `Authorization: Bot <token>` | not offered — no free tier for the API (not found) | $12 / 1,000 requests (Search API); Extract API $4/1,000 pages; invoiced every 30 days or at $100 usage | not fully detailed on the docs page fetched — includes a `meta.trace` field; full schema is behind their OpenAPI spec (not found) |
| Jina Reader (`r.jina.ai`) | `GET https://r.jina.ai/<url>` | `Authorization: Bearer <key>` (optional) | 20 req/min without a key, 500 req/min with a free key | token-based, 10M free tokens per new key, pay-as-you-go after | markdown text of the page plus title/url/timestamp metadata |
| Jina Search (`s.jina.ai`) | `GET https://s.jina.ai/<query>` | `Authorization: Bearer <key>` (optional) | 100 req/min with or without key | token-based, fixed 10,000-token minimum per search request, shares the 10M free-token pool | top 5 results as markdown, each with url/title/content/timestamp |

## Sources fetched
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool
- https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- https://platform.claude.com/docs/en/api/messages
- https://developers.openai.com/api/docs/guides/tools-web-search
- https://developers.openai.com/api/docs/pricing
- https://github.com/openai/codex (codex-rs: web_search.rs, hosted_spec.rs, spec_plan.rs, tool_spec.rs, ext/web-search/*, login/auth_env_telemetry.rs, config/thread_config.rs, features/*)
- https://openrouter.ai/docs/features/web-search
- https://learn.microsoft.com/en-us/azure/databricks/machine-learning/foundation-model-apis/api-reference
- https://opencode.ai/docs/zen/, https://opencode.ai/docs/go/
- https://github.com/anomalyco/opencode (packages/llm/AGENTS.md, packages/core/src/tool/websearch.ts, packages/core/src/tool/webfetch.ts)
- https://dev.meta.ai/docs/overview, https://dev.meta.ai/docs/search-grounding
- https://ai.google.dev/gemini-api/docs/google-search
- https://brave.com/search/api/, https://exa.ai/docs/reference/search, https://docs.tavily.com/documentation/api-reference/endpoint/search, https://kagi.com/api/docs, https://kagi.com/api/pricing, https://jina.ai/reader/
