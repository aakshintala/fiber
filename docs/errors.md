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

### Before a session exists

A failure before `fiber_started` has no session and no log. `fiber ask`
still prints one `fiber_exited` line on stdout, carrying the exit
code and `error`, and one sentence on stderr. The line has no `session_id`, so
filtering stdout by session still gives each session's log byte for byte, and
`fiber ask … | tail -1` reads the verdict whatever happened. A session the
hub started writes the same line, and the hub passes it to the client that
asked for the session.

| Code | When | Exit |
|---|---|---|
| `usage` | the invocation or its environment is wrong: a bad flag, no prompt with stdin on a terminal, `fiber` without a tty, an empty or relative `FIBER_HOME`, `git` is not installed | 2 |
| `config_invalid` | invalid JSON or a value of the wrong type in a configuration file (`docs/configuration.md`) | 1 |
| `io_failed` | a filesystem failure: a log write or fsync, or a configuration or credential file that exists but cannot be read or written; the message names the path | 1 |
| `log_corrupt` | a log line that cannot be encoded, or one read back that does not parse | 1 |
| `no_model` | nothing chose a model, an installed provider lacks the named model, or no installed provider has a bare model id (`docs/model-routing.md`, "Naming a model") | 1 |
| `model_ambiguous` | a bare model id matches models of two or more installed providers; the message lists every match (`docs/model-routing.md`, "Naming a model") | 1 |
| `credential_missing` | the session model's credential cannot be found, or its credential label names none | 1 |
| `credential_failed` | a stored credential is found but cannot be used, the provider's `credential()` call errors, or its `sign()` fails or returns unusable headers (`docs/model-routing.md`, "Credentials") | 1 |
| `session_not_found` | a resume names no session | 1 |
| `session_held` | another process holds the session's lock | 1 |
| `extension_missing` | the provider of a `provider/model` is not installed | 1 |
| `protocol_unsupported` | the session model's protocol is one this Fiber does not speak yet | 1 |
| `extension_required_failed` | an extension marked `required` failed to start | 1 |
| `mcp_required_server_failed` | an MCP server marked `required` failed to start | 1 |
| `extension_unapproved` | the repository declares a `required` extension nobody approved, and nobody could be asked (`docs/extensions.md`, "Code a repository ships") | 1 |
| `hook_unapproved` | the repository declares a `required` hook nobody approved, and nobody could be asked | 1 |
| `mcp_server_unapproved` | the repository declares a `required` MCP server nobody approved, and nobody could be asked | 1 |

Only `usage` exits 2, following the Unix convention (and clap's default) that
separates "called it wrong" from "ran and failed".

The credential check happens at startup, before `fiber_started`, so a headless
caller learns in milliseconds rather than at the first model call.
`credential_missing` means no key was found. `credential_failed` means Fiber
found one and could not use it. `authentication_failed` means the provider saw a
key and rejected it.

Each code names one fix. `no_model` means nothing chose a model: choose one.
`model_ambiguous` means the id matches several providers: prefix the provider,
as `provider/model`. `no_model` also covers an installed provider that lacks the
named model, and a bare id no installed provider has: run `fiber models` and
name one it lists. `extension_missing` means the provider is not installed:
install it. `protocol_unsupported` means the provider is installed
but Fiber cannot speak its protocol: pick another model. None of these is
retried.

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
| `authentication_failed` | HTTP 401, a rejected key, an OAuth refresh the token endpoint rejected | never |
| `context_overflow` | the request does not fit the model's context window | the overflow rule (`docs/handoff.md`, "Overflow") |
| `refused` | the provider declined to answer on policy grounds, including an `openai-completions` `finish_reason` of `content_filter` and a Gemini safety finish reason | never |
| `model_not_found` | the provider does not know the model | never |
| `invalid_request` | any other HTTP 4xx | never |
| `unknown_stop_reason` | a stop or finish reason the protocol does not map; the message carries the raw value | never |

Every stop or finish reason a protocol documents is mapped in its native
module. An unknown one fails the call as `unknown_stop_reason`, whatever the
protocol. `pause_turn` is not unknown: the loop continues it
(`docs/loop.md`, "A reply paused by a hosted tool"). Only a reply still
paused after the continuation bound fails the turn with `unknown_stop_reason`. A reason a vendor documents that Fiber has not mapped is a Fiber
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
- `hook_failed` from a `turn_start`, `before_model_call` or `turn_end` hook
  (`docs/extensions.md`, "When a hook fails")
- `blocked`: with no human to answer, the session used up its block budget
  (`docs/permissions.md`, "Headless")
- `output_truncated`: a second reply in a row cut off by the output-token limit
  (`docs/loop.md`, "A reply cut off by the output limit")
- `budget_exceeded`: the session reached `budget.usd`, or a `before_model_call`
  hook refused the request (`docs/loop.md`, "Spending budget")

## Registry

Every code Fiber emits. "Where" names the lines that carry it.

| Code | Where | Meaning |
|---|---|---|
| `ambiguous_match` | tool call | an edit block's text occurs more than once (`docs/tools.md`, "File tools") |
| `authentication_failed` | model call, turn | the provider rejected the credential |
| `blocked` | turn | the block budget ran out with no human to answer |
| `blocked_host` | tool call | `web_fetch` named a link-local address or a cloud metadata host (`docs/tools.md`, "Web fetch and web search") |
| `budget_exceeded` | turn | the spending budget was reached, or an extension refused a model request (`docs/loop.md`, "Spending budget") |
| `busy` | driver command | `prompt` or `reload` while a turn is running, or `rewind` mid-turn (`docs/invocation.md`, "What each command does") |
| `closing` | tool call, extension call, driver command | `session_message` named a session that was sent `close` (`docs/tools.md`, "Messaging other sessions"), or `state.set` or `state.unset` ran after `fiber_exited` (`docs/extensions.md`, "When a session ends"), or a driver command after `close` (`docs/invocation.md`, "Driver commands") |
| `config_invalid` | exit | a configuration file is invalid |
| `connection_failed` | model call, turn | the connection to the provider failed |
| `context_overflow` | model call, turn | the request does not fit the context window |
| `credential_failed` | exit, model call, turn | a stored credential cannot be used, or the provider's `credential()` or `sign()` failed; log in again or fix the credential |
| `credential_missing` | exit | no credential was found for the session's model, or its credential label names none; the message lists the provider's labels |
| `depth_exceeded` | tool call | a delegate tool at depth 2 (`docs/delegates.md`) |
| `duplicate_command` | driver command | a command repeats the id of one the session already accepted, so it was not applied again (`docs/invocation.md`, "The command line") |
| `extension_incompatible` | exit, notice | an extension needs a newer `fiber` or a different extension API version; `fiber extension install` refuses it and loading skips it (`docs/extensions.md`, "The extension API version") |
| `extension_missing` | exit | the provider of a `provider/model` is not installed |
| `extension_not_found` | exit | an install names a repository or tag that does not exist; fix the name. Not retried automatically (`docs/extensions.md`, "Names") |
| `extension_required_failed` | exit | a required extension failed to start |
| `extension_shadowed` | notice | a repository's approved copy of an extension loads in place of the personal install of the same name; the message names both versions (`docs/extensions.md`, "Code a repository ships") |
| `extension_unapproved` | exit | a repository's required extension is not approved; run `fiber approve` in the repository |
| `extension_unavailable` | tool call | the extension providing the tool died twice |
| `fetch_failed` | exit | an install or update could not fetch: git or the network failed; try again later. Not retried automatically (`docs/extensions.md`, "Installing") |
| `flooded` | job | a monitor was suppressed for 30 seconds (`docs/tools.md`) |
| `hook_failed` | tool call, turn, handoff, notice | an extension hook errored or ran out of time |
| `hook_unapproved` | exit | a repository's required hook is not approved; run `fiber approve` in the repository |
| `http_error` | tool call | `web_fetch` got a status other than 2xx |
| `indeterminate` | tool call, job | Fiber cannot tell whether the call completed |
| `invalid_arguments` | tool call, driver command | the arguments failed the tool's schema or checks, or a driver command's `args` (`docs/invocation.md`, "Driver commands") |
| `invalid_request` | model call, turn | the provider rejected the request for any other reason |
| `io_failed` | exit | a filesystem failure: a log write or fsync, or a configuration or credential file that exists but cannot be read or written; the message names the path |
| `log_corrupt` | exit | a log line that cannot be encoded, or one read back that does not parse |
| `mcp_cancel_requested` | tool call | a cancelled call the server may still act on |
| `mcp_required_server_failed` | exit | a required MCP server failed to start |
| `mcp_server_unapproved` | exit | a repository's required MCP server is not approved; run `fiber approve` in the repository |
| `mcp_server_unavailable` | tool call, MCP server | the server failed to start or died |
| `mcp_tool_removed` | tool call | the server has removed the tool |
| `message_refused` | tool call | the target session's `before_message` refused a session message |
| `model_ambiguous` | exit | a bare model id matches models of two or more installed providers; prefix the provider |
| `model_invalid` | notice | a model's `extra_body` names a field Fiber builds, or its `web_search` names a type its protocol does not read, so the model is left out of the model list (`docs/model-routing.md`, "Extra request body fields", "Hosted web search") |
| `model_not_found` | model call, turn | the provider does not know the model |
| `model_unconfigured` | notice | a model's base URL names a per-account host whose setting has no value, so the model is left out of the model list; the message names the setting (`docs/model-routing.md`, "A per-account host") |
| `name_pinned` | tool call | `name_session` was called while the person's name pins the session |
| `no_match` | tool call | an edit block's text was not found in the file |
| `no_model` | exit, notice | nothing chose a model, or an installed provider lacks the named model |
| `nonzero_exit` | tool call, job | a process exited nonzero |
| `not_found` | tool call, hub command | the path `read` or `edit` names does not exist, or `read_file` names no file (`docs/invocation.md`, "A session's files") |
| `orphaned` | job | the process that ran the job died |
| `output_cap` | job | a job's output file passed 5 GB |
| `output_truncated` | tool call, turn | a reply was cut off by the output-token limit, so its calls did not run |
| `pairing_failed` | hub connection | a pairing code was wrong, already used or more than 10 minutes old (`docs/invocation.md`, "Remote clients") |
| `path_changed` | tool call | a symbolic link changed between the permission decision and the read or write |
| `protocol_unsupported` | exit | the model's protocol is one this Fiber does not speak yet; pick another model |
| `provider_unavailable` | model call, turn | a provider server error or overload |
| `quota_exceeded` | model call, turn | a quota, billing or subscription limit |
| `rate_limited` | model call, turn | the provider rate-limited the request |
| `refused` | model call, turn | the provider declined on policy grounds |
| `repository_code_skipped` | notice | an extension, hook or MCP server the repository declares was skipped, because nobody approved it and nobody could be asked (`docs/extensions.md`, "Code a repository ships") |
| `session_has_dependents` | exit | a delete names a session that forks or rewinds point at; the message lists them, and `--cascade` deletes them too (`docs/invocation.md`, "Deleting and pruning") |
| `session_held` | exit | another process holds the session |
| `session_not_found` | exit | a resume names no session |
| `signal` | tool call, job | a process killed by a signal Fiber did not send |
| `skill_invalid` | notice | a skill's `SKILL.md` header does not parse or lacks `name` or `description`, so it is left out; the message names its path (`docs/system-prompt.md`, "Skills") |
| `skill_shadowed` | notice | two skills share a name; the message names both paths and which one won (`docs/system-prompt.md`, "Skills") |
| `skills_large` | notice | the skills listing passes 10% of the context window; the message names the sources that add the most (`docs/system-prompt.md`, "Size") |
| `stale_file` | tool call | a write would replace a file the session has not seen in its current state |
| `stale_request` | driver command | the command names a request, steering message, job or turn that is no longer pending, queued or running (`docs/invocation.md`) |
| `state_too_large` | extension call | a state value over 64 KiB |
| `stream_incomplete` | model call, turn | the stream ended early or carried an unmatched error |
| `summary_failed` | driver command | a `rewind` that asked for a summary could not get one, so no new session was created (`docs/events.md`, "Rewind") |
| `timeout` | tool call, job | a deadline passed |
| `too_large` | tool call, hub command | a `web_fetch` download, or a file `read_file` names, larger than 10 MiB |
| `tool_error` | tool call | the tool itself failed, or its effects function errored |
| `unauthenticated` | hub connection | a remote connection's first message presented no valid device token; the hub closes the connection (`docs/invocation.md`, "Remote clients") |
| `unknown_stop_reason` | model call, turn | the reply ended with a stop or finish reason Fiber does not map |
| `unknown_tool` | tool call | the model named a tool that does not exist |
| `unreachable` | tool call | `session_message` named an id no running session has |
| `unreadable_reply` | permission request | a model replied, but not in the format Fiber asked for, such as a reviewer verdict that could not be read on the second ask (`docs/permissions.md`, "What happens on a block") |
| `unsupported_file` | tool call | a file tool was given a directory, device or file it cannot handle |
| `usage` | exit | the invocation or its environment is wrong; exits 2 |
| `version_conflict` | exit | an install needs two majors of one dependency, no tag meets a minimum, or the versions cannot be settled; pick compatible versions. Not retried automatically (`docs/extensions.md`, "Versions") |

Notices, for a failure outside any action:

| Code | Meaning |
|---|---|
| `command_conflict` | two extensions registered the same command name |
| `config_key_ignored` | an unknown key, or a key a repository may not set |
| `extension_failed` | an extension failed to start or missed its deadline, or its install record is missing or unreadable, so loading skipped it (`docs/extensions.md`, "Installing") |
| `extension_incompatible` | an extension needs a newer `fiber` or a different extension API version, so loading skipped it |
| `extension_shadowed` | a repository's approved copy of an extension loads in place of the personal install of the same name; the message names both versions |
| `hook_failed` | a `non-blocking` hook or a watcher failed |
| `instructions_large` | the instruction text passes 10% of the context window (`docs/system-prompt.md`, "Size") |
| `model_invalid` | a model's `extra_body` names a field Fiber builds, such as `tools`, or its `web_search` names a type its protocol does not read, so the model is left out of the model list; the message names the model and the field or type (`docs/model-routing.md`, "Extra request body fields", "Hosted web search") |
| `model_unconfigured` | a model's base URL names a per-account host whose setting has no value, so the model is left out of the model list; the message names the model and the setting (`docs/model-routing.md`, "A per-account host") |
| `no_model` | nothing chose the reviewer's model; set `reviewer.model` (`docs/permissions.md`, "How it runs") |
| `repository_code_skipped` | an extension, hook or MCP server the repository declares was skipped, unapproved, with nobody to ask; the message names it and says to run `fiber approve` |
| `skill_invalid` | a skill's `SKILL.md` header does not parse or lacks `name` or `description`, so it is left out; the message names its path (`docs/system-prompt.md`, "Skills") |
| `skill_shadowed` | two skills share a name; the message names both paths and which one won (`docs/system-prompt.md`, "Skills") |
| `skills_large` | the skills listing passes 10% of the context window; the message names the sources that add the most (`docs/system-prompt.md`, "Size") |
| `tool_definitions_large` | full tool definitions take more than 10% of the context window |
| `web_search_unavailable` | `web_search` is not declared: several search backends are installed and `web_search.backend` is unset, or it names a backend that is not installed; the message names which (`docs/tools.md`, "Web fetch and web search") |

Driver command rejections (`malformed`, `not_subscribed`, `busy`, `stale_request`, `not_step_boundary`,
`session_held`, `delegate_session`, `summary_failed`, `invalid_arguments`,
`unknown_command`, `closing`, `duplicate_command`)
are `docs/invocation.md`, "Driver commands".

## Not settled here

- What a session does when a log write fails, such as a full disk. The code is
  `io_failed`.
- Rate-limit, overload, quota, billing and refusal bodies were not reached by
  the probe; their matches rest on protocol documentation until one is seen.
