# Hosted-search probe (#1648)

Probed 2026-10-16 for issue #1648: which listed model ids accept the vendor-hosted search tool. No keys, account data or response bodies are recorded here.

## Method

One request per listed model id: 43 ids on `openai-responses` with one `web_search` tool, and 8 ids on `google-generative-ai` with one `google_search` tool. Each request also carried one function tool, so accepting the search tool is distinct from accepting tools at all. Gemini requests set `includeServerSideToolInvocations`. A request that failed with 429, a 5xx status or a timeout was retried, up to 3 attempts per id; no id needed a retry (every row below shows 1 attempt in the source data). `accepted` means the request returned 200 with the search tool in play; `refused` means the vendor rejected the search tool for that id. No id was inconclusive.

Result: 30 of the 43 `openai-responses` ids accepted `web_search`, and 4 of the 8 `google-generative-ai` ids accepted `google_search`. A refused id stays unmarked in the provider table.

## Results

| model | family | result | HTTP status | error code |
| --- | --- | --- | --- | --- |
| `gpt-4` | openai-responses | refused | 400 | — |
| `gpt-4-turbo` | openai-responses | refused | 400 | — |
| `gpt-4.1` | openai-responses | accepted | 200 | — |
| `gpt-4.1-mini` | openai-responses | accepted | 200 | — |
| `gpt-4.1-nano` | openai-responses | refused | 400 | — |
| `gpt-4o` | openai-responses | accepted | 200 | — |
| `gpt-4o-2024-05-13` | openai-responses | accepted | 200 | — |
| `gpt-4o-2024-08-06` | openai-responses | accepted | 200 | — |
| `gpt-4o-2024-11-20` | openai-responses | accepted | 200 | — |
| `gpt-4o-mini` | openai-responses | accepted | 200 | — |
| `gpt-5` | openai-responses | accepted | 200 | — |
| `gpt-5-mini` | openai-responses | accepted | 200 | — |
| `gpt-5-nano` | openai-responses | accepted | 200 | — |
| `gpt-5-pro` | openai-responses | accepted | 200 | — |
| `gpt-5.1` | openai-responses | accepted | 200 | — |
| `gpt-5.2` | openai-responses | accepted | 200 | — |
| `gpt-5.2-chat-latest` | openai-responses | refused | 404 | model_not_found |
| `gpt-5.2-pro` | openai-responses | accepted | 200 | — |
| `gpt-5.3-chat-latest` | openai-responses | refused | 404 | model_not_found |
| `gpt-5.3-codex` | openai-responses | accepted | 200 | — |
| `gpt-5.3-codex-spark` | openai-responses | refused | 404 | model_not_found |
| `gpt-5.4` | openai-responses | accepted | 200 | — |
| `gpt-5.4-mini` | openai-responses | accepted | 200 | — |
| `gpt-5.4-nano` | openai-responses | accepted | 200 | — |
| `gpt-5.4-pro` | openai-responses | accepted | 200 | — |
| `gpt-5.5` | openai-responses | accepted | 200 | — |
| `gpt-5.5-pro` | openai-responses | accepted | 200 | — |
| `gpt-5.6-luna` | openai-responses | accepted | 200 | — |
| `gpt-5.6-sol` | openai-responses | accepted | 200 | — |
| `gpt-5.6-terra` | openai-responses | accepted | 200 | — |
| `gpt-6-astra` | openai-responses | accepted | 200 | — |
| `gpt-6-luna` | openai-responses | accepted | 200 | — |
| `gpt-6-sol` | openai-responses | accepted | 200 | — |
| `gpt-6.1-sol` | openai-responses | accepted | 200 | — |
| `gpt-daybreak-blue-latest` | openai-responses | refused | 403 | access_program_not_enabled |
| `gpt-daybreak-red-latest` | openai-responses | refused | 404 | model_not_found |
| `gpt-realtime-2.1` | openai-responses | refused | 404 | model_not_found |
| `o1` | openai-responses | refused | 400 | — |
| `o1-pro` | openai-responses | refused | 400 | — |
| `o3` | openai-responses | accepted | 200 | — |
| `o3-mini` | openai-responses | refused | 400 | — |
| `o3-pro` | openai-responses | refused | 404 | model_not_found |
| `o4-mini` | openai-responses | accepted | 200 | — |
| `deep-research-max-preview-04-2026` | google-generative-ai | refused | 400 | 400 |
| `deep-research-preview-04-2026` | google-generative-ai | refused | 400 | 400 |
| `gemini-3.1-flash-lite-image` | google-generative-ai | refused | 400 | 400 |
| `gemini-3.1-flash-live-preview` | google-generative-ai | refused | 400 | 400 |
| `gemini-flash-latest` | google-generative-ai | accepted | 200 | — |
| `gemini-flash-lite-latest` | google-generative-ai | accepted | 200 | — |
| `gemma-4-26b-a4b-it` | google-generative-ai | accepted | 200 | — |
| `gemma-4-31b-it` | google-generative-ai | accepted | 200 | — |

The 10 existing Gemini `google_search` marks from #654 keep their rows and were not re-probed, so they have no per-model record here.

Codex (`providers/codex`) is out of scope for this probe.
