# Model routing

How Fiber reaches a model, and how one is chosen. Vocabulary is `GLOSSARY.md`.
The provider seam is `docs/architecture.md`, and the extension runtime is
`docs/extensions.md`. The reasoning behind the protocol and provider split is
[ADR 0007](adr/0007-protocols-are-native-providers-are-extensions.md).
Each protocol's wire facts, read from reference implementations, are in
[research/provider-harvest](../research/provider-harvest/README.md).

## Protocols and providers

A protocol is a wire format: how a request is shaped, and how a streamed reply
is parsed into actions. A provider is an endpoint that speaks one or more
protocols: a name, a credential, base URLs and a list of models.

Protocols are native Rust in the `provider` module. Five speak to vendors:

| Protocol | Used by |
|---|---|
| `anthropic-messages` | Anthropic, OpenCode, OpenRouter, Databricks, muse, AWS Bedrock (Claude), Google Vertex (Claude), Azure (Foundry Claude) |
| `openai-completions` | OpenCode, OpenRouter, Databricks, muse, Azure |
| `openai-responses` | OpenAI, ChatGPT/codex, OpenCode, Databricks, muse, Azure |
| `google-generative-ai` | the Gemini API, Google Vertex (Gemini) |
| `bedrock-converse` | AWS Bedrock (models other than Claude) |

A sixth, `scripted`, speaks to no vendor: it reads a script file
("The scripted provider").

AWS event-stream framing is a per-model flag, not a protocol. `anthropic-messages`
reads it for Claude on Bedrock, which Bedrock serves through its Invoke API: a
model whose data declares `compat.bedrock_invoke` ("Bedrock Invoke wire
facts"). `bedrock-converse` always reads it ("Bedrock Converse wire facts").

ChatGPT/codex speaks `openai-responses` with two compatibility flags its
extension declares on each model, `store: false` and
`cache_key_header: "session_id"`, and one provider header, `originator: fiber`.
It sends no `OpenAI-Beta` header. The account-id header comes from its
`credential()` ("Keys, tokens and OAuth"). Its streamed
events, tool calls and reasoning items are parsed as plain Responses. A stream
ends with `response.completed`: all 45 SSE streams sent to `gpt-6-luna`
that returned 200 did, and none contained `response.done`. Requests with the
`OpenAI-Beta` header omitted, tried with one request body, also returned 200.
With `gpt-6-luna` the endpoint accepted
`parallel_tool_calls`, `tool_choice` and `text.verbosity`. It rejected
`temperature` with status 400, and a `service_tier` of `flex` or `auto`.

`openai-responses` maps a failed reply whose `error.code` or `error.type` is
`usage_limit_reached` or `usage_not_included` to `quota_exceeded`, on any
endpoint and whatever the HTTP status, and never retries it. No other vendor
sends these codes, so the match needs no flag. When the body has `resets_at`
(Unix seconds), the milliseconds from the reply's `Date` header until then
become the error's `retry_after_ms`, and
the message names the reset time. Without a usable `Date`, no wait is set. `rate_limit_exceeded` stays `rate_limited`.
No usage-limit reply has been probed: the match rests on the reference
implementations ([research/codex-responses-probe](../research/codex-responses-probe/README.md),
"The usage-limit error body").

On OpenAI's own endpoint, with `gpt-6-luna`, `instructions` and a system
message in `input` gave the same answer and the same prompt cache for one
prompt, and switching between the two kept the cached prefix. For one tool
schema with an optional property, a function tool with no `strict` key was
accepted, and the response reported `strict: true` with the schema rewritten
(every property required, `additionalProperties: false`). The same schema with
an explicit `strict: true` was rejected with a 400. Details:
[research/openai-responses-probe](../research/openai-responses-probe/README.md).

Required nullable integer, string and boolean properties were accepted with
`strict: true` by OpenAI Responses, Chat Completions, Anthropic Messages,
OpenCode Go Chat Completions and OpenRouter Messages. Each sampled call sent
`null` for the arguments the prompt asked it to leave unset. The probe records
and its limits are in
[research/strict-nullable-probe](../research/strict-nullable-probe/README.md).

So Fiber sends `strict` on every tool, on every protocol that has strict mode,
and decides it per tool, except on `bedrock-converse`, which sends no
`strict`: Nova refuses the field whether it is `true` or `false` ("Bedrock
Converse wire facts"). It sends `true` only when the tool's schema already
fits the vendor's strict subset, and `false` otherwise. Fiber never rewrites a
schema to fit, and never moves keywords into the description. Fiber's built-in
tools are written to fit the strict subset, except a tool whose section in
`docs/tools.md` says it is sent with `strict: false`.

An extension cannot add a protocol. A vendor with a new wire format needs a
Fiber release.

Every provider is an extension, the first-party ones included, except the
built-in `scripted` provider ("The scripted provider"). Extensions are
installed, not built into the binary. Installing Fiber installs every
first-party provider, and a person may remove any of them. How extensions
arrive and stay current is `docs/extensions.md`.

A first-party provider is one Fiber can probe and re-record. There are eleven:

| Provider | Protocols | Credential |
|---|---|---|
| Anthropic | `anthropic-messages` | key |
| OpenAI | `openai-responses` | key |
| Gemini API | `google-generative-ai` | key |
| ChatGPT/codex | `openai-responses`, with its flags | subscription login |
| OpenRouter | `openai-completions`, with `compat.anthropic` for Claude | key |
| OpenCode: `opencode-go` and `opencode-zen` | `anthropic-messages`, `openai-completions`, `openai-responses`, `google-generative-ai` | key, one for both |
| Databricks | `anthropic-messages`, `openai-completions`, `openai-responses` | key |
| muse (the Meta Model API) | `anthropic-messages`, `openai-completions`, `openai-responses` | key |
| AWS Bedrock | `anthropic-messages` for Claude and `bedrock-converse` for other models, both with AWS framing | Bedrock API key, or SigV4 through `sign()` |
| Google Vertex | `anthropic-messages` for Claude, `google-generative-ai` for Gemini | token from `credential()` |
| Azure | `openai-completions`, `openai-responses`; `anthropic-messages` for Foundry Claude | `api-key` header, or token from `credential()` |

The protocols are the ones each provider serves and Fiber speaks for it. A
package's shipped models may use only some of them, and a model on another
one is added once it is probed. The OpenAI provider sends `store: false` on every request. Google Vertex's
flags and URLs are data, like any provider's. AWS Bedrock ships its Claude
models under a geography's inference profile, such as
`global.anthropic.claude-haiku-4-5-20251001-v1:0`, on `anthropic-messages`.
Its other models speak `bedrock-converse`: each model that answered a probe's
ConverseStream request with a tool config, under its on-demand id and every
geography's profile models.dev lists for it, such as `openai.gpt-oss-20b-1:0`
or `global.amazon.nova-2-lite-v1:0` ("Bedrock Converse wire facts"). Google Vertex ships its Gemini 3
and later models only, at the `global` location: no Claude model answered on
the project it was probed with ("Google Vertex wire facts"), so its Claude
models are added once one is probed.

AWS Bedrock's `credential()` returns a Bedrock API key, sent as
`authorization: Bearer`, or AWS credentials, with which its `sign()` signs each
request with SigV4 in place of that header. The session's credential label
is the AWS profile name. Under any label but `default` it runs `aws configure
export-credentials --format process --profile <label>`, so a profile, SSO or an
assumed role works without an AWS SDK. A person picks the profile as any
label is picked ("Which credential a session uses"), such as in the global
file:

```json
{ "providers": { "bedrock": { "credential": "my-profile" } } }
```

and switches it mid-session with `/credential <profile>`, which gets that
profile's own credentials. Under `default` it takes the first of:
a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`; AWS keys in
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, with `AWS_SESSION_TOKEN`
when set; and the AWS CLI's own default chain, which reads `AWS_PROFILE`.
Credentials with an `Expiration` are refreshed before it, as any token is
("Keys, tokens and OAuth"). With none of these, the call fails with
`credential_failed`, and the extension's own message, in `provider.message`
(`docs/errors.md`, "The shape"), says to set `AWS_BEARER_TOKEN_BEDROCK`
or name a profile. `fiber login bedrock` is not supported: it stores a key,
and a provider with `credential()` reads none ("Which credential a session
uses"), so a Bedrock API key comes from the environment only. The AWS region is the package's `region` setting,
the per-account host its base URL names, read from `AWS_REGION` when unset
("A per-account host"). Its data declares the AWS CLI files that
`credential()` draws secrets from, `$AWS_SHARED_CREDENTIALS_FILE` else
`~/.aws/credentials`, `~/.aws/sso/cache/` and `~/.aws/cli/cache/`, and the
credential deny refuses tool calls that touch them
([Credentials](permissions.md#credentials)). A profile's
`credential_process` in `AWS_CONFIG_FILE` is not covered.

Google Vertex's `credential()` returns an OAuth access token from the Google
Cloud CLI, sent as `authorization: Bearer`. The session's credential label
names a gcloud configuration. Under any label but `default` it runs
`gcloud config config-helper --configuration <label>`, which prints that
configuration's account's token and its expiry; a person picks the
configuration as any label is picked ("Which credential a session uses"),
such as in the global file:

```json
{ "providers": { "vertex": { "credential": "work" } } }
```

and switches it mid-session with `/credential <configuration>`. Under
`default` it takes the active configuration's token. Only when gcloud has no
account selected at all, saying "You do not currently have an active account
selected", does it take Application Default Credentials' token instead:
`gcloud auth application-default print-access-token`. Any other failure, such
as a login that needs reauthentication or a selected account with no
credentials, fails with `credential_failed` and says to run `gcloud auth
login`, so a session never switches to the account Application Default
Credentials hold. Each command's `--format` prints the token and its expiry
and no other field. The token is refreshed before its expiry, as any token is
("Keys, tokens and OAuth"). With no account selected and no Application
Default Credentials, the call fails with `credential_failed`, and the
extension's own message, in `provider.message`, says to run `gcloud auth
login` or `gcloud auth application-default login`,
or name a configuration. The failure keeps gcloud's own reason, with an
address shown as `[account]` and any run of 20 or more token-like characters
as `[redacted]`.
`fiber login vertex` is not supported: it stores a key, and a provider with
`credential()` reads none. The Google Cloud project is the package's
`project` setting, which its base URL names, read from
`GOOGLE_CLOUD_PROJECT` when unset ("A per-account host"). Its data declares
the gcloud credential files that `credential()` draws secrets from:
`credentials.db`, `access_tokens.db`, `legacy_credentials/` and
`application_default_credentials.json` in the gcloud configuration
directory, `$CLOUDSDK_CONFIG` else `~/.config/gcloud`; the credential deny
refuses tool calls that touch them
([Credentials](permissions.md#credentials)).

A provider Fiber cannot probe is left to the community: a vendor whose
subscription Fiber does not hold, or a key vendor such as Groq, Mistral,
Moonshot or DeepSeek. The first-party extensions and `docs/extensions.md`
("What writing a provider looks like") are the examples to start from.

### Anthropic messages wire facts

Measured on `claude-sonnet-5-5` against api.anthropic.com
(`research/anthropic-messages-probe`).

- In 7 successful streams (text, tool use, `max_tokens`, `stop_sequence`,
  adaptive thinking), `message_delta` carried the `stop_reason` and was followed
  by `message_stop`, the last event.
- `/v1/messages?beta=true` returned the same status, header names, body keys and
  event names as `/v1/messages`.
- Without `tools`, `tool_choice` of `auto` or `none` returned 200. `any` and
  `tool` returned 400.
- A `tool_use` block carries `caller`: `{"type": "direct"}` for a client tool
  call, and `{"type": "code_execution_20250825", "tool_id": "srvtoolu_..."}`
  when server-side code execution calls the tool. Replaying the block without
  `caller` returned 200. An invalid `caller` returned 400.
- More than 4 `cache_control` blocks across `tools`, `system` and `messages`
  returned 400.
- A reply stops with `pause_turn` when the server-side loop for a hosted tool
  such as web search reaches its iteration limit, 10 by default. The reply may
  end in a `server_tool_use` block with no result block. Anthropic's
  continuation: "append the assistant's response to your messages and make
  another API request", with the same `tools`
  (<https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons>).
  A `pause_turn` reply is never `tool_use`: a client tool call stops with
  `tool_use`. Fiber continues a paused reply as `docs/loop.md`, "A reply paused
  by a hosted tool", says.
- More than 20 tools with `strict: true` returned 400, "The maximum number of
  strict tools supported is 20" (measured October 1, 2026).
- `thinking: {type: "enabled", budget_tokens: 1024}` with `max_tokens: 2048`
  returned 200 on `claude-haiku-4-5` through OpenRouter's native Messages
  endpoint (`research/reasoning-resume`, September 27, 2026), while `enabled`
  on `claude-sonnet-5-5` returned 400 ("Use thinking.type.adaptive and
  output_config.effort").

### Google Generative AI wire facts

Measured against `gemini-3.1-flash-lite` on the Gemini API:

- The `x-goog-api-key` header and the `?key=` query parameter both authenticate.
- `systemInstruction.role` set to `user`, `model` or left out is accepted, and
  the instruction is obeyed in all three.
- `parametersJsonSchema` accepted a schema with `$ref`, `$defs` and `anyOf`.
  `parameters` rejected the same `$ref` with HTTP 400.
- A replayed `functionCall` part without a `thoughtSignature` fails with
  HTTP 400. In the calls sampled, the model put an `id` on each `functionCall`
  it emitted. Four replay cases were accepted: a conforming id, an id outside
  `[a-zA-Z0-9_-]{1,64}`, a 65-character id, and a `functionResponse` id that
  differs from the call's.
- A `functionResponse` may carry an image in `parts`, and the model read it.
- A request that declares function tools beside the hosted `google_search`
  is refused with HTTP 400 unless `toolConfig` sets
  `includeServerSideToolInvocations` to `true`. With the flag the search
  arrives as `toolCall` and `toolResponse` content parts, each signed with
  its own `thoughtSignature`, before the answer text; the candidate's
  `groundingMetadata` on the last chunk carries the result URLs. Measured
  October 9, 2026 on `gemini-3-flash-preview`.
- Function-calling mode `VALIDATED` returned schema-valid arguments where
  `AUTO` returned arguments that broke an enum and an integer type. In the
  sample it did not force a call.

Gemini accepted required nullable integer, string and boolean properties in
both `AUTO` and `VALIDATED` mode, returning nulls in both samples
([nullable probe](../research/strict-nullable-probe/README.md)).

Fiber sends mode `VALIDATED` only when every tool in the request fits the
strict subset. `VALIDATED` on `gemini-3-flash-preview` does not enforce
`maxLength`, so Gemini requests keep `AUTO`, because `ask_user` and the
session naming tool both declare it
(`research/gemini-validated-probe/README.md`).
Fiber's own argument check still validates every call (`docs/tools.md`,
"Before a call runs").

A `functionCall` that arrives without an `id` is logged with no `provider_id`
(`docs/events.md`, `tool_call_requested`). Fiber pairs the call with its result
by its action id, which stays local. Only an id the model emitted is sent back.

A reply's text fragments, `thought` parts and `functionCall` parts each carry
their own `thoughtSignature` when the model gave one. Unsigned text fragments
join into one part. A signed fragment is its own part. An empty signed
fragment's signature rides on the text before it. Fiber logs each part as its
own item, in the order the model sent it, and each part replays in that order
with its own signature (`docs/loop.md`, "What the model is sent"). A reply
that repeats the same text in two parts is replayed as two parts, each with
its own signature.

Because a replayed `functionCall` needs the signature its own model gave it, a
tool call and its result that another model reference made are sent to Gemini
as plain text, not as `functionCall` and `functionResponse` parts
(`docs/loop.md`, "What the model is sent").

The Gemini API answers HTTP 404 "no longer available to new users" for
`gemini-2.5-flash-lite`, `gemini-2.5-flash` and `gemini-2.5-pro` on a key created
in September 2026.

Fiber targets Gemini 3 and later on `google-generative-ai`. Google Vertex
serves `gemini-2.5-flash-lite`, and there it emitted no id on a call, took a
made-up id on replay, and did not read an image in `functionResponse.parts`.
It also refuses the `includeThoughts` Fiber sends on every request while no
thinking level is set ("Google Vertex wire facts").

### Google Vertex wire facts

Measured on October 20, 2026 against `aiplatform.googleapis.com`, mostly with
`gemini-3.1-flash-lite`, and one tool call to each Gemini 3 and later model
(`research/vertex-probe`).

- Gemini's base URL is
  `https://aiplatform.googleapis.com/v1/projects/<project>/locations/global/publishers/google`,
  and `google-generative-ai` appends the Gemini API's own
  `/models/<id>:streamGenerateContent?alt=sse`. The stream is the Gemini
  API's, with `modelVersion`, `createTime` and `usageMetadata.trafficType`
  added. The token goes in `authorization: Bearer`; `x-goog-api-key` answered
  401 "API keys are not supported by this API".
- Every Gemini API fact above held for `gemini-3.1-flash-lite`:
  `systemInstruction.role` `user`, `model` or left out was obeyed;
  `parametersJsonSchema` took `$defs`, `$ref` and `anyOf` under `VALIDATED`,
  and `parameters` refused them with 400; a replayed `functionCall` without
  its `thoughtSignature` failed with 400, and the three odd ids were
  accepted; an image in `functionResponse.parts` was read. Calls carried an
  id on every model but `gemini-3.1-pro-preview-customtools`, whose call had
  none. A `thinkingLevel` streamed a `thought` part, the text, then an empty
  part carrying the signature.
- Function tools beside the hosted `google_search` were accepted without
  `includeServerSideToolInvocations`. With or without it the search came only
  as the last chunk's `groundingMetadata`, never as `toolCall` and
  `toolResponse` parts, so no Vertex model declares `web_search`. One of two
  requests with the flag ended after its first chunk with no finish reason.
- Implicit caching reports reads: a session's later call counted 3,167 of its
  3,590 input tokens as `cachedContentTokenCount`.
- `global` is served from `aiplatform.googleapis.com` and a region from
  `<region>-aiplatform.googleapis.com`; `global-aiplatform.googleapis.com`
  answered 404. `gemini-3.1-flash-lite` answered 404 at `us-central1` and
  `asia-south1`: Gemini 3 is served at `global` only, so every Vertex model's
  base URL names `global`.
- Claude's base URL is the same with `publishers/anthropic`. A model that
  declares `compat.vertex_raw_predict` posts the `anthropic-messages` body to
  `<base_url>/models/<id>:streamRawPredict`, with
  `anthropic_version: vertex-2023-10-16` in place of `model`. No Claude model
  answered on the probe's project, so this rests on Google's and Anthropic's
  documentation: each answered 404 "not found or your project does not have
  access to it", and `claude-fable-5` 403 "requires data sharing to be
  enabled for publisher 'anthropic'".
- Every `streamRawPredict` failure came as a list holding one error object,
  `[{"error": {...}}]`. Under `vertex_raw_predict` the object inside is read,
  as any provider's error body is (`docs/errors.md`).
- An unknown model, or one the project cannot call, answered 404
  `NOT_FOUND` with "Publisher model `projects/.../models/<id>` was not found";
  a garbage token and an expired one the same 401 `UNAUTHENTICATED`, reason
  `ACCESS_TOKEN_TYPE_UNSUPPORTED`; a 429 `RESOURCE_EXHAUSTED` "Resource
  exhausted. Please try again later." with no `Retry-After` and no
  `RetryInfo`. `gemini-4-argon` answered one call and then refused every
  request with 403 "Request rejected in accordance with our terms of use and
  policies", so it is not shipped. Gemini's error bodies arrived as
  `content-type: text/event-stream`.

### OpenCode wire facts

Measured on October 1, 2026 with one OpenCode key (`research/opencode-probe`).

- The `opencode` extension declares two providers, `opencode-go` and
  `opencode-zen`, because Go and Zen serve many of the same model ids and bill
  them differently: `opencode-go/gpt-6-luna` and `opencode-zen/gpt-6-luna` are
  two models. The same key works on both, and both providers read it from
  `OPENCODE_API_KEY`. Both providers name one stored credential
  ("Credentials"), so one stored key serves both.
- `opencode-go` is a subscription at `https://opencode.ai/zen/go`, and its
  models declare `"subscription": true` beside their prices ("Cost").
  `opencode-zen` is billed per token at `https://opencode.ai/zen`, and its
  models carry prices only.
- Both serve `openai-responses` at `/v1/responses`, `openai-completions` at
  `/v1/chat/completions` and `anthropic-messages` at `/v1/messages`. Zen also serves
  `google-generative-ai` at `/v1/models/<model>:generateContent`: one request
  each to `gemini-3.5-flash-lite` and `gemini-3.8-flash` answered 200
  (`research/opencode-zen-gemini-probe`).
- A model speaks one protocol. `muse-spark-1.3-contributor` on Go answered on
  `/v1/responses`, and on the other two it answered 400 with
  `ModelProtocolUnsupported`. Each model declares its protocol.
- Go and Zen list different models: `GET /zen/go/v1/models` listed 30 and
  `GET /zen/v1/models` listed 18, with ids only, no prices or protocols. A Go
  model sent to Zen's URL answered 400 "Model is unavailable".
- No response carries a cost, and nothing marks a call as subscription. Go's
  spend shows only as quota (`docs/tools.md`, "Provider quota").
- Requests carry `x-opencode-session` (`docs/prompt-cache.md`, "Cache
  markers and keys"). Go refuses a request without it: HTTP 400 "Request is
  missing x-opencode-session and cannot be routed efficiently", measured on
  October 1, 2026. Cloudflare answers 403 "error code: 1010" to Python's
  default `User-Agent`, so the probe sent its own.
- Go sends `store: false` on its `openai-responses` and
  `openai-completions` requests, as the OpenAI provider does on every
  request; its `anthropic-messages` requests carry no `store` key.

### Bedrock Invoke wire facts

Measured on October 20, 2026 against `bedrock-runtime.ap-south-1.amazonaws.com`
with `global.anthropic.claude-haiku-4-5-20251001-v1:0`, and one request to
`global.anthropic.claude-haiku-5-5` (`research/bedrock-invoke-probe`).

- A model with `compat.bedrock_invoke` posts to
  `<base_url>/model/<id>/invoke-with-response-stream`, with the id
  percent-encoded (`:` as `%3A`) and `accept: application/vnd.amazon.eventstream`.
  The body is the `anthropic-messages` body with no `model` and no `stream`,
  and `anthropic_version: bedrock-2023-05-31` where Anthropic takes the
  `anthropic-version` header. A model's beta flags go in the body's
  `anthropic_beta` list, through its `extra_body`; Fiber sends no
  `anthropic-beta` header.
- The reply is AWS event-stream framing. Each message is a prelude (its total
  length and its headers' length, big-endian 32-bit, then the prelude's
  CRC-32), string headers, a payload and the CRC-32 of the rest of the
  message. Each `:event-type` `chunk` message's JSON payload holds `bytes`,
  one Anthropic stream event in base64, beside a `p` field of padding. The
  events are Anthropic's own, `message_start` to `message_stop`, and
  `message_stop` also carries `amazon-bedrock-invocationMetrics`. No stream
  probed sent `ping`. A message whose checksum fails, or whose lengths are out
  of range, fails the call with `stream_incomplete`, since nothing after it
  can be trusted.
- `message_start` splits cache writes by lifetime in `cache_creation`, as
  Anthropic's does. Prompt caching wrote and read with a 5-minute marker, a
  1-hour `ttl` was accepted, and Fiber's own sessions recorded 1-hour writes
  and later reads, a resumed turn's included.
- `thinking: {type: "enabled", budget_tokens}` streamed a signed thinking
  block on Haiku 4.5, and a signed thinking block sent back in the next
  request was accepted. Adaptive thinking with `output_config.effort` was
  accepted on Haiku 5.5.
- A thinking block's signature carries the AWS account id in clear inside its
  base64, so a recording's signatures are scrubbed before it is checked in.
- A failure before the stream is a JSON body, `{"message": ...}`, or
  `{"Message": ...}` for a malformed API key, and names its exception only in
  the `X-Amzn-ErrorType` header, as
  `ValidationException:http://internal.amazon.com/coral/com.amazon.bedrock/`
  (`docs/errors.md`, "Recognising an AWS exception"). An unknown model id, and
  a geography's profile used from a region outside it, answered 400
  `ValidationException` "The provided model identifier is invalid.".
- A failure during the stream is a message whose `:message-type` is
  `exception`, naming itself in camel case in `:exception-type`, such as
  `throttlingException`, with its text in the payload's `message`. The probe
  reached none.

### Bedrock Converse wire facts

Measured on October 20, 2026 against `bedrock-runtime.ap-south-1.amazonaws.com`
with gpt-oss 20B and 120B, Ministral 3 8B, Qwen3 32B and VL, DeepSeek V3.2,
Nova Micro, Lite, Pro and 2 Lite and Grok 4.7, and with Claude Haiku 4.5 for a
signed reasoning block (`research/bedrock-converse-probe`).

- A request posts to `<base_url>/model/<id>/converse-stream`, the id
  percent-encoded as on Invoke, with `accept:
  application/vnd.amazon.eventstream`. A Bedrock API key goes in
  `authorization: Bearer`, and `sign()` replaces it with SigV4 ("Protocols
  and providers"). The body holds `system`, one text block, left out when the
  system prompt is empty; `messages`; `inferenceConfig.maxTokens`, the output
  limit (`docs/errors.md`, "Output tokens"); `toolConfig` when the request has
  tools; and `additionalModelRequestFields` for a thinking level
  ("Thinking").
- Consecutive inputs of one role fold into one `user` or `assistant` message,
  as on `anthropic-messages`. A person's text is a `text` block, and each
  image an `image` block, `{format, source: {bytes}}`, after it. A tool call
  is `toolUse` with `toolUseId` (the provider's id when it ends in nine
  letters and digits, else the action id),
  `name` and `input`. A result is `toolResult` with that `toolUseId` and
  `content: [{text}]`, plus `status: "error"` when the call failed: AWS
  documents the field for Nova and Claude only, and five other models
  accepted it. A result's images, and a PDF's pages rendered as images,
  follow it as `image` blocks in the same user message: Ministral 3 8B and
  Qwen3 VL refused an image inside `toolResult`, and took it beside.
- Each tool is `{toolSpec: {name, description, inputSchema: {json}}}`, in
  name order, with no `description` when it is empty, as AWS documents one of
  at least one character, and no `strict`: Nova Micro refused the field, `true` or
  `false`, where gpt-oss and Ministral took both. `toolChoice` is left out
  for `auto`, Converse's default, and for `none`, which Converse lacks; `any`
  sends `{any: {}}`, and a tool's name `{tool: {name}}`.
- A request whose messages hold a `toolUse` or `toolResult` block and that
  has no `toolConfig` answered 400. Fiber's requests without tools hold
  neither: the reviewer, a note request and an extension's `host.model` call
  send text.
- A Mistral model reads the last nine characters of a replayed `toolUseId`,
  and refuses them unless they are letters and digits. An id Bedrock issued
  passes, and so does an action id; another provider's id, such as
  `functions.read:0`, may not, so Fiber sends the action id in its place.
- The reply is AWS event-stream framing. Each message's `:event-type` names
  its event and its JSON payload is the event, beside a `p` field of padding:
  `messageStart`; per content block an optional `contentBlockStart`, its
  `contentBlockDelta`s and `contentBlockStop`, each with
  `contentBlockIndex`; `messageStop` with `stopReason`; then `metadata` with
  `usage`, the last event of every stream probed. A stream that ends before
  `metadata` fails with `stream_incomplete`. Only a tool call's block has a
  start, carrying its id and name; its input arrives as JSON text in
  `delta.toolUse.input`. A text or reasoning block opens with its first
  delta.
- Reasoning arrives in `delta.reasoningContent` as `text`, a `signature`
  (Claude only), or `redactedContent` (Grok). Fiber logs it in the form
  Converse takes back, `reasoningContent` holding `reasoningText` with its
  text and signature, or `redactedContent`, and sends it back unchanged to
  the model that produced it (`docs/loop.md`, "What the model is sent"). All
  three forms were accepted back. Nova 2 Lite's text is the literal
  `[REDACTED]`. A Claude signature carries the AWS account id inside its
  base64, as on Invoke.
- `stopReason` `end_turn`, `tool_use` and `stop_sequence` complete the reply,
  and `max_tokens` cuts it off at the output limit. `guardrail_intervened` and
  `content_filtered` fail the call with `refused`,
  `model_context_window_exceeded` with `context_overflow`, and
  `malformed_model_output` and `malformed_tool_use` with `stream_incomplete`,
  so the retry asks again. The probe reached `end_turn`, `tool_use` and
  `max_tokens`.
- `usage.inputTokens` excludes cache reads and writes: Nova Micro reported 4
  input beside 2,121 read. `cacheReadInputTokens` is the call's `cache_read`,
  and `cacheWriteInputTokens` a write under `"5m"`, the lifetime of a
  `cachePoint` sent without `ttl` (`docs/prompt-cache.md`, "Cache markers and
  keys"). No reply carried the `cacheDetails` AWS documents.
- Converse names no generation in its stream. Fiber takes the response's
  `x-amzn-RequestId` header, AWS's id for the call, as the generation id, so
  the model's declared prices apply ("Cost"). A 200 stream and a 400 error
  both carried one, a UUID. A response without it, or with an empty one,
  leaves the generation unnamed: Fiber mints its id, and the call is
  unpriced.
- A failure before the stream names its exception in `X-Amzn-ErrorType`, and
  one during it is an `exception` message, as on Invoke (`docs/errors.md`,
  "Recognising an AWS exception"). An unknown model id answered 400
  `ValidationException` "The provided model identifier is invalid.", and a
  model the account may not use 403 `AccessDeniedException`.

### The scripted provider

`scripted` is the one provider built into the binary, and the only one that
reaches no network. Its protocol of the same name reads a script file instead
of a socket. A script is a list of steps, each one model reply: text, a tool
call with its arguments, a given error such as a rate limit, or a reply
streamed slowly. Each request takes the next step, in order. A request is
never matched against a step, so a prompt that changes does not change the
reply. A request after the last step fails the turn. It takes the session's tools in name order, each as Fiber's own definition serialised to a JSON object, with no vendor wrapping and no `strict` flag.

A script is a JSON file, `{"steps": [...]}`, read whole each time a model on
it is connected: when a session starts or resumes on it, switches back to it,
resolves its reviewer to it, or resolves a delegate to it through a configured
`roles.*` reference (a string or an object's `model`). Each step is an object,
a reply or an error:

- A reply has `text`, a string or a list of strings streamed as fragments in
  order, or `tool_calls`, a list of `{"name": ..., "arguments": {...}}`, or
  both. It may add `reasoning`, a string streamed before the text; `usage`,
  the `input`, `output` and `cache_read` tokens it reports, each 0 when
  absent; and `every_ms`, a pause on the session's clock before every text
  fragment after the first. Its tool calls' ids are `call_<step>_<n>`, both
  counted from 1. It names no generation and reports no cost.
- An error is `{"error": {"code": ..., "message": ..., "retry_after_ms": ...}}`,
  alone in its step. `code` is one a failed model call has (`docs/errors.md`,
  "A failed model call"), and `retry_after_ms` may be left out.

```json
{"steps": [
  {"text": ["Reading ", "the file."], "tool_calls": [{"name": "read", "arguments": {"path": "notes.md"}}]},
  {"error": {"code": "rate_limited", "message": "Slow down.", "retry_after_ms": 2000}},
  {"text": "Done.", "every_ms": 200, "usage": {"input": 120, "output": 4}}
]}
```

Each request takes one step, a retried request included, so a `rate_limited`
step followed by a text step is a retry that succeeds. A request after the
last step fails `invalid_request`, which is never retried, naming the script
and the request. Each connection starts at step 1, and a reviewer or delegate
on a script has its own steps. A script that cannot be read fails with `io_failed`, and a
malformed one with `config_invalid` naming the file and the step: at startup
before any line is written, on a switch as its rejection, and for a reviewer
as a reviewer failure.

A session selects it with an ordinary model reference, `scripted/<path>`,
where the model id is the script's path, resolved against the session's
workspace. It needs no credential and has no quota, cost or prompt cache. The
model picker does not offer it, and it is never chosen unless named. A session
switches only to a scripted model it started with: `/model` naming another
`scripted/<path>` is rejected with `invalid_arguments`.

It exists for testing extensions, reproducing a bug from a script anyone can
replay, and running a pipeline end to end without a model. It bypasses the
vendor decoders, so it proves nothing about a vendor's wire format; recorded
and scripted streams on the fake server prove that (`docs/testing.md`, "Model
calls").

## What a provider extension declares

Most of a provider is data. For the provider:

- its name, which is the first half of every model reference
- how its credential is found (see [Credentials](#credentials)), or a
  `credential()` function that returns a token
- `login`, when `fiber login` runs that function as a login instead of
  reading a key: `browser`, whose device-code form takes `--device`
- `credential_label`, when that function supplies the credential and reads
  nothing Fiber stores, as AWS Bedrock's and Google Vertex's do: `names`,
  what a credential label names, such as `AWS profile`, `default`, what
  the `default` label reads, and `credential_paths`, the paths it draws
  secrets from, which the credential deny protects (`docs/permissions.md`,
  "Credentials"). Such a provider has no key for `fiber login` to
  store, so `fiber doctor` shows its label as that source, unchecked
  (`docs/invocation.md`, "Commands and flags"), and no sentence advises
  `fiber login` (`docs/errors.md`, "The shape"). It is never declared beside
  `login` or a `credential` key source
- headers sent on every request
- a `sign()` function, if every request must carry a signature
- `reviewer_model`, optional: the package default `fiber login` preselects
  as the provider's reviewer, and the final fallback when no reviewer is
  configured (`docs/permissions.md`, "How it runs"). A
  first-party package names the model its vendor's own agent reviews with,
  at the current generation: `claude-sonnet-5-5`, `gpt-6-luna`; Google
  ships no reviewer, so `gemini` names its middle tier,
  `gemini-3.8-flash`. A first-party package never names a contributor
  model as `reviewer_model`, and a package whose models are all
  contributor models names none.

For each model:

- its id, as the vendor spells it
- `name`, optional: its display name, such as `Claude Opus 5.5`, which the
  model picker's filter matches beside the provider and id (`docs/tui.md`,
  "Swapped views"); a model with no name is matched on provider and id only
- its protocol and base URL, which can differ between models of one provider,
  and may name a per-account host ("A per-account host")
- compatibility flags the native protocol reads, such as whether the vendor
  accepts `store`, which field carries the token limit, and which thinking
  dialect it speaks
- the thinking levels it supports ("Thinking")
- whether deferred tools work for it, declared only after a probe
  (`docs/tools.md`, "Which tools the model sees")
- `web_search`: the vendor's own hosted-search tool type, exactly as it is
  sent, such as `web_search_20250305` or `google_search`; absent when the
  model's provider hosts no search for it ("Hosted web search")
- extra request body fields, which may not name a field Fiber builds
  itself ("Extra request body fields")
- `prompt_addendum`, naming a Markdown file in the extension package whose
  text is appended to the system prompt for this model only
  (`docs/system-prompt.md`, "The model's addendum"); a named file that is
  missing or outside the package fails the load, under the same check
  `prompt` gets
- context window, which every model declares: one without it is left out of
  the model list with the notice `model_invalid`, because handoff and the
  size notices are measured against it
- output token limit, input kinds and cost
- whether a subscription login covers it ("Cost")

A first-party package's `models` list is generated, except `openrouter`'s and `databricks`'s, which their `models()` functions read from their vendors ("Model discovery"): `openrouter`'s takes each model's `name` from OpenRouter's list, and `databricks`'s matches the workspace's model services to its catalog rows. `cargo xtask models-dev` reads models.dev (`https://models.dev/api.json`) and keeps each source's models that call tools and output text, leaving out Gemini models before Gemini 3 ("Google Generative AI wire facts") and any model with no context window. It derives each model's name, protocol, context window, output token limit, input kinds and cost from models.dev, and takes every other field, such as `base_url`, `compat`, `web_search`, `thinking_levels` and the provider's `credential` and `reviewer_model`, from a table it keeps per package. A gateway package's model (`opencode-go`, `opencode-zen`) whose id a vendor package also lists takes that vendor's `thinking` row unless its own table names one; every other gateway model names its own `thinking` row, with levels and a default, or an explicit empty row for a model that takes none. A `databricks` model found by discovery takes the `thinking` row of the vendor model its bare service name matches, looked up by the same match key discovery routes it with; a service the table names no row for keeps no levels. The table also lists the ids a package keeps when its models are a subset of the source's (`codex` keeps four of models.dev's `openai` models; `bedrock` keeps its Claude models' inference profiles and the other models that answered a probe through ConverseStream, "Protocols and providers"), names a model's protocol where models.dev cannot (`bedrock`'s Claude models speak `anthropic-messages`, its others `bedrock-converse`), and pins a derived field the vendor's endpoint differs on (`codex`'s `context_window` is the ChatGPT backend's 272000, where models.dev lists the API's 1050000). A rerun on an unchanged models.dev changes nothing. It runs only when someone runs it: CI never fetches, and a test regenerates the lists from a checked-in copy of models.dev and fails when a committed file differs.

Fiber never guesses a flag from a URL or a provider name. A flag the vendor
needs is declared, or it is not set.

### Extra request body fields

A model's `extra_body` adds fields to every request sent to it, after Fiber
has built the request. It may add a field or replace one such as
`max_tokens` or a thinking setting. It may not name a field that carries
what Fiber records, or that Fiber relies on to read the reply:

| Protocol | Fields `extra_body` may not name |
|---|---|
| `anthropic-messages` | `model`, `system`, `messages`, `tools`, `tool_choice`, `stream`, `cache_control` |
| `openai-completions` | `model`, `messages`, `tools`, `tool_choice`, `stream` |
| `openai-responses` | `model`, `instructions`, `input`, `tools`, `tool_choice`, `stream` |
| `google-generative-ai` | `systemInstruction`, `contents`, `tools`, `toolConfig` |
| `bedrock-converse` | `system`, `messages`, `toolConfig` |

`google-generative-ai` and `bedrock-converse` carry the model and streaming
in the URL, not the body. On `bedrock-converse`, `extra_body`'s
`inferenceConfig` and `additionalModelRequestFields` are laid over Fiber's own
key by key, so a model can add a field beside the output limit and the
thinking level; the output limit still caps a `maxTokens` it names. `cache_control` is reserved because cache markers
are Fiber's alone (`docs/prompt-cache.md`, "Cache markers and keys").

A model whose `extra_body` names one of these is left out of the model list,
with the notice `model_invalid` naming the model and the field. The same
applies to a model a provider's `models()` function returns. So the tools
and system prompt in the session log are always the ones sent, and the tool
set is fixed for a given Fiber build.

### Hosted web search

A model's `web_search` names the vendor's hosted-search tool type as the
vendor spells it. Each protocol accepts only the types its code reads back
(`docs/tools.md`, "Hosted by the provider"): `openai-completions` reads
`openrouter:web_search`. A model whose `web_search` names
any other type is left out of the model list, with the notice
`model_invalid` naming the model and the type, as for a forbidden
`extra_body` field.

### A per-account host

Some providers serve each account from its own host, such as a Databricks
workspace or an Azure resource, or from its own path, such as a Google Cloud
project on Vertex. A model's `base_url` names that part with a placeholder,
`{name}`:

```json
"base_url": "https://{workspace}/ai-gateway/anthropic/v1"
```

A placeholder in the path is filled and checked the same way, so its value
must have a host's shape, as a Google Cloud project id does.

The extension's manifest lists the same pattern under `providers`
(`docs/extensions.md`, "What a package holds"), so an install or an offer
shows where the host comes from. The value is one of the extension's own
settings, which only the person sets (`docs/configuration.md`, "Extension
settings"), globally or per project, with `fiber config set` or in
`config/<extension>.json`. The package may name an environment variable to
read when the setting is unset, such as `DATABRICKS_HOST`. A placeholder's
setting is never a `repo_settings` key, so a repository never sets it, as it
never changes a base URL ("Choosing the model").

Before a value fills its placeholder, Fiber strips one leading `https://`
and one trailing `/`, so `DATABRICKS_HOST` can carry
`https://adb-123.azuredatabricks.net/`. What remains must be a host: ASCII
letters, digits, `.` and `-`, with an optional `:` and a port from 0 to
65535. A setting that is not a host is not replaced by the environment
variable.

A model whose placeholder has no value, or a value that is not a host, is
left out of the model list, with the notice `model_unconfigured`. For a
value that is not a host the notice names the model and the setting and
says so, without repeating the value. For a placeholder with no value,
`fiber models` prints one notice per provider and setting, not one per
model: it names the provider and the setting and counts the models it
leaves out, then lists the models that are configured, possibly none. A
search filters the count as it filters the list: the count holds the
left-out models whose `provider/model` contains the text, and no notice
prints when none does. Every other surface keeps the notice per model,
naming the model: the terminal's model list, `fiber doctor`, a session that
chooses it, and `fiber config set model`. The model can still be named ("Naming a model"): a
session that chooses it exits with `model_unconfigured`, and
`fiber config set model` accepts it with that notice
(`docs/configuration.md`, "When Fiber writes"). The notice names three
fixes, login first: run `fiber login <provider>`, which asks for the host;
set the named setting in `config/<extension>.json` in Fiber home, for every
project at once; or, when it names an environment variable, export that
variable in the shell profile and run `fiber hub refresh`, since the hub
reads its environment when it starts. For a provider whose data declares
`credential_label` it names only the last two: its `fiber login` would store
a key the provider never reads ("What a provider extension declares").

`fiber login <provider>` asks for each missing value after the credential
is stored and before the reviewer, saving each answer globally. When a
project-layer empty hides a saved answer, the login names that file: the
answer holds for every other project and session.

#### What the login asks

One question per missing value, in declaration order: the setting, with
its declared environment variable in parentheses when one exists. An
answer that is not a host is refused and asked again; an empty line is
refused the same way. End of input ends the questions with nothing saved
for the remaining values. Without a terminal the login prints one line
per missing value instead, naming the setting with the config file that
fills it, and the declared variable when one exists.

### openai-completions facts

- OpenAI's `gpt-6-luna` rejects `max_tokens` with a 400 and takes
  `max_completion_tokens`. OpenRouter, with `z-ai/glm-5.3-flash`, accepts both.
- OpenAI, with `gpt-6-luna`, sent a usage chunk (`choices: []`) in the one
  stream that set `stream_options.include_usage` and none in the one that did not. OpenRouter, with
  `z-ai/glm-5.3-flash`, sends a usage chunk either way, and repeats
  `finish_reason` on it.
- OpenRouter, with `z-ai/glm-5.3-flash`, streams reasoning as `reasoning` plus
  `reasoning_details` entries of type `reasoning.text`. Across the streams run
  on that model, it sent no `reasoning_content` or `reasoning_text`.
- OpenRouter, with `z-ai/glm-5.3-flash` pinned to one upstream, reads `reasoning`,
  `reasoning_content` and `reasoning_details` on a replayed assistant message.
- OpenRouter, with `z-ai/glm-5.3-flash`, accepts `cache_control` on a system
  part, the last message and a tool, and accepts `ttl` on the system part. It also accepted one request with
  an invalid `type`. This shows acceptance only. With
  `anthropic/claude-haiku-4.5` pinned to the Anthropic upstream, OpenRouter
  passes markers on the system part, the last message and a tool, and `ttl`,
  through to Anthropic (`research/openai-completions-probe`).
- Tool-call deltas from `gpt-6-luna` and from OpenRouter with
  `z-ai/glm-5.3-flash` carry `index`. OpenRouter adds
  `: OPENROUTER PROCESSING` comment lines to the stream.

Every protocol reports `input` without cache reads and writes, as
`docs/events.md` (`tokens`) defines it. Where a vendor's input figure includes
them, the protocol's module subtracts them. OpenRouter's `prompt_tokens`
includes `cached_tokens`, for example 15 with 14 cached
(`research/openai-completions-probe`).

Each protocol reads cache reads and writes from these usage fields:

| Protocol | `cache_read` | `cache_write` |
|---|---|---|
| `anthropic-messages` | `cache_read_input_tokens` | `cache_creation.ephemeral_5m_input_tokens` under `"5m"`, `cache_creation.ephemeral_1h_input_tokens` under `"1h"` |
| `openai-completions` | `prompt_tokens_details.cached_tokens` | `prompt_tokens_details.cache_write_tokens`, always under `"5m"` |
| `openai-responses` | `input_tokens_details.cached_tokens` | `input_tokens_details.cache_write_tokens`, always under `"5m"` |
| `bedrock-converse` | `cacheReadInputTokens` | `cacheWriteInputTokens`, under `"5m"` |

OpenAI keeps one 30-minute lifetime and Responses sends no lifetime, so a
Responses write lands under `"5m"`. OpenAI's
`input_tokens` includes both counts: a
cache-warming request reported 2,347 input with 2,344 written, and the next
reported 2,347 with 2,344 cached (`research/openai-responses-probe`).

Here is the Databricks gateway as an example. It serves about 53 models. Claude
models work only through its Anthropic route, because its default route rejects
`reasoning_effort`. So the extension declares each Claude model with
`anthropic-messages`, the `/ai-gateway/anthropic/v1` base URL and `compat.bearer`. Models that
support the Responses API get `openai-responses`, and the rest get
`openai-completions`. It also sends one custom header on every request. All of
that is data.

### Model discovery

A provider may also declare a Lua `models()` function that returns its model
list. Fiber stores the result on disk in [Fiber home](state.md)'s cache, and
every surface that shows a model list draws from that copy at once: the
terminal's model picker, `fiber models`, and anything an agent reads. A refresh
runs in the background and never holds a list back. The generator leaves out ids a package's table skips because the vendor rejects them, and a gateway model no probe reaches is skipped.

- **At start,** Fiber refreshes each provider that has a credential and whose
  cached list is older than `model_lists.refresh_after` (`docs/configuration.md`),
  one day by default. Each provider that ran is unloaded once its list is
  written. Only the extensions the session uses stay loaded, among them each
  provider whose quota a client asked for (`docs/tools.md`, "Fetching"), and
  a provider with no credential never runs.
- **When no cached copy exists,** `models()` runs when the list is first
  needed. `models()` receives `{ key, placeholders }`: the provider's key as
  a session reads it, absent when its token comes from `credential()`, and
  the host each declared placeholder holds. Fiber does not call `models()`
  while a declared placeholder has no value or a value that is not a host,
  so the key goes only to a host the person set.
- **The model picker** refreshes, in the background, any provider whose list
  is older than `model_lists.refresh_after`, and updates the list when the result
  arrives. Its refresh button refreshes every provider with a credential,
  whatever the age of its list.

The age of a list is the age of its cache file, so sessions that start
together refresh a provider once. A provider already refreshing is not
started again. A `models()` that fails, or returns something other than a
model list, leaves the cached list in place, with an `extension_failed`
notice naming the extension. Nothing refreshes on a timer, so an idle Fiber
does no work.
A refresh never changes the tool definitions a session has sent: an agent sees
a new list only through a tool result (`docs/prompt-cache.md`).

The function can ask the vendor's own listing endpoint, read a file, or look up
metadata anywhere, models.dev included. A vendor with no listing endpoint ships a
static list instead.

A provider runs Lua in five functions at most: `models()`, `quota()`,
`credential()`, `sign()` and `cost()`. Only `sign()` runs on the request path,
`credential()` only when the cached token has already expired ("Keys, tokens
and OAuth"), and `cost()` only after a call has ended ("Cost").

### Signing a request

A provider may declare a Lua `sign()` function for a scheme that signs each
request, such as AWS SigV4. It receives the method, the URL, the headers, the
SHA-256 of the body and the time, in whole Unix seconds from the extension's
clock, since SigV4 signs over the time and Lua has no clock of its own. It
returns headers to add. It cannot change the body. If `sign()` fails or
returns headers Fiber cannot use, the call fails with `credential_failed`, and
is not retried.
SigV4's chain of HMACs uses the `host.hmac_sha256` host call
(`docs/extensions.md`, "Host calls").
It declares a timeout like every callback (`docs/extensions.md`, "How an
extension runs"), and a retry signs again.

Nothing changes a request's body on its way to a provider. A transform such as
redacting secrets runs where the text enters the session, in the
`before_message` and `after_tool` hooks (`docs/extensions.md`, "Hooks"), so
the secret never reaches the log or any request.

### Quota

A provider may declare a Lua `quota()` function that returns how much quota
it has left: windows, each with percent used and a reset time where the vendor
reports one, or credit remaining with its limit. It asks the vendor's usage
endpoint through `host.http`. Each vendor reports quota differently, and
overage past a cap is invisible in every response Fiber has seen
([fiber-zig#96](https://github.com/aakshintala/fiber-zig/issues/96)), so the
shape stays in the provider's package rather than in the protocol. When it
runs, and what the model and the person see, is `docs/tools.md`, "Provider
quota".

### Cost

A call's `cost` in `usage_recorded` is the vendor's own figure where the
response or a generation lookup reports one, as OpenRouter's does
(`docs/events.md`, "Usage and notices"). Otherwise it is the model's declared
`cost` prices applied to the call's `tokens`, each kind at its own price; a
`cache_write` entry keyed `"1h"` prices at twice the selected `input` price. A
model with neither has `cost` `null`, and the person sees its tokens only. A
call whose generation the provider never named has `cost` `null`, whatever the
model declares.

A provider whose vendor offers a generation lookup declares a Lua `cost()`
function. It receives a table `{ generation_id, base_url, key }`: the
generation's id, the base URL the call went to, and the key the session used
for the call (absent when the provider's token comes from `credential()`). It
returns the generation's cost in US dollars, or nothing when the vendor does
not have it. It asks the vendor through `host.http`, so the lookup's URL and the shape of its answer stay in
the provider's package, as `quota()`'s do. The loop calls it once, 30 seconds
after a call that ended without the vendor's own figure, on the session's
injected clock: a stream closed early, cancelled or failed. It never calls it
for an id Fiber minted, because the vendor never named that generation
(`docs/events.md`, `usage_recorded`). A returned cost is
written as a second `usage_recorded` with the same `generation_id`. Nothing
returned, an error or a timeout leaves the first record as it is, and the loop
does not ask again. A session that ends before the 30 seconds pass writes
nothing more. OpenRouter's lookup answers "not found" until the cost is ready,
which took up to 30 seconds when probed (`research/openrouter-cost`).

A model priced by request size declares `tiers` (`docs/configuration.md`, "A
provider's data"). The tier with the highest `input_tokens_above` that the
call's whole input exceeds gives every price for the whole call; below every
threshold, the base prices apply. A `"1h"` write costs twice that tier's
`input` price. The whole input is `input` plus
`cache_read` plus `cache_write` from the call's `tokens`, because a vendor
that prices by request size counts the whole prompt, cached or not.

A model that a subscription login serves declares `"subscription": true`
beside its prices, which are the vendor's API prices. Its calls are logged
with `subscription`, so the person sees what the work would cost on an API
key, kept apart from money billed per token, and `budget.usd` never counts
them (`docs/loop.md`, "Spending budget"). ChatGPT/codex declares every model
this way.

## Image limits

Every image is processed once, when it enters the session: an image from
`read`, in an MCP tool result, or pasted in a `prompt` or `steer` command.
`web_fetch` saves an image as downloaded and gives its path, so the image
enters through `read`. The processed file is what is written to the session's
`artifacts/`, what the log's `image` part names (path, mime_type, width and
height of the stored file), and what every request sends. A resume sends the
same bytes. The provider module does not resize.

The image child does the work (`docs/invocation.md`, "Processes"). It reads
the header. An image over 50 megapixels (width × height > 50,000,000) is
refused without decoding. A GIF always becomes PNG (first frame). A PNG,
JPEG or WebP within the cap is stored byte for byte, not decoded. Otherwise
it is fitted.

One cap for every protocol: longest side 2000 px, and at most 1 MB
(1,048,576 bytes) as base64. Every image in context is resent as base64 on
every request (Fiber keeps no vendor-side state), so one 4.5 MB image would
outweigh a 600k-token text context in bytes. 2000 px fits Anthropic's
2000 px per-image rule for requests with more than 20 images,
Bedrock/Vertex's 5 MB, and OpenAI's 30,000-patch limit (2000×2000 is 3,969
patches). Measured: a 2000 px screenshot is 162,676 bytes of base64 as PNG at Default
compression, and six photographs fitted inside 2000 px are 232,204 to
672,632 bytes of base64 as JPEG at quality 80
(`research/image-limits/README.md`).

Fitting keeps the aspect ratio, never enlarges, and uses Lanczos3. A JPEG
input is re-encoded as JPEG at quality 80; any other input as PNG at
`image`'s Default compression level. If the result is still over 1 MB of
base64, JPEG at quality 80 is tried and kept if it fits; if neither fits,
the longest side is cut to three quarters and both are tried again, until
one fits.

There is no limit on the total image bytes in one request. A request over a
vendor's request limit (Gemini documents 20 MB inline, Anthropic 32 MB)
fails as the vendor returns it.

The table is what each vendor documents. The sources are
`platform.claude.com/docs/en/build-with-claude/vision`,
`developers.openai.com/api/docs/guides/images-vision` and
`ai.google.dev/gemini-api/docs/image-understanding`.

| Protocol | Largest dimension | Largest encoded image | Images per request | Resizing the vendor documents |
|---|---|---|---|---|
| `anthropic-messages` | 8000 px; 2000 px per image once a request holds more than 20 | 10 MB as base64 (5 MB on Bedrock and Google Cloud); 32 MB per request | 600, or 100 for a model with a 200k-token context | to 2576 px and 4784 tokens on Claude 4.7 and later, 1568 px and 1568 tokens on other models; a computer-use or browser-use `tool_result` image over the limit is rejected |
| `openai-responses`, `openai-completions` | none; 30,000 patches of 32 px per image (the gpt-5.6 family: `high` detail fits 2048 px and 2,500 patches) | 512 MB per request | 1,500 | at `detail: high`; `original` keeps the size and rejects over 30,000 patches |
| `google-generative-ai` | none stated | 20 MB per request, inline | 3,600 | none stated; the page describes 768 px tiles for counting tokens |

Requests to check the table, on September 29, 2026, one image each:

- `claude-sonnet-5-5`: an 8000 by 6000 px PNG was accepted and counted 4,788
  input tokens. A 9000 by 9000 px PNG failed with a 400 naming the 8000 px
  limit, and a 23 MB base64 PNG with a 400 naming the 10 MB limit. A GIF was
  accepted.
- `gpt-6-luna`, default detail: on each protocol, an 8000 by 6000 px PNG (47,000
  patches) and a 9000 by 9000 px PNG were rejected with a 400 naming the 30,000
  patch limit. A 23 MB base64 PNG was accepted on both, and a GIF on
  `openai-responses`.
- `gemini-3.1-flash-lite`: the 8000 by 6000 px and 9000 by 9000 px PNGs, a 23 MB
  base64 PNG (over the 20 MB the page states) and a GIF (which the page does not
  list) were all accepted, at 1,091 to 1,116 prompt tokens.

The measurements are in `research/image-limits/README.md`.

## Naming a model

A session's stored model reference is always `provider/model`, for example
`databricks/databricks-claude-opus-5`.

When a person types a model:

1. Fiber tries the exact string as `provider/model`.
2. If nothing matches and the string ends in `:` and a thinking level (`off`,
   `minimal`, `low`, `medium`, `high`, `xhigh` or `max`), Fiber strips the
   suffix, matches the rest and applies that thinking level.
3. A bare model id works if exactly one installed provider has it. Two matches
   are the error `model_ambiguous`, which lists every match. Prefix the
   provider, as `provider/model`. A provider that is installed but lacks the
   named model is `no_model`. A bare id that no installed provider has is
   `no_model` too, and its message says to run `fiber models`. Only a
   `provider/model` whose provider is not installed is `extension_missing`.
   These rules match a model left out with `model_unconfigured` too, so naming
   one is `model_unconfigured`, not `no_model`, and a bare id it shares with
   another provider's model is `model_ambiguous`.

The exact match comes first because OpenRouter model ids contain colons.

A delegate's model is named with its harness first:
`harness:provider/model:level`, such as `fiber:openai/gpt-5.6:xhigh`,
`claude:opus:high` or `cursor-agent:composer-2.5`. The rules for it are
`docs/delegates.md` ("Choosing a model").

## Thinking

A session has one reasoning setting, its thinking level: `off`, `minimal`,
`low`, `medium`, `high`, `xhigh` or `max`. Fiber has no separate effort
setting. Each protocol module maps the level to whatever its vendor takes,
whether a token budget, an effort parameter or both. A model
declares the levels it supports, and the model picker offers only those.

The level comes from, in order: a `:level` suffix typed with the model, which
applies to that session only; the session's own choice, from the picker or
`/thinking`; `models."provider/model".thinking`; the top-level `thinking` key;
and otherwise the model's own default (`docs/configuration.md`).

A level the model does not declare is checked by where it came from. One asked
for now, by a `:level` suffix, the session's choice or a driver, fails with
`invalid_arguments`. A configured one, from `models."provider/model".thinking`
or the top-level `thinking`, is ignored: the session uses the model's own
default and logs the notice `config_key_ignored`, naming the key, the level and
the model. A model that declares no levels, such as a local Ollama model, runs
with none.

On `anthropic-messages` a model takes adaptive thinking unless its data
declares `compat.thinking_budget`. Adaptive thinking sends the level as
`thinking: {type: "adaptive"}` with `output_config: {effort}`. A
budget-tier model sends the level as
`thinking: {type: "enabled", budget_tokens}` with no `output_config`:
`minimal` sends 1024, `low` 2048, `medium` 8192, and `high`, `xhigh` and
`max` 16384. The budget is measured against the sent `max_tokens`: with
none, the level's budget is sent; at or below 1024, no `thinking` key is
sent; otherwise the budget is capped so at least 1024 tokens stay for the
reply, and is never below 1024.

On `bedrock-converse` a model takes a level only through the field its data
names in `compat.reasoning_field`, sent in `additionalModelRequestFields`:
`reasoning_effort` sends `{reasoning_effort: <level>}`, as gpt-oss takes it,
and `reasoningConfig` sends `{reasoningConfig: {type: "enabled",
maxReasoningEffort: <level>}}`, as Nova 2 Lite takes it. At `high` the second
sends no `maxTokens`, because Nova 2 Lite refuses it beside that level.
`off` sends neither field. A model without the flag is sent no level, so the
`bedrock` package declares none for it ("Bedrock Converse wire facts").

## Choosing the model

Fiber picks the model for a session in this order:

1. `--model` given on a resume that starts the process, which both doors
   accept: `fiber ask`, and the terminal, which passes it to each session
   it starts. `-c model=` on the same run joins this layer.
2. The model a resumed session was using.
3. The default in config.

If none of these gives a model, the terminal opens a model picker, and saving
the choice writes the config default. A headless run fails with the error code
`no_model`. Fiber never picks a model that nobody chose.

A one-shot review run gets a different model by passing `--model`. A role is a
configured name for a delegate's model reference (`docs/delegates.md`). Roles
name only the models delegates use; the session's own model is chosen in
the order above. Roles are configured at `roles."<name>"`
(`docs/configuration.md`).

A repository's configuration may choose the default model from providers already installed.
It cannot declare a provider or change a provider's base URL. If it could, a
cloned repository could point `openrouter` at its own server, and Fiber would
send it your OpenRouter key. A repository that needs a provider ships an
extension, whose manifest names the provider and its base URLs. The extension
loads only after a person approves it, and the offer shows each base URL
(`docs/extensions.md`, "Code a repository ships").

## Credentials

A provider may hold several credentials, such as two subscriptions or a work
and a personal key. Each has a credential label, such as `work` or
`alice@example.com`. A stored credential is a file only the owner can read
(mode 0600) in [Fiber home](state.md) at `credentials/<name>/<label>`.
`<name>` is the provider's own name, unless its provider data names a shared
credential.

A key can come from:

- a stored credential
- an environment variable
- a file
- the output of a command, run once per process

A key that is not stored takes its label from the configuration that points at
it. A person sets these at `providers."<name>".credentials."<label>"`
(`docs/configuration.md`, "Secrets"), never from a repository. The source a
provider's data declares has the label `default`.

A stored credential owns its label. If it fails, Fiber reports the failure. It
does not fall back to an environment variable or a command under the same
label.

### Logging in

`fiber login <provider> --as <label>` stores a credential under that label.
Without `--as`, the label is the account's email when the login reveals one,
as `credential()`'s `email` does, and `default` otherwise. A login whose label is
already stored is refused, and the message names `--as`. The first label a
provider stores is written to `providers."<name>".credential` in the global
file, unless that key is already set. A provider that logs in by browser
also takes `--device`, which logs in with a device code instead.

At a terminal the login asks, once the credential is stored, which model
reviews that provider's calls: the provider's models with the saved
`providers."<name>".reviewer_model` preselected, else the package default,
and nothing preselected when neither exists. The pick is saved to that
global key, one per provider; `reviewer.model` stays the override that
applies to every provider. Without a terminal the credential is stored and
nothing is asked.

### Which credential a session uses

A session picks its credential label when it starts. Fiber picks it in this
order:

1. `--credential <label>` on a resume, which both doors accept.
2. The label a resumed session was using.
3. For a delegate started with a role, the role's `credential`
   (`docs/delegates.md`, "Choosing a model").
4. `providers."<name>".credential`, from any layer except a repository's.
5. Otherwise `default`, the source the provider's data declares.

The order applies to a provider that takes a credential: a scripted session has no label ("The scripted provider"), so any label asked for one fails with `credential_missing`.

A label that names no credential fails with `credential_missing`, naming the
labels the provider has.

The label is part of the request settings, so `preamble_built` records it
(`docs/events.md`, "Preamble"). A resume with `--credential` that changes it
records `model_changed`, as a switch does.

A person switches credential with `/credential <label>`, and a driver with the
`credential` command (`docs/invocation.md`). A credential switch accepted
while a model switch is queued applies to the queued model's provider.
The `credentials` query lists that provider's labels and the label it uses now.
With no label, `/credential` opens the credential picker (`docs/tui.md`,
"Swapped views"): Enter chooses a label and saves it; Ctrl+S chooses it for
this session only and saves nothing. The switch applies at the next
turn boundary and rebuilds the prompt cache, because a vendor holds the cache
per account or workspace (`docs/prompt-cache.md`, "Switching model"). The
terminal saves the label to the global `providers."<name>".credential`, unless
the person chooses it with Ctrl+S in the credential picker. A per-project file can pin a
label for one project. Saving the label never changes another session that
is already running.

Fiber never changes a session's credential by itself. It does not rotate
credentials, and it does not move to another label when one runs out of quota
or is rejected. The failure is reported as any other.

### When a credential is missing or fails

Fiber looks for the session's credential at startup, before the session
starts. A run with none fails there with `credential_missing`
(`docs/errors.md`, "Before a session exists"). A credential that is stored but
cannot be used, or a `credential()` call that errors, fails with
`credential_failed`, except a failed OAuth refresh ("Keys, tokens and OAuth").
Neither is retried.

A `model` or `credential` switch reads the credential it switches to when the switch is
prepared, if this process has not read it yet. A failure rejects the switch
with the same codes and keeps nothing, so the next switch reads it again.

### Keys, tokens and OAuth

Most first-party providers use a key. The table in
[Protocols and providers](#protocols-and-providers) says which use something
else.

A provider whose credential is a token that expires declares a Lua
`credential()` function. It returns `{ token = <string>, expires_at = <Unix
seconds>, headers = { [name] = <string> }, email = <string> }`: the token, the
time it expires, and two optional fields. It receives `{ label = <string>,
credential = <string> }`: the session's credential label, and the stored
credential's name, which is the shared credential name when the provider's
data names one and the provider's own name otherwise. During `fiber login`,
`label` is the `--as` value, and is absent when the label comes from the
returned `email`.

- `headers` are sent on every request that uses the token, and are cached and
  refreshed with it. ChatGPT/codex returns its `chatgpt-account-id` here. A
  header Fiber builds itself, such as `authorization`, fails the call with
  `credential_failed`.
- `email` is read only by `fiber login`, which uses it as the label when
  `--as` is absent ("Logging in"). Every other call ignores it.

Fiber caches the
token per credential name and label, so a `/credential` switch gets that
label's token, never the previous one. It calls the function again, off the request path, when a request
finds the token within 5 minutes of expiry. A request that finds the token
already expired, because the session sat idle past it or the earlier refresh
failed, waits for one call: an idle Fiber does no work, so nothing refreshes
on a timer, and failing the request would only make the person send it again. A cloud's own sign-in, such as Google
Vertex's, is this function, written in the extension, not in Fiber.

An OAuth login is extension code too. The extension's `credential()` builds its
vendor's flow from native host calls: opening the browser, a one-shot localhost
callback, PKCE, device-code polling and the locked credential file
(`docs/extensions.md`, "Host calls"). It adds its vendor's own steps. codex
reads the account id from the access token's `https://api.openai.com/auth`
claim `chatgpt_account_id`, and the email from the `id_token`'s `email` claim.

ChatGPT/codex is the only subscription login Fiber ships. A subscription login
ships when its vendor permits use from other harnesses and Fiber can probe it.
The Claude subscription is reachable only by running Claude Code, as a harness
extension (`docs/delegates.md`).

The refresh lock is native. A refresh takes a lock on the credential file,
re-reads it, and refreshes once, so two sessions never refresh the same token
twice. If the refresh fails, the stored credential stays in place, and the call
fails with `authentication_failed`, or `connection_failed` when the token
endpoint could not be reached. Logging in again is the fix for a rejected
refresh.

A headless run whose credential has expired and cannot be refreshed fails with
`authentication_failed`. It never prompts, because nobody is there to answer.
Mid-session, with a `full` client attached, `credential()` may run the provider's
login instead, and the client receives its URL (`docs/extensions.md`, "Host
calls"). At startup nobody is attached yet.

## When a model call fails

A failed model call is recorded as `docs/events.md` describes: an assistant
message that completed with a failed outcome and an `error`.
A retry is a new action. Which failure gets which code, and which codes are
retried, is `docs/errors.md`, "A failed model call".

Fiber retries these failures:

- rate limits (HTTP 429)
- server errors (HTTP 5xx) and overload responses
- a request timeout (HTTP 408) and a conflict (HTTP 409)
- a failure with no HTTP status, such as a dropped connection
- a stream that ends before its protocol's terminal event

A model call gives each address its host resolves to 15 seconds to
connect, and fails when it waits 300 seconds for any byte to arrive or be
sent, from the request through the end of the stream, the wait for the
response's headers included. Reasoning models can stay silent for minutes,
so the idle limit is long. Either failure is `connection_failed` and is
retried like a dropped connection. Neither is a configuration key.

A response header `x-should-retry` overrides the status: `true` retries the
failure, `false` does not. Fiber never retries quota or billing errors, an
`unknown_stop_reason` or a `paused_too_long`, whatever the header says.
An extension's `host.model` call applies this policy within the callback's
timeout (`docs/extensions.md`, "Host calls").

Among the responses probed, only Anthropic sent `x-should-retry`: `false` on its
400 and 404 responses, `true` on a 429. Anthropic's 429 also carried
`retry-after` in seconds. muse sent `retry-after: 60` on a 429 for an oversized
`max_tokens`; three sends of it returned the same 429, so a client that honours
the header retries it in a loop. Among the responses OpenAI, Gemini,
ChatGPT/codex and OpenRouter returned to cheap failing requests, none carried
`x-should-retry`, `retry-after-ms` or `retry-after`, and none of those vendors
reached a 429 (`research/retry-signals/`). No vendor sent `retry-after-ms`.
The saved responses had statuses 200, 400, 401, 403, 404, 405 and 429; no 409,
425 or 501 appeared, which does not show a vendor never sends them.

The wait a provider asks for, in seconds, becomes the error's `retry_after_ms`, in milliseconds rounded up.
Fiber reads it from `retry-after`. On `google-generative-ai`, Fiber reads
`Retry-After` or the error body's `RetryInfo.retryDelay`, whichever is present.
That is the shape Google documents, and no Gemini API 429 was reached
(`research/retry-signals/`). The one 429 Google Vertex sent carried neither,
so Fiber waits its own backoff for one ("Google Vertex wire facts").

The defaults are 3 retries with exponential backoff: 2 seconds, then 4, then
8, with each delay capped at 60 seconds. If the server asks Fiber to wait longer
than 60 seconds, the call fails at once with that wait in the error, so a person
or a caller can decide. The values are config. The terminal and the headless
door use the same policy. A caller that wants to outlast a long outage retries
the whole run.

When a stream dies midway, the partial text is not kept, because deltas are
ephemeral. A tool call the model finished emitting inside a failed message
never runs. The retry asks the model again.

An `openai-responses` stream that ends in `response.failed` is one such
failure. Everything it already streamed is dropped, finished tool calls
included, and the call is retried. Its code comes from the error body, and is
`stream_incomplete` when no other code matches (`docs/errors.md`).

When the retries run out, the step fails with the provider's error. Fiber never
switches to another model or provider on its own.
