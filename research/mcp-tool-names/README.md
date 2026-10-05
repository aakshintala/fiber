# Tool-name length limits (#588)

Question: how long may a function-tool name be on each wire protocol, so the cut in `crates/mcp/src/name.rs` fits the shortest.

## Method

`probe.py` sends one request per name length, a tool named `a` repeated N times, with `tool_choice` none and at most 16 output tokens. It tries 64, doubles while accepted, then bisects between the last accepted and first refused length. Run on 2026-10-05 from macOS. Raw requests' status and the first 500 bytes of each reply are in `raw/`. Keys are read in the script and never printed or saved.

## Results

| Protocol | Reached through | Longest accepted | Evidence |
|---|---|---:|---|
| Codex Responses | `chatgpt.com/backend-api/codex/responses`, `gpt-6-luna`, Codex login | 128 | 129 refused: "Invalid 'tools[0].name': string too long. Expected a string with maximum length 128" |
| OpenAI Responses | opencode Zen `/zen/v1/responses`, `gpt-6-luna` | 128 | same message at 129 |
| OpenAI Responses | `api.openai.com/v1/responses`, `gpt-5.4-nano` | 128 | 129 refused: "Invalid 'tools[0].name': string too long. Expected a string with maximum length 128" (2026-10-05, #707) |
| OpenAI Completions | opencode Go `/zen/go/v1/chat/completions`, `glm-5.3-flash` | at least 1024 | all of 64 to 1024 accepted; this is a third-party model behind a relay, not a vendor limit |
| Anthropic Messages | `api.anthropic.com/v1/messages`, `claude-sonnet-5-5` | 128 | 129 refused: "tools.0.custom.name: String should have at most 128 characters" (2026-10-05, #707) |
| Google Generative AI | `generativelanguage.googleapis.com/v1beta` `generateContent`, `gemini-3.5-flash-lite` | 128 | 129 refused: "Invalid function name. ... with a maximum length of 128" (2026-10-05, #707) |

The first OpenAI Responses row went through opencode's relay; the second is direct to `api.openai.com`. Both agree.

## What Fiber uses

`MAX_NAME_LEN` is 128, the shortest limit measured on every first-party protocol (Anthropic Messages, Codex Responses, OpenAI Responses, Google Generative AI). `HASH_LEN` stays 8. A name longer than 128 is cut to 128 characters ending in `_` and 8 hex characters of SHA-256.
