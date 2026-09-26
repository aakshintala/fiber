# The event stream

What Fiber writes down, what it emits, and what a consumer can rely on. This is
what is true now, not a plan. It is settled by
[What is the event stream, and what is durable?](https://github.com/aakshintala/fiber/issues/6);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Session, turn, step, action, event and tool call
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

Kind-specific fields live under `payload`. A consumer skips any `kind` it does
not recognise and ignores fields it does not know.

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

A Fiber delegate's lines are relayed onto its parent's stdout as they are, each
carrying the delegate's `session_id`, so filtering by `session_id` still gives
the parent's log (`docs/delegates.md`, "One stream").

The TUI is a separate process that reads the same event stream from the
session's socket (`docs/invocation.md`, "Processes"), so it holds no private
path to state; anything it renders exists in this contract, as an ephemeral
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
- A provider's own id for a tool call is recorded in the payload of the durable
  completed line, so provider history can be rebuilt on resume. It is never used
  for correlation — it means a different thing per provider, redaction can
  rewrite it, and switching model mid-session can move it under a consumer.
- `seq` is the cursor. Ids say what a line is about; `seq` says where it sits.
  Several durable lines share one action, and some sit between turns, so no id
  can serve as a cursor.

## Kinds

Fiber's loop emits the kinds below. MCP elicitation adds no kind of its own: it
raises the confirm, select and text input interactions every driver already
answers with `reply` (`docs/mcp.md`, "Elicitation, sampling and roots").

### Process boundary

| Kind | Durable | Payload |
|---|---|---|
| `fiber_started` | yes | Fiber version, `schema_version`, new session or resumed |
| `fiber_exited` | yes | exit code, the final message's `action_id` and its text, `error` if it failed (`docs/errors.md`, "What a caller gets"), `suspended_on` naming the `request_id` when the process exited on a pending approval (`docs/invocation.md`, "Lifecycle") |

A process is not a named unit in the glossary; these two lines record its
boundary without inventing one. They are durable for one reason: a
`fiber_started` with no matching `fiber_exited` is the only record that a process
died rather than finished. A session that was rewound ends with `rewound`
instead, which closes the boundary the same way ("Rewind"). That is the same trick tool calls use below.

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
`assistant_message_completed`, which is a one-liner today and stops being one
as soon as a delegate relays its own messages onto the same stdout.

### Session and turn

| Kind | Durable | Payload |
|---|---|---|
| `session_started` | yes | creation time, workspace root; optional `parent { session_id, delegate_id }` for a delegate and `forked_from { session_id, seq }` for a fork or a rewind (`docs/delegates.md`, "Forks"; "Rewind" below); for a rewind, `rewind { summary?, note, jobs }` |
| `rewound` | yes | the new session's `session_id`, the `seq` of the point, and the `job_id`s handed to the new session (`jobs`); the last line of a session that was rewound ("Rewind" below) |
| `turn_started` | yes | the input that started it; for a turn started by jobs, a source naming those `job_id`s |
| `turn_completed` | yes | `outcome` (`completed`, `interrupted`, `failed`), `error` on failure (`docs/errors.md`, "What ends a turn") |
| `steering_applied` | yes | the text a running turn received at a step boundary, and where it came from |
| `context_added` | yes | the text a hook added to the conversation, the extension's name and the hook point (`docs/extensions.md`, "Hooks") |

A **steering message** — input sent while a turn is running — joins that turn
at its next step boundary, and `steering_applied` is how the log shows what
the turn actually received. The loop drains its inbox once more before a turn
completes (`docs/loop.md`, "Ending a turn"), so only a message that arrives
after that becomes the next turn's input, and it appears on the next
`turn_started` instead. The
threading this rests on is the concurrency section of `docs/architecture.md`;
the driver commands that send, amend and withdraw one — `steer`, `steer_amend`
and `steer_drop` — are `docs/invocation.md`.

`turn_completed` means settled. Retries and handoffs happen inside the turn
and appear as actions, so there is never a second "really finished" event.

A **step** gets no event. It is one round-trip to the model, and its boundary is
derivable from the action sequence, so naming it on the wire would add a line
that carries nothing a consumer cannot compute.

### Actions

Every line carries `action_id`. Deltas are ephemeral; everything else is
durable.

- `assistant_message_started` / `_delta` / `_completed`
- `reasoning_started` / `_delta` / `_completed`
- `tool_call_requested` — the model finished emitting the call: name, full
  arguments, provider id.
- `tool_call_started` — execution began, wherever it runs, including a
  provider-hosted tool the provider reports as in progress. Carries the call's
  declared effects, whether it is reversible, and its paths, where the tool
  declared them (`docs/permissions.md`, "Effects"). When a `before_tool` hook
  rewrote the arguments, it also carries the arguments that ran.
- `tool_call_delta` — streamed output and progress. Ephemeral.
- `tool_call_completed` — outcome.

A failed model call is an assistant message that completed with a failed
outcome, an `error` and an attempt number; the retry is a new action. Its codes
are `docs/errors.md`, "A failed model call". There is no
separate error channel, so no failure is ever reported twice.

`tool_call_completed` carries:

- `status`, a closed set: `completed | failed | denied | cancelled`. Adding a
  value here is a breaking change.
- `reason`, an open set, on a denial.
- `error { code, message }` on a failure, with a stable `code` so a consumer can
  treat `timeout` differently from `invalid_arguments` or `unknown_tool`
  without parsing English. A nonzero exit is `failed` with code `nonzero_exit`.
- `process { exit_code?, signal?, timed_out }` on any call that ran a process,
  keyed on whether the field is present rather than on the tool's name.
- `content`, `details` and, when the result was cut, `artifact`; their meaning
  is `docs/tools.md`.

An unknown `error.code` is a generic failure and an unknown `reason` is a
generic denial; the consumer shows the message. Adding either value is additive.
Every error code is listed in `docs/errors.md`.

A line whose content a hook changed carries `changed_by`, the names of the
extensions that changed it, in the order they ran. It appears on
`tool_call_started` for rewritten arguments, on `tool_call_completed` for a
rewritten result, and on `turn_started` and `steering_applied` for a rewritten
message. The line holds what the hook returned; the original is never logged
(`docs/extensions.md`, "Hooks").

A call stopped by Fiber or the user is `cancelled`, not a signal failure. An
interrupt is not a crash.

### Approval

| Kind | Durable | Payload |
|---|---|---|
| `permission_requested` | yes | `request_id`, the tool call's `action_id`, what is being asked |
| `permission_resolved` | yes | the decision, any feedback, who answered |

Both are durable so that a driver reconnecting to an unattended session learns
it is blocked on a human rather than hanging on silence, and so that a session
that exited on a pending approval can raise it again on resume. `request_id` is minted
like any other id; a reply naming a request that is no longer pending is
rejected and does nothing, so a late approval can never authorise a different
action.

The full shape of approvals, and what happens with no human present, is
`docs/permissions.md`. It fixes these two payloads' contents: a request
carries the tool call's `action_id`, the call's declared effects and paths,
and why it was raised; a resolution carries the decision, the reason, and what
decided it — the credential deny, a human, a standing rule, a session grant,
the reviewer, or the mode.

### Usage and notices

| Kind | Durable | Payload |
|---|---|---|
| `usage_recorded` | yes | generation id, model, tokens (uncached input, input read from the cache, input written to the cache by lifetime, output), cost or `null` when unknown, `action_id` where it belongs to one |
| `retry_scheduled` | no | cause, attempt, delay |
| `notice` | no | open-set `code` and message, for a failure outside any action |

One `usage_recorded` per model call, whatever started it. A cost that settles
late is a second `usage_recorded` with the same generation id, replacing the
first. Consumers sum; resume rebuilds the ledger by folding. No pending queue,
no watermarks, no reconciliation file.

### Preamble

Behaviour is `docs/prompt-cache.md`.

| Kind | Durable | Payload |
|---|---|---|
| `preamble_built` | yes | `reason` (`start`, `resume`, `reload`, `switch`), model, effort, thinking, `tool_choice`, cache lifetime, the system prompt text, and the tool definitions as sent, each marked whether it is deferred |
| `model_changed` | yes | the model, effort, thinking and cache lifetime before and after the switch, and who asked for it |

`preamble_built` follows `session_started` or `fiber_started`, `reloaded`, or
`model_changed`, before the next model request. A fork or a rewind sends the
latest `preamble_built` before its point. `reason` is a closed set: adding a
value is a breaking change.

### Opening message

Behaviour is `docs/system-prompt.md`.

| Kind | Durable | Payload |
|---|---|---|
| `opening_message` | yes | the environment (date, platform, shell, workspace, git, session log path), each instruction file's path and content, and the skills listing |
| `instruction_file` | yes | `path`, `reason` (`subdirectory`, `created`, `changed`, `deleted`, `own_edit`), the file's `content` now (absent when deleted), and `sent` (`full`, `diff`, `deleted`, `none`) |
| `date_changed` | yes | `date` |

`opening_message` is written at session start and after each completed
handoff. `instruction_file` with `own_edit` records the content after the
session's own call changed the file, and sends nothing. A diff is rendered from
`content` and the content the model last had, both in the log. The texts are
rendered from these payloads. `reason` and `sent` are closed sets: adding a
value is a breaking change.

### Handoff

Behaviour is `docs/handoff.md`.

| Kind | Durable | Payload |
|---|---|---|
| `handoff_started` | yes | `trigger` (`auto`, `person`, `overflow`, `tool`); written before the note request, and not written for a tool-started handoff, which makes none |
| `handoff_completed` | yes | `outcome` (`completed`, `failed`, `cancelled`), `error { code, message }` on failure, `note` (the `action_id`s of the actions carrying the note, in call order), `tokens_before`, and the person's `instructions` when there were any; for a note a `before_handoff` hook wrote, `note_text` and the extension's name in place of `note` |
| `context_nudged` | yes | `tokens`, the context size when the nudge was given, and `trigger_at`, the size at which an automatic handoff runs |

The note request is an ordinary assistant message action with its own
`usage_recorded`. `handoff_completed` points at the note and never copies its
text ("Writing"), except a note a hook wrote, which appears on no earlier line. `outcome` is a closed set: adding a value is a breaking
change. `context_nudged` is durable because the model saw it; the nudge's text
is generated from its payload.

A handoff that fails or is cancelled leaves the model's context as it was. A
cancelled handoff is a person's cancellation of the turn, which then completes
`interrupted`.

### MCP servers

Behaviour is `docs/mcp.md`.

| Kind | Durable | Payload |
|---|---|---|
| `mcp_server_failed` | yes | the server's name, why it failed (did not start, missed its startup deadline, not logged in, died), and whether Fiber will restart it |
| `reloaded` | yes | the servers kept, restarted, started and stopped, the extensions reloaded, and any server that failed, with why |

`reloaded` is written once the new tool set is declared, and `preamble_built`
follows it. The next model request misses the prompt cache.

### Extensions

Behaviour is `docs/extensions.md`.

| Kind | Durable | Payload |
|---|---|---|
| `extension_state_set` | yes | the extension's name, `key`, the whole new `value` (JSON, at most 64 KiB), and `on_fork` (`at_point`, `latest`, `fresh`) |
| `extension_state_unset` | yes | the extension's name and `key` |
| `extension_ui` | no | the extension's name, and either its `status` line or a `widget` id with its lines; latest wins, and a client that attaches is sent the latest of each |
| `extension_message` | no | the extension's name and the data its session half sent to its own TUI extension with `host.emit` |
| `extension_exec` | yes | the extension's name, the program, its arguments and working directory, and `process` as on `tool_call_completed`; for a program an extension ran outside a tool call |

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

`on_fork` is a closed set: adding a value is a breaking change.

### Jobs

Behaviour is `docs/tools.md` ("Background jobs"); delegates are
`docs/delegates.md`.

| Kind | Durable | Payload |
|---|---|---|
| `job_started` | yes | `job_id`, the `action_id` of the tool call that started it or the name of the extension that did (`host.delegate`), the tool name, a short description, the output file's path |
| `delegate_started` | yes | `job_id`, the delegate's `session_id`, harness, model reference (role resolved), workspace, worktree path and branch when isolated, `forked_from` for a fork |
| `job_delta` | no | progress for clients, paced like `tool_call_delta` (`docs/tools.md`, "Progress") |
| `job_line` | yes | `job_id`, the batch of lines a monitor delivered to the model (cut as `docs/tools.md` describes), and a count of deliveries suppressed since the last one, when any were |
| `delegate_finished` | yes | `job_id`, the final message (bounded, with `artifact` when cut), usage totals, worktree state (path, branch, dirty) |
| `job_completed` | yes | `status` (`completed`, `failed`, `cancelled`), `error { code, message }`, `process` as on `tool_call_completed`, and for a failed job the tail of its output, capped |
| `jobs_pending_notified` | yes | the `job_id`s named in the ending notice (`docs/tools.md`, "Background jobs") |

A delegate's `session_id` is on `delegate_started`, which is written after
`job_started` for each run; `delegate_finished` is written just before
`job_completed`. Both are keyed by `job_id`, as `job_line` is
(`docs/delegates.md`, "Events"). There is no job-kind field: tool identity is
an opaque name (`docs/tools.md`).

`job_line` is durable because the model saw it, and resume must rebuild what
the model saw.

`job_completed` has no `denied`: the starting call is what gets denied.
`status` is a closed set. `error.code` values defined here are `nonzero_exit`
and `timeout` (as on `tool_call_completed`), `signal` (a process killed by a
signal Fiber did not send), `indeterminate`, `orphaned`,
`flooded`, and `output_cap`. The output tail is the first time those bytes
enter the log.

Only the loop thread writes durable events, and it drains its inbox at step
boundaries (`docs/architecture.md`, "One inbox"), so `job_completed` and
`job_line` are written at the step boundary where the model receives them.
Their position in the log is the delivery point.

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
| `tool_call_started`, no `tool_call_completed` | uncertain; never blindly re-run |
| `tool_call_completed` | ran, with its outcome |
| `job_started`, no `job_completed` | the process that ran it died; on open Fiber writes `job_completed` with `status: failed` and `error.code: orphaned`, unless a `rewound` lists the job |
| `turn_started`, no `turn_completed` | the turn was cut short; render what was logged and say so, unless a `fiber_exited` with `suspended_on` follows, in which case the turn resumes with the request raised again |
| `fiber_started`, no `fiber_exited` or `rewound` after it | that process died rather than exited |
| `rewound` last | the session continued elsewhere; the jobs it lists were handed over, so they are not orphaned |
| `handoff_started`, no `handoff_completed` | the process died during a handoff; the handoff did not take effect, and the model's context is what it was before it |

On open, Fiber writes that `job_completed` and does not touch any process. A
crash does not kill a child in its own process group, so Fiber cannot know
whether the job finished or still runs, and the status is not `cancelled`.

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
  summarise the path after the point.
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
  to stop it or adopt it. Stop is the default. A driver answers with `reply`.
  Stopping is the normal job stop. An adopted job's later `job_line` and
  `job_completed` go to the new session's log.
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
bracket each effect, so a quiet text turn costs two.

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
never negotiated at startup — a line relayed from another Fiber build has to be
readable on its own.

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
