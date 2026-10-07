# The event stream

What Fiber writes down, what it emits, and what a consumer can rely on. A
consumer is anything that reads these lines: a client, or a program reading a
session's `events.jsonl`. This is
what is true now, not a plan. It is settled by
[What is the event stream, and what is durable?](https://github.com/aakshintala/fiber/issues/6);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Session, turn, step, action, event and tool call
mean what it says there and nothing else.

## The rule everything else follows from

**The session log is the only state of record** for what happened in a
session. Anything the loop, the TUI or an extension needs to know about a
session after a resume is an event, or a fold of events. Configuration,
credentials and an extension's data directories describe no single session.
They live in Fiber home (`docs/state.md`), and a repository's configuration in
its `.fiber/` directory (`docs/configuration.md`). Runtime
objects may cache and index; none of them is ever a second authority. No file
beside the log holds state.

Two consequences that get violated first, so they are stated first:

- **No sidecar.** A session directory holds the log, a lock, and directories for
  bytes too big to inline. No `session.json`, no usage ledger, no checkpoint, no
  manifest. Token totals, history length and what the last turn was doing are
  folds computed at open.
- **The log is append-only for the life of the session.** A handoff appends
  new events. It never rewrites, renames or restarts the file, because a sequence
  number a consumer stored yesterday must still point at the same event today.

## The envelope

One JSON object per line, one event per line, every line valid on its own.

| Field | On | Meaning |
|---|---|---|
| `kind` | every line | the only discriminator a consumer switches on |
| `session_id` | every line | the session this event belongs to |
| `ts` | every line | milliseconds since the epoch |
| `schema_version` | every line | every line must be readable without negotiation |
| `turn_id` | lines about a turn, or an action in one | correlation |
| `action_id` | lines about an action | correlation |
| `seq` | durable lines only | position; contiguous per session, never reset or reused |
| `payload` | every line | kind-specific body, nested so it can never collide with the envelope |

A durable line, which carries `seq`, and an ephemeral one, which does not:

```json
{"kind":"turn_started","session_id":"s_4c1d","ts":1759150000000,"schema_version":1,"turn_id":"t_9a02","seq":7,"payload":{"input":[{"command_id":"c_7f3a","content":[{"text":"fix the failing test","type":"text"}],"source":"driver","type":"message"}]}}
{"kind":"assistant_message_delta","session_id":"s_4c1d","ts":1759150000123,"schema_version":1,"turn_id":"t_9a02","action_id":"a_03f7","payload":{"text":"Hel"}}
```

Kind-specific fields live under `payload`. A consumer skips any `kind` it does
not recognise and ignores fields it does not know.

The one line that is not an envelope is the `fiber_exited` a process prints when
it fails before any session exists (`docs/errors.md`, "Before a session
exists"). It is not written to any log. It carries `kind`, `schema_version`
and a `payload` holding `exit_code` and `error`, as `fiber_exited` does. It has no
`session_id`, `ts` or `seq`. A caller reads `.payload.exit_code` and
`.payload.error` on the last line whatever happened. Apart from the hub's own
lines below, only this line may lack a session, and its own type says so, so no
other kind can drift into having none. It keeps `kind` so that `fiber ask … | tail -1` reads the
verdict whatever happened.

```json
{"kind":"fiber_exited","schema_version":1,"payload":{"error":{"code":"no_model","message":"No model is configured. Set one in a configuration file or pass --model."},"exit_code":1}}
```

The hub sends four lines of its own, only on a connection to the hub and
never to a log (`docs/invocation.md`, "The hub"). Each carries `kind`, `ts`,
`schema_version` and `payload`, and no `session_id` in the envelope, since it
is about the hub or names its session in the payload:

- `hub_hello`, the first line on every connection: `payload` holds
  `fiber_version` (string). The envelope's `schema_version` is the hub's.
- `attention`, when a top-level session needs the person: `payload` holds
  `session_id`, `name` and `workspace` (strings), `reason` (`waiting` or
  `finished`, a closed set) and, with `waiting`, `summary` (string), the line
  from `session_status`.
- `device_changed`, when a device is paired or revoked: `payload` holds
  `change` (`paired` or `revoked`, a closed set), `device` (string), the
  device paired or revoked, and `by` (string), the device whose client asked,
  or `local` for the hub's machine.
- `session_left`, on the feed when a session's process ends: `payload` holds
  `session_id` (string) and `how`, `exited` or `crashed`, a closed set
  (`docs/invocation.md`, "The hub").

The hub's `command_accepted` and `command_rejected` for a hub command carry
no `session_id` in the envelope either.

## Durable and ephemeral

**A line is durable if and only if it carries `seq`.** There is no separate
flag, because a flag can disagree with the thing it describes.

- **Durable** means a client that was not listening needs this line to know the
  session's true state, work in flight included: a tool call that started, an
  approval still pending, a turn that never ended.
- **Ephemeral** means a later durable line makes it obsolete: text deltas,
  progress ticks, command acknowledgements.

The durable lines, in order, are the log. Filter a non-interactive run's stdout
to durable lines carrying its own `session_id` and you have `events.jsonl`, byte
for byte. There is no second format and no replay command: a client catching up
reads the file.

A delegate's lines stay on the delegate's own stream; nothing is relayed onto
its parent's (`docs/delegates.md`, "Streams").

The TUI is a separate process that reads the same event stream through the
hub (`docs/invocation.md`, "Processes"), so it holds no private path to
state; anything it renders exists in this contract, as an ephemeral
event where it is display-only.

## Identity and ordering

- Fiber mints every id — `session_id`, `turn_id`, `action_id` — from random
  bytes, when the thing first appears and before any line about it is emitted.
  The id a consumer sees first is the id it keeps, through execution,
  persistence and resume. There is no provisional id and no reconciliation
  event.
- Ids are opaque and unique within their session. A consumer keys on
  (`session_id`, `action_id`); nothing has to be globally unique.
- Random, not a counter: a crash before the first durable write would let a
  resumed session remint a number a consumer already saw.
- A provider's own id for a tool call is recorded in the payload of
  `tool_call_requested`, so provider history can be rebuilt on resume. It is never used
  for correlation — it means a different thing per provider, redaction can
  rewrite it, and switching model mid-session can move it under a consumer.
- `seq` is the cursor. Ids say what a line is about; `seq` says where it sits.
  Several durable lines share one action, and some sit between turns, so no id
  can serve as a cursor.

## Payload types

These rules and shapes hold for every kind's `payload`. Each kind's table
below names a shape from this section by its name.

- Keys are snake_case. The one exception is an `ask_user` question, whose keys
  are the tool's argument names, `multiSelect` included.
- An optional key is absent when it does not apply, never `null`. A key whose
  row says "or null" gives `null` a meaning of its own.
- A time is milliseconds since the epoch, as `ts` is. A duration is in
  milliseconds, and its key ends `_ms`.
- Ids are strings. A token count is an integer.
- A path to a file in the session directory is relative to that directory,
  such as `artifacts/j_5e10.log`. Every other path is absolute.
- A value listed as a closed set is breaking to extend ("Versioning"). An open
  set is additive, and a consumer treats an unknown value as the table says.

### `error`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `code` | string | yes | a stable label from `docs/errors.md`, "Registry"; an open set, and an unknown code is a generic failure |
| `message` | string | yes | Fiber's own sentence, saying what to do when there is a fix |
| `retry_after` | number | no | on a failed model call, the seconds the provider asked Fiber to wait |
| `provider` | object | no | on a failed model call: `name` (string), `status` (integer, the HTTP status) and `message` (string, the provider's own message) |

### `process`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `exit_code` | integer | no | the exit code, when the process exited |
| `signal` | string | no | the signal's name, such as `SIGKILL`, when a signal ended the process |
| `timed_out` | boolean | yes | whether Fiber stopped it at its timeout |

A consumer keys on whether `process` is present, never on the tool's name.

### Content parts

`content` is an array of parts, in order. `type` is an open set, and a
consumer shows an unknown part as a placeholder.

| `type` | Keys | Meaning |
|---|---|---|
| `text` | `text` (string) | text |
| `image` | `path` (string), `mime_type` (string), `width` (integer), `height` (integer) | the processed image file in the session's `artifacts/` (`docs/model-routing.md`, "Image limits"), its type, such as `image/png`, and its size in pixels, so a client lays it out without decoding it |
| `pdf` | `path` (string), `page_count` (integer), `pages` (image parts, optional) | the PDF in the session's `artifacts/`, the number of pages sent, and the pages rendered as image parts, absent when they could not be rendered (`docs/tools.md`, "read") |

The log never holds an image's bytes. A tool's image and a pasted image are
both written to `artifacts/` and named by path.

### Declared effects

Three keys, on `tool_call_started` and `permission_requested`, in the
vocabulary of `docs/permissions.md`, "Effects":

| Key | Type | Required | Meaning |
|---|---|---|---|
| `effects` | array of strings | yes | each of `reads`, `writes`, `executes` and `network` that applies, a closed set; empty when the call declared none |
| `reversible` | boolean | yes | whether the call is reversible |
| `paths` | array of strings | no | the paths the call touches, where the tool declared them |

### `tokens`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `input` | integer | yes | input tokens neither read from nor written to the cache |
| `cache_read` | integer | yes | input tokens read from the cache |
| `cache_write` | object | yes | input tokens written to the cache, keyed by cache lifetime (`"5m"`, `"1h"`); `{}` when nothing was written |
| `output` | integer | yes | output tokens, reasoning included |

### `usage`

Totals over some set of model calls, folded from their `usage_recorded`
lines. They are output, never a source.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `tokens` | `tokens` | yes | the calls' tokens, summed |
| `cost` | number or null | yes | US dollars billed per token, summed over the calls without `subscription` whose cost is known; `0` when there were none; `null` when there were some and none had a known cost |
| `subscription_cost` | number | yes | US dollars at API prices for the calls with `subscription`; `0` when there were none |

### `questions`

An array of `ask_user` questions as the model called them: `header`,
`question`, `options` (each a `label` and an optional `description`) and an
optional `multiSelect` (`docs/tools.md`, "The call").

### Where a message came from

| Key | Type | Required | Meaning |
|---|---|---|---|
| `source` | string | yes | `driver`, a client's command; `extension`, an extension's `host.drive`, or a message a `turn_end` hook returned; `session`, another session's `session_message` (`docs/tools.md`, "Messaging other sessions"); or `fiber`, Fiber's own message, such as the ending notice (`docs/tools.md`, "Background jobs"); a closed set |
| `extension` | string | no | the extension's name, when `source` is `extension` |
| `from_session_id` | string | no | the sending session's id, when `source` is `session` |
| `command_id` | string | no | the id of the `prompt`, `steer` or `message` command that sent it; present unless `source` is `fiber` or a `turn_end` hook returned the message (`docs/extensions.md`, "The hook points") |

### `changed_by`

An array of the names of the extensions whose hooks changed the line's
content, in the order they ran. Absent when no hook changed it.

### `ran_by`

An inner call's anchor: the extension that ran it with `host.tool`, and the
outer call or command it ran inside (`docs/extensions.md`, "Running a tool").
Exactly one of `outer_action_id` and `command_id` is present.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `outer_action_id` | string | no | the `action_id` of the extension's tool call the inner call ran inside |
| `command_id` | string | no | the id of the `command` driver command whose invocation the inner call ran inside |

## Kinds

Fiber's loop emits the kinds below. MCP elicitation and `ask_user` add no kind
of their own: both raise interactions ("Interactions") that every driver
answers with `reply`.

Each kind's table lists the keys of its `payload`. The envelope's fields,
`turn_id` and `action_id` included, are never repeated in it.

### Process boundary

#### `fiber_started`

Durable. The first line a process writes when it resumes a session; in a new
session it follows `session_started`, which is always the log's first line.
The schema version is the envelope's `schema_version`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `version` | string | yes | the Fiber version, such as `0.0.1` |
| `resumed` | boolean | yes | `false` for a new session, `true` for a resumed one |

#### `fiber_exited`

Durable. The last line a process writes for a session.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `exit_code` | integer | yes | the process's exit code (`docs/invocation.md`, "Lifecycle") |
| `usage` | `usage` | yes | this process's model calls for the session, its delegates included (`docs/loop.md`, "Spending budget"); a cost that settles after exit is missing from it |
| `final_action_id` | string | no | the `action_id` of the final assistant message, when there is one |
| `text` | string | no | that message's text: its `text_completed` parts joined in order; present exactly when `final_action_id` is |
| `error` | `error` | no | why the process failed (`docs/errors.md`, "What a caller gets") |
| `suspended_on` | string | no | the `request_id` of the pending approval or question the process exited on (`docs/invocation.md`, "Lifecycle") |
| `questions` | `questions` | no | copied from the last `turn_completed`, when its turn ended on questions |

A process is the unit these two lines bound (`GLOSSARY.md`, "Process"). They are
durable for one reason: a `fiber_started` with no matching `fiber_exited` is the
only record that a process died rather than finished. A session that was rewound
ends with `rewound` instead, which closes the boundary the same way ("Rewind").
That is the same trick tool calls use below.

`fiber_exited` copies the final message's text as well as pointing at it, so a
one-shot caller reads the last line and is done:

```sh
answer=$(fiber ask "$prompt" | tail -1 | jq -r .payload.text)
```

**That copy is output, never a source.** Fiber never reads it back, no fold
consults it, and if it ever disagrees with the action it points at, the action
wins. This is the one place a line restates content another line already
carries, and it is safe for a specific reason: it is written once, at exit,
from the message it copies, so it cannot drift while a session is running.
Every other duplicate is the bug this page exists to prevent — a stored fold
that is read back and goes stale.

The alternative was making callers filter the stream for the last
`assistant_message_completed`, which a one-shot caller should not have to
know about.

### Session and turn

#### `session_started`

Durable. The first line of every session's log. The session's creation time is
the envelope's `ts`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `workspace` | string | yes | the workspace root |
| `variables` | object | yes | the environment variables the session runs commands with, without their values: `path` (string, the `PATH`), `names` (array of strings, the other variables' names, sorted) and `source`, `login_shell` when the hub's login-shell capture succeeded, or `inherited` when it failed or for a session `fiber ask` started; a delegate carries its parent's (`docs/invocation.md`, "A session's environment") |
| `parent` | object | no | for a delegate: `session_id`, its parent session, and `delegate_id`, the delegate's `job_id` there (`docs/delegates.md`) |
| `forked_from` | object | no | for a fork or a rewind: `session_id` and `seq`, the point it continues from (`docs/delegates.md`, "Forks"; "Rewind" below) |
| `rewind` | object | no | for a rewind: `summary` (string, optional), `note` (string) and `jobs` (array of strings, the `job_id`s adopted) |

#### `rewound`

Durable. The last line of a session that was rewound ("Rewind" below).

| Key | Type | Required | Meaning |
|---|---|---|---|
| `new_session_id` | string | yes | the session that continues this one |
| `seq` | integer | yes | the point |
| `jobs` | array of strings | yes | the `job_id`s handed to the new session; empty when none |

#### `turn_started`

Durable. Its envelope carries the new `turn_id`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `input` | array | yes | everything that started the turn, in arrival order, as items below |

Each item has a `type`, an open set; a consumer skips an item it does not know.

| `type` | Keys | Meaning |
|---|---|---|
| `message` | `content` (content parts), the keys of "Where a message came from", `changed_by` | a message from a driver, an extension, another session or Fiber |
| `shell_command` | `seq` (integer) | a `shell_command` line since the last turn, named by its `seq` |
| `jobs` | `job_ids` (array of strings) | jobs whose news started the turn; their `job_completed` and `job_line` lines follow at the first step boundary |
| `handoff` | `command_id` (string) | a `handoff` command sent between turns, which is a turn of its own (`docs/invocation.md`) |

#### `step_started`

Durable. The payload is `{}`; the envelope's `turn_id` is all it carries. It
opens a step (below).

#### `turn_completed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `outcome` | string | yes | `completed`, `interrupted` or `failed`; a closed set |
| `error` | `error` | no | on `failed` (`docs/errors.md`, "What ends a turn") |
| `questions` | `questions` | no | when an `ask_user` call ended the turn for a driver that is a program (`docs/tools.md`, "Asking the person") |

#### `steering_applied`

Durable. A steering message a running turn received at a step boundary.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `content` | content parts | yes | the message as the turn received it |
| `source` | string | yes | as in "Where a message came from" |
| `extension` | string | no | as in "Where a message came from" |
| `from_session_id` | string | no | as in "Where a message came from" |
| `command_id` | string | no | as in "Where a message came from" |
| `changed_by` | `changed_by` | no | when a hook rewrote the message |

#### `steering_queue`

Ephemeral. Written whenever the queue changes; the latest wins.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `messages` | array | yes | every steering message still queued, oldest first, each with `content` and the keys of "Where a message came from"; empty when the queue is |

#### `shell_command`

Durable. A command the person ran with `send` true (`docs/invocation.md`,
`shell`). It joins the next turn's input.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `command` | string | yes | the command as typed |
| `output` | string | yes | its output, cut as a shell call's result is (`docs/tools.md`, "Result and output") |
| `artifact` | string | no | the full output's path, when cut |
| `process` | `process` | yes | how it ended |

#### `session_named`

Durable. The latest is the session's name (`docs/tools.md`, "Naming the
session").

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string or null | yes | the name; `null` when the person cleared theirs |
| `by` | string | yes | `person`, from the `name` command, which pins the name, or `model`, from `name_session`; a closed set |

#### `clients`

Ephemeral. Written whenever a `full` connection attaches or leaves
(`docs/invocation.md`, "Driver commands", `subscribe`).

| Key | Type | Required | Meaning |
|---|---|---|---|
| `count` | integer | yes | the `full` connections attached to the session, the receiving client included; `summary` connections are not counted |

#### `session_status`

Ephemeral. The session's own summary of itself, for a client that is not
showing it: a session list, a rail, a delegate card. Written when any field
changes. The latest wins, and a client that subscribes, at either level, is
sent the latest.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | the session's name, or its first prompt when it has none |
| `workspace` | string | yes | the workspace path |
| `parent` | string | no | on a delegate, the parent's `session_id` |
| `model` | string | yes | the model reference in use |
| `state` | string | yes | `streaming`, `tool`, `retrying`, `waiting`, `jobs` (no turn running, jobs running) or `idle` (nothing in flight); a closed set |
| `tool` | string | no | with `tool`, the running tool's name |
| `waiting` | object | no | with `waiting`: `request_id` (string), `kind` (`approval` or `question`) and `summary` (string, one line) |
| `since` | integer | yes | when this state began, as `ts` |
| `git` | object | no | present in a git repository: `branch`, a string, or `null` when HEAD is detached |
| `context` | object | no | after the first request: `tokens`, the context's size in tokens at the latest request, and `window`, the model's context window (integers) |
| `spend` | `usage` | yes | the session's spend so far, delegates included, from the `usage` fold |
| `delegates` | integer | yes | delegates running |
| `jobs` | integer | yes | jobs running, delegates excluded |

#### `context_added`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | yes | the text a hook added to the conversation |
| `extension` | string | yes | the hook's extension |
| `hook` | string | yes | the hook point, such as `turn_start` (`docs/extensions.md`, "The hook points") |

A **steering message** — input sent while a turn is running — joins that turn
at its next step boundary, and `steering_applied` is how the log shows what
the turn actually received. The loop drains its inbox once more before a turn
completes (`docs/loop.md`, "Ending a turn"), so only a message that arrives
after that becomes the next turn's input, and it appears on the next
`turn_started` instead. The
threading this rests on is the concurrency section of `docs/architecture.md`;
the driver commands that send and withdraw one, `steer` and `steer_drop`, are
`docs/invocation.md`.

`clients` lets a client know whether it is the only one attached, which the terminal asks before quitting (`docs/tui.md`, "Quit"). The latest wins, and a client that attaches is sent the latest.

`steering_queue` lets every attached client show and edit the queue, not only
the client that sent a message. It is ephemeral because `steering_applied`
makes each entry obsolete. The latest wins, and a client that attaches is sent
the latest, as with `extension_ui`.

`turn_completed` means settled. Retries and handoffs happen inside the turn
and appear as actions, so there is never a second "really finished" event.

A **step** is one round-trip to the model, and `step_started` opens it. It is
written before anything else in the step: the `steering_applied` and
`job_line` lines the step takes in, and any handoff it runs before calling the
model. A step ends where the next `step_started` or the `turn_completed` is,
so there is no closing line. A step's number is the count of `step_started`
lines in its turn. The boundary is not derived from the actions, because a
retry and a handoff's note request are each a model call inside one step:
counting `assistant_message_started` gives attempts, never steps.

### Actions

Every line carries the envelope's `action_id`. Deltas are ephemeral;
everything else is durable.

#### `assistant_message_started`

Durable. The payload is `{}`. Every model call the loop makes for the
conversation opens with one, the note request included, written before the
request is sent ("Writing").

#### `assistant_message_delta`

Ephemeral.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | yes | the text added since the last delta |

#### `assistant_message_completed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `outcome` | string | yes | `completed` or `failed`; a closed set |
| `error` | `error` | no | on `failed`, with `retry_after` and `provider` where they apply (`docs/errors.md`, "A failed model call") |
| `attempt` | integer | no | on `failed`: 1 for the first attempt at this request, 2 for its first retry, and so on |

#### `text_completed`

Durable. One text part of the reply. A reply with several text parts has one
line for each, even when two parts hold the same text. The lines carry the
assistant message's `action_id`. They record no effect: `assistant_message_completed`
makes them durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | yes | the part's text; `""` for a part the provider sent with only opaque data |
| `provider_item` | any JSON | no | the provider's own form of the part, with its thought signature or other opaque data, sent back unchanged only to the model that produced it (`docs/loop.md`, "What the model is sent"); absent when the part carries none |

A reply's items are logged in the order the model produced them: its
`text_completed`, `reasoning_completed` and `tool_call_requested` lines follow
its `assistant_message_started` in that order, and its
`assistant_message_completed` closes it. Replay rebuilds the reply from them
in that order.

#### `tool_call_arguments_delta`

Ephemeral. A tool call the model is still emitting, under the assistant
message's `action_id`. The arguments are parsed once, when the call finishes
(`docs/loop.md`, "One step"); `tool_call_requested` then carries them.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `index` | integer | yes | the call's position within the message, from 0 |
| `name` | string | no | the tool's name, once the provider has sent it |
| `text` | string | yes | the raw argument text added since the last delta |

#### `reasoning_started`

Durable. The payload is `{}`.

#### `reasoning_delta`

Ephemeral.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | yes | the readable reasoning added since the last delta |

#### `reasoning_completed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | yes | the readable reasoning; `""` when the provider sent none |
| `provider_item` | any JSON | no | the provider's reasoning item exactly as it arrived (`docs/loop.md`, "What the model is sent"); absent when the provider sent none |

#### `tool_call_requested`

Durable. The model finished emitting the call, or an extension ran an inner
call with `host.tool`. A line with `ran_by` is an inner call: it is never
part of the history a provider is sent, and its outcome reaches the model only
through the outer call's result.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | the tool's name as the model called it |
| `arguments` | any JSON | yes | the arguments as the model sent them: an object, or a string holding the raw text when it was not JSON |
| `provider_id` | string | no | the provider's own id for the call ("Identity and ordering"); absent when the reply carried none |
| `repaired` | object | no | the arguments after repair (`docs/tools.md`, "Before a call runs"); absent when nothing was repaired |
| `repairs` | array | no | with `repaired`, one object per fix: `path` (string, a JSON Pointer into `arguments`) and `fix`, one of `null_dropped`, `string_to_number`, `string_to_boolean` or `string_parsed` |
| `ran_by` | `ran_by` | no | on an inner call, its extension and anchor; absent on the model's calls |
| `provider_item` | any JSON | no | on a call the provider ran itself, such as a hosted web search, the provider's call block exactly as it arrived, sent back unchanged only to the model that produced it (`docs/tools.md`, "Hosted by the provider"); absent on a call Fiber runs |

#### `tool_call_started`

Durable. Execution began, wherever it runs, including a provider-hosted tool
the provider reports as in progress.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `effects` | array of strings | yes | as in "Declared effects" |
| `reversible` | boolean | yes | as in "Declared effects" |
| `paths` | array of strings | no | as in "Declared effects" |
| `arguments` | object | no | the arguments that ran, when a `before_tool` hook rewrote them |
| `changed_by` | `changed_by` | no | with `arguments` |

#### `tool_call_delta`

Ephemeral. Streamed output and progress, paced as `docs/tools.md`,
"Progress", says.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `text` | string | no | output added since the last delta |
| `details` | any JSON | no | progress for clients, shaped as the tool chooses; a client that does not recognise it ignores it |

#### `tool_call_completed`

Durable. The call's outcome.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `status` | string | yes | `completed`, `failed`, `denied` or `cancelled`; a closed set |
| `reason` | string | no | on `denied`, why; an open set, and an unknown reason is a generic denial |
| `error` | `error` | no | on `failed` |
| `process` | `process` | no | on any call that ran a process |
| `content` | content parts | yes | exactly what the model is sent (`docs/tools.md`, "What a result carries") |
| `details` | any JSON | no | data for clients, such as an edit's diff; never sent to the model |
| `artifact` | string | no | the full output's path, when the result was cut or a hook returned text for it |
| `changes` | array | no | on a call that changed files, one object per file: `path` (string) and `added` and `removed` (integers, lines) |
| `control` | object | no | instructions to the loop: `handoff` (string), a handoff note (`docs/handoff.md`); `questions` (`questions`); `name` (string), a session name (`docs/tools.md`, "What a result carries") |
| `changed_by` | `changed_by` | no | when an `after_tool` hook rewrote the result |
| `provider_item` | any JSON | no | on a call the provider ran, its result block exactly as it arrived, sent back unchanged only to the model that produced it (`docs/tools.md`, "Hosted by the provider"); absent on a call Fiber runs |

A failed model call is an assistant message that completed with a failed
outcome, an `error` and an attempt number; the retry is a new action. Its codes
are `docs/errors.md`, "A failed model call". There is no
separate error channel, so no failure is ever reported twice.

On `tool_call_completed`, `error.code` lets a consumer treat `timeout`
differently from `invalid_arguments` or `unknown_tool` without parsing
English. A nonzero exit is `failed` with code `nonzero_exit`. Every error code
is listed in `docs/errors.md`. `changes` is the one tool-neutral account of a
change: any tool may set it, so a client shows the counts without knowing
which tool ran (`docs/tools.md`, "What a result carries").

A line whose content a hook changed carries `changed_by`. It appears on
`tool_call_started` for rewritten arguments, on `tool_call_completed` for a
rewritten result, and on a `turn_started` message and `steering_applied` for
a rewritten message. The line holds what the hook returned; the original is
never logged (`docs/extensions.md`, "Hooks").

A call stopped by Fiber or the user is `cancelled`, not a signal failure. An
interrupt is not a crash.

### Approval

#### `permission_requested`

Durable. The envelope's `action_id` is the tool call.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | yes | the id a `reply` names |
| `effects` | array of strings | yes | as in "Declared effects" |
| `reversible` | boolean | yes | as in "Declared effects" |
| `paths` | array of strings | no | as in "Declared effects" |
| `step` | string | yes | which step of `docs/permissions.md`, "The order a call is judged in", raised it: `standing_ask` (3) or `review` (7); a closed set |
| `standing_rule` | object | no | on `standing_ask`, the rule that asked: `scope` (`global` or `project`) and `prefix` (string) |
| `escalation` | object | no | on `review`, why the reviewer handed the call to a person: `cause`, which is `consecutive_blocks`, `session_blocks` or `reviewer_failed`, a closed set; `reason` (string), the reviewer's reason for blocking this call, on the two block causes; `error` (`error`), on `reviewer_failed` |
| `rule` | object | no | on `review`, the rule an allow can remember: `subject` (string), the call's primary argument as its tool reads it, and `prefix` (string), the widening the tool offers, which `subject` starts with (`docs/permissions.md`, "What a rule matches"). Absent when no rule can match the call |

#### `permission_resolved`

Durable. The envelope's `action_id` is the tool call.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | no | the request it answers; absent when the decision raised none |
| `decision` | string | yes | `allow` or `deny`; a closed set |
| `decided_by` | string | yes | `credential_deny`, `person`, `standing_rule`, `session_grant`, `reviewer` or `cancel`, the turn cancelled while the request was pending (`docs/architecture.md`, "Cancellation"); a closed set |
| `reason` | string | no | why, in words, such as the reviewer's reason |
| `feedback` | string | no | what the person typed with a denial, which the model receives |
| `grant` | object | no | on a person's `allow` that added a session grant: `tool` and `prefix` (strings), the later calls it allows (`docs/permissions.md`, "What a rule matches") |
| `rule` | object | no | on a person's `allow` that added a standing rule to the project's rules file: `tool` and `prefix` (strings) |
| `reviewer` | object | no | when `decided_by` is `reviewer`: `model` (string, a model reference) and `stage` (integer, `1` or `2`) |

Both are durable so that a driver reconnecting to an unattended session learns
it is blocked on a human rather than hanging on silence, and so that a session
that exited on a pending approval can raise it again on resume. `request_id` is minted
like any other id; a reply naming a request that is no longer pending is
rejected and does nothing, so a late approval can never authorise a different
action.

The full shape of approvals, and what happens with no human present, is
`docs/permissions.md`.

### Interactions

#### `interaction_requested`

Durable. The envelope carries no `action_id`; the payload names the calls.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | yes | the id a `reply` names |
| `kind` | string | yes | `confirm`, `select`, `multi_select`, `text_input` or `form`; a closed set |
| `action_ids` | array of strings | no | the tool calls that raised it: one, or for an MCP elicitation with several calls in flight on its server, each of them (`docs/mcp.md`, "Elicitation, sampling and roots"); absent when no tool call raised it |
| `extension` | string | no | the extension that raised it with `host.ask` |
| `prompt` | string | no | the question, on every kind but `form` |
| `options` | array | no | on `select` and `multi_select`: each a `label` (string) and an optional `description` (string) |
| `fields` | `questions` | no | on `form`, one per question |

#### `interaction_resolved`

Durable. It carries exactly one answer: `declined: true`, or the answer keys
for its kind, and no others.

| Kind | Answer keys |
|---|---|
| `confirm` | `confirmed` |
| `select` | `labels`, one label |
| `multi_select` | `labels`, possibly empty |
| `text_input` | `text` |
| `form` | `answers`, and `note` when the person wrote one |

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | yes | the request it answers |
| `by` | string | yes | `person`, a client's `reply`, or `fiber`, which declines when no answer is possible or the turn was cancelled; a closed set |
| `declined` | boolean | no | `true` when declined |
| `confirmed` | boolean | no | the answer to `confirm` |
| `labels` | array of strings | no | the chosen options' labels, on `select` (one) and `multi_select` |
| `text` | string | no | the typed answer, on `text_input` |
| `answers` | array | no | on `form`, one per field in field order: `{ "skipped": true }`, or `labels` (array of strings, possibly empty) with `text` (string) when the person typed any |
| `note` | string | no | on `form`, the person's note on the whole form |

These carry every interaction except approval, which keeps
`permission_requested` and `permission_resolved` because `docs/permissions.md`
fixes their payloads. `ask_user` raises one `form` per call
(`docs/tools.md`, "Asking the person"). MCP elicitation raises one interaction
per field (`docs/mcp.md`, "Elicitation, sampling and roots"). They are durable
for the reasons approvals are, and a reply naming a request that is no longer
pending is rejected in the same way. An offer of a repository's code has its
own pair too ("Repository code").

### Repository code

#### `repository_code_offered`

Durable. The offer of everything a repository declares that has no approval
for this project and this content, raised before the session's first model
request (`docs/extensions.md`, "Code a repository ships").

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | yes | the id a `reply` names |
| `items` | array | yes | one per item offered, each: `kind` (string: `extension`, `hook` or `mcp_server`), `name` (string), `hash` (string, the content hash an approval records), `required` (boolean), `summary` (string, what an install shows), `version` (string, for an extension), and `diff` (string, against the copy approved before, when the content changed since an approval) |

#### `repository_code_resolved`

Durable. One decision per item, in the offer's order.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `request_id` | string | yes | the offer it answers |
| `decisions` | array of strings | yes | per item: `approve`, `skip` (for this session) or `never`; a closed set |

Only a person answers an offer, through a client's `reply`. A model never
does, and an extension never does: `host.drive` sends no answer to an offer,
as it sends none to an approval. A session with no client that can answer
raises no offer and skips or fails each item instead
(`docs/extensions.md`, "Code a repository ships"). The pair is durable so that
a session that exited on a pending offer raises it again, with the same
`request_id`, on resume.

### Usage and notices

#### `usage_recorded`

Durable. One per model call, whatever started it. The envelope's `action_id` is
the action the call belongs to; a reviewer's or an extension's call belongs to
none.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `generation_id` | string | yes | the provider's id for the generation |
| `model` | string | yes | the model reference, `provider/model` |
| `tokens` | `tokens` | yes | the call's tokens |
| `web_searches` | integer | no | hosted web searches, where the provider reports them |
| `cost` | number or null | yes | in US dollars: the vendor's own figure where it reports one, otherwise the model's declared prices applied to `tokens` (`docs/model-routing.md`, "Cost"); `null` when neither exists |
| `subscription` | boolean | no | `true` when a subscription login covered the call, so `cost` is an API-price estimate, not money billed; absent means billed per token |
| `extension` | string | no | the extension whose `host.model` made the call |
| `origin_session_id` | string | no | on a copy, the session whose call it was (`docs/delegates.md`, "Streams"); absent on the session's own calls |

#### `quota_noticed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `provider` | string | yes | the provider's name |
| `credential` | string | yes | the credential label whose quota crossed (`docs/model-routing.md`, "Credentials") |
| `window` | string | yes | the window's name, as the provider reports it |
| `percent_used` | number | yes | the percent used when the notice was given |
| `resets_at` | integer | no | when the window resets, where the provider reports it |
| `notice_at` | number | yes | the threshold it crossed, `quota.notice_at` |

#### `retry_scheduled`

Ephemeral. The envelope's `action_id` is the failed assistant message.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `code` | string | yes | the failed call's `error.code` |
| `attempt` | integer | yes | the attempt about to be made |
| `delay_ms` | integer | yes | the wait before it |

#### `notice`

Ephemeral. A failure outside any action.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `code` | string | yes | an open set (`docs/errors.md`, "Registry"); a consumer shows the message for an unknown code |
| `message` | string | yes | Fiber's own sentence |
| `extension` | string | no | the extension it concerns |

One `usage_recorded` per model call, whatever started it, however it ended. A
call that is cancelled, fails or closes early, after the provider named its
generation, writes its `usage_recorded` at once, with the tokens it saw and
`cost` as for any call without the vendor's figure (`docs/model-routing.md`,
"Cost"). A cost that settles late, from the provider's `cost()`, is a second
`usage_recorded` with the same `generation_id`, replacing the first. A parent writes a copy of each `usage_recorded` it receives from a
delegate, with the same payload and `origin_session_id` added, so a session's
log holds its whole tree's spend. The fold counts one line per
`generation_id`, the latest, so a copy of a copy is still one call. A late correction
in a delegate's log is copied like any other line, so in the parent's log too
the correction comes after the copy it replaces, and the latest wins in both. Consumers
sum; resume rebuilds the ledger by folding. No pending queue,
no watermarks, no reconciliation file.

On 20 completed calls to `z-ai/glm-5.3-flash` through OpenRouter, streaming and not,
`cost` came inline and equalled the generation lookup, watched through 120
seconds. A completed call carried its final cost inline, so it did not need the
second record. One stream closed early carried no `cost`; the lookup returned
one by 30 seconds, and until then the record has `cost` null.

### Preamble

Behaviour is `docs/prompt-cache.md`.

#### `preamble_built`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `reason` | string | yes | `start`, `resume`, `reload` or `switch`; a closed set |
| `model` | string | yes | the model reference |
| `context_window` | integer | yes | the model's context window, in tokens |
| `trigger_at` | integer | no | the context size, in tokens, at which an automatic handoff runs (`docs/handoff.md`, "Automatic"); absent when automatic handoff is off |
| `thinking` | string | no | the thinking level, where the model takes one (`docs/model-routing.md`, "Thinking") |
| `tool_choice` | string | yes | the tool choice as sent |
| `cache_lifetime` | string | yes | `5m` or `1h` |
| `credential` | string | no | the credential label every later request uses (`docs/model-routing.md`, "Which credential a session uses"); absent when the provider takes no credential |
| `system_prompt` | string | yes | the system prompt text as sent |
| `tools` | array | yes | each tool as sent: `name` (string), `registered_by` (string: `builtin` for Fiber's own tools, otherwise the name of the extension or MCP server that registered it), `deferred` (boolean) and `definition` (object, the definition in the protocol's own shape) |
| `replaced` | array | no | each tool registered under a name already taken: `name` (string), `from` (string, the `registered_by` of the tool replaced) and `to` (string, the `registered_by` of the tool that replaced it); absent when no tool was replaced |

#### `model_changed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `before` | object | yes | `model`, `thinking`, `cache_lifetime` and `credential` before the switch, as on `preamble_built` |
| `after` | object | yes | the same keys after it |
| `source` | string | yes | who asked for it: `driver` or `extension`, as in "Where a message came from" |
| `extension` | string | no | the extension's name, when `source` is `extension` |

A switch of credential label, by `/credential`, the `credential` driver command
or `--credential` on a resume, is a `model_changed` whose `credential` differs.

`preamble_built` follows `session_started` or `fiber_started`, `reloaded`, or
`model_changed`, before the next model request. A fork or a rewind sends the
latest `preamble_built` before its point.

### Opening message

Behaviour is `docs/system-prompt.md`.

#### `opening_message`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `environment` | object | yes | as below |
| `instruction_files` | array | yes | each file sent, in order: `path` and `content` (strings) |
| `extension_sections` | array | no | each extension section sent, in order, each entry as below; absent when empty |
| `skills` | array | yes | the skills listing, each entry as below |

| `environment` key | Type | Required | Meaning |
|---|---|---|---|
| `date` | string | yes | the date, `YYYY-MM-DD` |
| `os` | string | yes | the operating system, such as `linux` or `macos` |
| `arch` | string | yes | the architecture, such as `x86_64` or `aarch64` |
| `shell` | string | yes | the shell |
| `workspace` | string | yes | the workspace |
| `git` | object | no | present in a git repository: `branch`, a string, or `null` when HEAD is detached |
| `session_log` | string | yes | the session log's path |

| `skills` entry key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | the skill's name |
| `description` | string | yes | its description |
| `path` | string | yes | the path of its `SKILL.md` |
| `source` | string | yes | `repository`, `personal`, `extension` or `builtin`; a closed set (`docs/system-prompt.md`, "Skills") |

| `extension_sections` entry key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the section's extension |
| `files` | array | yes | each section file sent, in order: `path` and `content` (strings) |
| `budget_bytes` | number | no | the section's byte budget, when the manifest gives one |

#### `instruction_file`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `path` | string | yes | the file's path |
| `reason` | string | yes | `subdirectory`, `created`, `changed`, `deleted` or `own_edit`; a closed set |
| `extension` | string | no | the section's extension, when the file is a section file; absent for an instruction file |
| `content` | string | no | the file's content now; absent when deleted |
| `sent` | string | yes | what the model was sent: `full`, `diff`, `deleted` or `none`; a closed set |

#### `date_changed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `date` | string | yes | the new date, `YYYY-MM-DD` |

#### `skills_changed`

Durable. Skills added or removed since the listing was last given, sent at a
turn start (`docs/system-prompt.md`, "Added and removed skills").

| Key | Type | Required | Meaning |
|---|---|---|---|
| `added` | array | yes | each skill added, as a `skills` entry of `opening_message` |
| `removed` | array of strings | yes | the names of the skills removed |

#### `skills_resent`

Durable. Written after `handoff_completed` when the previous context had
loaded skills (`docs/handoff.md`, "What the model sees after a handoff").

| Key | Type | Required | Meaning |
|---|---|---|---|
| `skills` | array | yes | each skill sent again, in the order it was first loaded: `name`, `path` and `content` (strings), the body read from disk at the handoff |

`opening_message` is written at session start and after each completed
handoff. Each `extension_sections` entry records one extension's section with
its extension, files and budget. `instruction_file` with `own_edit` records the
content after the session's own call changed the file, and sends nothing. A
diff is rendered from `content` and the content the model last had, both in the
log. The texts are rendered from these payloads.

### Handoff

Behaviour is `docs/handoff.md`.

#### `handoff_started`

Durable. Written before the note request. A tool-started handoff makes no note
request and writes none.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `trigger` | string | yes | `auto`, `person` or `overflow`; a closed set |

#### `handoff_completed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `outcome` | string | yes | `completed`, `failed` or `cancelled`; a closed set |
| `error` | `error` | no | on `failed` |
| `note` | array of strings | no | the `action_id`s of the actions carrying the note, in call order |
| `note_text` | string | no | a note a `before_handoff` hook wrote, in place of `note` |
| `extension` | string | no | with `note_text`, the hook's extension |
| `tokens_before` | integer | yes | the context size before the handoff, in tokens |
| `instructions` | string | no | the person's instructions, when there were any |

#### `context_nudged`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `tokens` | integer | yes | the context size when the nudge was given |
| `trigger_at` | integer | yes | the context size at which an automatic handoff runs |

The note request is an ordinary assistant message action with its own
`usage_recorded`. `handoff_completed` points at the note and never copies its
text ("Writing"), except a note a hook wrote, which appears on no earlier line.
`context_nudged` is durable because the model saw it; the nudge's text is
generated from its payload.

A handoff that fails or is cancelled leaves the model's context as it was. A
cancelled handoff means the turn was cancelled, by a person or a shutdown, and
the turn completes `interrupted`.

### MCP servers

Behaviour is `docs/mcp.md`.

#### `mcp_server_failed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `server` | string | yes | the server's name |
| `reason` | string | yes | `start_failed`, `deadline` (missed its startup deadline), `not_logged_in` or `died`; a closed set |
| `will_restart` | boolean | yes | whether Fiber will restart it |
| `error` | `error` | yes | with code `mcp_server_unavailable` (`docs/errors.md`, "The shape") |

#### `mcp_server_ready`

Durable. A server that died is running again after its restart, so a client
clears the failure it showed.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `server` | string | yes | the server's name |

#### `reloaded`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `servers` | object | yes | `kept`, `restarted`, `started` and `stopped`, each an array of server names |
| `extensions` | array of strings | yes | the extensions reloaded |
| `failed` | array | no | each server that failed: `server`, `reason` and `error`, as on `mcp_server_failed` |

`reloaded` is written once the new tool set is declared, and `preamble_built`
follows it. The next model request misses the prompt cache.

### Extensions

Behaviour is `docs/extensions.md`.

#### `extensions_loaded`

Durable. The full set of extensions the session loaded. Written at each
process start, a resume included, before the first model request, and again
after every `reload`, following `reloaded`. It always carries the whole set, never a
difference; the latest wins, and a `summary` connection is sent the latest.
A client half reads it to decide whether it draws for this session
(`docs/extensions.md`, "Client halves").

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extensions` | array | yes | one object per loaded extension: `name` (string) and `version` (string) |

#### `extension_state_set`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `key` | string | yes | the state key |
| `value` | any JSON | yes | the key's whole new value, at most 64 KiB |
| `on_fork` | string | yes | `at_point`, `latest` or `fresh`; a closed set |

#### `extension_state_unset`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `key` | string | yes | the state key removed |

An unset carries no `on_fork`. The key keeps the rule of its last
`extension_state_set`: when that rule reads the key after the unset, the new
session gets no key.

#### `extension_ui`

Ephemeral. One line is one of two variants: `extension` with `status`, or
`extension` with `widget` and `lines`; never both. The latest status, and the
latest of each widget, wins, and a client that attaches is sent them.

Status lines and widgets are data for a client to show or ignore, not
interactions (`docs/architecture.md`, "Asking a human"): they ask nothing and
take no `reply`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `status` | string | no | its status line; `""` clears it |
| `widget` | string | no | a widget's id |
| `lines` | array of strings | no | with `widget`, its lines; empty removes it |

#### `extension_message`

Ephemeral.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `data` | any JSON | yes | what its session half sent its own TUI extension with `host.emit` |

#### `extension_log`

Ephemeral. A line an extension wrote with `host.log`. The session also
records it in its diagnostic log (`docs/state.md`, "What each part holds"); it is
never saved in the session log.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `message` | string | yes | the line, as the extension wrote it |

#### `extension_exec`

Durable. A program an extension ran outside a tool call.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `extension` | string | yes | the extension's name |
| `program` | string | yes | the program |
| `args` | array of strings | yes | its arguments |
| `cwd` | string | yes | its working directory |
| `process` | `process` | yes | how it ended |

Extension state is a fold: the latest `extension_state_set` or
`extension_state_unset` for each extension and key, up to the point being
read. A write made inside a hook is written just before the line that hook
changed, and is not written at all if the hook fails. Any other write is
written at the loop's next drain of its inbox ("One inbox" in
`docs/architecture.md`), so a crash before then loses it.

A fork or a rewind folds its parent's log. For each key, the `on_fork` of its
last write up to the point decides what the new session gets:

| `on_fork` | The new session gets |
|---|---|
| `at_point` | the value as of the point |
| `latest` | the value at the end of the parent's log when the new session starts |
| `fresh` | nothing |

### Jobs

Behaviour is `docs/tools.md` ("Background jobs"); delegates are
`docs/delegates.md`.

#### `job_started`

Durable. The envelope's `action_id` is the tool call that started the job;
absent when an extension started it.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the job's id |
| `tool` | string | no | the name of the tool that started it |
| `extension` | string | no | the extension that started it with `host.delegate` |
| `description` | string | yes | a short description |
| `output_path` | string | yes | the job's output file |

#### `delegate_started`

Durable. Written after `job_started`, for each run.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the delegate's job |
| `delegate_session_id` | string | yes | the delegate's `session_id`; on another harness, the harness's own session id, which Fiber sets before it starts |
| `harness` | string | yes | the harness, such as `fiber` |
| `model` | string | yes | the model reference, with any role resolved |
| `workspace` | string | yes | the delegate's workspace |
| `worktree` | object | no | when isolated: `path` and `branch` (strings) |
| `forked_from` | object | no | for a fork: `session_id` and `seq` |

#### `job_delta`

Ephemeral. Progress for clients, paced as `tool_call_delta` is.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the job |
| `text` | string | no | output added since the last delta |
| `details` | any JSON | no | progress, as on `tool_call_delta` |

#### `job_line`

Durable. What a monitor delivered to the model.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the monitor's job |
| `lines` | string | yes | the batch of lines delivered, cut as `docs/tools.md`, "Background jobs", says |
| `suppressed` | integer | no | deliveries suppressed since the last one, when any were |

#### `delegate_finished`

Durable. Written just before `job_completed`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the delegate's job |
| `text` | string | yes | the final message, bounded as `docs/tools.md`, "Bounded results", says |
| `artifact` | string | no | the full final message's path, when cut |
| `questions` | `questions` | no | when the delegate's turn ended on `ask_user` |
| `usage` | `usage` | yes | the run's totals |
| `worktree` | object | no | when isolated: `path` and `branch` (strings) and `dirty` (boolean) |

#### `job_completed`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_id` | string | yes | the job |
| `status` | string | yes | `completed`, `failed` or `cancelled`; a closed set |
| `error` | `error` | no | on `failed` |
| `process` | `process` | no | for a job that ran a process |
| `output_tail` | string | no | for a failed job, the tail of its output, capped |

#### `jobs_pending_notified`

Durable.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `job_ids` | array of strings | yes | the jobs named in the notice |
| `reason` | string | yes | `ending`, the ending notice (`docs/tools.md`, "Background jobs"), or `unattended`, the jobs check (`docs/invocation.md`, "Lifecycle"); a closed set |

Either notice starts a turn whose input is one `message` item with
`source` `fiber`, carrying the notice's text. The request is built from that
text. `jobs_pending_notified` is the first line after that turn's first
`step_started`, and it adds nothing to the request.

A delegate's `session_id` is on `delegate_started`, and `delegate_finished` is
keyed by `job_id`, as `job_line` is (`docs/delegates.md`, "Events"). There is
no job-kind field: tool identity is an opaque name (`docs/tools.md`).

`job_line` is durable because the model saw it, and resume must rebuild what
the model saw.

`job_completed` has no `denied`: the starting call is what gets denied.
`error.code` values defined here are `nonzero_exit` and `timeout` (as on
`tool_call_completed`), `signal` (a process killed by a signal Fiber did not
send), `indeterminate`, `orphaned`, `flooded`, and `output_cap`. The output
tail is the first time those bytes enter the log.

Only the loop thread writes durable events, and it drains its inbox at step
boundaries (`docs/architecture.md`, "One inbox"), so `job_completed` and
`job_line` are written at the step boundary where the model receives them.
Their position in the log is the delivery point.

### Command acknowledgements

Every driver command is answered with exactly one of these, echoing its id
(`docs/invocation.md`, "Driver commands"). Both are ephemeral.

#### `command_accepted`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `command_id` | string | yes | the command's id |
| `result` | object | no | on the commands in the table below; absent for every other command |

| Command | `result` keys |
|---|---|
| `rewind` | `new_session_id` (string), the session that continues this one |
| `tools` | `tools`, an array with one object per declared tool: `name` (string), `source` (`builtin`, `extension` or `mcp`), `server` or `extension` (string, the tool's server or extension, when not built in), `state` (`full`, `deferred` or `loaded`), `bytes` (integer) and `tokens` (integer, estimated, absent before the first request) (`docs/tools.md`, "Seeing the tools") |
| `history` | `lines`, an array of the session's durable lines in the range asked for, each a whole line as the log holds it, in `seq` order |
| `shell` | `output` (string), `artifact` (string, when cut) and `process` (`process`), as on `shell_command` |
| `start` | `session_id` (string), the session the hub started, over the hub (`docs/invocation.md`, "The hub") |
| `recent` | `sessions`, an array of `recent.jsonl` rows, newest first, at most 50, delegates skipped (`docs/state.md`, "What each part holds"), over the hub (`docs/invocation.md`, "The hub") |
| `prompt_history` | `prompts`, an array of prompt history lines, newest first, each a whole line as `history.jsonl` holds it (`docs/state.md`, "What each part holds"), at most 256; and `before` (integer), the byte offset where the oldest returned line starts, present only when older lines remain, sent back to read the next page, over the hub (`docs/invocation.md`, "The hub") |
| `status` | `running` (boolean, always true), `fiber_version` (string) and `clients` (integer, the open connections, the asker included), over the hub (`docs/invocation.md`, "The hub") |

#### `command_rejected`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `command_id` | string | no | the command's id; absent when the line was `malformed` and carried none that could be read |
| `code` | string | yes | the rejection code (`docs/invocation.md`, "Driver commands") |
| `message` | string | yes | Fiber's own sentence |

## Resume

A session is reconstructed from the log and its
[configuration](configuration.md), nothing else.

Open memory-maps or scans the file into an offset table and folds the
latest-wins facts as it goes. Only the window a consumer actually needs is
parsed. The model's context, the TUI's viewport and any search are three
residency policies over one primitive: a range read by `seq`. A handoff
shortening what the model sees must not shorten what a person can scroll back
to.

What the reader can tell about work that was in flight, from the log alone:

| What the log shows | What it means |
|---|---|
| `tool_call_requested`, no `tool_call_started` | provably never ran; safe to run or discard |
| `tool_call_requested` with `provider_item` | the provider ran it; Fiber never reviews, runs or answers it on resume |
| `tool_call_started`, no `tool_call_completed` | uncertain; Fiber never re-runs it, and the model is told its outcome is unknown |
| `tool_call_completed` | ran, with its outcome |
| `job_started`, no `job_completed` | the process that ran it died; on open Fiber writes `job_completed` with `status: failed` and `error.code: orphaned`, unless a `rewound` lists the job |
| `turn_started`, no `turn_completed` | the turn was cut short; render what was logged and say so, unless a `fiber_exited` with `suspended_on` follows, in which case the turn resumes with the request raised again |
| `fiber_started`, no `fiber_exited` or `rewound` after it | that process died rather than exited |
| `rewound` last | the session continued elsewhere; the jobs it lists were handed over, so they are not orphaned |
| `handoff_started`, no `handoff_completed` | the process died during a handoff; the handoff did not take effect, and the model's context is what it was before it |

On open, Fiber writes that `job_completed` and does not touch any process. A
crash does not kill a child in its own process group, so Fiber cannot know
whether the job finished or still runs, and the status is not `cancelled`.

Fiber re-runs no call a crash left without a result, even one that only reads.
The turn it belonged to ended with the crash, so a re-run would answer the
earlier call with a later result, and a call that only reads, such as a search
across a large repository, can still be slow. The model is sent that the outcome
is unknown (`docs/loop.md`, "What the model is sent") and may make the call
again.

Partial assistant text from an interrupted response is gone, because deltas are
ephemeral. The log does not pay to store text a completion would supersede.

An attempt count is derived by counting `assistant_message_started` lines, never
from a stored counter, so it cannot drift from the record.

## Rewind

A session is a line. `seq` is its only position: there is no tree, no leaf and
no event that moves a position. The evidence is
[research/rewind](../research/rewind/README.md).

A **rewind** starts a new session that continues an existing one from an
earlier point. A person starts one from the terminal, a driver with the
`rewind` command (`docs/invocation.md`).

- **It is a pointer.** The new session's `session_started` carries
  `forked_from { session_id, seq }`, the pointer a fork uses
  (`docs/delegates.md`, "Forks"), and no `parent`. No `parent` is what tells a
  rewind from a delegate's fork. Nothing is copied, and the old session keeps
  its history. The pointer is the record: listing sessions finds "A continued
  as B" on B's first line, which listing already reads.
- **The point is a step boundary:** the start of a turn, just after the
  person's input, or just after a batch of tool results. Every tool call before
  it has its result.
- **It rewinds the conversation only.** It never restores or touches files.
  Fiber lists, for the person and in a note to the model, the files its own
  tools wrote after the point and the shell calls after it that may have
  changed files. Both come from the effects on each `tool_call_started` after
  the point: the paths of calls that declared `writes`, and the calls that
  declared `executes` (`docs/permissions.md`, "Effects").
- **A summary is optional.** A rewind may ask the old session's model to
  summarise the path after the point. A requested summary that fails fails
  the whole `rewind` with `summary_failed`: no new session is created and the
  old one is untouched. The client offers to retry, to rewind without a
  summary, or to cancel, and each of the first two is a new `rewind`.
- **What the model receives, in order:** the old session's history up to the
  point, then the summary if there is one, then Fiber's note: the files written
  and shell calls since the point, and the jobs adopted or stopped. The first
  request matches the old session's up to the point, so it hits the prompt
  cache while the cache is warm. Everything after the history rides on the new
  session's `session_started`, as `rewind { summary?, note, jobs }`, where
  `jobs` is the adopted `job_id`s.
- **A handoff is inherited by position.** A handoff completed in the old
  session at or before the point applies to the new session. One after it does
  not.
- **Jobs.** A job started before the point and still running is adopted by the
  new session: its history shows the job starting, so it must own it. Each job
  started after the point and still running is listed, and the person chooses
  to stop it or adopt it. Stop is the default. The choice travels on the
  `rewind` command, which names the jobs to adopt; every other such job stops
  (`docs/invocation.md`). The client lists them from the log before it sends
  the command, so no interaction is raised. Stopping is the normal job stop. An adopted job's later `job_line` and
  `job_completed` go to the new session's log.
- **It never starts a turn.** The new session waits for a prompt, whichever
  point it continues from, including one just after the person's input or
  after a step.
- **The old session is closed first.** While the process still holds the old
  session's lock, it writes a `job_completed` with `status: cancelled` for each
  job the person chose to stop. The summary's model call, when there is one,
  is made on the old session, and its `usage_recorded` goes in the old
  session's log. Then it writes `rewound`, naming the new session, the
  point and the jobs handed over. `rewound` is the last line the process writes
  to that log, and it closes the process boundary there as `fiber_exited`
  does. Only then does the new session start.
- **Only the holder rewinds.** The process that rewinds a session must hold
  it, or open it and take its lock when no process holds it, so it can close
  the session properly. Rewinding a session another process holds is refused,
  naming the holder: its jobs live in that process, and a session has one
  writer. A delegate cannot be rewound; its parent forks again instead.

The model does not rewind. For planned speculative work it uses
`delegate_fork`, with `isolation: worktree` where files matter. For an
unplanned dead end it hands off: it restarts its own context from a note it
writes. How a handoff works is `docs/handoff.md`.

## Writing

**Fsync the record of a side effect after it happens and before causing the
next one.** Two exceptions, both because the effect costs money or touches the
world: `tool_call_started` is fsynced *before* the tool runs, and
`assistant_message_started` *before* the model request is sent. Two fsyncs
bracket each effect, so a quiet text turn costs two. `step_started` records no
effect and is flushed with the step's first fsync.

**No line restates the content of an earlier line in the same turn.** This is
part of the durability rule, not an optimisation. A per-step line that carries
steps 1..N makes the log quadratic in tool calls within a turn; measured on the
Zig implementation, a 429-call turn wrote 412 MB and peaked at 2.1 GiB RSS,
where per-action lines carry the same information written once each. Any future
line that summarises the turn so far reintroduces this, so it needs this
decision overturned first. The one restatement in the contract, the final text
on `fiber_exited`, sits outside any turn and is never read back.

`extension_state_set` writes a key's whole value each time, so an extension
that rewrites a growing value on every step would bring the quadratic log
back within its own key. The 64 KiB cap bounds each line, and a value that
grows with the session, such as one record per turn or per delegate, belongs
under one key per record (`docs/extensions.md`, "State").

**A torn tail is discarded.** A reader stops at the last complete line and a
writer truncates a partial line before appending, so a power cut cannot make a
session unopenable.

**One writer per session, enforced by a lock file.** A second Fiber process
opening the same session refuses and names the holder, plainly, rather than
hanging or corrupting the log. Readers need no lock at all: append-only plus
"stop at the last complete line" is the whole protocol.

## The session directory

```
~/.fiber/projects/<key>/sessions/<session_id>/
  events.jsonl     the log
  session.lock     one writer
  artifacts/       bytes too large to inline
```

Nothing else. A directory is a session when its log parses and begins with
`session_started`; no marker file can outlive the thing it marks. Listing
sessions reads the logs — 601 session files' first lines took 39 ms warm on
macOS arm64, so there is nothing for an index to save yet. Fiber writes no
derived database; where the directory lives and how projects are keyed is
[Fiber home](state.md). Any future derived store is derived from the logs,
rebuildable, and never the truth.

## Versioning

One integer `schema_version` on every line, shared by durable and ephemeral,
never negotiated at startup — a line read by a client of another Fiber build
has to be readable on its own. It stays `1` until the first release: before
then there are no readers to break, so the rules below apply from that
release on.

- **Additive, no bump:** a new kind, a new optional field, a new value in an open
  set (`error.code`, denial `reason`, `notice.code`). Consumers skip unknown
  kinds and ignore unknown fields.
- **Breaking, bump:** removing, renaming or retyping a field or kind; changing a
  field's meaning; making an optional field required; adding a value to a closed
  set (`status`).

Before 1.0 there are no migrations: a session written against an older version
may fail to open, because there is no installed base to strand. From the first
release, every breaking bump ships a migration that upgrades an older log when
Fiber opens it, plus its test. Fiber then reads exactly one version, so reader
branches never accumulate.

## Testing

A test asserts on what a consumer sees: the lines and the file left behind. A
test that reaches inside Fiber to check an emitter was called proves nothing
about the stream, and the central invariant — durable output equals the log,
byte for byte — is not even expressible from in there.

The numbers quoted on this page were measured on the archived Zig
implementation. They justify the rules; they are not Fiber's budgets. Fiber
measures its own, on Linux (`docs/performance.md`), because the fsync cost that drove the write path is
25–53% of turn wall time there and under 3% on macOS.
