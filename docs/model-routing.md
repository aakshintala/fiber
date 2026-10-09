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
reads it for Claude on Bedrock, which Bedrock serves through its Invoke API, and
`bedrock-converse` reads it too.

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
(Unix seconds), the milliseconds until then become the error's `retry_after_ms`, and
the message names the reset time. `rate_limit_exceeded` stays `rate_limited`.
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

So Fiber sends `strict` on every tool, on every protocol that has strict mode,
and decides it per tool. It sends `true` only when the tool's schema already
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

Fiber sends mode `VALIDATED` only when every tool in the request fits the
strict subset. A tool declared in every session, such as `ask_user`, is sent
with `strict: false`, so Gemini requests use `AUTO` until #1214 settles it.
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
  `/v1/chat/completions` and `anthropic-messages` at `/v1/messages`. Zen lists
  `google-generative-ai` models, but the one request probed on that route was
  refused (`research/opencode-zen-gemini-probe`).
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

### The scripted provider

`scripted` is the one provider built into the binary, and the only one that
reaches no network. Its protocol of the same name reads a script file instead
of a socket. A script is a list of steps, each one model reply: text, a tool
call with its arguments, a given error such as a rate limit, or a reply
streamed slowly. Each request takes the next step, in order. A request is
never matched against a step, so a prompt that changes does not change the
reply. A request after the last step fails the turn.

A script is a JSON file, `{"steps": [...]}`, read whole each time a model on
it is connected: when a session starts or resumes on it, switches back to it,
or resolves its reviewer to it. Each step is an object, a reply or an error:

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
and the request. Each connection starts at step 1, and a reviewer on a script
has its own steps. A script that cannot be read fails with `io_failed`, and a
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
- headers sent on every request
- a `sign()` function, if every request must carry a signature
- `reviewer_model`, optional: one of its models that reviews calls when
  `reviewer.model` is unset (`docs/permissions.md`, "How it runs"). A
  first-party package names the model its vendor's own agent reviews with,
  at the current generation: `claude-sonnet-5-5`, `gpt-6-luna`; Google
  ships no reviewer, so `gemini` names its middle tier,
  `gemini-3.8-flash`. A first-party package never names a contributor
  model as `reviewer_model`, and a package whose models are all
  contributor models names none.

For each model:

- its id, as the vendor spells it
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

A first-party package's `models` list is generated, except `openrouter`'s, which its `models()` reads from OpenRouter ("Model discovery"). `cargo xtask models-dev` reads models.dev (`https://models.dev/api.json`) and keeps each source's models that call tools and output text, leaving out Gemini models before Gemini 3 ("Google Generative AI wire facts") and any model with no context window. The `opencode-zen` package also leaves out its `google-generative-ai` models: the one request probed on that route, to `gemini-3.5-flash-lite`, answered 403 "Model access is disabled" (`research/opencode-zen-gemini-probe`). It derives each model's protocol, context window, output token limit, input kinds and cost from models.dev, and takes every other field, such as `base_url`, `compat`, `web_search`, `thinking_levels` and the provider's `credential` and `reviewer_model`, from a table it keeps per package. A rerun on an unchanged models.dev changes nothing. It runs only when someone runs it: CI never fetches, and a test regenerates the lists from a checked-in copy of models.dev and fails when a committed file differs.

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
in the URL, not the body. `cache_control` is reserved because cache markers
are Fiber's alone (`docs/prompt-cache.md`, "Cache markers and keys").

A model whose `extra_body` names one of these is left out of the model list,
with the notice `model_invalid` naming the model and the field. The same
applies to a model a provider's `models()` function returns. So the tools
and system prompt in the session log are always the ones sent, and the tool
set is fixed for a given Fiber build.

### Hosted web search

A model's `web_search` names the vendor's hosted-search tool type as the
vendor spells it. Each protocol accepts only the types its code reads back
(`docs/tools.md`, "Hosted by the provider"). A model whose `web_search` names
any other type is left out of the model list, with the notice
`model_invalid` naming the model and the type, as for a forbidden
`extra_body` field.

### A per-account host

Some providers serve each account from its own host, such as a Databricks
workspace, an Azure resource or a Vertex region. A model's `base_url` names
that part with a placeholder, `{name}`:

```json
"base_url": "https://{workspace}/ai-gateway/anthropic"
```

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
left out of the model list, with the notice `model_unconfigured` naming the
model and the setting; for a value that is not a host it says so, without
repeating the value. The model can still be named ("Naming a model"): a
session that chooses it exits with `model_unconfigured`, and
`fiber config set model` accepts it with that notice
(`docs/configuration.md`, "When Fiber writes").

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
list. Fiber stores the result on disk in [Fiber home](state.md)'s cache, and
every surface that shows a model list draws from that copy at once: the
terminal's model picker, `fiber models`, and anything an agent reads. A refresh
runs in the background and never holds a list back.

- **At start,** Fiber refreshes each provider that has a credential and whose
  cached list is older than `model_lists.refresh_after` (`docs/configuration.md`),
  one day by default. Each provider that ran is unloaded once its list is
  written. Only the extensions the session uses stay loaded, and a provider
  with no credential never runs.
- **When no cached copy exists,** `models()` runs when the list is first
  needed.
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
request, such as AWS SigV4. It receives the method, the URL, the headers and
the SHA-256 of the body, and returns headers to add. It cannot change the body. If `sign()` fails or
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
`cost` prices applied to the call's `tokens`, each kind at its own price. A
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
threshold, the base prices apply. The whole input is `input` plus
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
file, unless that key is already set.

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
`credential` command (`docs/invocation.md`). The switch applies at the next
turn boundary and rebuilds the prompt cache, because a vendor holds the cache
per account or workspace (`docs/prompt-cache.md`, "Switching model"). The
terminal saves the label to the global `providers."<name>".credential`, unless
the person marks the switch as this session only. A per-project file can pin a
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

The wait a provider asks for, in seconds, becomes the error's `retry_after_ms`, in milliseconds rounded up.
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
