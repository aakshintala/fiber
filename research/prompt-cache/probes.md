# Prompt cache probes

Live and local probes behind `docs/prompt-cache.md`, run on September 24, 2026.
The scripts are in this directory and read their keys from the environment variables `MUSE_API_KEY` and
`OPENROUTER_API_KEY`.

## Serialization (`probe_serde.rs`)

Local, macOS arm64, serde_json 1.

- A `HashMap<String, u32>` of eight tool names serialised in three different
  orders across three runs of the same binary.
- The same JSON schema parsed with its keys in two orders serialised to
  identical bytes with serde_json's default map.
- With the `preserve_order` feature on, the two serialised differently.

## Live cache (`probe_openrouter.py`, `probe_muse.py`)

Each case sends a request twice (a control, which must hit), then the same
request with one change. Figures are tokens read from the cache out of the
prompt's total.

- Anthropic: `anthropic/claude-sonnet-5` through OpenRouter, pinned to the
  Anthropic provider, with markers on the system prompt and the last message.
- OpenAI: `openai/gpt-6-luna` through OpenRouter, with a `prompt_cache_key`.

| Change | Sonnet 5 | GPT-6 Luna |
|---|---|---|
| None | 8141 of 8143 | 5257 of 5260 |
| Two messages appended | 8145 of 8156 | 5257 of 5273 |
| Tools reversed | 0 | 0 |
| One schema's keys reordered | 8141 of 8143 | 0, in both reruns |
| One tool added at the end | 0 | 0 |
| Start of system prompt edited | 0 | 0 |
| End of system prompt edited | 0 | 0 |
| `tool_choice` required | 8057 of 8267 | 5134 of 5260 |
| `tool_choice` named | 8057 of 8272 | not run |
| Reasoning turned on or effort set | 0 | 0 |
| Different `prompt_cache_key` | not applicable | 0 |
| 15 parallel calls and results appended, end marker only | 8143 of 9324 | 5257 of 5687 |
| 12 exchanges appended, end marker only | 8055 of 8275 (system prompt only) | not run |
| Model switched to `anthropic/claude-haiku-4.5` | 0 | not run |

Sonnet 5 hit with reordered schema keys. Whether Anthropic or OpenRouter
normalised the keys is not known.

### Cache lifetime (`probe_ttl.py`)

A prefix written with 5-minute markers, then the same prefix plus two messages
with 1-hour markers:

- a 1-hour marker after a 5-minute marker in one request was refused: "a
  ttl='1h' cache_control block must not come after a ttl='5m' cache_control
  block"
- with only 1-hour markers, the request read 0 and wrote all 8152 tokens again,
  whether the system prompt was marked or not
- the next request with the same 1-hour markers read 8152

### Muse Spark (`probe_muse.py`)

`muse-spark-1.3-contributor` through `https://api.meta.ai/v1/responses`. Muse
caching is unreliable: the control missed in 6 of 11 cases, so only cases with
a hit on the control count.

| Change | Control | Changed |
|---|---|---|
| Two messages appended | 4465 of 4496 | 4465 of 4509 |
| One schema's keys reordered | 4465 of 4498 | 0 |
| One tool added at the end | 4465 of 4498 | 0 |
| Model switched to `muse-spark-1.2-contributor` | 4465 of 4500 | 0 |
| `tool_choice: none` | 4465 of 4500 | refused: "only `\"auto\"` is supported for `tool_choice`" |

## Cache lifetime cost (`ttl.py`)

Replays each session's requests with their real timestamps. A request within
the lifetime of the previous one reads the previous context and writes the
growth. After the lifetime it writes the whole context again. Handoffs are
ignored. Write prices: 1.25 for 5 minutes, 2 for 1 hour. Read: 0.05 for Opus
5.5, 0.10 for Sonnet 5.

| Sessions | Count | Gaps of 5 minutes to 1 hour | 1 hour vs 5 minutes, Opus 5.5 | Sonnet 5 |
|---|---|---|---|---|
| pi | 641 | 1.6% | 0.880 | 0.929 |
| Claude Code | 169 | 6.4% | 0.572 | 0.694 |
| Claude Code subagents | 143 | 0.91% | 1.011 | 1.011 |

By share of gaps between 5 minutes and 1 hour, on Opus 5.5:

| Share | pi sessions | 1 hour vs 5 minutes | Claude Code sessions | 1 hour vs 5 minutes |
|---|---|---|---|---|
| none | 521 | 1.212 | 40 | 1.294 |
| under 1% | 23 | 0.946 | 1 | 0.980 |
| 1% to 2% | 17 | 0.860 | 10 | 0.854 |
| 2% to 5% | 37 | 0.647 | 30 | 0.643 |
| 5% to 10% | 23 | 0.480 | 41 | 0.512 |
| 10% or more | 20 | 0.421 | 47 | 0.429 |

Keeping a 5-minute cache warm with a request every 270 seconds, for up to an
hour of idle, cost 0.796 (pi) and 0.558 (Claude Code) on Opus 5.5. It was not
adopted: a session with no client exits when idle, and an idle Fiber does no
work.

## Deferred tools (September 24, 2026)

Scripts: `probe_defer_responses.py` (OpenRouter, GPT-6 Luna, Responses),
`probe_defer_muse.py` (Muse Spark 1.3, Responses) and
`probe_defer_openrouter_anthropic.py` (OpenRouter, Sonnet 5, Anthropic
messages). Five direct tools plus three calendar tools in a namespace, either
in full or with `defer_loading` and a client-run `tool_search`.

| Route | Full | Deferred | Loaded, cache read | Model behaviour |
|---|---|---|---|---|
| OpenRouter, GPT-6 Luna | 5,655 in | 5,948 in | not reached | called the deferred tool directly, no search |
| Muse Spark 1.3 | 6,425 in | 6,410 in | 6,385 of 6,980 | called `tool_search`, then kept the cache |
| OpenRouter, Sonnet 5 | 4,857 in | ran | not measured | BM25 search rejected: "OpenRouter implements the regex tool-search variant only"; regex search worked |

OpenRouter's Responses route does not defer for GPT-6 Luna. OpenAI direct and
Anthropic direct were not probed; OpenAI's documentation covers the first.
