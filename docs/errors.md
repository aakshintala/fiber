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

A failed model call adds two optional fields:

- `retry_after`, in seconds, when the provider asked Fiber to wait.
- `provider { name, status, message }`: the provider's name, the HTTP status and
  the provider's own message. Provider messages can mislead (OpenRouter answers
  a bad key with "Missing Authentication header"), which is why they sit here
  and not in `message`.

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
- **`fiber serve`** runs many turns, and a driver has seen each one's
  `turn_completed`. A failed turn does not fail the process. `serve` exits 1
  with an `error` only when the process itself failed.
- **A signal** exits 129, 130 or 143 with no `error`; the exit code says what
  happened (`docs/invocation.md`, "Shutdown").

### Before a session exists

A failure before `fiber_started` has no session and no log. `fiber ask` and
`fiber serve` still print one `fiber_exited` line on stdout, carrying the exit
code and `error`, and one sentence on stderr. The line has no `session_id`, so
filtering stdout by session still gives each session's log byte for byte, and
`fiber ask … | tail -1` reads the verdict whatever happened. The terminal door
prints the sentence on stderr only.

| Code | When | Exit |
|---|---|---|
| `usage` | a bad flag, two prompt sources, no prompt with stdin on a terminal, `fiber` without a tty | 2 |
| `config_invalid` | invalid JSON or a value of the wrong type in a configuration file (`docs/configuration.md`) | 1 |
| `no_model` | nothing chose a model (`docs/model-routing.md`, "Choosing the model") | 1 |
| `credential_missing` | the session model's credential cannot be found | 1 |
| `session_not_found` | a resume names no session | 1 |
| `session_held` | another process holds the session's lock | 1 |
| `extension_missing` | the repository declares an extension that is not installed, or the session model's provider is not installed | 1 |
| `extension_unapproved` | the repository brings an extension nobody approved | 1 |
| `extension_required_failed` | an extension marked `required` failed to start | 1 |
| `mcp_required_server_failed` | an MCP server marked `required` failed to start | 1 |
| `mcp_server_unapproved` | the repository declares an MCP server nobody approved | 1 |

Only `usage` exits 2, following the Unix convention (and clap's default) that
separates "called it wrong" from "ran and failed".

The credential check happens at startup, before `fiber_started`, so a headless
caller learns in milliseconds rather than at the first model call.
`credential_missing` means no key was found. `authentication_failed` means the
provider saw a key and rejected it.

## A failed model call

A failed model call is an assistant message that completed with a failed
outcome, a code and an attempt number (`docs/events.md`, "Actions"). The retry
policy is `docs/model-routing.md`, "When a model call fails".

| Code | What it covers | Retried |
|---|---|---|
| `rate_limited` | HTTP 429 | yes |
| `provider_unavailable` | HTTP 5xx, including 503 and 529 overload, HTTP 408 and HTTP 409 | yes |
| `connection_failed` | DNS, TLS, a refused or dropped connection | yes |
| `stream_incomplete` | a stream that ended before its protocol's terminal event, an `openai-responses` terminal event whose status is `in_progress` or `queued`, or an error inside an HTTP 200 response that no other code matches | yes |
| `quota_exceeded` | quota, billing or a subscription limit | never |
| `authentication_failed` | HTTP 401, a rejected key, an OAuth refresh that failed | never |
| `context_overflow` | the request does not fit the model's context window | the overflow rule (`docs/handoff.md`, "Overflow") |
| `refused` | the provider declined to answer on policy grounds, including an `openai-completions` `finish_reason` of `content_filter` and a Gemini safety finish reason | never |
| `model_not_found` | the provider does not know the model | never |
| `invalid_request` | any other HTTP 4xx | never |
| `unknown_stop_reason` | a stop or finish reason the protocol does not map; the message carries the raw value | never |

Every stop or finish reason a protocol documents is mapped in its native
module. An unknown one fails the call as `unknown_stop_reason`, whatever the
protocol. A reason a vendor documents that Fiber has not mapped is a Fiber
bug, and a release fixes it.

The status alone cannot classify: OpenRouter sends an upstream's context
overflow as HTTP 200 with the error in the body or the last stream chunk. The
provider crate reads the body, per protocol and per upstream.

The Retried column is the default. A response header `x-should-retry: true`
or `false` overrides it for that response (`docs/model-routing.md`, "When a
model call fails"). It never overrides `quota_exceeded` or
`unknown_stop_reason`, which are never retried.

A wait longer than 60 seconds fails at once as `rate_limited` with
`retry_after` set, so a person or a caller can decide.

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

Fiber also checks its own token estimate before sending, and hands off at 0.7
of the window by default (`docs/handoff.md`), so a provider-reported overflow is
the exception.

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

- a model call that failed after its retries: that call's code
- `context_overflow` after the overflow rule's one retry, or with automatic
  handoff off (`docs/handoff.md`)
- `hook_failed` from a `turn_start` or `turn_end` hook (`docs/extensions.md`,
  "When a hook fails")
- `blocked`: with no human to answer, the session used up its block budget
  (`docs/permissions.md`, "Headless")
- `output_truncated`: a second reply in a row cut off by the output-token limit
  (`docs/loop.md`, "A reply cut off by the output limit")

## Registry

Every code Fiber emits. "Where" names the lines that carry it.

| Code | Where | Meaning |
|---|---|---|
| `ambiguous_match` | tool call | an edit block's text occurs more than once (`docs/tools.md`, "File tools") |
| `authentication_failed` | model call, turn | the provider rejected the credential |
| `blocked` | turn | the block budget ran out with no human to answer |
| `closing` | tool call | `session_message` named a session that was sent `close` (`docs/tools.md`, "Messaging other sessions") |
| `config_invalid` | exit | a configuration file is invalid |
| `connection_failed` | model call, turn | the connection to the provider failed |
| `context_overflow` | model call, turn | the request does not fit the context window |
| `credential_missing` | exit | no credential was found for the session's model |
| `depth_exceeded` | tool call | a delegate tool at depth 2 (`docs/delegates.md`) |
| `extension_missing` | exit | a declared extension, or the session model's provider, is not installed |
| `extension_required_failed` | exit | a required extension failed to start |
| `extension_unapproved` | exit | a repository's extension is not approved |
| `extension_unavailable` | tool call | the extension providing the tool died twice |
| `flooded` | job | a monitor was suppressed for 30 seconds (`docs/tools.md`) |
| `hook_failed` | tool call, turn, handoff, notice | an extension hook errored or ran out of time |
| `indeterminate` | tool call, job | Fiber cannot tell whether the call completed |
| `invalid_arguments` | tool call | the arguments failed the tool's schema or checks |
| `invalid_request` | model call, turn | the provider rejected the request for any other reason |
| `mcp_cancel_requested` | tool call | a cancelled call the server may still act on |
| `mcp_required_server_failed` | exit | a required MCP server failed to start |
| `mcp_server_unapproved` | exit | a repository's MCP server is not approved |
| `mcp_server_unavailable` | tool call, MCP server | the server failed to start or died |
| `mcp_tool_removed` | tool call | the server has removed the tool |
| `message_refused` | tool call | the target session's `before_message` refused a session message |
| `model_not_found` | model call, turn | the provider does not know the model |
| `name_pinned` | tool call | `name_session` was called while the person's name pins the session |
| `no_match` | tool call | an edit block's text was not found in the file |
| `no_model` | exit | nothing chose a model |
| `nonzero_exit` | tool call, job | a process exited nonzero |
| `not_found` | tool call | the path `read` or `edit` names does not exist |
| `orphaned` | job | the process that ran the job died |
| `output_cap` | job | a job's output file passed 5 GB |
| `output_truncated` | tool call, turn | a reply was cut off by the output-token limit, so its calls did not run |
| `path_changed` | tool call | a symbolic link changed between the permission decision and the write |
| `provider_unavailable` | model call, turn | a provider server error or overload |
| `quota_exceeded` | model call, turn | a quota, billing or subscription limit |
| `rate_limited` | model call, turn | the provider rate-limited the request |
| `refused` | model call, turn | the provider declined on policy grounds |
| `session_held` | exit | another process holds the session |
| `session_not_found` | exit | a resume names no session |
| `signal` | tool call, job | a process killed by a signal Fiber did not send |
| `stale_file` | tool call | a write would replace a file the session has not seen in its current state |
| `state_too_large` | extension call | a state value over 64 KiB |
| `stream_incomplete` | model call, turn | the stream ended early or carried an unmatched error |
| `timeout` | tool call, job | a deadline passed |
| `tool_error` | tool call | the tool itself failed, or its effects function errored |
| `unreachable` | tool call | `session_message` named an id no running session has |
| `unknown_stop_reason` | model call, turn | the reply ended with a stop or finish reason Fiber does not map |
| `unknown_tool` | tool call | the model named a tool that does not exist |
| `unsupported_file` | tool call | a file tool was given a directory, device or file it cannot handle |
| `usage` | exit | Fiber was called wrongly; exits 2 |

Notices, for a failure outside any action:

| Code | Meaning |
|---|---|
| `command_conflict` | two extensions registered the same command name |
| `config_key_ignored` | an unknown key, or a key a repository may not set |
| `extension_failed` | an extension failed to start or missed its deadline |
| `hook_failed` | a `non-blocking` hook or a watcher failed |
| `tool_definitions_large` | full tool definitions take more than 10% of the context window |

Driver command rejections (`malformed`, `busy`, `stale_request`, `not_step_boundary`,
`session_held`, `delegate_session`, `invalid_arguments`, `unknown_command`,
`closing`)
are `docs/invocation.md`, "Driver commands".

## Not settled here

- What a session does when a log write fails, such as a full disk, and which
  code `fiber serve` exits with then.
- Rate-limit, overload, quota, billing and refusal bodies were not reached by
  the probe; their matches rest on protocol documentation until one is seen.
