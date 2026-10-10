# OpenCode Zen Gemini probe

Question: does OpenCode Zen answer a `google-generative-ai` request for one of
its Gemini models with the OpenCode key? (#1642, criterion 1.)

## Method

`probe.py <model>` sends one request per run, with the key read from
`~/.config/probe-keys/opencode-key` and never written out (the raw file is
checked for it). No retry:
`POST https://opencode.ai/zen/v1/models/<model>:generateContent` with
`contents` (one user text part), `generationConfig.maxOutputTokens: 16`, and the
headers `x-goog-api-key`, `Authorization: Bearer`, `x-opencode-session` and a
`User-Agent`. The raw request and response of the latest run per model are in
`raw/generate-<model>.json`.

## Findings

Run 1, October 9, 2026, macOS, `gemini-3.5-flash-lite`: **403**,
`{"error":{"code":403,"status":"UNKNOWN","message":"Upstream request failed: Model access is disabled"}}`.
The cause was the account's model access, not the request shape. The owner then
enabled Gemini access on the Zen account.

Run 2, the same day, one request each:

| Model | Status | Finish reason | Prompt tokens | Output tokens | Thought tokens | Total |
| --- | --- | --- | --- | --- | --- | --- |
| `gemini-3.5-flash-lite` | 200 | `STOP` | 7 | 1 | none | 8 |
| `gemini-3.8-flash` | 200 | `MAX_TOKENS` | 7 | none | 12 | 19 |

- **The route works with the OpenCode key.** Both replies have Gemini's
  `generateContent` shape: `candidates[].content.parts[].text`, `finishReason`,
  `modelVersion` and `usageMetadata`.
- **Usage fields:** `promptTokenCount`, `candidatesTokenCount` (absent when the
  model wrote no text), `thoughtsTokenCount` (only on `gemini-3.8-flash`),
  `totalTokenCount`, and per-modality `promptTokensDetails` and
  `candidatesTokensDetails`.
- **`gemini-3.8-flash` spent the 16-token cap on thinking** and returned no
  text: `candidates[0]` has a `finishReason` and no `content`. A client must
  accept a candidate without `content`.
- **`gemini-3.5-flash-lite` returned a `thoughtSignature`** on its text part.

## Consequence

The generator keeps Zen's `google-generative-ai` models. Of the eight
`@ai-sdk/google` models on `opencode` in models.dev, seven are kept; the eighth,
`gemini-3-pro`, is `deprecated` and dropped as every deprecated model is. Five
kept models were not probed; they share the route and the protocol.
