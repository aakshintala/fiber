# Claude on AWS Bedrock's Invoke API, probed

Live probe for Bedrock's AWS event-stream framing and its error shapes
([#2766](https://github.com/aakshintala/fiber/issues/2766), which carries the
probe [#221](https://github.com/aakshintala/fiber/issues/221) parked). Run on
October 20, 2026 from macOS, against
`https://bedrock-runtime.ap-south-1.amazonaws.com`, signed with SigV4 from one
AWS profile's static keys.

## Method

`PROBE_PROFILE=<profile> probe.py <case>...` signs each request itself
(standard library only) with that AWS profile's credentials, sends
it to `/model/<id>/invoke-with-response-stream`, and saves:

- `raw/<case>.eventstream`: a 200 reply's bytes exactly as they arrived,
  except that the account id is zeroed (below);
- `raw/<case>.json`: the request body, the status, the response's
  `content-type` and `x-amzn-errortype` only, and either the decoded events
  with their usage and cost, or the error body.

Request headers are never saved, and the credentials are never printed: the
probe reads them from `aws configure export-credentials`. `probe.py rescrub`
scrubs the saved files again and checks that no file holds the account id.

- Model: `global.anthropic.claude-haiku-4-5-20251001-v1:0`; the `adaptive`
  case uses `global.anthropic.claude-haiku-5-5`.
- Price used for the ledger: Haiku 4.5's $1 per million input tokens, $5
  output, $0.10 cache read, $1.25 cache write (5 minutes). The `adaptive`
  case is priced the same, ten times Haiku 5.5's own price.
- Spend: $0.009584 at those rates, from the `usage` in the saved files.

| Case | Status | Input | Output | Cache read | Cache write | Cost |
|---|---|---:|---:|---:|---:|---:|
| `plain` | 200 | 13 | 10 | 0 | 0 | $0.000063 |
| `tool` | 200 | 584 | 90 | 0 | 0 | $0.001034 |
| `thinking` | 200 | 50 | 73 | 0 | 0 | $0.000415 |
| `cache-write` | 200 | 9 | 5 | 0 | 5,874 | $0.007377 |
| `cache-read` | 200 | 9 | 5 | 5,874 | 0 | $0.000621 |
| `ttl-1h` | 200 | 15 | 4 | 0 | 0 | $0.000035 |
| `adaptive` | 200 | 24 | 3 | 0 | 0 | $0.000039 |
| `bad-model`, `validation` | 400 | | | | | $0 |
| `bad-signature`, `bad-key-id`, `bad-bearer` | 403 | | | | | $0 |

## The stream

Every 200 reply had `content-type: application/vnd.amazon.eventstream`. Each
message carried three string headers, `:event-type` `chunk`, `:content-type`
`application/json` and `:message-type` `event`, and a JSON payload
`{"bytes": "<base64>", "p": "<padding>"}`, where `bytes` is one Anthropic
stream event and `p` a run of `abcd...` of varying length. Both CRC-32s
checked on every message.

The events are Anthropic's own: `message_start`, `content_block_start`,
`content_block_delta`, `content_block_stop`, `message_delta`, `message_stop`.
No stream sent `ping`. `message_stop` also carried
`amazon-bedrock-invocationMetrics` (`inputTokenCount`, `outputTokenCount`,
`invocationLatency`, `firstByteLatency`). `message_start`'s usage split cache
writes by lifetime in `cache_creation`, as Anthropic's does, and the
`thinking` case's `message_delta` added `output_tokens_details.thinking_tokens`.

- `tool`: two `tool_use` blocks in one reply, ids `toolu_bdrk_...`, each input
  streamed as `input_json_delta` after an empty first delta.
- `thinking`: `thinking: {type: "enabled", budget_tokens: 1024}` gave a
  thinking block, its `signature_delta`, then the text.
- `cache-write`, `cache-read`: a 5,874-token system block with a 5-minute
  marker was written, then read, by the same request sent twice.
- `ttl-1h`: a marker with `ttl: "1h"` was accepted; the prompt was below the
  cacheable minimum, so nothing was written.
- `adaptive`: `thinking: {type: "adaptive"}` with `output_config: {effort:
  "low"}` was accepted on Haiku 5.5.

A thinking block's signature holds the AWS account id in clear inside its
base64. `probe.py` decodes each signature, zeroes the id, encodes it again at
the same length, re-encodes the chunk and recomputes both CRCs, so the
recordings still frame and decode. The signature no longer verifies with
Bedrock, which no test needs.

## Errors

Every error was a JSON body with the exception named only in
`x-amzn-errortype`, as `<Name>:http://internal.amazon.com/coral/<namespace>/`:

| Case | Status | `x-amzn-errortype` | Body |
|---|---|---|---|
| `bad-model` (an unknown id) | 400 | `ValidationException` | `{"message":"The provided model identifier is invalid."}` |
| `validation` (`max_tokens: 0`) | 400 | `ValidationException` | `{"message":"stream cannot be true when max_tokens is 0"}` |
| `bad-signature` (a wrong secret key) | 403 | `InvalidSignatureException` | the expected canonical request and string to sign |
| `bad-key-id` (an unknown access key id) | 403 | `UnrecognizedClientException` | `{"message":"The security token included in the request is invalid."}` |
| `bad-bearer` (a malformed API key) | 403 | `AccessDeniedException` | `{"Message":"Invalid API Key format: Delimiter ':' not found"}` |

`bad-signature`'s body shows the canonical request AWS expected: the path's
`%3A` encoded again as `%253A`, as SigV4 does for every service but S3.

Not reached: an exception inside a stream, a throttling or quota exception,
and a Bedrock API key that works. Fiber's own sessions, run against the same
endpoint with the `bedrock` package, are in the pull request that added it.
