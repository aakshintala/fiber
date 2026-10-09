# OpenCode Zen Gemini probe

Question: does OpenCode Zen answer a `google-generative-ai` request for one of
its Gemini models with the OpenCode key? (#1642, criterion 1.)

## Method

`probe.py` on October 9, 2026, from macOS, with the key read from
`~/.config/probe-keys/opencode-key` and never written out (the raw file is
checked for it). One request, no retry:
`POST https://opencode.ai/zen/v1/models/gemini-3.5-flash-lite:generateContent`
with `contents` (one user text part), `generationConfig.maxOutputTokens: 16`,
and the headers `x-goog-api-key`, `Authorization: Bearer`, `x-opencode-session`
and a `User-Agent`. The raw request and response are in `raw/generate.json`.

Spend: none recorded. The request was refused before any generation.

## Findings

- **The request was refused: 403.** The body was
  `{"error":{"code":403,"status":"UNKNOWN","message":"Upstream request failed: Model access is disabled"}}`.
  The reply has Google's error envelope (`error.code`, `error.status`,
  `error.message`), so the route exists and the path was understood.
- **No usage fields were seen.** The refusal carries no `usageMetadata`.
- **The cause is the model's access, not the request shape.** The message names
  the model, not a header or a field. Whether it is the account, the workspace
  or Zen's model settings is not measured. Only `gemini-3.5-flash-lite` was
  tried; the other Gemini models on Zen are unprobed.

## Consequence

The generator keeps Zen's `google-generative-ai` models out
(`xtask/src/models_dev_table.rs`). A later probe on an enabled Gemini model
reopens the question.
