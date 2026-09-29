# Model routing

How Fiber reaches a model, and how one is chosen. Vocabulary is `CONTEXT.md`.
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

Protocols are native Rust in the `provider` module. There are four:

| Protocol | Used by |
|---|---|
| `anthropic-messages` | OpenCode, OpenRouter, Databricks, muse |
| `openai-completions` | OpenCode, OpenRouter, Databricks, muse |
| `openai-responses` | OpenCode, Databricks, muse, ChatGPT/codex |
| `google-generative-ai` | OpenCode Zen's Gemini models |

ChatGPT/codex speaks `openai-responses` with compatibility flags its extension
declares: the request fields it requires or rejects, such as `store: false`,
the headers its login supplies, and its usage-limit error body. Its streamed
events, tool calls and reasoning items are parsed as plain Responses. A stream
ends with `response.completed`; the endpoint never sends `response.done` over
SSE. The `OpenAI-Beta` header is not required. The endpoint accepts
`parallel_tool_calls`, `tool_choice` and `text.verbosity`. It rejects
`temperature` with status 400, and rejects a `service_tier` of `flex` or
`auto`.

On OpenAI's own endpoint, with `gpt-6-luna`, `instructions` and a system
message in `input` gave the same answer and the same prompt cache for one
prompt, and switching between the two kept the cached prefix. For one tool
schema with an optional property, a function tool with no `strict` key was
accepted, and the response reported `strict: true` with the schema rewritten
(every property required, `additionalProperties: false`). The same schema with
an explicit `strict: true` was rejected with a 400. Details:
[research/openai-responses-probe](../research/openai-responses-probe/README.md).

An extension cannot add a protocol. A vendor with a new wire format needs a
Fiber release.

Every provider is an extension, the five Fiber ships included: OpenCode,
ChatGPT/codex, muse (the Meta Model API), OpenRouter and Databricks. Extensions
are fetched and installed, not built into the binary. Installing Fiber
installs these five. How extensions arrive and stay current is
`docs/extensions.md`.

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

The Gemini API answers HTTP 404 "no longer available to new users" for
`gemini-2.5-flash-lite`, `gemini-2.5-flash` and `gemini-2.5-pro` on a key created
in September 2026.

## What a provider extension declares

Most of a provider is data. For the provider:

- its name, which is the first half of every model reference
- how its credential is found (see [Credentials](#credentials))
- headers sent on every request

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
  an invalid `type`. This shows acceptance only. Whether OpenRouter passes
  any placement or `ttl` upstream is not settled.
- Tool-call deltas from `gpt-6-luna` and from OpenRouter with
  `z-ai/glm-5.3-flash` carry `index`. OpenRouter adds
  `: OPENROUTER PROCESSING` comment lines to the stream.

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

This and `quota()` are the only Lua a provider runs. Neither runs on the
request path.
Nothing transforms a request on its way to a provider. A transform such as
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
2. `--model`, which every door accepts: the terminal, `fiber ask` and
   `fiber serve`.
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
(mode 0600) in [Fiber home](state.md) at `credentials/<provider>`.
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

OAuth flows are native, like protocols. An extension chooses a flow and
supplies its parameters, such as the client id and endpoints. ChatGPT/codex
uses one. The other four providers Fiber ships use keys.

Fiber refreshes an OAuth token when it is within 5 minutes of expiry. The
refresh takes a lock on the credential file, re-reads it, and refreshes once,
so two sessions never refresh the same token twice. If the refresh fails, the
stored credential stays in place, and the call fails with an auth error. Logging
in again is the fix.

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
failure, `false` does not. Fiber never retries quota or billing errors, whatever
the header says.

The defaults are 3 retries with exponential backoff: 2 seconds, then 4, then
8, with each delay capped at 60 seconds. If the server asks Fiber to wait longer
than 60 seconds, the call fails at once with that wait in the error, so a person
or a caller can decide. The values are config. The terminal and the headless
door use the same policy. A caller that wants to outlast a long outage retries
the whole run.

When a stream dies midway, the partial text is not kept, because deltas are
ephemeral. A tool call the model finished emitting inside a failed message
never runs. The retry asks the model again.

When the retries run out, the step fails with the provider's error. Fiber never
switches to another model or provider on its own.
