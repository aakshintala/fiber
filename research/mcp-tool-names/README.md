# Tool-name length limits (#588)

Question: how long may a function-tool name be on each wire protocol, so the cut in `crates/mcp/src/name.rs` fits the shortest.

## Method

`probe.py` sends one request per name length, a tool named `a` repeated N times, with `tool_choice` none and at most 16 output tokens. It tries 64, doubles while accepted, then bisects between the last accepted and first refused length. Run on 2026-10-05 from macOS. Raw requests' status and the first 500 bytes of each reply are in `raw/`. Keys are read in the script and never printed or saved.

## Results

| Protocol | Reached through | Longest accepted | Evidence |
|---|---|---:|---|
| Codex Responses | `chatgpt.com/backend-api/codex/responses`, `gpt-6-luna`, Codex login | 128 | 129 refused: "Invalid 'tools[0].name': string too long. Expected a string with maximum length 128" |
| OpenAI Responses | opencode Zen `/zen/v1/responses`, `gpt-6-luna` | 128 | same message at 129 |
| OpenAI Completions | opencode Go `/zen/go/v1/chat/completions`, `glm-5.3-flash` | at least 1024 | all of 64 to 1024 accepted; this is a third-party model behind a relay, not a vendor limit |
| Anthropic Messages | not measured | none | every request 401 `authentication_error`: the key in `/tmp/anthropic-key-2nd-ws` is invalid |
| Google Generative AI | not measured | none | no key available |

The OpenAI Responses row went through opencode's relay, not `api.openai.com`; no OpenAI key was available.

## What Fiber uses

`MAX_NAME_LEN` stays 64 and `HASH_LEN` stays 8. The measured limits (128 and above) are all longer than 64, so 64 is safe on them. Anthropic Messages and Google Generative AI are not measured, and 64 is the conservative floor until they are. A name longer than 64 is cut to 64 characters ending in `_` and 8 hex characters of SHA-256.
