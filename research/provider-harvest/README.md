# Provider protocol harvest

One fact table for each of five wire formats: Fiber's four protocols
(`docs/model-routing.md`, "Protocols and providers") and the ChatGPT/codex
variant of Responses. Each file compares pi (`@earendil-works/pi-ai`
0.87.1, compiled JavaScript, plus the vendor SDKs it pins) with rig
(`~/work/rig`, commit 42f4e06, September 26, 2026).

Every fact has a file and line in pi and in rig, and one mark: `agree`,
`disagree`, `pi only` or `rig only`. Each file ends with its disagreements.

| Protocol | File | Agree | Disagree | pi only | rig only |
|---|---|---|---|---|---|
| `anthropic-messages` | [anthropic-messages.md](anthropic-messages.md) | 17 | 21 | 36 | 6 |
| `openai-completions` | [openai-completions.md](openai-completions.md) | 7 | 28 | 20 | 11 |
| `openai-responses` | [openai-responses.md](openai-responses.md) | 25 | 24 | 17 | 15 |
| `openai-codex-responses` | [openai-codex-responses.md](openai-codex-responses.md) | 19 | 15 | 15 | 2 |
| `google-generative-ai` | [google-generative-ai.md](google-generative-ai.md) | 21 | 23 | 14 | 14 |

The codex file covers only what differs from plain Responses. It also breaks
down pi's 1,302-line codex module; that breakdown is why codex is a variant of
Responses rather than a protocol of its own (ADR 0007).

## Reading the marks

A disagreement is one of two kinds. Some are vendor behaviour that a live
request settles, such as which role Gemini expects on `systemInstruction`.
Others are design choices where pi and rig differ and no request can decide,
such as whether a failed stream keeps its finished tool calls. Each probe
ticket lists the two kinds separately:

- retry signals, all protocols: #131
- `anthropic-messages`: #132
- `openai-completions`: #133
- `openai-responses`: #134
- `openai-codex-responses`: #135
- `google-generative-ai`: #136

`pi only` is not a gap in rig, or the reverse. pi covers more vendors and
login paths; rig models more stream states and is stricter about malformed
input.

## Neither source covers

- A PDF in a tool result. pi has no PDF handling in any protocol. rig's
  tool-result content is text, an image or JSON
  (`crates/rig-core/src/completion/message.rs:372-382`); it sends a PDF only
  as user-message content. See
  [#121](https://github.com/aakshintala/fiber/issues/121).

## Related research

- [research/rig](../rig/README.md): rig's design as a whole.
- [research/provider-errors](../provider-errors/README.md): error shapes
  probed live.
