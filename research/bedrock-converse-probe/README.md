# AWS Bedrock's Converse API, probed

Live probe for `bedrock-converse`: Bedrock's ConverseStream request shapes,
its AWS event-stream reply, and its error shapes, on Bedrock's models other
than Claude, plus one Claude model for a signed reasoning block. Run on
October 20, 2026 from macOS, against
`https://bedrock-runtime.ap-south-1.amazonaws.com`, signed with SigV4 from one
AWS profile's keys.

## Method

`PROBE_PROFILE=<profile> python3 -I probe.py <case>...` exits before any AWS
call when `PROBE_PROFILE` is unset. It signs each request itself (standard
library only), sends it to `/model/<id>/converse-stream`, and saves:

- `raw/<name>.eventstream`: a 200 reply's bytes as they arrived, scrubbed
  (below), with both CRC-32s recomputed;
- `raw/<name>.json`: the request body, the status, the response's
  `content-type` and `x-amzn-errortype` only, and either the decoded events
  with their usage and cost, or the error body.
- `raw/classify/<id>.json`: one check per model, decoded events only.

Request headers are never saved, and the credentials are never printed: the
probe reads them from `aws configure export-credentials`. The account id is
read from the ARNs `aws bedrock list-inference-profiles` returns, held in
memory, and zeroed wherever a saved byte carries it: in clear, and inside any
base64 run (a reasoning signature, a `redactedContent`), each run re-encoded
at its own length. A request id, which an error body from a Mistral model
carried, is zeroed too. `probe.py rescrub` scrubs every saved file again and
checks that none holds the account id, in clear or in base64, or a request
id.

AWS calls made: ConverseStream, `aws configure export-credentials`, and
`aws bedrock list-foundation-models` and `list-inference-profiles` (read
only). No resource was created and no model access was requested.

## Spend

Prices are models.dev's `amazon-bedrock` entry, per million tokens. The probe
kept a running total in `raw/spent.json`: $0.025474, the request-id check's
$0.000002 included. The saved records sum to
$0.025034; the difference is a first `tool-results` run whose records were
overwritten when it was rerun with a valid tool-use id. One check,
`nvidia.nemotron-super-3-120b`, timed out with no reply to price; at its
200-token cap it cost at most $0.00015.

| Model | Calls | Input | Output | Cache read | Cache write | Cost |
|---|---:|---:|---:|---:|---:|---:|
| `global.amazon.nova-2-lite-v1:0` | 10 | 3,697 | 2,851 | 0 | 1,999 | $0.008836 |
| `global.xai.grok-4.7` | 3 | 328 | 298 | 0 | 0 | $0.002444 |
| `apac.amazon.nova-pro-v1:0` | 2 | 417 | 49 | 0 | 2,121 | $0.002296 |
| `global.openai.gpt-6-astra` | 1 | 55 | 18 | 0 | 0 | $0.001450 |
| `apac.amazon.nova-micro-v1:0` | 14 | 1,314 | 146 | 6,771 | 35,554 | $0.001447 |
| `global.moonshotai.kimi-k3` | 1 | 189 | 48 | 0 | 0 | $0.001287 |
| `global.anthropic.claude-haiku-4-5-20251001-v1:0` | 2 | 128 | 127 | 0 | 0 | $0.000763 |
| `deepseek.v3.2` | 3 | 727 | 78 | 0 | 0 | $0.000594 |
| `openai.gpt-oss-20b-1:0` | 13 | 1,615 | 1,234 | 0 | 0 | $0.000483 |
| `mistral.ministral-3-8b-instruct` | 13 | 738 | 135 | 0 | 0 | $0.000131 |
| 41 other models and ids, mostly one `classify` check each | 48 | | | | | $0.005303 |

## The stream

Every 200 reply had `content-type: application/vnd.amazon.eventstream`, the
framing Bedrock's Invoke API uses (`research/bedrock-invoke-probe`). Each
message carried three string headers, `:event-type`, `:content-type`
`application/json` and `:message-type` `event`, and a JSON payload: the event
itself, not base64 inside a `chunk` as Invoke wraps it, beside a `p` field of
padding.

- Events, in order: `messageStart` (`role`), then per content block an
  optional `contentBlockStart`, its `contentBlockDelta`s and
  `contentBlockStop`, each with `contentBlockIndex`, then `messageStop`
  (`stopReason`), then `metadata` (`usage`, `metrics.latencyMs`). `metadata`
  was the last event of every stream, after `messageStop`.
- A text block and a reasoning block open with their first delta: only a
  tool-use block has a `contentBlockStart`, carrying `toolUse.toolUseId` and
  `toolUse.name`. Its input arrives as `delta.toolUse.input`, a JSON string
  fragment, whole in one delta in every recording.
- Most models opened with an empty text delta at index 0, then a stop, before
  any other block.
- `tool-ministral`, `tool-qwen`, `tool-nova`: two `toolUse` blocks in one
  reply, `stopReason` `tool_use`. Bedrock's ids look like
  `tooluse_tHrm6EVp9eFVhl3bFbWxUS`.
- Reasoning arrives as `delta.reasoningContent`: `text` (gpt-oss, Kimi,
  MiniMax, Claude), `signature` in its own last delta (Claude only), or
  `redactedContent` (Grok 4.6 and 4.7). Nova 2 Lite streams the literal text
  `[REDACTED]`. No model other than Claude sent a signature.
- `messageStop.stopReason` seen: `end_turn`, `tool_use`, `max_tokens`.
- No event names the generation: there is no message id. `request-id`,
  `request-id-error`: the 200 stream and a 400 error each carried an
  `x-amzn-RequestId` header holding a UUID. The records keep only a
  synthetic value in its place and its shape.
- `usage` holds `inputTokens`, `outputTokens`, `totalTokens`,
  `serverToolUsage`, and on a model that caches, `cacheReadInputTokens` and
  `cacheWriteInputTokens` (each repeated as `...TokenCount`). `inputTokens`
  excludes both: `cache-read` reported 4 input and 2,121 read. No reply
  carried `cacheDetails`, which AWS documents as the writes by TTL.

## Requests

- `system: [{text}]`, `messages: [{role, content}]`, `inferenceConfig:
  {maxTokens}`, `toolConfig: {tools: [{toolSpec: {name, description,
  inputSchema: {json}}}]}` were accepted by all 47 models that answered `classify`.
- `no-toolconfig`: a `toolUse` and `toolResult` in the history with no
  `toolConfig` answered 400 "The toolConfig field must be defined when using
  toolUse and toolResult content blocks."
- `toolResult` `status: error` was accepted by gpt-oss 20B, Ministral 3 8B,
  Qwen3 32B, Nova Micro and DeepSeek V3.2, and each model's reply spoke of the
  failure. AWS documents the field as "only supported by Amazon Nova and
  Anthropic Claude 3 and 4 models". An empty result text was accepted.
- A Mistral model checks a replayed `toolUseId`: `tooluse_probe_1` answered
  400 "Tool call id was e_probe_1 but must be a-z, A-Z, 0-9, with a length of
  9." Bedrock passes the id's last nine characters, so an id Bedrock issued
  passes, and `tooluse_probe0001` did.
- `image`: an image block in a user message, `{format: "png", source:
  {bytes}}`, was read by Ministral 3 8B. An image inside a `toolResult`'s
  content was refused by Ministral 3 8B and Qwen3 VL ("This model doesn't
  support the image field for user messages") and read by Nova 2 Lite. The
  same image beside the `toolResult`, as the next block of the same user
  message, was accepted by all three.
- `strict` on a `toolSpec`, `true` or `false`, was accepted by gpt-oss 20B and
  Ministral 3 8B and refused by Nova Micro: "This model doesn't support the
  strict field. Remove strict and try again."
- `toolChoice: {auto: {}}` was accepted by gpt-oss 20B; AWS documents `auto`
  as the default. Converse has no `none`.
- `maxTokens: 128000` was accepted by gpt-oss 20B, whose model card states a
  16K output limit.

## Reasoning

- `additionalModelRequestFields: {reasoning_effort: "low"}` and `"high"` on
  gpt-oss 20B, and `"low"` on gpt-oss 120B, were accepted; `high` spent 164
  output tokens where `low` spent 21 on the same question. Without the field
  gpt-oss reasons anyway.
- `additionalModelRequestFields: {reasoningConfig: {type: "enabled",
  maxReasoningEffort: "low" | "medium"}}` on Nova 2 Lite was accepted.
  `"high"` with `maxTokens` set answered 400 "`maxTokens` must be unset when
  reasoningConfig type is 'enabled' and maxReasoningEffort is 'high'"; with
  `inferenceConfig` left out it was accepted.
- DeepSeek V3.2 sent no reasoning without a switch.
- `replay-oss`, `replay-nova2`: the reply's `reasoningContent.reasoningText`
  (text, no signature) sent back before its text was accepted.
- `signature`, `signature-replay`: Claude Haiku 4.5 through Converse, with
  `additionalModelRequestFields: {thinking: {type: "enabled", budget_tokens:
  1024}}`, streamed a signed reasoning block, and the block sent back with its
  signature was accepted. The signature carries the AWS account id in clear
  inside its base64, as on Invoke, so the saved signatures no longer verify.
- `redacted-replay`: Grok 4.7's `redactedContent` sent back was accepted.

## Prompt caching

- `cache-write`, `cache-read` (Nova Micro): a `{cachePoint: {type:
  "default"}}` block after a 2,121-token system text wrote it, then read it.
  `cache-write-*`: Nova Lite, Nova Pro and Nova 2 Lite wrote it too.
- `cache-ttl-1h`: `ttl: "1h"` answered 400 "Extended TTL prompt caching is
  only supported for Anthropic models".
- `cache-turn-1`, `cache-turn-2`: markers after a user text in `messages`
  wrote, then read, the turn before.
- `cache-messages`, `cache-after-tool-use`: a `cachePoint` after a
  `toolResult` or a `toolUse` block answered 400 "extraneous key [cachePoint]
  is not permitted". `cache-text-after-result`: after a text that follows a
  `toolResult` in the same message, it read and wrote.
- `cache-over-20k`: a marker after 31,181 tokens wrote all of them. AWS
  documents a 20K-token ceiling for Nova's caching.

## Which models speak it

`classify` sent each models.dev `amazon-bedrock` model that ap-south-1 serves,
by on-demand id or inference profile, one user text under a `toolConfig`
holding one tool. 47 of 49 answered 200, and 42 called the tool; the rest
stopped at their output cap or answered in text. `global.zai.glm-5.3`
answered 403 `AccessDeniedException` "not available for this account", and
`nvidia.nemotron-super-3-120b` sent no response head within 300 seconds.

## Errors

| Case | Status | `x-amzn-errortype` | Body |
|---|---|---|---|
| `bad-model` (an unknown id) | 400 | `ValidationException` | `{"message":"The provided model identifier is invalid."}` |
| `validation` (`maxTokens: 0`) | 400 | `ValidationException` | `1 validation error detected: Value '0' at 'inferenceConfig.maxTokens' ...` |
| `classify/global.zai.glm-5.3` | 403 | `AccessDeniedException` | `... is not available for this account ...` |

Not reached: an exception inside a stream, a throttling exception, a
`stopReason` other than the three above, and a Bedrock API key.

## Fiber's own sessions

Run the same day with `fiber ask` and the `bedrock` package from the pull
request that adds `bedrock-converse`, the credential label naming the
probe's AWS profile (so `credential()` ran `aws configure export-credentials`
and `sign()` signed with SigV4), `AWS_REGION=ap-south-1`, and an isolated
Fiber home. Fiber logged `cost: null` for every call but the last: those runs
predate taking `x-amzn-RequestId` as the generation id, so each call's id was
minted. The last run's id was the request's UUID, and Fiber logged its cost,
$0.00022267, the tokens at the model's declared prices. The cost column is
the tokens at models.dev's prices.

| Run | Model | Requests | Input | Cache write | Cache read | Output | Cost |
|---|---|---:|---:|---:|---:|---:|---:|
| plain text | `openai.gpt-oss-20b-1:0` | 1 | 3,035 | | | 32 | $0.000222 |
| `read` a file, then answer | `mistral.ministral-3-8b-instruct` | 2 | 8,547 | | | 26 | $0.001286 |
| the same | `apac.amazon.nova-micro-v1:0` | 2 | 0 | 35,415 | 0 | 131 | $0.001330 |
| the same, again | `apac.amazon.nova-micro-v1:0` | 2 | 0 | 15,362 | 20,324 | 66 | $0.000766 |
| `:low` | `openai.gpt-oss-20b-1:0` | 1 | 3,038 | | | 15 | $0.000217 |
| `:high` | `global.amazon.nova-2-lite-v1:0` | 1 | 0 | 18,894 | | 727 | $0.007486 |
| plain text, priced | `openai.gpt-oss-20b-1:0` | 1 | 3,031 | | | 35 | $0.000223 |

Every run exited 0. The Ministral run sent Bedrock's own `toolUseId` back
with the result and was answered. In the first Nova Micro run the second
request wrote its prefix again rather than reading it; in the second run the
second request read 17,164 tokens up to the previous request's end, so
Fiber's markers read when the request lands where the write did. The `:low`
run's reasoning was one sentence, and the `:high` run, which sends no
`maxTokens`, streamed `[REDACTED].` before its answer. Fiber's spend:
$0.011530, for a total with the probe of $0.037004, plus at most $0.00015
for the check that timed out.

## Where AWS's documentation and the recordings differ

- `toolResult.status` is documented for Nova and Claude only; five other
  models accepted `error`.
- `cacheDetails` is documented in ConverseStream's `usage`; no reply carried
  it.
- Nova's caching is documented to stop at 20K tokens; a 31,181-token marker
  wrote every token.
- gpt-oss 20B's model card states 16K output tokens; `maxTokens: 128000` was
  accepted.
- The image block inside `toolResult.content` is documented for every model;
  two of three vision models refused it.
