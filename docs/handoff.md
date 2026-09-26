# Handoff

What Fiber does when a session outgrows the model's context. This is what is
true now, not a plan. It is settled by
[Handoff: when a session outgrows its context](https://github.com/aakshintala/fiber/issues/24);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Handoff, handoff note, session log, step, step
boundary and artifact mean what it says there and nothing else. The events are
`docs/events.md`; the tool result contract is `docs/tools.md`.

## What a handoff is

A handoff restarts the model's context from a handoff note written by the
session's own model. It is one mechanism with three triggers: Fiber, a person
or a tool. Nothing is deleted: the handoff is appended to the session log, and
a person can still scroll back through everything before it.

Every handoff makes the next request miss the prompt cache for everything after
the system prompt and tools, because the conversation that request starts from
is new.

## Triggers

### Automatic

Before each request, Fiber checks how full the model's context is. A handoff
runs when the context reaches whichever comes first:

- T tokens, 400,000 by default
- f times the model's context window, with f 0.7 by default

Both are set in configuration, globally and per model (`docs/configuration.md`).

Fiber asks the model for its note in the same session: it appends an
instruction to the context, and the model's reply is the note. That request
goes to the session's own model with the same system prompt and tools, so it
reads the cached prefix. It is never a separate request to another model. The
request after the handoff misses the prompt cache.

At most one handoff runs per step. If the context is still over the trigger
after a handoff, Fiber does not hand off again in that step.

Automatic handoff can be turned off in configuration. With it off, a turn that
reaches the model's limit fails with `context_overflow`, and the overflow rule
below does not apply. A person and a tool can still start a handoff.

### The nudge

Once per context, when the context reaches two thirds of the automatic trigger
(about 267,000 tokens with the defaults), Fiber appends one line to the model's
context. It says:

- how full the context is
- that a handoff keeps the work going
- the path of the session log

A bare fullness warning makes a model wrap up early and take shortcuts, the
behaviour Anthropic and Cognition call context anxiety. Their documented fix is
telling the model the work will continue, which is why the nudge says so
([research/compaction/thresholds.md](../research/compaction/thresholds.md),
section 4). Each handoff starts a new context, so the nudge can be given again.

### A person

In the terminal, a person types `/handoff`, optionally followed by
instructions. A driver sends the `handoff` driver command, with optional
instructions (`docs/invocation.md`). Instructions say what the next stretch of
work focuses on.

Sent while a turn is running, it applies at the next step boundary, as a
steering message does. Sent between turns, it is a turn of its own whose input
is the command.

Several `/handoff` or `handoff` commands that arrive before one step boundary
make one handoff. Their instructions are joined in the order they arrived.

Fiber asks for the note exactly as for an automatic handoff, with the
instructions added. The request after the handoff misses the prompt cache.

### A tool

The model starts a handoff with the built-in `handoff` tool:

```
handoff { note }
```

The tool returns a tool result carrying `control.handoff`, the note
(`docs/tools.md`, "What a result carries"). The loop acts on that field, never
on the tool's name, because tool identity is an opaque name. Any tool, built in
or from an extension, may set `control.handoff`.

The `handoff` tool is always declared, because tool definitions never change
during a session. It declares no effects, so it is never reviewed. Its
description tells the model that the next request misses the prompt cache.

A tool-started handoff makes no request for a note: the tool's argument is the
note. The `handoff` tool's result has empty `content` and carries only
`control.handoff`. The model never reads that result, because its context
restarts from the note.

## What the model sees after a handoff

In order:

1. The system prompt and tools.
2. A new opening message, written from the current instruction files and date
   (`docs/system-prompt.md`, "After a handoff").
3. This turn's input from the person and any steering messages applied in this
   turn, verbatim. A handoff between turns has none.
4. The handoff note.
5. A line Fiber writes listing the jobs still running, with each job's id and
   description, as the rewind note does (`docs/events.md`, "Rewind").

When a tool set `control.handoff`, the other calls in that step and their
results follow the note verbatim, because the model had not seen them. They go
as one message holding those calls in call order, then one message holding
their results in the same order. The call that set `control.handoff`, and its
result, are not sent. When two tools in one step set it, their notes are joined
in call order.

The cut is the step boundary at which the handoff ran. Everything after that
boundary in the log follows as usual. This is the rule `docs/prompt-cache.md`
builds on.

## Looking back

The model reads or searches the session log, `events.jsonl`, with its ordinary
tools. There is no tool for it. The note instruction and the nudge both name the
log's path.

## Overflow

When a request would not fit in the model's context window, or the provider
rejects it for size:

1. Fiber replaces the last step's tool results with a line pointing at their
   full text in the session's `artifacts/`. Where a result has no artifact yet,
   Fiber writes one then.
2. Fiber asks for the note on that smaller request.
3. The session continues from the note.

There is one retry. If the note request is rejected for size too, the turn
fails with `context_overflow`, a stable error code.

A single step's results can be large: in the owner's pi sessions the largest
step, with each result cut at 16 KiB, was about 49,000 tokens, about 25% of a
200,000 token window
([research/compaction/usage.md](../research/compaction/usage.md), section 6).
That is why the last step's results are what gets moved out.

## The handoff note

An extension can write the note instead, through a `before_handoff` hook
(`docs/extensions.md`, "Hooks"). When one does, no note request is made, and
`handoff_completed` carries the note's text and the extension's name.

The note is written so a fresh agent can continue the work. It:

- refers to specs, issues, commits, files, artifacts and searches of the
  session log by path or URL instead of copying them
- names the skills the next agent should load, where skills exist
- leaves out secrets

Instructions given to `/handoff` or the `handoff` driver command say what the
next stretch of work focuses on. The note has no fixed headings. The request
for the note is the `handoff-note` section of `crates/loop/prompt/messages.md`,
followed by `handoff-focus` when there are instructions
(`docs/system-prompt.md`, "The texts").

## Recording

A handoff happens inside a turn, as actions. The log is append-only
(`docs/adr/0001-session-log-is-the-only-state-of-record.md`), so a handoff adds
lines and never rewrites earlier ones. The kinds are `docs/events.md`
("Handoff").

- `handoff_started { trigger }`, where `trigger` is `auto`, `person`,
  `overflow` or `tool`. It is durable and written before the note request, so a
  crash during a handoff can be read from the log. A tool-started handoff makes
  no note request and writes no `handoff_started`.
- The note request is an ordinary assistant message action with its own
  `usage_recorded`.
- `handoff_completed { outcome, note, tokens_before, instructions? }`. `note`
  lists the `action_id`s of the actions that carry the note: the model's reply,
  or the tool call or calls whose result set `control.handoff`, in call order.
  The note's text is never copied into this line, because no line restates the
  content of an earlier line in the same turn.
- `context_nudged { tokens, trigger_at }`. It is durable because the model saw
  it. The nudge's text is generated from this payload.

`outcome` is a closed set: `completed | failed | cancelled`. Adding a value is
a breaking change.

- `failed` carries `error { code, message }`. A note request that fails follows
  the normal retry rules. If it still fails, `handoff_completed` records the
  failure, the context stays as it was, and the turn continues. The automatic
  trigger does not fire again in that turn, so the turn goes on until it ends
  or the overflow rule applies.
- `cancelled` means a person cancelled the turn while the note request ran. The
  turn completes `interrupted` and the context stays as it was.

### Resume

The handoff in force is the latest `handoff_completed` with outcome
`completed` in the session's own log. For a session that began as a rewind or
a fork, it can be one inherited by position through `forked_from`: a handoff at
or before the point applies, and one after it does not (`docs/events.md`,
"Rewind").

A `handoff_started` with no `handoff_completed` means the process died during
the handoff. The handoff did not take effect, and the context is what it was
before.

## What a person and a driver see

Scrollback keeps everything. A handoff shortens what the model sees, never
what a person can scroll back to.

The terminal draws a divider at each handoff, showing the trigger and the
context size before and after. The note can be expanded under the divider.

A driver sees the same thing as events: `handoff_started`, the note request's
actions, and `handoff_completed`.

## Cost

Every handoff makes the next request miss the prompt cache after the system
prompt and tools. On token cost alone, handing off earlier is still cheaper,
down to a floor where a fresh context is already near the trigger
([research/compaction/cost.md](../research/compaction/cost.md)). Replayed over
the owner's sessions, handing off at 400,000 tokens costs 0.75 to 0.92 of never
handing off on Opus 5.5, and a handoff at 150,000 to 400,000 tokens repays its
cost within 2 to 12 steps where sessions ran a median of 42 or more further
steps.

Cost therefore cannot choose T; quality does. The owner's experience is that
current models with 1 million token windows decay around 400,000 tokens, and
the owner ruled that point the default. The default f of 0.7 keeps the trigger
clear of a smaller window's limit, with room for a large final step, and the
cost model supports handing off at that point too.

## Evidence

- [research/compaction/README.md](../research/compaction/README.md): how pi,
  codex and Claude Code handle an overflowing session, from primary sources.
- [research/compaction/usage.md](../research/compaction/usage.md): the owner's
  usage. 1.8% of 685 pi sessions compacted. In Claude Code the owner ran
  `/handoff` 40 times, `/clear` 117 times and `/compact` 11 times.
- [research/compaction/thresholds.md](../research/compaction/thresholds.md):
  price cliffs, context anxiety, and the reference agents' triggers.
- [research/compaction/cost.md](../research/compaction/cost.md): how often each
  threshold hands off, and what it costs.

## Related

- Rewind: `docs/events.md`, "Rewind", settled by
  [Rewind](https://github.com/aakshintala/fiber/issues/32)
- The tool contract: `docs/tools.md`, settled by
  [The tool contract](https://github.com/aakshintala/fiber/issues/14)
- The prompt cache: `docs/prompt-cache.md`, settled by
  [Prompt cache](https://github.com/aakshintala/fiber/issues/33)
