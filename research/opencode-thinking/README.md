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

## Re-probe (2026-10-15)

The owner enabled every model and provider on the OpenCode account, so every
model blocked by the account was re-probed: the three Go DeepSeek models that
had answered 400 `This Go model requires Global regions`, and every Zen
model that had answered 403 `Model access is disabled`. Each re-probe
overwrote its record in place, so every `raw/` file below is the latest run.
The region block is lifted (all three Go DeepSeek models 200 every level),
and all but four of the disabled Zen models now answer: `kimi-k2.5`,
`kimi-k2.6` and `kimi-k2.7-code` answer 410 `Endpoint is unavailable`, and
`deepseek-v4-flash-vision-exp` does too. The per-model tables name the
re-probed records.

## Account limits met while probing

- Free tier: `big-pickle`, `longcat-2.5-preview-free`,
  `mimo-v2.6-flash-free`, the two `nemotron` frees, `space-bunny-free`,
  `step-5-preview-free` and `muse-spark-1.3-contributor-free` answer 403
  `FreeTierError` outside OpenCode, by design rather than by account
  setting. They leave the Zen package through its `skip` list;
  `longcat-2.5-preview-free` and `step-5-preview-free` keep their Go rows,
  whose same-model Go probes 200.
- `exo-free` answers 410 `ModelDeprecated`; it leaves through the skip list.
- `ling-3.0-flash-fin-free` and `ling-3.1-flash-free` answer 400 `Model is
  unavailable`; they leave through the skip list.
- `kimi-k2.5` answers 410 `Endpoint is unavailable` on Zen (re-probed) and
  400 `Model is unavailable` on Go; no probe reaches it on either route, so
  it leaves through the skip list.
- `kimi-k2.6` and `kimi-k2.7-code` answer 410 `Endpoint is unavailable` on
  Zen (re-probed), but their same-model Go probes 200, so their Zen rows
  cite the Go records.
- `deepseek-v4-flash-vision-exp` answers 410 `Endpoint is unavailable` on
  Zen (re-probed); it leaves the Zen package through its `skip` list while
  its Go row cites the same-model Go probe.
- `grok-build-0.1` answers 400 `does not support parameter reasoningEffort`
  at every level: the probe shows the model takes no level, so it keeps an
  explicit empty row.

## Findings per model

Go (`raw/go-<model>.json`); `*` marks a row citing another record:

| Model | Accepts | Record |
| --- | --- | --- |
| `deepseek-v4-flash` | all seven | `raw/go-deepseek-v4-flash.json` (re-probe; the Global-regions block is lifted) |
| `deepseek-v4-flash-vision-exp` | all seven | `raw/go-deepseek-v4-flash-vision-exp.json` |
| `deepseek-v4-pro` | all seven | `raw/go-deepseek-v4-pro.json` (re-probe; the Global-regions block is lifted) |
| `deepseek-v4.1-flash` | all seven | `raw/go-deepseek-v4.1-flash.json` (re-probe; the Global-regions block is lifted) |
| `glm-5` | none shown | `raw/go-glm-5.json` (`Model is unavailable` at every level; skipped) |
| `glm-5.1` | `minimal`–`max` | `raw/go-glm-5.1.json` (`off` 400: thinking-only model; the error names the GLM-5.3 backend) |
| `glm-5.2` | `minimal`–`max` | `raw/go-glm-5.2.json` (`off` 400: thinking-only model) |
| `glm-5.3` | `minimal`–`max` | `raw/go-glm-5.3.json` (reasoning tokens move with the level) |
| `glm-5.3-flash` | all seven | `raw/go-glm-5.3-flash.json` |
| `hy3` | all seven | `raw/go-hy3.json` (completion tokens move with the level) |
| `hy4-preview` | all seven | `raw/go-hy4-preview.json` |
| `kimi-k2.5` | none shown | `raw/go-kimi-k2.5.json` (`Model is unavailable` at every level; skipped) |
| `kimi-k2.6` | all seven | `raw/go-kimi-k2.6.json` (every level 200s, including `off`) |
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
| `qwen3.5-plus` | not in the Go source | `raw/go-qwen3.5-plus.json` (`Model is unavailable` at every level; models.dev's `opencode-go` source lists no such model, so only Zen keeps a row, via `raw/zen-qwen3.5-plus.json`) |
| `qwen3.6-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.6-plus.json` (`minimal` 400, as on its siblings) |
| `qwen3.7-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.7-plus.json` |
| `qwen3.8-flash` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.8-flash.json` (thinking blocks in every reply) |
| `qwen3.8-max` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/go-qwen3.8-max.json` |
| `space-bunny` | `minimal`–`max` | `raw/go-space-bunny.json` |
| `step-5-preview-free` | `minimal`–`max` | `raw/go-step-5-preview-free.json` |

Zen (`raw/zen-<model>.json`); `*` marks a row citing another record:

| Model | Accepts | Record |
| --- | --- | --- |
| `big-pickle` | none shown | `raw/zen-big-pickle.json` (free tier; skipped) |
| `deepseek-v4-flash` | `low`, `high`, `xhigh`, `max` | `raw/zen-deepseek-v4-flash.json` (re-probe; served by `deepseek-v4.1-flash` upstream, which names those four as its supported efforts and rejects `off`, `minimal` and `medium`) |
| `deepseek-v4-flash-vision-exp` | none shown | `raw/zen-deepseek-v4-flash-vision-exp.json` (`Endpoint is unavailable` at every level; skipped on Zen, kept on Go) |
| `deepseek-v4-pro` | all seven | `raw/zen-deepseek-v4-pro.json` (re-probe) |
| `deepseek-v4.1-flash` | all seven | `raw/zen-deepseek-v4.1-flash.json` |
| `exo-free` | none shown | `raw/zen-exo-free.json` (deprecated; skipped) |
| `gemini-3-flash` | `off`, `minimal`, `low`, `medium`, `high`, `max` | `raw/zen-gemini-3-flash.json` (re-probe; `xhigh` 400s as an invalid thinking level) |
| `gemini-3.1-pro` | `low`, `medium`, `high`, `max` | `raw/zen-gemini-3.1-pro.json` (re-probe; `off`, `minimal` and `xhigh` 400) |
| `glm-5` | `minimal`–`max` | `raw/zen-glm-5.json` (re-probe; `off` 400: thinking-only model) |
| `glm-5.1` | `minimal`–`max` | `raw/zen-glm-5.1.json` (re-probe; `off` 400: thinking-only model) |
| `glm-5.2`, `glm-5.3` | `minimal`–`max` | `raw/zen-glm-5.2.json`, `raw/zen-glm-5.3.json` (re-probes) |
| `glm-5.3-flash` | `minimal`–`max` | `raw/zen-glm-5.3-flash.json` (`off` 400: thinking-only model) |
| `gpt-5-codex` | `low`, `medium`, `high` | `raw/zen-gpt-5-codex.json` (re-probe; the route enumerates the supported values) |
| `gpt-5.1-codex` | `low`, `medium`, `high` | `raw/zen-gpt-5.1-codex.json` (re-probe; same enumeration) |
| `gpt-5.1-codex-max` | `low`, `medium`, `high`, `xhigh` | `raw/zen-gpt-5.1-codex-max.json` (re-probe; same enumeration) |
| `gpt-5.1-codex-mini` | `low`, `medium`, `high` | `raw/zen-gpt-5.1-codex-mini.json` (re-probe; same enumeration) |
| `gpt-5.2-codex` | `low`, `medium`, `high`, `xhigh` | `raw/zen-gpt-5.2-codex.json` (re-probe; same enumeration) |
| `grok-build-0.1` | none shown | `raw/zen-grok-build-0.1.json` (every level 400s `reasoningEffort` unsupported: the model takes no level) |
| `kimi-k2.5` | none shown | `raw/zen-kimi-k2.5.json` (`Endpoint is unavailable`; unavailable on Go too; skipped) |
| `kimi-k2.6` | all seven `*` | `raw/go-kimi-k2.6.json` (Zen answers `Endpoint is unavailable`, re-probed) |
| `kimi-k2.7-code` | `minimal`–`max` `*` | `raw/go-kimi-k2.7-code.json` (Zen answers `Endpoint is unavailable`, re-probed) |
| `ling-3.0-flash-fin-free`, `ling-3.1-flash-free` | none shown | `raw/zen-<model>.json` (unavailable; skipped) |
| `longcat-2.5-preview-free` | none shown on Zen | `raw/zen-longcat-2.5-preview-free.json` (free tier; skipped on Zen, kept on Go via `raw/go-longcat-2.5-preview-free.json`) |
| `mimo-v2.6-flash-free` | none shown on Zen | `raw/zen-mimo-v2.6-flash-free.json` (free tier; skipped) |
| `minimax-m2.5`, `minimax-m2.7`, `minimax-m3` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-<model>.json` (re-probes; `minimal` 400s) |
| `mistral-large-4` | `off`, `high` | `raw/zen-mistral-large-4.json` (the `high` reply carries a thinking chunk) + Mistral reasoning docs |
| `muse-spark-1.2` | `minimal`–`xhigh` | `raw/zen-muse-spark-1.2.json` (re-probe; `off` and `max` rejected with the supported set) |
| `muse-spark-1.3-contributor-free` | none shown on Zen | `raw/zen-muse-spark-1.3-contributor-free.json` (free tier; skipped; the `ask.rs` levels assertion for it is reported for removal) |
| `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free` | none shown | `raw/zen-<model>.json` (free tier; skipped) |
| `qwen3.5-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.5-plus.json` (re-probe; `minimal` 400, as on its siblings) |
| `qwen3.6-plus` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.6-plus.json` (re-probe; `minimal` 400) |
| `qwen3.8-flash` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.8-flash.json` |
| `qwen3.8-max` | `off`, `low`, `medium`, `high`, `xhigh`, `max` | `raw/zen-qwen3.8-max.json` |
| `space-bunny-free` | none shown on Zen | `raw/zen-space-bunny-free.json` (free tier; skipped) |
| `step-5-preview-free` | none shown on Zen | `raw/zen-step-5-preview-free.json` (free tier; skipped on Zen, kept on Go via `raw/go-step-5-preview-free.json`) |

## Consequence

The two gateway tables name a `thinking` row for every generated model they
keep: inherited vendor rows where the id matches, vendor docs where they
settle the levels, and these probes otherwise. A row marked `*` cites another
route's record of the same model because the Zen route leaves that model
unreached; the blocked record sits beside it. A model no probe reaches on any
route leaves its package through the table's `skip` list, each id commented
with its blocking probe, and its `thinking` row goes with it. The only empty
row left is `grok-build-0.1`, whose probe shows the route supports no
thinking level there.
