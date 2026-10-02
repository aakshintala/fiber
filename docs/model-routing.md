# Model routing

How Fiber reaches a model, and how one is chosen. Vocabulary is `GLOSSARY.md`.
The provider seam is `docs/architecture.md`, and the extension runtime is
`docs/extensions.md`. The reasoning behind the protocol and provider split is
[ADR 0007](adr/0007-protocols-are-native-providers-are-extensions.md). pi is the
reference for wire and auth behavior. What it does is in
[How pi does providers, auth and routing](https://github.com/aakshintala/fiber/issues/4).
Each protocol's wire facts, read from pi and rig side by side, are in
[research/provider-harvest](../research/provider-harvest/README.md).

## Protocols and providers

A protocol is a wire format: how a request is shaped, and how a streamed reply
is parsed into actions. A provider is an endpoint that speaks one or more
protocols: a name, a credential, base URLs and a list of models.

Protocols are native Rust in the `provider` module. There are five:

| Protocol | Used by |
|---|---|
| `anthropic-messages` | Anthropic, OpenCode, OpenRouter, Databricks, muse, AWS Bedrock (Claude), Google Vertex (Claude), Azure (Foundry Claude) |
| `openai-completions` | OpenCode, OpenRouter, Databricks, muse, Azure |
| `openai-responses` | OpenAI, ChatGPT/codex, OpenCode, Databricks, muse, Azure |
| `google-generative-ai` | the Gemini API, Google Vertex (Gemini), OpenCode (Zen's Gemini models) |
| `bedrock-converse` | AWS Bedrock (models other than Claude) |

AWS event-stream framing is a per-model flag, not a protocol. `anthropic-messages`
reads it for Claude on Bedrock, which Bedrock serves through its Invoke API, and
`bedrock-converse` reads it too.

ChatGPT/codex speaks `openai-responses` with compatibility flags its extension
declares: the request fields it requires or rejects, such as `store: false`,
the headers its login supplies, and its usage-limit error body. Its streamed
events, tool calls and reasoning items are parsed as plain Responses. A stream
ends with `response.completed`: all 45 SSE streams sent to `gpt-6-luna`
that returned 200 did, and none contained `response.done`. Requests with the
`OpenAI-Beta` header omitted, tried with one request body, also returned 200.
With `gpt-6-luna` the endpoint accepted
`parallel_tool_calls`, `tool_choice` and `text.verbosity`. It rejected
`temperature` with status 400, and a `service_tier` of `flex` or `auto`.

On OpenAI's own endpoint, with `gpt-6-luna`, `instructions` and a system
message in `input` gave the same answer and the same prompt cache for one
prompt, and switching between the two kept the cached prefix. For one tool
schema with an optional property, a function tool with no `strict` key was
accepted, and the response reported `strict: true` with the schema rewritten
(every property required, `additionalProperties: false`). The same schema with
an explicit `strict: true` was rejected with a 400. Details:
[research/openai-responses-probe](../research/openai-responses-probe/README.md).

So Fiber sends `strict` on every tool, on every protocol that has strict mode,
and decides it per tool. It sends `true` only when the tool's schema already
fits the vendor's strict subset, and `false` otherwise. Fiber never rewrites a
schema to fit, and never moves keywords into the description. Fiber's built-in
tools are written to fit the strict subset.

An extension cannot add a protocol. A vendor with a new wire format needs a
Fiber release.

Every provider is an extension, the first-party ones included. Extensions are
fetched and installed, not built into the binary. Installing Fiber installs no
provider: a person installs one by choosing it in the model picker, or with
`fiber install <name>`. How extensions arrive and stay current is
`docs/extensions.md`.

A first-party provider is one Fiber can probe and re-record. There are eleven:

| Provider | Protocols | Credential |
|---|---|---|
| Anthropic | `anthropic-messages` | key |
| OpenAI | `openai-responses` | key |
| Gemini API | `google-generative-ai` | key |
| ChatGPT/codex | `openai-responses`, with its flags | subscription login |
| OpenRouter | `anthropic-messages`, `openai-completions` | key |
| OpenCode: `opencode-go` and `opencode-zen` | `anthropic-messages`, `openai-completions`, `openai-responses`, `google-generative-ai` | key, one for both |
| Databricks | `anthropic-messages`, `openai-completions`, `openai-responses` | key |
| muse (the Meta Model API) | `anthropic-messages`, `openai-completions`, `openai-responses` | key |
| AWS Bedrock | `anthropic-messages` for Claude and `bedrock-converse` for other models, both with AWS framing | Bedrock API key, or SigV4 through `sign()` |
| Google Vertex | `anthropic-messages` for Claude, `google-generative-ai` for Gemini | token from `credential()` |
| Azure | `openai-completions`, `openai-responses`; `anthropic-messages` for Foundry Claude | `api-key` header, or token from `credential()` |

The OpenAI provider sends `store: false` on every request. Google Vertex's
flags and URLs are data, like any provider's.

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
- Function-calling mode `VALIDATED` returned schema-valid arguments where
  `AUTO` returned arguments that broke an enum and an integer type. In the
  sample it did not force a call.

A `functionCall` that arrives without an `id` is logged with no `provider_id`
(`docs/events.md`, `tool_call_requested`). Fiber pairs the call with its result
by its action id, which stays local. Only an id the model emitted is sent back.

Because a replayed `functionCall` needs the signature its own model gave it, a
tool call and its result that another model reference made are sent to Gemini
as plain text, not as `functionCall` and `functionResponse` parts
(`docs/loop.md`, "What the model is sent").

The Gemini API answers HTTP 404 "no longer available to new users" for
`gemini-2.5-flash-lite`, `gemini-2.5-flash` and `gemini-2.5-pro` on a key created
in September 2026.

Fiber targets Gemini 3 and later on `google-generative-ai`. How Gemini 2.x
models treat function-call ids and images in `functionResponse.parts` is not
measured, and is settled when Fiber adds them.

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
  `/v1/chat/completions` and `anthropic-messages` at `/v1/messages`. Zen also
  serves `google-generative-ai` for its Gemini models.
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

## What a provider extension declares

Most of a provider is data. For the provider:

- its name, which is the first half of every model reference
- how its credential is found (see [Credentials](#credentials)), or a
  `credential()` function that returns a token
- headers sent on every request
- a `sign()` function, if every request must carry a signature
- `reviewer_model`, optional: one of its models, small and fast, that reviews
  calls when `reviewer.model` is unset (`docs/permissions.md`, "How it runs")

For each model:

- its id, as the vendor spells it
- its protocol and base URL, which can differ between models of one provider
- compatibility flags the native protocol reads, such as whether the vendor
  accepts `store`, which field carries the token limit, and which thinking
  dialect it speaks
- whether deferred tools work for it, declared only after a probe
  (`docs/tools.md`, "Which tools the model sees")
- whether its provider hosts a web search for it, and which variant
  (`docs/tools.md`, "Web fetch and web search")
- extra request body fields
- a prompt addendum, text appended to the system prompt for this model only
  (`docs/system-prompt.md`, "The model's addendum")
- context window, output token limit, input kinds and cost
- whether a subscription login covers it ("Cost")

Fiber never guesses a flag from a URL or a provider name. A flag the vendor
needs is declared, or it is not set.

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

Here is the Databricks gateway as an example. It serves about 53 models. Claude
models work only through its Anthropic route, because its default route rejects
`reasoning_effort`. So the extension declares each Claude model with
`anthropic-messages` and the `/ai-gateway/anthropic` base URL. Models that
support the Responses API get `openai-responses`, and the rest get
`openai-completions`. It also sends one custom header on every request. All of
that is data.

### Model discovery

A provider may also declare a Lua `models()` function that returns its model
list. It runs when the list is needed and there is no cached copy, and again
in the background each time Fiber starts. Fiber stores the result on disk in
[Fiber home](state.md)'s cache and serves that copy until the refresh returns.
Nothing refreshes on a timer, so an idle Fiber does no work.

The function can ask the vendor's own listing endpoint, read a file, or look up
metadata anywhere, models.dev included. A vendor with no listing endpoint ships a
static list instead.

A provider runs Lua in four functions at most: `models()`, `quota()`,
`credential()` and `sign()`. Only `sign()` runs on the request path.

### Signing a request

A provider may declare a Lua `sign()` function for a scheme that signs each
request, such as AWS SigV4. It receives the method, the URL, the headers and
the SHA-256 of the body, and returns headers to add. It cannot change the body.
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
`cost` prices applied to the call's `tokens`, each kind at its own price. A
model with neither has `cost` `null`, and the person sees its tokens only.

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
   are an error that lists both.

The exact match comes first because OpenRouter model ids contain colons.

A delegate's model is named with its harness first:
`harness:provider/model:effort`, such as `fiber:openai/gpt-5.6:xhigh`,
`claude:opus:high` or `cursor-agent:composer-2.5`. The rules for it are
`docs/delegates.md` ("Choosing a model").

## Choosing the model

Fiber picks the model for a session in this order:

1. The model a resumed session was using.
2. `--model`, which both doors accept: `fiber ask`, and the terminal, which
   passes it to each session it starts.
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
extension, which a person approves before it loads.

## Credentials

Each provider has one credential, stored in a file only the owner can read
(mode 0600) in [Fiber home](state.md) at `credentials/<name>`: its own name,
unless its provider data names a shared credential.
A key can come from:

- the stored credential
- an environment variable
- a file
- the output of a command, run once per process

A person can override where a key comes from in configuration
(`docs/configuration.md`, "Secrets"), never from a repository.

A stored credential owns its provider. If it fails, Fiber reports the failure.
It does not fall back to an environment variable.

Fiber looks for the session model's credential at startup, before the session
starts. A run with none fails there with `credential_missing`
(`docs/errors.md`, "Before a session exists").

Most first-party providers use a key. The table in
[Protocols and providers](#protocols-and-providers) says which use something
else.

A provider whose credential is a token that expires declares a Lua
`credential()` function. It returns `{ token = <string>, expires_at = <Unix
seconds> }`: the token and the time it expires. Fiber caches the
token and calls the function again when the token is within 5 minutes of
expiry. It runs off the request path. A cloud's own sign-in, such as Google
Vertex's, is this function, written in the extension, not in Fiber.

An OAuth login is extension code too. The extension's `credential()` builds its
vendor's flow from native host calls: opening the browser, a one-shot localhost
callback, PKCE, device-code polling and the locked credential file
(`docs/extensions.md`, "Host calls"). It adds its vendor's own steps, such as
reading the ChatGPT account id from codex's token.

ChatGPT/codex is the only subscription login Fiber ships. A subscription login
ships when its vendor permits use from other harnesses and Fiber can probe it.
The Claude subscription is reachable only by running Claude Code, as a harness
extension (`docs/delegates.md`).

The refresh lock is native. A refresh takes a lock on the credential file,
re-reads it, and refreshes once, so two sessions never refresh the same token
twice. If the refresh fails, the stored credential stays in place, and the call
fails with an auth error. Logging in again is the fix.

A headless run whose credential has expired and cannot be refreshed fails with
`authentication_failed`. It never prompts, because nobody is there to answer.

## When a model call fails

A failed model call is recorded as `docs/events.md` describes: an assistant
message that completed with a failed outcome, an `error` and an attempt number.
A retry is a new action. Which failure gets which code, and which codes are
retried, is `docs/errors.md`, "A failed model call".

Fiber retries these failures:

- rate limits (HTTP 429)
- server errors (HTTP 5xx) and overload responses
- a request timeout (HTTP 408) and a conflict (HTTP 409)
- a failure with no HTTP status, such as a dropped connection
- a stream that ends before its protocol's terminal event

A response header `x-should-retry` overrides the status: `true` retries the
failure, `false` does not. Fiber never retries quota or billing errors, or an
`unknown_stop_reason`, whatever the header says.

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

The wait a provider asks for, in seconds, becomes the error's `retry_after`.
Fiber reads it from `retry-after`. On `google-generative-ai`, Fiber reads
`Retry-After` or the error body's `RetryInfo.retryDelay`, whichever is present.
That is the shape Google documents, and it is unprobed: no Gemini 429 was
reached (`research/retry-signals/`).

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
