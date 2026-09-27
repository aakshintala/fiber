# The loop

What one turn does, from its input arriving to `turn_completed`. This is what
is true now, not a plan. It is settled by
[The turn: what one run of the agent loop does](https://github.com/aakshintala/fiber/issues/112);
that ticket's resolution holds the rationale and the rejected alternatives.
How pi, codex and Claude Code run a turn is
[research/the-turn/README.md](../research/the-turn/README.md).

Vocabulary is `CONTEXT.md`. Turn, step, action, tool call and event mean what
it says there and nothing else. The threads and the inbox are
`docs/architecture.md`, "Concurrency"; the events are `docs/events.md`.

## Starting a turn

When the loop is idle, the first thing to arrive in its inbox starts a turn: a
person's or a driver's message, or news that a job finished
(`docs/tools.md`, "Background jobs"). Everything waiting in the inbox at that
moment is the turn's input, in the order it arrived, so three messages sent
together start one turn, not three.

`before_message` runs on each message before it is logged. Then
`turn_started` is written, and `turn_start` runs before the first model
request (`docs/extensions.md`, "Hooks").

## One step

A step is one round-trip to the model. Each step does this, in order:

1. Drain the inbox. Every steering message and every job notice waiting joins
   the conversation, in arrival order; each steering message is logged as its
   own `steering_applied`. Nothing is held back for a later step.
2. Check how full the context is, and hand off if it is past the threshold
   (`docs/handoff.md`, "Triggers").
3. Build the request: the preamble (`docs/prompt-cache.md`) followed by the
   conversation ("What the model is sent").
4. Send it and stream the reply, emitting its actions as they arrive. A failed
   call is retried as `docs/model-routing.md`, "When a model call fails",
   says.
5. For each tool call in the reply, in the order the model emitted them: check
   the tool exists and the arguments match its schema, run its effects
   function, run `before_tool`, then decide permission (`docs/permissions.md`).
   Every call is decided before any runs.
6. Run the approved calls concurrently, one thread each
   (`docs/architecture.md`, "Tool calls in a step"). `after_tool` runs on each
   call that ran.
7. Return every result to the model in the order the calls were requested,
   and take the next step.

A reply with no tool call ends the step without a next one ("Ending a turn").

While the loop waits for a person's `reply` to an approval, it reads its inbox
for that reply. Other commands and job notices that arrive meanwhile wait for
the next step boundary. An `interrupt` ends the turn ("Interrupt").

## Tool calls that do not run

A call the loop cannot run fails, and the turn continues. The model is told
why, in words it can act on:

| Cause | Status | Code |
|---|---|---|
| No tool has that name | `failed` | `unknown_tool` |
| The arguments are not JSON, or do not match the schema | `failed` | `invalid_arguments` |
| Permission refused it | `denied` | |
| The reply was cut off ("A reply cut off by the output limit") | `failed` | `output_truncated` |

A denied call does not stop the calls beside it: they run, and the turn
continues. A person who wants the turn to stop sends `interrupt`; `reply` has
no option that does both.

## A reply cut off by the output limit

When a reply stops because it reached the request's output-token limit, its
text is kept and none of its tool calls runs, however many there are. Each
fails with `output_truncated`, and its result tells the model its arguments
may be incomplete and to re-issue the call, split into smaller calls if it was
large. The turn continues.

If the next reply is cut off too, the turn completes `failed` with code
`output_truncated`. A model that cannot fit a call in its output limit would
otherwise pay for that limit on every step, with nothing to stop it.

Fiber does not resend the same request after a cut-off. The same request tends
to stop at the same place.

## Ending a turn

A turn ends in one of three ways:

- Completed. The model replied without calling a tool.
- Failed. A step's model call failed after its retries, a reply was cut off
  twice in a row, or a hook failed (`docs/extensions.md`, "When a hook
  fails").
- Interrupted ("Interrupt").

Before a reply with no tool call completes the turn, the loop drains its inbox
once more. Anything waiting, such as a steering message sent while the model
wrote its final reply, or a job that finished meanwhile, continues the same
turn with another step. Only when nothing is waiting does `turn_end` run, and
a message it returns also continues the turn. When `turn_end` returns nothing,
`turn_completed` is written. A one-turn `fiber ask` therefore includes a job
that finished during its last reply.

A reply with no text and no tool call is a reply with no tool call: the turn
completes normally.

There is no limit on steps per turn. Spending too much is the concern of a
budget, not a step count, and a caller can always send `interrupt`.

## Interrupt

An `interrupt` stops the model request or the running tool calls
(`docs/tools.md`, "Cancellation"). Before `turn_completed` is written with
outcome `interrupted`, every tool call in the step that has no
`tool_call_completed` gets one with status `cancelled`: calls waiting for
approval, calls not yet started, and calls stopped mid-run. The log therefore
never ends a turn with a call left open.

## What the model is sent

The conversation in each request is built from the session log's durable
events, the only state of record (ADR 0001). The loop keeps it in memory as
the turn goes, and never re-reads the file to build a request. Deltas are
ephemeral, so partial text from an interrupted reply is never sent.

Every tool call is sent with a result, because every provider refuses a
request that has a call without one. A call with no `tool_call_completed`,
which only a crash can leave (`docs/events.md`, "Resume"), is sent with a
fixed result: that it never ran when the log shows no `tool_call_started`, or
that its outcome is unknown when it does. Whether such a call may be run
again is
[Revisit: may a tool that never finished be re-run after a crash?](https://github.com/aakshintala/fiber/issues/40).

The reasoning state a provider returns with a reply, such as Anthropic's
signed thinking blocks, OpenAI's encrypted reasoning and Gemini's thought
signatures, is logged exactly as the provider returned it, and sent back
unchanged to the model that produced it. The provider checks only the opaque
part, the signature or encrypted content, and refuses a request in which it
was changed. Leaving it out is accepted but loses the prompt cache from that
point on.

Each item goes only to the model that produced it, by its whole model
reference, `provider/model` (`docs/model-routing.md`). After `/model` or the
`model` command switches to another model reference, a request leaves out
every item another one produced: both the opaque part and its readable text.
A change of effort or thinking alone keeps the model, so its items are still
sent. A fork keeps its parent's model and sends them unchanged. Probed on
Sonnet 5, Haiku 4.5 and GPT-6 Luna: `research/reasoning-resume/`.

## What the loop does not do

- Detect repetition. None of pi, codex or Claude Code does it in the loop.
  Repairing malformed model output before the loop sees it is the map's
  "canonical turn" item.
- Guard against a `turn_end` hook that always continues. Such an extension
  runs the turn until a person or caller sends `interrupt`.
