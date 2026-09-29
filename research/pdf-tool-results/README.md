# PDF inside a tool result, per protocol

Findings for [#121](https://github.com/aakshintala/fiber/issues/121). Probed live on 2026-09-29 from
macOS, directly against each vendor (OpenRouter is left out: it would test only its own translation).

## Answer

| Protocol | In the tool result | Form |
|---|---|---|
| `anthropic-messages` | accepted | native: a `document` block (base64 `application/pdf`) inside `tool_result.content` |
| `openai-responses` | accepted | native: an `input_file` item (`file_data` data URI with `filename`, or `file_id`) in the `function_call_output.output` array |
| `openai-completions` | refused (400) | a `file` part in a tool message is rejected. A `file` part in a user message after the tool message works, and the cached prefix survives. An `image_url` part in a tool message returns 200, and in two requests the model did not answer about it (see below) |
| `google-generative-ai` | accepted | native: `functionResponse.parts[]` with `inlineData` (`application/pdf`), on `gemini-3.1-flash-lite` |

The text PDF answered the quote question and the scanned PDF answered the figure question on every
accepted shape, except where the table below says otherwise.

## Method

`make_inputs.py` writes `text.pdf` (two text pages, hand-built; page one holds "The launch code is
PELICAN-4471.") and a PNG of an orange circle with a white "73"; `sips -s format pdf` turns the PNG
into `scanned.pdf` (image only, no text layer). Both are under 20 KB.

`probe.py <anthropic|responses|completions|gemini>` builds a fake history (user asks, assistant calls
`read`, the tool result carries the PDF), asks the question, then sends a second request that extends
the first with the assistant's answer and a follow-up, to read cached tokens. Each vendor runs the PDF
in the tool result and, as a comparison, as a user-message part right after the tool result. Raw
requests (any string over 2,000 characters, such as the scanned PDF's base64 and the filler, is replaced by a
placeholder; the 1.2 KB base64 of `text.pdf` stays in full) and responses are in `raw/<protocol>.<shape>.<file>.<n>.json`.
System prompt filler makes the prefix several thousand tokens long. `probe.py extras` sends the requests outside the
shape loops (the 404, the three 400s, the Gemini repeat, the completions image cases).

Models and prices (per million tokens):

| Vendor | Model | Input / cached / output | Source |
|---|---|---|---|
| Anthropic | `claude-sonnet-5-5` | $2 / $0.20 / $10 (cache write $2.50) | https://platform.claude.com/docs/en/about-claude/pricing |
| OpenAI | `gpt-6-luna` | $0.10 / $0.01 / $0.50 | https://developers.openai.com/api/docs/pricing |
| Google | `gemini-3.1-flash-lite` | $0.25 / $0.025 / $1.50 | https://ai.google.dev/gemini-api/docs/pricing |

The Gemini key gets HTTP 404 "no longer available to new users" for `gemini-2.5-flash-lite`
(`raw/gemini.404-2.5-flash-lite.json`), so all Gemini rows use `gemini-3.1-flash-lite`.

Spend (estimated from response usage, under the $1.00 cap each): the committed Anthropic raw files account
for about $0.22; the rest of the Anthropic spend (about $0.27 more) is from two discarded runs with no
raw kept. OpenAI about $0.01 across both protocols, Google about $0.03.

## Per protocol

### anthropic-messages

- In the tool result: 200. `raw/anthropic.in-result.text.1.json` quotes the sentence and the code;
  `raw/anthropic.in-result.scanned.1.json` says "an orange circle ... the number 73".
- After the tool result, as a `document` block in the same user message: also 200, same answers
  (`raw/anthropic.after-result.*`).
- Cache: `cache_control` on the `tool_result` block wrote 12,839 tokens (text) and 11,249 (scanned);
  the follow-up read all of them (`raw/anthropic.in-result.text.2.json`: 12,839 read, 80 new). So the
  PDF is inside the cached prefix.
- Limits (https://platform.claude.com/docs/en/build-with-claude/pdf-support): 32 MB request size
  (varies by platform), 600 pages per request, or 100 when the request's context window is under 1M
  tokens; standard PDF, no password. Both limits cover the whole payload. Not tested: no request
  approached them.

### openai-responses

- In the tool result: `function_call_output.output` as an array holding one `input_file`. With
  `file_data` (data URI) plus `filename`: 200 (`raw/responses.in-result-file_data.*`). With `file_id`
  from `POST /v1/files` (`purpose=user_data`): 200 (`raw/responses.in-result-file_id.*`). With `file_id` together with `filename`: 400 "Mutually exclusive
  parameters: 'input[2].output[0]'. Ensure you are only providing one of: 'file_id' or 'filename'."
  (`raw/responses.file_id-with-filename.1.json`).
- After the tool result, as an `input_file` in a user message: 200 (`raw/responses.after-result.*`).
- Both questions were answered in all three shapes.
- Cache: the follow-up read 6,772 of 6,820 input tokens (text, in the result) and 7,388 of 7,428
  (scanned). Automatic caching, `prompt_cache_key` set. The follow-up prefix includes the PDF.
- Limits (https://developers.openai.com/api/docs/guides/pdf-files): each file under 50 MB, 50 MB
  combined per request; no page limit stated. Not tested. The docs do not say whether a PDF is
  allowed in a function output; the probe shows it is.

### openai-completions

- In the tool result: 400, `messages[3].content[0]`: "Invalid value: 'file'. Supported values are:
  'text', 'refusal', 'image_url', and 'input_audio'." (`raw/completions.in-result.text.1.json`).
- After the tool result, as a `file` part (`file_data` data URI plus `filename`) in a user message:
  200, text question answered (`raw/completions.after-result.text.1.json`). The scanned question was
  answered "23" then "23" on the follow-up (`raw/completions.after-result.scanned.1.json`); the
  figure is a 73. The model ran with `reasoning_effort: none`, the only setting this model accepts
  with function tools on `/v1/chat/completions` (400 "Function tools with reasoning_effort are not
  supported ... set reasoning_effort to 'none'", `raw/completions.reasoning-effort-with-tools.1.json`), and the Responses probe with `low` read it right.
  Read the wrong digit as this run's model result, not as a protocol fact.
- Cache: the follow-up read 7,056 of 7,112 (text) and 6,784 of 7,336 (scanned). The user-message
  PDF sits inside the cached prefix.
- Rendered pages: a PNG of the scanned page as an `image_url` part inside the tool message returned
  200. In the two committed requests the model answered with a second `read` call and no text
  (`raw/completions.image-in-result.scanned.1.json` and `.2.json`). An earlier run of the same request,
  whose raw files were overwritten, answered in words that it could not access the page image. The same PNG in a
  user message after the tool message was answered "73" in the committed run
  (`raw/completions.image-after-result.scanned.1.json`; an earlier run read "3"). Two requests with one PNG:
  this shows the model did not use the image in the tool message, not that no such image is ever used.
- Limits: same page as Responses (50 MB per file, 50 MB combined). `file_data` and `file_id` only;
  no URL.

### google-generative-ai

- In the tool result, `functionResponse.parts[]` holding `inlineData`: 200, both questions answered
  (`raw/gemini.in-result-parts.*`). The shape follows the API's multimodal function response; the
  function-calling doc page fetched did not describe it, so the shape is from the request that
  worked, not from a doc.
- A `functionResponse.response` field holding an `inlineData` object: 200, but the base64 is read
  as JSON text (23,322 input tokens for the scanned file against 8,831 in the parts shape), the
  model called the tool again with no answer on one request and described "a red circle with the
  number 1" on the follow-up (`raw/gemini.in-result-response-field.scanned.*`). Not a PDF part.
- After the tool result, as an `inlineData` part beside the `functionResponse`: 200, both answered
  (`raw/gemini.after-result.*`).
- A model turn made up by the script needs `thoughtSignature`; the script uses the documented
  placeholder `skip_thought_signature_validator`. Without any signature: 400 "Function call is
  missing a thought_signature" (`raw/gemini.no-thought-signature.text.1.json`).
- Cache: implicit caching. In `raw/gemini.cache-repeat.in-result-parts.text.*` (three identical
  requests, 8 seconds apart in the script) the first reported no cached tokens; the second and third
  reported 4,016 of 9,365, made up of 3,570 text and 446 IMAGE tokens (`cacheTokensDetails`). The PDF is the
  only image input, and its prompt count is 1,040 IMAGE tokens, so 446 of the PDF's 1,040 tokens were
  cached and the rest was not. The cached part stops short of the whole prompt, so the follow-up does not
  re-read all of it. The cause of the cutoff is not known. The caching page lists 2,048 as the minimum for
  2.5 Flash and 4,096 for 3.x Flash models (https://ai.google.dev/gemini-api/docs/caching); no figure for this model.
- Limits (https://ai.google.dev/gemini-api/docs/document-processing): 50 MB, 1,000 pages, 258
  tokens per page. Not tested.

## Malformed replies

- Anthropic, `claude-sonnet-5-5`, 2026-09-29: with a filler of random lowercase words in the system
  prompt, six of eight requests in the second run (both PDFs, in-result and after-result, first and
  follow-up) returned `stop_reason: "refusal"`, `stop_details.category: "bio"`, HTTP 200, and were
  billed (`raw/anthropic-bio-refusals/`). Five had empty `content`; `anthropic.in-result.text.2.json`
  had two `thinking` blocks and no text. In the first run, with a longer filler of the same kind, the
  scanned in-result request also refused; no raw was kept. Replacing the filler with plain
  build-log sentences gave eight normal replies. Cause not isolated; the PDFs and questions were benign.
- OpenAI `gpt-6-luna` on completions read the pixel digits "73" as "23"; the Responses run read them right.
- Gemini `gemini-3.1-flash-lite` answered one request with a `functionCall` and no text
  (`raw/gemini.in-result-response-field.scanned.1.json`), a consequence of the base64-in-JSON shape.

## For the owner

- The Decide items on the ticket were not ruled on. Evidence for the "native or rendered pages" choice:
  three protocols take the PDF natively in the tool result (Anthropic, OpenAI Responses, Gemini). On
  `openai-completions` the PDF is not accepted there, but a native `file` part in a user message after the tool
  result works and keeps the cache; rendered pages would also have to go in a user message, because
  an image in the tool message was accepted but reported unseen.
- Anthropic's user-part-after-result and OpenAI's are both accepted too, so "in the result" is
  chosen, not forced, on those two.
- Scope: one small text PDF and one one-page scan per shape; one model per vendor; sizes and page
  limits are the vendors' stated limits and were not exercised.
