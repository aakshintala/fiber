# OpenCode thinking-level probes

Question: which thinking levels does each remaining OpenCode Go and Zen model
take, for the gateway `thinking` rows (#2335)? Inheritance from the vendor
packages and the vendors' own docs settle part of the two gateway tables;
this records the probes for every model neither settles.

## Method

`probe.py <route> <protocol> <model> ...` sends one tiny request per thinking
level (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`), shaped the
way Fiber sends that level on the protocol, and records every status in
`raw/<route>-<model>.json`. One request per level, no retry:

- `completions` (`/v1/chat/completions`): `reasoning_effort` is the level, or
  `"none"` for `off`; `max_tokens: 64`.
- `responses` (`/v1/responses`): `reasoning.effort` is the level, or `"none"`
  for `off`; `max_output_tokens: 16`.
- `messages` (`/v1/messages`): adaptive `thinking` plus
  `output_config.effort` for every level but `off`, which sends no `thinking`
  key; `max_tokens: 64`.
- `google` (`/v1/models/<model>:generateContent`): `thinkingConfig` carries
  `thinkingLevel` (upper-cased) or `thinkingBudget: 0` for `off`;
  `maxOutputTokens: 16`.

Headers on every request: `Authorization: Bearer` with the key read from
`~/.config/probe-keys/opencode-key`, `x-opencode-session`, and
`User-Agent: fiber-probe/0.1` (Cloudflare 403s Python's default); the
Anthropic and Gemini routes add `x-api-key` and `x-goog-api-key`. The key is
never printed, never on a command line, and every record is checked for it
before writing. A dropped connection is recorded as a transport error, not
retried; `zen/step-5-preview-free` needed its run repeated for one.

"Accepts" below means the route answered 200 to that level's wire form. The
route validates per model: rejections name the supported values (for example
`"reasoning.effort" does not support "none" with this model`, or upstream
`reasoning_effort must be one of 'none', 'low', 'medium' or 'high'`), and
accepted levels differ between neighboring models, so a bare 200 is not the
gateway ignoring the field. Where the reply carries usage, reasoning-token
counts move with the level (see for example `raw/go-glm-5.3.json`: 13
reasoning tokens at `medium`, 42 at `xhigh`).

## Account limits met while probing

- Go regions: `deepseek-v4-flash`, `deepseek-v4-pro` and `deepseek-v4.1-flash`
  answer 400 `This Go model requires Global regions` on Go, at every level.
  Their Zen rows are probed on Zen instead; the Go rows cite the same-model
  Zen record.
- Zen access: the Zen account has DeepSeek, GLM-5 through GLM-5.3, Kimi K2.5
  and K2.6, MiniMax, Qwen 3.5/3.6 Plus, the GPT codex variants, `grok-build`,
  `muse-spark-1.2`, `gemini-3-flash` and `gemini-3.1-pro` disabled (`Model
  access is disabled`). Rows for these cite the same- or nearest-model probe
  that did run, beside the blocked record.
- Free tier: `big-pickle`, `longcat-2.5-preview-free`, `mimo-v2.6-flash-free`,
  the two `nemotron` frees, `space-bunny-free`, `step-5-preview-free` and
  `muse-spark-1.3-contributor-free` answer 403 `FreeTierError` outside
  OpenCode. Rows for the reachable ones cite the same-model Go probe; the
  unreachable `big-pickle` and both `nemotron` frees take empty rows.
- `exo-free` answers 410 `ModelDeprecated`; it takes an empty row.
- `ling-3.0-flash-fin-free` and `ling-3.1-flash-free` answer 400 `Model is
  unavailable`; they take empty rows.

## Findings per model

Go (`raw/go-<model>.json`); `*` marks a row citing another record:

| Model | Accepts | Record |
| --- | --- | --- |
| `deepseek-v4-flash` | all seven `*` | alias of V4.1-Flash (DeepSeek docs) + `raw/zen-deepseek-v4.1-flash.json` |
| `deepseek-v4-flash-vision-exp` | all seven | `raw/go-deepseek-v4-flash-vision-exp.json` |
| `deepseek-v4-pro` | none shown | `raw/go-deepseek-v4-pro.json`, `raw/zen-deepseek-v4-pro.json` (both blocked) |
| `deepseek-v4.1-flash` | all seven `*` | `raw/zen-deepseek-v4.1-flash.json` |
| `glm-5.2` | `minimal`–`max` | `raw/go-glm-5.2.json` (`off` 400: thinking-only model) |
| `glm-5.3` | `minimal`–`max` | `raw/go-glm-5.3.json` (reasoning tokens move with the level) |
| `glm-5.3-flash` | all seven | `raw/go-glm-5.3-flash.json` |
| `hy3` | all seven | `raw/go-hy3.json` (completion tokens move with the level) |
| `hy4-preview` | all seven | `raw/go-hy4-preview.json` |
| `kimi-k2.7-code` | `minimal`–`max` | `raw/go-kimi-k2.7-code.json` |
| `longcat-2.0` | all seven | `raw/go-longcat-2.0.json` |
| `longcat-2.5-preview-free` | all seven | `raw/go-longcat-2.5-preview-free.json` |
| `mimo-v2.5` | all seven | `raw/go-mimo-v2.5.json` |
| `mimo-v2.5-pro` | `off`, `low`, `medium`, `high`, `max` | `raw/go-mimo-v2.5-pro.json` (upstream enumerates its set) |
| `mimo-v2.6-flash` | `off`, `low`, `medium`, `high`, `max` | `raw/go-mimo-v2.6-flash.json` |
| `mimo-v2.6-pro` | `off`, `low`, `medium`, `high`, `max` | `raw/go-mimo-v2.6-pro.json` |
| `minimax-m2.7` | all seven, on `messages` | `raw/go-minimax-m2.7.json`; `raw/go-minimax-m2.7-completions.json` shows `completions` is `ModelProtocolUnsupported` there, so the table drops the stale override |
| `minimax-m3` | all seven | `raw/go-minimax-m3.json` (thinking blocks in every reply) |
| `muse-spark-1.2-contributor` | `minimal`–`xhigh` | `raw/go-muse-spark-1.2-contributor.json` (the route enumerates the set) |
| `qwen3.7-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.7-plus.json` |
| `qwen3.8-flash` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.8-flash.json` (thinking blocks in every reply) |
| `qwen3.8-max` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.8-max.json` |
| `space-bunny` | `minimal`–`max` | `raw/go-space-bunny.json` |
| `step-5-preview-free` | `minimal`–`max` | `raw/go-step-5-preview-free.json` |

Zen (`raw/zen-<model>.json`):

| Model | Accepts | Record |
| --- | --- | --- |
| `big-pickle` | none shown | `raw/zen-big-pickle.json` (free tier) |
| `deepseek-v4-flash` | all seven `*` | alias of V4.1-Flash + `raw/zen-deepseek-v4.1-flash.json` |
| `deepseek-v4-flash-vision-exp` | all seven `*` | `raw/go-deepseek-v4-flash-vision-exp.json` |
| `deepseek-v4-pro` | none shown | `raw/zen-deepseek-v4-pro.json` (blocked) |
| `deepseek-v4.1-flash` | all seven | `raw/zen-deepseek-v4.1-flash.json` |
| `exo-free` | none shown | `raw/zen-exo-free.json` (deprecated) |
| `gemini-3-flash` | `minimal`–`high` `*` | Google thinking docs (preview values) + `raw/zen-gemini-3-flash.json` (blocked) |
| `gemini-3.1-pro` | `low`–`high` `*` | Google thinking docs (preview values) + `raw/zen-gemini-3.1-pro.json` (blocked) |
| `glm-5`, `glm-5.1` | `minimal`–`max` `*` | `raw/go-glm-5.2.json`, `raw/go-glm-5.3.json` + Zhipu thinking-mode docs |
| `glm-5.2`, `glm-5.3` | `minimal`–`max` `*` | `raw/go-glm-5.2.json`, `raw/go-glm-5.3.json` |
| `glm-5.3-flash` | `minimal`–`max` | `raw/zen-glm-5.3-flash.json` |
| `gpt-5-codex`, `gpt-5.1-codex`, `gpt-5.1-codex-max`, `gpt-5.1-codex-mini`, `gpt-5.2-codex` | none shown | `raw/zen-<model>.json` (access disabled) |
| `grok-build-0.1` | none shown | `raw/zen-grok-build-0.1.json` (access disabled; not in xAI docs) |
| `kimi-k2.5`, `kimi-k2.6` | `minimal`–`max` `*` | `raw/go-kimi-k2.7-code.json` + Moonshot thinking-models docs |
| `kimi-k2.7-code` | `minimal`–`max` `*` | `raw/go-kimi-k2.7-code.json` |
| `ling-3.0-flash-fin-free`, `ling-3.1-flash-free` | none shown | `raw/zen-<model>.json` (unavailable) |
| `longcat-2.5-preview-free` | all seven `*` | `raw/go-longcat-2.5-preview-free.json` |
| `mimo-v2.6-flash-free` | `off`, `low`, `medium`, `high`, `max` `*` | `raw/go-mimo-v2.6-flash.json` |
| `minimax-m2.5`, `minimax-m2.7`, `minimax-m3` | none shown | `raw/zen-<model>.json` (access disabled; no completions evidence) |
| `mistral-large-4` | `off`, `high` | `raw/zen-mistral-large-4.json` (the `high` reply carries a thinking chunk) + Mistral reasoning docs |
| `muse-spark-1.2` | `minimal`–`xhigh` `*` | `raw/go-muse-spark-1.2-contributor.json` |
| `muse-spark-1.3-contributor-free` | `minimal`–`max` `*` | the 1.3 family rows (vendor + both gateways) + `raw/zen-muse-spark-1.3-contributor-free.json` (blocked) |
| `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free` | none shown | `raw/zen-<model>.json` (free tier) |
| `qwen3.5-plus`, `qwen3.6-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` `*` | `raw/go-qwen3.7-plus.json`, `raw/go-qwen3.8-flash.json` and the two Zen Qwen probes |
| `qwen3.8-flash` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.8-flash.json` |
| `qwen3.8-max` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.8-max.json` |
| `space-bunny-free` | `minimal`–`max` `*` | `raw/go-space-bunny.json` |
| `step-5-preview-free` | `minimal`–`max` `*` | `raw/go-step-5-preview-free.json` |

## Consequence

The two gateway tables name a `thinking` row for every generated model:
inherited vendor rows where the id matches, vendor docs where they settle the
levels, and these probes otherwise. A row marked `*` cites another model's
record because the account block above left no same-route probe; the blocked
record sits beside it. Models with no shown level take an explicit empty row,
so Fiber never sends a level there.
