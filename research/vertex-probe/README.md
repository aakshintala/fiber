# Google Vertex, probed

Live probe for Claude and Gemini on Google Vertex
([#222](https://github.com/aakshintala/fiber/issues/222), carried by
[#2766](https://github.com/aakshintala/fiber/issues/2766)). Run on October 20,
2026 from macOS, against `https://aiplatform.googleapis.com` and the regional
hosts named below, with one Google Cloud project billed per token, written
`example-project` in every saved file.

## Method

`PROBE_PROJECT=<project> python3 -I -B probe.py <case>...` sends each request
itself (standard library only) with the token `gcloud auth print-access-token`
prints, and saves:

- `raw/<case>.sse`: a 200 stream's bytes, scrubbed (below);
- `raw/<case>.json`: the request's path and body, the status, the response's
  header names with the values of `content-type`, `retry-after` and
  `x-should-retry` only, and either the decoded events with their usage and
  cost, or the error body;
- `raw/spent.json`: the ledger, each case's cost from its own `usage`.

The token is never printed or saved, and request headers are never saved.
Before a file is written the project id becomes `example-project`, the
project number is zeroed, the gcloud account becomes `person@example.com`,
and each message, tool-use and response id is replaced by a stand-in of the
same length. A thinking signature is decoded and checked for those values
too: no Gemini `thoughtSignature` held any, so each is saved as it came.
`probe.py rescrub` scrubs the saved files again and checks none holds a
secret. The run stops before its spend passes $0.12.

Free calls came first: `count-tokens:rawPredict` (Claude) and `countTokens`
(Gemini) cost nothing, and answer 200 for every id tried
(`raw/discover.json`), so they show which ids Vertex knows, not which a
project can call.

## Ledger

Prices are Google's Vertex AI pricing page, standard tier at `global`, US
dollars per million tokens (October 20, 2026): Gemini 3.1 Flash-Lite $0.25
input, $1.50 output (response and reasoning), $0.025 cached input; 3 Flash
Preview $0.50 and $3.00; 3.1 Pro Preview $2.00 and $12.00; 3.5 Flash $1.50
and $9.00; 3.5 Flash-Lite $0.30 and $2.50; 3.7 and 3.8 Flash $0.75 and
$3.75 (through December 31, 2026); 4 Argon $2.00 and $10.00 (through
November 30, 2026). Gemini 2.5 Flash-Lite, which the page no longer lists, is
priced at models.dev's $0.10 and $0.40. Thinking tokens bill as output. A
regional ("non-global") endpoint costs 10% more. Every failed request cost
nothing.

| Case | Status | Input | Output | Thinking | Cost |
|---|---|---:|---:|---:|---:|
| `gemini-sysrole-user`, `-model`, `-none` | 200 each | 17 each | 3 each | | $0.000027 |
| `gemini-tool` | 200 | 58 | 26 | | $0.000053 |
| `gemini-replay-signed`, `-id-odd`, `-id-long65`, `-id-mismatch` | 200 each | 86 each | 13, 13, 13, 8 | | $0.000157 |
| `gemini-imgparts-call`, `gemini-imgparts` (twice: the first PNG was malformed and is not kept) | 200 | 36, 1148 | 10, 2 | | $0.000627 |
| `gemini-thinking`, `gemini-thinking-replay` | 200 | 23, 44 | 15, 15 | 328, 100 | $0.000703 |
| `gemini-models-*`, one tool call per Gemini 3 and later model | 200 each | 27 to 58 | 13 to 26 | 0 to 160 | $0.005484 |
| `gemini-argon-tool` (saved as `gemini-models-gemini-4-argon.sse`; its JSON was replaced by the refusal that followed) | 200 | | | | $0.000624 |
| `gemini-search-no-flag`, `gemini-search` | 200 | 64, 64 | 38, 31 | | $0.000136 |
| `gemini2-tool`, `gemini2-replay-id` | 200 | 27, 47 | 22, 17 | | $0.000024 |
| `gemini2-imgparts-call`, `gemini2-imgparts`, `gemini2-image-user-part` | 200 | 28, 298, 272 | 10, 6, 2 | | $0.000074 |
| `gemini2-regional` (us-central1, +10%) | 200 | 11 | 1 | | $0.000002 |
| `claude-access`: 17 one-token requests | 404, 403 | | | | $0 |
| Every other case | 400, 401, 403, 404, 429 | | | | $0 |
| **Probe** | | | | | **$0.007911** |
| Fiber, `fiber ask` "Reply with the single word: hello" | 200 | 3,188 | 1 | | $0.000799 |
| Fiber, a `gemini-3.1-flash-lite` tool turn stopped by hand: the model repeated one invalid `shell` call through 7 model and 7 reviewer calls | 200 each | | | | $0.008957 |
| Fiber, `gemini-3-flash-preview` reading `notes.md` with `read`, then answering | 200, 200 | 3,197, 3,230 | 28, 3 | | $0.003306 |
| **Total** | | | | | **$0.020973** |

## Gemini

`POST <base>/models/<id>:streamGenerateContent?alt=sse`, where `<base>` is
`https://aiplatform.googleapis.com/v1/projects/<project>/locations/global/publishers/google`,
is the Gemini API's own path after the base URL, so `google-generative-ai`
builds it unchanged. The stream is the Gemini API's: `data:` events of
`GenerateContentResponse`, the last carrying `finishReason`, `usageMetadata`
and `responseId`; Vertex adds `modelVersion`, `createTime` and
`usageMetadata.trafficType`. Every request carried
`authorization: Bearer <token>`.

The Gemini API facts in `docs/model-routing.md`, rechecked on
`gemini-3.1-flash-lite`:

- `systemInstruction.role` set to `user`, `model` or left out: each 200, and
  each answered PINEAPPLE as instructed (`raw/gemini-sysrole-*`).
- `parametersJsonSchema` with `$defs`, `$ref`, `anyOf` and
  `additionalProperties`, under function-calling mode `VALIDATED`: 200, and a
  call whose `unit` met the `$ref`'d enum (`raw/gemini-tool.json`). The same
  schema in `parameters`: 400, `Unknown name "$defs"` and `Unknown name
  "$ref"` (`raw/gemini-schema-parameters-ref.json`).
- A replayed `functionCall` without its `thoughtSignature`: 400, "Function
  call is missing a thought_signature" (`raw/gemini-replay-unsigned.json`).
  With it: 200. An id outside `[a-zA-Z0-9_-]`, a 65-character id, and a
  `functionResponse` id that differs from the call's: 200 each.
- Function calls carried an id, such as `call_275871`, on every model but
  `gemini-3.1-pro-preview-customtools`, whose one call had none
  (`raw/gemini-models.json`). Each call's part carried a signature.
- An image in `functionResponse.parts`: read. The model named a blue 16x16
  PNG "Blue." (`raw/gemini-imgparts.json`).
- `thinkingLevel: MEDIUM` with `includeThoughts`: a `thought` part, the
  answer's text parts, then an empty text part carrying the signature
  (`raw/gemini-thinking.sse`); the reply replayed as received was accepted.

Where Vertex differs from the Gemini API:

- An API key does not authenticate. `x-goog-api-key` alone answered 401
  `UNAUTHENTICATED`, "API keys are not supported by this API"
  (`raw/gemini-api-key-header.json`).
- Function tools beside the hosted `google_search` were accepted without
  `includeServerSideToolInvocations` (`raw/gemini-search-no-flag.json`),
  which the Gemini API refuses. With or without the flag the search arrived
  only as the last chunk's `groundingMetadata` (`webSearchQueries`,
  `groundingChunks`, `groundingSupports`, `searchEntryPoint`): no `toolCall`
  or `toolResponse` parts (`raw/gemini-search.sse`). One of the two requests
  with the flag ended after its first chunk, with no finish reason and no
  usage (`raw/gemini-search-cut.sse`); the repeat completed.
- Error bodies, a 429's included, arrived with `content-type:
  text/event-stream`.

Implicit caching works: in Fiber's own tool turn, a later call reported
3,167 of its 3,590 input tokens as `cachedContentTokenCount`.

### Gemini 2.x (#136's rows)

Vertex serves `gemini-2.5-flash-lite` to this project, at `global` and at
`us-central1`.

- Function-call ids: its call carried no `id` and no signature
  (`raw/gemini2-tool.json`). A replayed call and response with the made-up id
  `call abc/123!` were accepted (`raw/gemini2-replay-id.json`).
- Images in `functionResponse.parts`: not read. With a blue PNG there, it
  answered "The image is not available." (`raw/gemini2-imgparts.json`); the
  same PNG as a user part, "Blue." (`raw/gemini2-image-user-part.json`).
- `thinkingConfig.includeThoughts`, which Fiber sends on every request,
  answered 400 "include_thoughts is only enabled when thinking is enabled"
  with no thinking level set (`raw/gemini2-thoughts.json`).

### Models

Every Gemini 3 and later id models.dev lists under `google-vertex` answered
one tool call at `global` with a `functionCall` (`raw/gemini-models.json`):
`gemini-3-flash-preview`, `gemini-3.1-flash-lite`, `gemini-3.1-pro-preview`,
`gemini-3.1-pro-preview-customtools`, `gemini-3.5-flash`,
`gemini-3.5-flash-lite`, `gemini-3.7-flash`, `gemini-3.8-flash` (after one 429,
`raw/gemini-429.json`) and `gemini-4-argon`. `gemini-4-argon` then refused
every request, the same tool call included, with 403 `PERMISSION_DENIED`,
"Request rejected in accordance with our terms of use and policies."
(`raw/gemini-argon-refused.json`, `raw/gemini-argon-*.json`).

## Claude

No Claude model answered on this project. `streamRawPredict` answered 404
`NOT_FOUND`, "Publisher model `projects/<project>/locations/global/publishers/anthropic/models/<id>`
was not found or your project does not have access to it", for all 13 ids
from `claude-haiku-4-5@20251001` to `claude-opus-5-5`, and at `us-east5` and
`europe-west1` for Haiku 4.5. `claude-fable-5` and `claude-fable-5-1`
answered 403 `PERMISSION_DENIED`, "Access to this model requires data sharing
to be enabled for publisher 'anthropic'" (`raw/claude-access.json`,
`raw/claude-data-sharing.json`). Calling one needs the model enabled in
Model Garden for the project, which this probe did not do. So the stream,
`cache_control`, tool calls and thinking with signature replay on Vertex
are unmeasured, and `anthropic_version: vertex-2023-10-16` in the body with
no `model` rests on Google's and Anthropic's documentation.

Every `streamRawPredict` failure, Google's own or not, came as a JSON list
holding one error object, `[{"error": {"code": ..., "message": ...,
"status": ...}}]`, with `content-type: application/json`.

## Errors

| Case | Status | Body |
|---|---|---|
| `gemini-unknown-model`, `claude-unknown-model` | 404 | `NOT_FOUND`, "Publisher model `projects/.../models/<id>` was not found or your project does not have access to it. ..." |
| `gemini-bad-request` (a role `robot`) | 400 | `INVALID_ARGUMENT`, "Please use a valid role: user, model." |
| `gemini-bad-token`, `claude-bad-token` (a garbage bearer) | 401 | `UNAUTHENTICATED`, "Request had invalid authentication credentials. ...", `ErrorInfo` reason `ACCESS_TOKEN_TYPE_UNSUPPORTED` |
| `claude-no-auth` (no `authorization`) | 401 | `UNAUTHENTICATED`, "Request is missing required authentication credential. ...", reason `CREDENTIALS_MISSING` |
| `claude-expired-token`, `gemini-expired-token` (a token past its expiry) | 401 | the same body and reason as a garbage bearer, `ACCESS_TOKEN_TYPE_UNSUPPORTED` |
| `gemini-api-key-header` | 401 | `UNAUTHENTICATED`, "API keys are not supported by this API. ...", reason `CREDENTIALS_MISSING` |
| `gemini-429` | 429 | `RESOURCE_EXHAUSTED`, "Resource exhausted. Please try again later. ..." |
| `claude-data-sharing` | 403 | `PERMISSION_DENIED`, "Access to this model requires data sharing ..." |
| `gemini-argon-refused` | 403 | `PERMISSION_DENIED`, "Request rejected in accordance with our terms of use and policies." |
| `claude-bad-location` (location `nowhere1`) | 404 | Google's HTML "Error 404 (Not Found)!!1" page |

No failure carried `retry-after`, `x-should-retry` or a `RetryInfo` detail,
the 429 included. A quota or billing error was not reached.

## Locations

`global` is served from `aiplatform.googleapis.com`; a region from
`<region>-aiplatform.googleapis.com`. `global-aiplatform.googleapis.com`
resolves but answered 404 for `locations/global`
(`raw/discover.json`). The global host answered a Gemini `countTokens` for
`locations/us-central1`, and 501 `UNIMPLEMENTED` for a Claude one at
`us-east5`.

`gemini-3.1-flash-lite` answered 404 at `us-central1` and `asia-south1`
(`raw/gemini-regional.json`): Gemini 3 is served at `global` only, while
`gemini-2.5-flash-lite` answered at `us-central1`
(`raw/gemini2-regional.json`). Claude's `count-tokens` answered at `us-east5`
and `europe-west1`, and 404 at `asia-south1`.

## The token

`gcloud auth print-access-token` prints a bare token, 261 bytes here, with
no expiry. `gcloud config config-helper --format='json(credential.access_token,credential.token_expiry)'`
prints the same account's token with its expiry, `2026-10-20T17:41:19Z`, and
no other field. `gcloud auth application-default print-access-token
--format='json(token,expiry.datetime)'` prints the Application Default
Credentials' token with an expiry that has no zone, `2026-10-20
18:00:51.894905`, which is UTC; it mints a new token on each run. Without
`--format` that command's JSON also holds the refresh token and client
secret. gcloud's own cache refreshes a token within 300 seconds of its
expiry (`_CREDENTIALS_EXPIRY_WINDOW` in the SDK's
`googlecloudsdk/core/credentials/store.py`), the same window Fiber uses.

With no account logged in, `config-helper` exits 1 with "ERROR:
(gcloud.config.config-helper) You do not currently have an active account
selected."; a configuration that does not exist adds a `WARNING:` line
naming its file under the home directory first. An account that is selected
but has no credentials (`gcloud config set account` in an empty
`CLOUDSDK_CONFIG`) exits 1 with "Your current active account [<account>]
does not have any valid credentials", naming the account. A refresh that
fails says "There was a problem refreshing your current auth tokens: ...",
or "... auth tokens for account <account>: ...", and one that needs
reauthentication "There was a problem reauthenticating while refreshing your
current auth tokens: ..." (the SDK's `TokenRefreshError` and
`TokenRefreshReauthError` in `googlecloudsdk/core/credentials/exceptions.py`;
neither was reached here).
