# Errors

What a failure reports, and the one list of error codes. Settled by
[Error taxonomy: stable codes and what a caller is told](https://github.com/aakshintala/fiber/issues/113).
The provider evidence is
[research/provider-errors/README.md](../research/provider-errors/README.md).

## The shape

Every failure is `error { code, message }`: on `tool_call_completed`,
`job_completed`, `handoff_completed`, `mcp_server_failed`, a failed model call,
`turn_completed` and `fiber_exited`. A `notice` carries the same `code` and message.

- `code` is a stable label. A consumer switches on it and never parses the
  message. A label means the same thing wherever it appears: `timeout` is a
  deadline that passed, on a tool call, a job or a delegate.
- Codes are an open set. An unknown code is a generic failure, and the consumer
  shows the message. Adding a code is additive; renaming or removing one is a
  breaking change (`docs/events.md`, "Versioning").
- `message` is Fiber's own sentence, and it says what to do when there is a
  fix: "OpenRouter rejected the API key (HTTP 401). Run `fiber login
  openrouter`." The terminal shows the message as it is. A headless caller reads
  the same advice.
- A sentence about a provider's credential advises `fiber login <provider>`
  only where `fiber login` can fix it: for a provider that reads the key
  `fiber login` stores, and for one whose data declares `login`
  (`docs/model-routing.md`, "What a provider extension declares"). A provider
  with `credential()` and no `login`, such as Bedrock or Vertex, reads no
  stored key, so its sentence carries no such advice. `provider.message`
  below keeps the provider's own words, which for a failed `credential()`
  name its remedy. `fiber doctor` and the `model_unconfigured` notice run no
  extension code, so they read the same fact from the provider's data: its
  `credential_label`, which such a provider declares.

A failed model call adds two optional fields:

- `retry_after_ms`, in milliseconds, rounded up from the seconds the provider sent.
- `provider { name, status, message }`: the provider's name, the HTTP status and
  the provider's own message. Provider messages can mislead (OpenRouter answers
  a bad key with "Missing Authentication header"), which is why they sit here
  and not in `message`. For an extension provider whose `credential()` or
  `sign()` failed, `message` is the first line of the extension's error and
  `status` is absent. Before it is stored, every credential value and every
  header value the credential, `credential()` or `sign()` supplied is replaced
  with `[redacted]`. Fiber's `message` then names the provider and the function, such as "codex's credential() failed. Run `fiber login codex`." or, for a provider `fiber login` cannot fix, "bedrock's credential() failed.", and a `credential()` that fails at startup adds the same `provider` to `fiber_exited.error`.

## Where a code comes from

Each library crate has its own error enum (`docs/code-quality.md`, "Errors").
The crate that defines the enum also maps each case to a code, in a match with
no wildcard arm, so a new case does not compile until it has one. The codes
themselves are data in `contract`: names and nothing else, because `contract`
holds no behaviour.

This page is the readable list. A code added in a crate is added to the
registry below in the same change.

## What a caller gets

`fiber_exited`, the last line, is the verdict (`docs/invocation.md`, "What a
caller gets back").

- **`fiber ask`** runs one turn. If the turn failed, `fiber_exited.error` copies
  the turn's error and the process exits 1.
- **A session the hub started** runs many turns, and its clients have seen
  each one's `turn_completed`. A failed turn does not fail the process. It
  exits 1 with an `error` only when the process itself failed.
- **A signal** exits 129, 130 or 143 with no `error`; the exit code says what
  happened (`docs/invocation.md`, "Shutdown").

Every exit of `fiber ask` with an `error` also prints the error's message as
one sentence on stderr, `fiber: ` and the message, whether the failure came
before the session existed or inside it. Stdout is unchanged. A CI log, or a
person who ran it by hand, sees why it failed without parsing the event
stream.

A subcommand other than `ask`, such as `fiber extension`, `fiber login` or
`fiber approve`, reports a failure as one sentence on stderr, `fiber: ` and the
message, and exits with its code's status ("Before a session exists"). It
prints no `fiber_exited` line and no code: the code belongs to the crate's
error, and that crate's tests check it.

### Before a session exists

A failure before `fiber_started` has no session and no session log. A signal before Fiber starts writing `fiber_started` writes one diagnostic line instead (`docs/state.md`, "What each part holds"). `fiber ask`
still prints one `fiber_exited` line on stdout, carrying the exit
code and `error`, and one sentence on stderr. The line has no `session_id`, so
filtering stdout by session still gives each session's log byte for byte, and
`fiber ask … | tail -1` reads the verdict whatever happened. A session the
hub started writes the same line, and the hub passes it to the client that
asked for the session.

| Code | When | Exit |
|---|---|---|
| `usage` | the invocation or its environment is wrong: a bad flag, no prompt with stdin on a terminal, `fiber` without a tty, an empty or relative `FIBER_HOME`, `git` is not installed, `fiber ask --worktree` outside a git repository, or an unknown `--hub` name | 2 |
| `config_invalid` | invalid JSON, a value of the wrong type, one extension key set under both its full and short name in a configuration or script file (`docs/configuration.md`; `docs/model-routing.md`, "The scripted provider"), or an MCP server marked `required` that declares both `command` and `url` or whose `url` carries a user name or password, or `hub.default` names a missing entry or a stored hub address is invalid | 1 |
| `io_failed` | a filesystem failure, or a `git` command on a worktree that failed: a log write or fsync, or a configuration or credential file that exists but cannot be read or written; the message names the path; or Fiber could not install its signal handlers, and the message starts `signals:`; or the terminal could not start its input reader, and the message starts `terminal:` | 1 |
| `log_corrupt` | a log line that cannot be encoded, or one read back that does not parse | 1 |
| `no_model` | nothing chose a model, an installed provider lacks the named model, or no installed provider has a bare model id (`docs/model-routing.md`, "Naming a model") | 1 |
| `model_ambiguous` | a bare model id matches models of two or more installed providers; the message lists every match (`docs/model-routing.md`, "Naming a model") | 1 |
| `model_unconfigured` | the session's model names a per-account host whose setting has no value or a value that is not a host; the message names the model and the setting (`docs/model-routing.md`, "A per-account host") | 1 |
| `credential_missing` | the session model's credential cannot be found, its credential label names none, or a stdio MCP server's `{ "secret" }` env value names no stored credential, or a `ws`/`wss` hub entry has no device token | 1 |
| `credential_failed` | a stored credential is found but cannot be used, the provider's `credential()` call errors, or its `sign()` fails or returns unusable headers (`docs/model-routing.md`, "Credentials") | 1 |
| `authentication_failed` | the startup OAuth refresh of the session model's token was rejected by the token endpoint, or its `credential()` needed a person to log in and nobody was attached (`docs/model-routing.md`, "Keys, tokens and OAuth"), or `fiber mcp login` got no callback in time, or a listed hub refused the client's device token | 1 |
| `connection_failed` | the startup OAuth refresh could not reach the token endpoint, or a client could not reach a listed hub | 1 |
| `session_not_found` | a resume, or `fiber feedback --session`, names no session | 1 |
| `session_held` | another process holds the session's lock, or runs it at a schema version this build cannot read | 1 |
| `extension_missing` | the provider of a `provider/model` is not installed, or configuration declares hooks and the `hooks` extension is not installed (`docs/extensions.md`, "Hooks declared in configuration") | 1 |
| `extension_required_failed` | an extension marked `required` failed to start | 1 |
| `extension_not_installed` | `fiber extension remove <name>` found no directory for the name; the message names the extension and the directory it looked for | 1 |
| `mcp_required_server_failed` | an MCP server marked `required` failed to start | 1 |
| `extension_unapproved` | the repository declares a `required` extension nobody approved, and nobody could be asked (`docs/extensions.md`, "Code a repository ships") | 1 |
| `hook_unapproved` | the repository declares a `required` hook nobody approved, and nobody could be asked | 1 |
| `mcp_server_unapproved` | the repository declares a `required` MCP server nobody approved, and nobody could be asked, or `fiber mcp login` named a repository's server nobody approved | 1 |

Only `usage` exits 2, following the Unix convention (and clap's default) that
separates "called it wrong" from "ran and failed".

The credential check happens at startup, before `fiber_started`, so a headless
caller learns in milliseconds rather than at the first model call.
`credential_missing` means no key was found. `credential_failed` means Fiber
found one and could not use it. `authentication_failed` means the provider saw a
key and rejected it, or a login was needed and nobody was attached to answer.

Each code names one fix. `no_model` means nothing chose a model: choose one.
`model_ambiguous` means the id matches several providers: prefix the provider,
as `provider/model`. `no_model` also covers an installed provider that lacks the
named model, and a bare id no installed provider has: run `fiber models` and
name one it lists. `model_unconfigured` means the model needs its host: set
the setting it names. `extension_missing` means the provider is not installed:
install it. When configuration declares hooks, it means the `hooks`
extension is not installed: run `fiber extension install hooks`. None of these is retried.

### `model_unconfigured` remedies

Three fixes, in the order the message names them. `fiber login
<provider>` asks for the host after the credential is stored and saves it
globally; the message leaves it out for a provider whose data declares
`credential_label`, which reads no stored credential
(`docs/model-routing.md`, "A per-account host"). Setting the named setting in `config/<extension>.json` in Fiber
home fills it for every project at once. When the message names an
environment variable, exporting it in the shell profile and running `fiber
hub refresh` fills it: the hub reads its environment when it starts, so a
newly exported variable needs a refresh before the list sees it.

## A failed model call

A failed model call is an assistant message that completed with a failed
outcome and a code (`docs/events.md`, "Actions"). The retry
policy is `docs/model-routing.md`, "When a model call fails".

| Code | What it covers | Retried |
|---|---|---|
| `rate_limited` | HTTP 429 other than a quota or billing error; an OpenRouter in-flight budget 402 that carries a `Retry-After` in seconds; an AWS throttling exception ("Recognising an AWS exception") | yes |
| `provider_unavailable` | HTTP 5xx, including 503 and 529 overload, HTTP 408 and HTTP 409; an AWS server or model-stream exception ("Recognising an AWS exception") | yes |
| `connection_failed` | DNS, TLS, a refused or dropped connection, no address connecting within 15 seconds each, or a connection silent for 300 seconds (`docs/model-routing.md`, "When a model call fails"), or a client could not reach a listed hub | yes |
| `stream_incomplete` | a stream that ended before its protocol's terminal event, an `openai-responses` terminal event whose status is `in_progress` or `queued`, or an error inside an HTTP 200 response that no other code matches | yes |
| `quota_exceeded` | quota, billing or a subscription limit, as an HTTP status or inside the stream ("Recognising a quota or billing error") | never |
| `authentication_failed` | HTTP 401, a rejected key, an AWS exception refusing the signature or the credential ("Recognising an AWS exception"), an OAuth refresh the token endpoint rejected, a login with nobody attached, or a login that did not finish within `credential()`'s timeout | never |
| `context_overflow` | the request does not fit the model's context window | the overflow rule (`docs/handoff.md`, "Overflow") |
| `refused` | the provider declined to answer on policy grounds, including an `openai-completions` `finish_reason` of `content_filter`, a Gemini safety finish reason, and a `bedrock-converse` `stopReason` of `guardrail_intervened` or `content_filtered` | never |
| `model_not_found` | the provider's error body says it does not know the model: a code `model_not_found`, a `not_found_error` whose message names the model, Gemini's `NOT_FOUND` for a `models/` name, Google Vertex's `NOT_FOUND` for a `Publisher model`, which also covers a model the project may not call, OpenRouter's "is not a valid model ID" (`research/provider-errors/README.md`, "Unknown model"), or Bedrock's "The provided model identifier is invalid."; or AWS names a `ResourceNotFoundException`. A 404 alone is not enough, since a wrong base URL also returns 404 | never |
| `invalid_request` | any other HTTP 4xx; for a 404 the message names the status and the requested host and path, never the query string | never |
| `unknown_stop_reason` | a stop or finish reason the protocol does not map; the message carries the raw value | never |
| `paused_too_long` | a reply was still paused (`pause_turn`) after 5 continuations in a row; the message carries `pause_turn` | never |

Every stop or finish reason a protocol documents is mapped in its native
module. An unknown one fails the call as `unknown_stop_reason`, whatever the
protocol. `pause_turn` is not unknown: the loop continues it
(`docs/loop.md`, "A reply paused by a hosted tool"). Only a reply still
paused after the continuation bound fails the turn, with `paused_too_long`. A reason a vendor documents that Fiber has not mapped is a Fiber
bug, and a release fixes it.

The status alone cannot classify: OpenRouter sends an upstream's context
overflow as HTTP 200 with the error in the body or the last stream chunk. The
provider crate reads the body, per protocol and per upstream.

The Retried column is the default. A response header `x-should-retry: true`
or `false` overrides it for that response (`docs/model-routing.md`, "When a
model call fails"). It never overrides `quota_exceeded`, `unknown_stop_reason` or `paused_too_long`, which are never retried.

A wait longer than 60 seconds fails at once as `rate_limited` with
`retry_after_ms` set, so a person or a caller can decide.

### Recognising a context overflow

`context_overflow` is matched only on shapes that have been seen:

- OpenRouter's own check, any protocol: HTTP 400, "This endpoint's maximum
  context length is … tokens". The same message rejects an oversized
  `max_tokens`; its numbers tell the two apart.
- Anthropic and Amazon Bedrock: "prompt is too long".
- OpenAI chat completions: `context_length_exceeded`.
- OpenAI responses: `invalid_prompt` with "exceeds the context window".

Anything else is `invalid_request` with the provider's message. muse reports
an overflow as a generic "invalid parameters" 400, so on muse it is
`invalid_request`. A catch-all word match was rejected: a message such as
"max_tokens exceeds …" would start a handoff whose note request fails the same
way.

Fiber also checks its own token estimate before sending, and hands off at
the point `docs/handoff.md`, "Automatic", sets, so a provider-reported overflow is
the exception.

### Recognising a quota or billing error

`quota_exceeded` is matched only on the shapes vendors document
(`research/provider-errors/quota.md`, one fixture each in
`research/provider-errors/documented/`):

- Anthropic: HTTP 402 `billing_error`; HTTP 429 `rate_limit_error` with
  `details.error_code` `enforced_spend_limit_reached` and no `retry-after`;
  HTTP 400 `invalid_request_error` with a message beginning `You have
  reached your specified API usage limits` or `You have reached your
  specified workspace API usage limits`.
- OpenAI: HTTP 429 with `error.code` `credit_balance_exhausted`,
  `organization_spend_limit_exceeded`, `project_spend_limit_exceeded` or
  `organization_usage_limit_exceeded`, or `insufficient_quota` as the
  `error.type` or the `error.code`.
- Gemini: HTTP 402 on depleted Prepay credits; on any status, an
  `error.details[]` entry whose `@type` is
  `type.googleapis.com/google.rpc.ErrorInfo` and whose `reason` is
  `BILLING_DISABLED` or `RESOURCE_QUOTA_EXCEEDED`.
- OpenRouter: HTTP 402 "Your account or API key has insufficient credits".
- Inside a 200 stream: an Anthropic `error` event of the 402 or 429 shapes
  above; an OpenRouter chunk whose `error.code` is the number 402; a Gemini
  in-stream `error` whose `code` is the number 402 or whose `details` hold
  one of the quota `ErrorInfo` reasons above.

Authentication is checked first: a 401, or a 400 with an `API_KEY_INVALID`
reason, keeps `authentication_failed`. Any other match above is
`quota_exceeded`, whatever the status: it wins over a 429, a 5xx, a 404 and
an unknown-model or overflow shape, since only paying or raising a limit
fixes it.

What stays `rate_limited`: a bare `RESOURCE_EXHAUSTED`, with only a
`google.rpc.QuotaFailure` detail or none, in the status or the stream; a
Google `ErrorInfo` reason `RATE_LIMIT_EXCEEDED`; the Claude Code
workspace's spend-limit 429, which carries `retry-after` and has no field
telling it apart from a rate limit; and an OpenRouter in-flight budget 402
that carries a `Retry-After` in seconds.

Amazon Bedrock's `ServiceQuotaExceededException` is not matched: no probe has
reached it, so whether it is `quota_exceeded` or `rate_limited` is unsettled
("Not settled here"), and it is `invalid_request`, as any other 400 is.

### Recognising an AWS exception

AWS names an exception only in the `X-Amzn-ErrorType` header of a failed
response, and in the `:exception-type` of an exception message inside the
stream, on Bedrock's Invoke and Converse APIs alike (`docs/model-routing.md`,
"Bedrock Invoke wire facts" and "Bedrock Converse wire facts"). The name,
matched without regard to case, decides the code:

- `AccessDeniedException`, `UnrecognizedClientException`,
  `InvalidSignatureException`, `IncompleteSignatureException`,
  `MissingAuthenticationTokenException` and `ExpiredTokenException`:
  `authentication_failed`. Bedrock answers a malformed API key with
  `AccessDeniedException`.
- `ThrottlingException` and `ModelNotReadyException`: `rate_limited`.
- `InternalServerException`, `ServiceUnavailableException`,
  `ModelStreamErrorException`, `ModelTimeoutException` and
  `ModelErrorException`: `provider_unavailable`.
- `ResourceNotFoundException`: `model_not_found`.

Any other name, `ValidationException` among them, leaves the code to the
status and the body, as for any provider: Bedrock's "The provided model
identifier is invalid." is `model_not_found`, "prompt is too long"
`context_overflow`, and the rest of its 400s `invalid_request`. Inside the
stream a `validationException` is `invalid_request`, unless its message is an
overflow, and any other name `stream_incomplete`.

A usage-limit body wins over the name, then authentication, then a quota or
billing shape, as for every provider.

### Output tokens

A request's `max_tokens` never exceeds the model's `max_output_tokens` from its
provider data (`docs/configuration.md`, "A provider's data"). muse answers an
oversized `max_tokens` with 429 `rate_limit_exceeded` and `Retry-After: 60`, a
request that can never succeed. With correct provider data Fiber never sends
one. With wrong data the retries waste three minutes and then fail
`rate_limited`; the fix is the data, not a muse-specific match.

## What ends a turn

A tool call's failure never ends a turn: the model reads it as the call's
result. `turn_completed` is `failed`, with `error` set to the cause, on:

- a model call that failed after its retries, other than a handoff's note
  request (`docs/handoff.md`, "Recording"): that call's code
- `context_overflow` after the overflow rule's one retry, or with automatic
  handoff off (`docs/handoff.md`)
- `hook_failed` from a `turn_start`, `before_model_call` or `turn_end` hook
  (`docs/extensions.md`, "When a hook fails")
- `blocked`: with no human to answer, the session used up its block budget
  (`docs/permissions.md`, "Headless")
- `output_truncated`: a second reply in a row cut off by the output-token limit
  (`docs/loop.md`, "A reply cut off by the output limit")
- `paused_too_long`: a reply still paused after 5 continuations in a row (`docs/loop.md`, "A reply paused by a hosted tool")
- `budget_exceeded`: the session reached `budget.usd`, or a `before_model_call`
  hook refused the request (`docs/loop.md`, "Spending budget")

## Registry

Every code Fiber emits, except the codes only a driver command's rejection
carries, which `docs/invocation.md`, "Driver commands", lists. "Where" names
the lines that carry it.

| Code | Where | Meaning | Notice |
|---|---|---|---|
| `ambiguous_match` | tool call | an edit block's text occurs more than once (`docs/tools.md`, "File tools") | — |
| `authentication_failed` | exit, extension call, model call, turn, driver command (`rewind`), hub command | the provider rejected the credential, an OAuth refresh was rejected, a login was needed with nobody attached, or a login did not finish within `credential()`'s timeout, or `fiber mcp login` got no callback in time, or the server's discovery, registration or token exchange failed, or a listed hub refused the client's device token; or a hub login was cancelled before it started or at its store commit, with nothing stored, or the provider has no device-code form | — |
| `blocked` | turn | the block budget ran out with no human to answer | — |
| `blocked_host` | tool call | `web_fetch` named a link-local address or a cloud metadata host (`docs/tools.md`, "Web fetch and web search") | — |
| `budget_exceeded` | turn | the spending budget was reached, or an extension refused a model request (`docs/loop.md`, "Spending budget") | — |
| `busy` | driver command | `prompt` or `reload` while a turn is running, or `rewind` mid-turn (`docs/invocation.md`, "What each command does") | — |
| `cache_rebuild` | notice | the terminal rebuilt its completion cache after a model or credential switch (`docs/tui.md`, "Notices") | event |
| `cancelled` | driver command | a `cancel` stopped a `shell` before its command started (`docs/invocation.md`, "Driver commands") | — |
| `closing` | exit, tool call, extension call, driver command, notice, hub command | `session_message` named a session that was sent `close` (`docs/tools.md`, "Messaging other sessions"), or `state.set`, `state.unset` or `host.drive` ran once the session closed its extension state, just before `fiber_exited` (`docs/extensions.md`, "When a session ends"), or a driver command after `close` (`docs/invocation.md`, "Driver commands"), or the session `fiber ask` attached to ended before its turn completed, or `host.delegate` ran while its entry script runs, before the session's loop wired its inbox, after the session's end began stopping its jobs, or after its extension was stopped (`docs/extensions.md`, "Host calls") | event |
| `config_invalid` | exit, extension call, notice, driver command (`rewind`), hub command | a configuration or script file is invalid, or an MCP server declares both `command` and `url` or has a user name or password in its `url`, or `hub.default` names a missing entry or a stored hub address is invalid; or a hub configuration view cannot load its configuration | event |
| `connection_failed` | exit, extension call, model call, tool call, turn, notice, driver command (`rewind`), hub command | the connection to the provider or its token endpoint failed, or `web_fetch` could not reach the host, or `fiber mcp login` could not reach the server or its authorization server, or the terminal lost its hub connection, or a client could not reach a listed hub; or a device login on the hub could not reach its token endpoint | condition |
| `context_overflow` | model call, turn | the request does not fit the context window | — |
| `credential_failed` | exit, extension call, model call, turn, driver command (`rewind`), `mcp_server_failed`, tool call, notice, hub command | a stored credential cannot be used, the provider's `credential()` or `sign()` failed, or a stdio MCP server's `{ "secret" }` env value names a stored credential that cannot be read; log in again or fix the credential; a hub device login also returns this when its provider fails, times out or is cancelled during the poll | event |
| `credential_missing` | exit, extension call, driver command, `mcp_server_failed`, tool call, notice | no credential was found for the session's model, its credential label names none, or a stdio MCP server's `{ "secret" }` env value names no stored credential; the provider-credential messages list the provider's labels, while a stdio MCP server's message names the missing secret instead; a `ws`/`wss` hub entry has no device token | event |
| `depth_exceeded` | tool call, extension call | a delegate tool, or `host.delegate`, at depth 2 (`docs/delegates.md`) | — |
| `duplicate_command` | driver command | a command repeats the id of one the session already accepted, so it was not applied again (`docs/invocation.md`, "The command line") | — |
| `extension_incompatible` | exit, notice, driver command (`rewind`) | an extension needs a newer `fiber` or a different extension API version; `fiber extension install` refuses it and loading skips it (`docs/extensions.md`, "The extension API version") | event |
| `extension_missing` | exit, driver command (`rewind`), notice | the provider of a `provider/model` is not installed; or configuration declares hooks and the `hooks` extension is not installed (`docs/extensions.md`, "Hooks declared in configuration") | event |
| `extension_not_found` | exit, driver command (`rewind`) | an install names a repository or tag that does not exist, or a repository with no version tag; fix the name. Not retried automatically (`docs/extensions.md`, "Names" and "Versions") | — |
| `extension_not_installed` | exit, driver command (`rewind`) | `fiber extension remove` names an extension with no directory under `extensions/`; nothing is deleted (`docs/extensions.md`, "Installing") | — |
| `extension_required_failed` | exit, driver command (`rewind`) | a required extension failed to start | — |
| `extension_shadowed` | notice | a repository's approved copy of an extension loads in place of the personal install of the same name, or in place of another version of it that another of the repository's extensions brings; the message names both versions (`docs/extensions.md`, "Code a repository ships") | event |
| `extension_unapproved` | exit, driver command (`rewind`) | a repository's required extension is not approved; run `fiber approve` in the repository | — |
| `extension_unavailable` | tool call | the extension providing the tool died twice | — |
| `fetch_failed` | exit, driver command (`rewind`) | an install or update could not fetch: git or the network failed; try again later. Not retried automatically (`docs/extensions.md`, "Installing") | — |
| `flooded` | job | a monitor was suppressed for 30 seconds (`docs/tools.md`) | — |
| `history_loading` | notice | the terminal is still reading the session's history (`docs/tui.md`, "Notices") | condition |
| `hook_failed` | tool call, turn, handoff, notice, exit | an extension hook errored or ran out of time, or a TUI extension's renderer errored or returned the wrong shape (one notice for each extension and slot until `/reload`; `docs/tui.md`, "When an extension fails"); or an approved repository hook's pinned copy could not be made: a `required` one fails the start with exit 1 naming it, any other is skipped with a notice naming it (`docs/extensions.md`, "Hooks") | event |
| `hook_unapproved` | exit, driver command (`rewind`) | a repository's required hook is not approved; run `fiber approve` in the repository | — |
| `http_error` | extension call, tool call | `web_fetch` got a status other than 2xx, was redirected more than 10 times, or was redirected to a URL it cannot parse | — |
| `hub_version` | notice | the hub runs another Fiber version or commit than this terminal (`docs/tui.md`, "Notices") | event |
| `indeterminate` | tool call, job, driver command, hub command | Fiber cannot tell whether the call completed | — |
| `invalid_arguments` | driver command, hub command, extension call, tool call, notice | the arguments failed the tool's schema or checks, or a driver or hub command's `args` (`docs/invocation.md`, "Driver commands" and "The hub") | event |
| `invalid_request` | model call, turn | the provider rejected the request for any other reason | — |
| `io_failed` | exit, driver command, hub command, extension call, notice | a filesystem failure, or a `git` command on a worktree that failed: a log write or fsync, or a configuration or credential file that exists but cannot be read or written; the message names the path; or Fiber could not install its signal handlers, and the message starts `signals:`; or the terminal could not start its input reader, and the message starts `terminal:`; or the hub could not start the worker thread for a file search or a device login, or the file search worker stopped; the message says which | event |
| `log_corrupt` | exit, hub command, driver command (`rewind`), job | a log line that cannot be encoded, or one read back that does not parse | — |
| `login_url` | notice | an extension's `host.oauth.open` or `host.oauth.show` needs a person to finish a login; the message gives the URL to open, and the code to enter for a device-code login | condition |
| `malformed` | driver command, hub command, notice | a command line that is not a JSON object, has no string `id` or `command`, has `args` of the wrong type, or has a key no command line takes (`docs/invocation.md`, "Driver commands"), or a hub command's answer carried no code the terminal can read | event |
| `mcp_cancel_requested` | tool call | a cancelled call the server may still act on | — |
| `mcp_prompt_failed` | driver command | Fiber could not get an MCP server's prompt: the server failed, did not answer in time, refused it, or returned content Fiber cannot send | — |
| `mcp_required_server_failed` | exit, driver command (`rewind`) | a required MCP server failed to start | — |
| `mcp_server_unapproved` | exit, driver command (`rewind`) | a repository's required MCP server is not approved; run `fiber approve` in the repository, or `fiber mcp login` named a repository's server nobody approved | — |
| `mcp_server_unavailable` | tool call, MCP server | the server failed to start, died, or ended its session | — |
| `mcp_tool_removed` | tool call | the server has removed the tool | — |
| `message_refused` | tool call, driver command | a `before_message` hook refused a message: a session message, or a person's or a driver's | — |
| `model_ambiguous` | exit, driver command (`rewind`), hub command | a bare model id matches models of two or more installed providers; prefix the provider; or a hub `configure` operation sets `model` to an ambiguous bare id | — |
| `model_invalid` | notice | a model's `extra_body` names a field Fiber builds, its `web_search` names a type its protocol does not read, it declares no `context_window`, or its `thinking_default` is not among its `thinking_levels`, so the model is left out of the model list (`docs/model-routing.md`, "Extra request body fields", "Hosted web search", "Thinking") | event |
| `model_not_found` | model call, turn | the provider's error body says it does not know the model | — |
| `model_unconfigured` | exit, notice, driver command (`rewind`) | a model's base URL names a per-account host whose setting has no value or a value that is not a host, so the model is left out of the model list; the message names the model and the setting, or, in `fiber models` when the setting has no value, the provider, the setting and the count of models it leaves out (`docs/model-routing.md`, "A per-account host") | event |
| `models_loading` | notice | the terminal's model list is not read yet (`docs/tui.md`, "Notices") | condition |
| `name_pinned` | tool call | `name_session` was called while the person's name pins the session | — |
| `no_match` | tool call | an edit block's text was not found in the file | — |
| `no_model` | exit, notice, driver command (`rewind`), hub command | nothing chose a model, an installed provider lacks the named model, or no installed provider has a bare model id; or a hub `configure` operation sets `model` to one no installed provider has | event |
| `nonzero_exit` | tool call, job | a process exited nonzero | — |
| `not_found` | extension call, tool call, hub command, driver command | the path `read` or `edit` names does not exist, or `read_file` names no file (`docs/invocation.md`, "A session's files"), or `revoke` names no paired device (`docs/invocation.md`, "Remote clients"), or `artifact_page` names no file (`docs/invocation.md`, "Driver commands") | — |
| `notes_unchanged` | notice | `/reviewer-setup` reached its diff and the save would change neither layer, so no file was written (`docs/tui.md`, "Rules") | event |
| `orphaned` | job | the process that ran the job died | — |
| `output_cap` | job | a job's output file passed 5 GB | — |
| `output_truncated` | tool call, turn, handoff | a reply was cut off by the output-token limit, so its calls did not run | — |
| `pairing_failed` | hub connection | a pairing code was wrong, already used or more than 10 minutes old (`docs/invocation.md`, "Remote clients") | — |
| `path_changed` | tool call | a symbolic link changed between the permission decision and the read or write | — |
| `paused_too_long` | turn | a reply was still paused (`pause_turn`) after 5 continuations in a row; send the request again or simplify it | — |
| `provider_unavailable` | model call, turn | a provider server error or overload | — |
| `quota_exceeded` | model call, turn | a quota, billing or subscription limit | — |
| `rate_limited` | extension call, model call, turn | the provider rate-limited the request | — |
| `refused` | model call, turn | the provider declined on policy grounds | — |
| `repository_code_skipped` | notice | an extension, hook or MCP server the repository declares was skipped, because nobody approved it and nobody could be asked (`docs/extensions.md`, "Code a repository ships") | event |
| `reviewer_selection_failed` | notice | the reviewer could not choose which of the person's messages still bind at a handoff, so it kept every one | event |
| `session_has_dependents` | exit, hub command, driver command (`rewind`) | a delete names a session that forks or rewinds point at; the message lists them, and `--cascade` deletes them too (`docs/invocation.md`, "Deleting and pruning") | — |
| `session_held` | exit, hub command, notice, driver command (`rewind`) | another process holds the session, or runs it at a schema version this build cannot read (`docs/invocation.md`, "Processes"), or the terminal cannot attach: the session runs another Fiber build than this terminal, or its status did not parse | event |
| `session_not_found` | exit, hub command, notice, driver command (`rewind`) | a resume names no session, or `fiber feedback --session` names no session or names a delegate (`docs/invocation.md`, "Commands and flags"), or a command whose `session_id` names no session, running or exited, or names a delegate that is not running, or a hub command's `session` argument names none, or a `start` `continues` names a session whose worktree is gone (`docs/invocation.md`, "The hub" and "Lifecycle"), or the terminal has no session on screen | event |
| `signal` | tool call, job, diagnostic log | a process killed by a signal Fiber did not send, or Fiber itself stopped by one before it started writing `fiber_started` | — |
| `skill_invalid` | notice | a skill's `SKILL.md` header does not parse or lacks `name` or `description`, so it is left out; the message names its path (`docs/system-prompt.md`, "Skills") | event |
| `skill_shadowed` | notice | two skills share a name, an MCP server's prompt is named like a skill, an extension's command is named like a skill or prompt template, an MCP server's prompt is named like an extension's command, or two servers' prompts share a name; the message names both and which one won (`docs/system-prompt.md`, "Skills") | event |
| `skills_large` | notice | the skills listing passes 10% of the context window; the message names the sources that add the most (`docs/system-prompt.md`, "Size") | event |
| `stale_file` | tool call | a write would replace a file the session has not seen in its current state | — |
| `stale_request` | driver command, hub command, notice | the command names a request, steering message, job or turn that is no longer pending, queued or running, or a `dismiss` names a session that is not a crashed session in the feed, or a cascading `delete`'s `expect` differs from the sessions it would remove; the message names them (`docs/invocation.md`, "Deleting and pruning"), or the terminal has no request waiting; or a Ctrl+G `write_config` version no longer matches the file on the hub; or a newer `search_files` query superseded this one, a device-login command has no login, or another result command is already waiting | event |
| `state_too_large` | extension call | a state value over 64 KiB | — |
| `stream_closed` | notice | the hub could not read the open session's log to serve this terminal, so the conversation stops at the last line it received; reopening the session subscribes again (`docs/tui.md`, "Notices") | condition |
| `stream_incomplete` | model call, turn | the stream ended early or carried an unmatched error | — |
| `summary_failed` | driver command | a `rewind` that asked for a summary could not get one, so no new session was created (`docs/events.md`, "Rewind") | — |
| `switch_pending` | notice | a model or level choice sent while a turn runs: it applies when the turn ends, and the chips hold the old model and level until the session accepts it (`docs/tui.md`, "Notices") | condition |
| `timeout` | extension call, tool call, job, diagnostic log | a deadline passed | — |
| `too_large` | extension call, tool call, hub command | a `web_fetch` download, or a file `read_file` names, larger than 10 MiB; or Ctrl+G configuration text exceeds 64 KiB | — |
| `tool_error` | tool call, extension call | the tool itself failed, or its effects function errored | — |
| `unauthenticated` | hub connection | a remote connection's first message presented no valid device token; the hub closes the connection (`docs/invocation.md`, "Remote clients") | — |
| `unknown_command` | driver command, hub command | a driver or hub command Fiber does not have, or a `command` naming one no extension registered (`docs/invocation.md`, "Driver commands") | — |
| `unknown_stop_reason` | model call, turn | the reply ended with a stop or finish reason Fiber does not map | — |
| `unknown_tool` | tool call | the model named a tool that does not exist | — |
| `unreachable` | tool call | `session_message` named an id no running session has | — |
| `unreadable_reply` | extension call, permission request, handoff | a model replied, but not in the format Fiber asked for, such as a reviewer verdict that could not be read on the second ask (`docs/permissions.md`, "What happens on a block") | — |
| `unsupported_file` | extension call, tool call | a file tool was given a directory, device or file it cannot handle | — |
| `usage` | exit, driver command (`rewind`), hub command | the invocation or its environment is wrong, including an unknown `--hub` name; exits 2; or a hub configuration operation is refused by the configuration or credential writer | — |
| `version_conflict` | exit, driver command (`rewind`) | an install needs two majors of one dependency, no tag meets a minimum, or the versions cannot be settled; pick compatible versions. Not retried automatically (`docs/extensions.md`, "Versions") | — |

Notices, for a failure outside any action or a login a person must finish:

| Code | Class | Meaning |
|---|---|---|
| `command_conflict` | event | two extensions registered the same command name, replaced the same TUI slot, or bound the same key |
| `config_key_ignored` | event | an unknown key, a key a repository may not set, a configured `thinking` level the session's model does not declare (`docs/model-routing.md`, "Thinking"), or `tools."tool_search".deferred`, which has no effect (`docs/tools.md`, "Tool search") |
| `extension_failed` | event | an extension failed to start or missed its deadline, its `init.lua` registered a provider, or its install record is missing or unreadable, so loading skipped it; or loading skipped one of its registrations; or a provider's `providers/<name>.lua` failed to load on first use; or one of its commands failed; or a TUI extension failed to load or passed its loading timeout, or a timeout switched it off until `/reload`; the message names which (`docs/extensions.md`, "Installing") |
| `extension_incompatible` | event | an extension needs a newer `fiber` or a different extension API version, so loading skipped it; or a TUI extension draws nothing for a session whose session half it does not support; the message names both versions when present, or the TUI version plus "not loaded" for an omitted half |
| `extension_missing` | event | configuration declares hooks but the `hooks` extension is not installed, so its declared hooks will not run; the message says to run `fiber extension install hooks` |
| `extension_shadowed` | event | a repository's approved copy of an extension loads in place of the personal install of the same name, or in place of another version of it that another of the repository's extensions brings; the message names both versions |
| `hook_failed` | event | a `non-blocking` hook, a watcher, or an extension delegate's `run` failed; or an approved non-required repository hook's pinned copy could not be made: it is skipped with a notice naming it (`docs/extensions.md`, "Hooks") |
| `hub_version` | event | the hub runs another Fiber version or commit than this terminal (`docs/tui.md`, "Notices") |
| `instructions_large` | event | the instruction text passes 10% of the context window (`docs/system-prompt.md`, "Size") |
| `login_url` | condition | an extension's `host.oauth.open` or `host.oauth.show` needs a person to finish a login, and a `full` client is attached; the message gives the URL to open, and the code to enter for a device-code login (`docs/extensions.md`, "Host calls") |
| `model_invalid` | event | a model's `extra_body` names a field Fiber builds, such as `tools`, its `web_search` names a type its protocol does not read, it declares no `context_window`, or its `thinking_default` is not among its `thinking_levels`, so the model is left out of the model list; the message names the model and the field or type (`docs/model-routing.md`, "Extra request body fields", "Hosted web search", "Thinking") |
| `model_unconfigured` | event | a model's base URL names a per-account host whose setting has no value or a value that is not a host, so the model is left out of the model list; the message names the model and the setting, or, in `fiber models` when the setting has no value, the provider, the setting and the count of models it leaves out (`docs/model-routing.md`, "A per-account host") |
| `no_model` | event | nothing chose the reviewer's model; set `reviewer.model` (`docs/permissions.md`, "How it runs") |
| `repository_code_skipped` | event | an extension, hook or MCP server the repository declares was skipped, unapproved, with nobody to ask; the message names it and says to run `fiber approve` |
| `reviewer_selection_failed` | event | the reviewer could not choose which of the person's messages still bind at a handoff, so it kept every one; the message names the cause |
| `skill_invalid` | event | a skill's `SKILL.md` header does not parse or lacks `name` or `description`, so it is left out; the message names its path (`docs/system-prompt.md`, "Skills") |
| `skill_shadowed` | event | two skills share a name, an MCP server's prompt is named like a skill, an extension's command is named like a skill or prompt template, an MCP server's prompt is named like an extension's command, or two servers' prompts share a name; the message names both and which one won (`docs/system-prompt.md`, "Skills") |
| `skills_large` | event | the skills listing passes 10% of the context window; the message names the sources that add the most (`docs/system-prompt.md`, "Size") |
| `tool_definitions_large` | event | full tool definitions take more than 10% of the context window |
| `web_search_unavailable` | event | `web_search` is not declared: several search backends are installed and `web_search.backend` is unset, or it names a backend that is not installed; the message names which (`docs/tools.md`, "Web fetch and web search") |

Driver command rejections (`malformed`, `not_subscribed`, `busy`, `stale_request`, `not_step_boundary`,
`session_held`, `delegate_session`, `summary_failed`, `invalid_arguments`, `not_found`,
`unknown_command`, `closing`, `indeterminate`, `cancelled`, `io_failed`, `duplicate_command`, `session_not_found`,
`message_refused`, `hook_failed`, `mcp_prompt_failed`, `authentication_failed`, `config_invalid`, `connection_failed`, `credential_failed`, `credential_missing`, `extension_failed`, `extension_incompatible`, `extension_missing`, `extension_not_found`, `extension_not_installed`, `extension_required_failed`, `extension_unapproved`, `fetch_failed`, `hook_unapproved`, `log_corrupt`, `mcp_required_server_failed`, `mcp_server_unapproved`, `model_ambiguous`, `model_unconfigured`, `no_model`, `session_has_dependents`, `usage`, `version_conflict`)
are `docs/invocation.md`, "Driver commands".

Codes no table lists — a refusal or driver code the hub names
dynamically — are events: they fade after one showing.

## Not settled here

- What a session does when a log write fails, such as a full disk. The code is
  `io_failed`.
- Rate-limit, overload and refusal bodies were not reached by
  the probe; their matches rest on protocol documentation until one is seen.
  Quota and billing matches rest on vendor documentation
  (`research/provider-errors/quota.md`) until a live body is seen.
  AWS's throttling, server and model-stream exceptions, and every exception
  inside a Bedrock stream, rest on AWS's documentation of Bedrock's errors
  until one is seen. Bedrock's `ServiceQuotaExceededException` is unmatched
  until a probe shows whether it is a quota or a rate limit.
  ChatGPT/codex documents no usage-limit body, so its match rests on reference
  implementations (`docs/model-routing.md`, "Protocols and providers").
