# Model routing

How Fiber reaches a model, and how one is chosen. Vocabulary is `CONTEXT.md`.
The provider seam is `docs/architecture.md`, and the extension runtime is
`docs/extensions.md`. The reasoning behind the protocol and provider split is
[ADR 0007](adr/0007-protocols-are-native-providers-are-extensions.md). pi is the
reference for wire and auth behavior. What it does is in
[How pi does providers, auth and routing](https://github.com/aakshintala/fiber/issues/4).

## Protocols and providers

A protocol is a wire format: how a request is shaped, and how a streamed reply
is parsed into actions. A provider is an endpoint that speaks one or more
protocols: a name, a credential, base URLs and a list of models.

Protocols are native Rust in the `provider` module. There are five:

| Protocol | Used by |
|---|---|
| `anthropic-messages` | OpenCode, OpenRouter, Databricks, muse |
| `openai-completions` | OpenCode, OpenRouter, Databricks, muse |
| `openai-responses` | OpenCode, Databricks, muse |
| `openai-codex-responses` | ChatGPT/codex |
| `google-generative-ai` | OpenCode Zen's Gemini models |

An extension cannot add a protocol. A vendor with a new wire format needs a
Fiber release.

Every provider is an extension, the five Fiber ships included: OpenCode,
ChatGPT/codex, muse (the Meta Model API), OpenRouter and Databricks. Extensions
are fetched and installed, not built into the binary. Installing Fiber
installs these five. How extensions arrive and stay current is
`docs/extensions.md`.

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
- extra request body fields
- a prompt addendum, text appended to the system prompt for this model only
  (`docs/system-prompt.md`, "The model's addendum")
- context window, output token limit, input kinds and cost

Fiber never guesses a flag from a URL or a provider name. A flag the vendor
needs is declared, or it is not set.

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

This is the only Lua a provider runs. It never runs on the request path.
Nothing transforms a request on its way to a provider. A transform such as
redacting secrets runs where the text enters the session, in the
`before_message` and `after_tool` hooks (`docs/extensions.md`, "Hooks"), so
the secret never reaches the log or any request.

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
- a dropped connection
- a stream that ends before its protocol's terminal event

It never retries quota or billing errors.

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
