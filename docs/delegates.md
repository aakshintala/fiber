# Delegates

How a session hands part of its work to another agent session, talks to it, and
gets its result. This is what is true now, not a plan. It is settled by
[Subagents and delegates: one delegate, on one stream](https://github.com/aakshintala/fiber/issues/21);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Delegate, harness, fork, job, session, steering
message and step boundary mean what it says there and nothing else. Job
behaviour is `docs/tools.md`, "Background jobs"; the events are
`docs/events.md`.

## What a delegate is

- A delegate is an agent session a session starts to do part of the work. It
  runs Fiber or another harness, such as Claude Code or cursor-agent.
- Every delegate is a job. It gets a receipt, an output file, a wake on
  completion, and is listed, waited for and stopped with `jobs`. A delegate
  always runs in the background; no call blocks until a delegate finishes. A
  parent that needs the answer at once calls `jobs wait`.
- A delegate is its own session, with its own `session_id` and its own log. Its
  `session_started` records the parent's `session_id` and its delegate id. That
  is how any code tells a delegate from a top-level session.
- The session that starts a delegate is its parent.

## Harnesses

- The Fiber harness is built in.
- Every other harness is an extension: data plus Lua, as a provider is. What it
  declares is
  [Harness extensions: running another agent as a delegate](https://github.com/aakshintala/fiber/issues/77).
- Fiber ships the Claude Code and cursor-agent harnesses. codex and others
  follow, unless adding them is trivial.
- The delegate tools belong to Fiber. Another harness's own subagents stay
  switched on, inside its process and under its own limits. Fiber serves no MCP
  ([ADR 0005](adr/0005-no-mcp-server-the-supervisor-is-external.md)). Any
  harness can run `fiber ask` from its shell.

## The tools

| Tool | Arguments | What it does |
|---|---|---|
| `delegate_spawn` | `description`, `prompt`, `model`, `isolation`, `workspace`, `timeout_ms` | Starts a delegate. Returns a receipt with the delegate id and the output path. |
| `delegate_fork` | `description`, `prompt`, `isolation`, `timeout_ms` | Starts a Fiber delegate from the parent's conversation. No model or effort: see "Forks". |
| `delegate_message` | `id`, `message` | Steers a running delegate, or resumes a finished one. |
| `delegate_models` | none | Returns the configured roles, what each maps to now, and the full references available. |
| `jobs` | as `docs/tools.md` | Lists, waits for and stops delegates. |

- `description` is a short label for people.
- `isolation` is `none` (the default) or `worktree`.
- `workspace` defaults to the parent's workspace. A delegate started inside a
  worktree delegate defaults to that worktree.
- `timeout_ms` is optional, with no default. At the deadline, the delegate is
  stopped like any job and ends `failed` with code `timeout`. A delegate has no
  other deadline: a Fiber delegate's shell commands carry their own
  `timeout_ms`, and another harness is bounded by `timeout_ms`, `jobs stop`,
  `job_stop`, or the caller's SIGTERM.
- The output path is the delegate's own `events.jsonl` for a Fiber delegate, and
  the harness's raw stream in the parent's `artifacts/` for any other.
- Every tool is declared from the first request of a session and never removed.
  A tool that cannot be used, such as a delegate tool at the depth cap, fails
  its call with a stable code.

## Choosing a model

- `model` is a configured role name, or a full reference
  `harness:provider/model:effort`, such as `claude:opus:high`,
  `fiber:openai/gpt-5.6:xhigh` or `cursor-agent:composer-2.5` (cursor-agent
  writes effort inside its model name).
- It is required and checked when the call is made. An invalid value fails with
  `invalid_arguments` and lists the valid roles and references.
- Neither the roles nor the references appear in any tool definition or the
  system prompt. They change when config changes, when a provider withdraws a
  model, or when model discovery refreshes, and a tool definition that changes
  during a session misses the whole prompt cache. `delegate_models` returns them
  as a tool result instead.
- A role is a name for a model reference, for example `deep` for
  `claude:opus:high`. Written instructions such as skills, prompt templates and
  AGENTS.md name roles, so they survive a model being withdrawn and work on any
  machine. Remapping a role changes nothing the model sees.
- The model is fixed when the delegate starts and never changes.

## Talking to a delegate

- `delegate_message` to a running delegate whose harness takes messages while
  running (Fiber; Claude Code through `--input-format stream-json`, to be
  confirmed on [#77](https://github.com/aakshintala/fiber/issues/77)) sends a
  steering message. It joins the delegate's turn at its next step boundary.
- To a running delegate whose harness cannot take one, the message is queued and
  delivered as a resume when the delegate finishes.
- To a finished delegate, it resumes it.
- The receipt says which of these happened.
- A person reaches any delegate in the tree: the driver commands `steer` and
  `reply` take an optional `session_id`, and each parent forwards a command
  addressed to a descendant down the tree. How the terminal shows and steers
  delegates is `docs/tui.md`.
- `delegate_message` acts only on the caller's own delegates, as `jobs` does.

## Identity and resume

- A delegate has one id for life, which is its `job_id`.
- Each run writes one `job_started` and one `job_completed` under that id. A
  resume is a new run of the same job.
- A resume continues the delegate's own session and goes back into its kept
  worktree.

## Results

- When a delegate finishes, the parent is woken as for any job. The wake carries
  the delegate's final message, bounded as `docs/tools.md`, "Bounded results",
  says: the first 16 KiB in the notice, the full text in `artifacts/`.
- A delegate that asks with `ask_user` ends its turn, and the wake carries its
  questions from `delegate_finished`. The parent answers with
  `delegate_message`, which resumes the delegate, or asks the person first
  (`docs/tools.md`, "Asking the person").
- There is no status field beyond `job_completed`'s `completed`, `failed` and
  `cancelled`. None of 529 results in the owner's pi sessions ended with a
  requested `STATUS:` line.
- The wake is built only from logged fields. Nothing in it is rendered from the
  clock.

## Events

The `job_*` kinds are unchanged. A delegate adds two kinds keyed by `job_id`, as
`job_line` is for monitors:

| Kind | Durable | Payload |
|---|---|---|
| `delegate_started` | yes | `job_id`, the delegate's `session_id`, harness, model reference (role resolved), workspace, worktree path and branch when isolated, `forked_from` for a fork |
| `delegate_finished` | yes | `job_id`, the final message (bounded, with `artifact` when cut), `questions` when the delegate's turn ended on `ask_user`, usage totals, worktree state (path, branch, dirty) |

- `delegate_started` is written after `job_started`, for each run.
  `delegate_finished` is written just before `job_completed`.
- The usage totals are output, never a source, as `fiber_exited`'s copy of the
  final text is. The cost ledger is the fold over the delegate's own
  `usage_recorded` lines. A cost that settles after the delegate finishes
  appears only in the delegate's log.
- A delegate's `session_started` carries `parent { session_id, delegate_id }`
  and, for a fork, `forked_from { session_id, seq }`.

## One stream

- A Fiber delegate's lines are relayed onto the parent's stdout as they are,
  each carrying the delegate's `session_id`. Filtering stdout to the parent's
  `session_id` still gives the parent's log byte for byte.
- The parent's log holds only the parent's own events.
- Another harness's raw stream goes to the job's output file, never onto the
  parent's stream.

## Permissions

- A Fiber delegate starts in its parent's mode, and a mode change on the
  parent applies to it too. A delegate in `readonly` never asks to leave it
  (`docs/permissions.md`, "Leaving readonly"), so a delegate is never in a
  more permissive mode than its parent.
- In `auto`, the delegate's own reviewer judges its calls.
- An escalation from a delegate is relayed up the tree to whoever drives the
  root, and answered with `reply` naming the delegate's `session_id`. With
  nobody attached, it is a block, as for any unattended run, and the delegate
  carries on.
- The parent's model never answers a delegate's approval. It shapes the
  delegate's reviewer only through the prompt it wrote and `delegate_message`,
  which the reviewer reads as the human's messages.
- Starting any delegate declares `executes`, so the parent's mode judges the
  start. A `readonly` parent asks to leave `readonly` first. Another harness runs in the mode its extension
  sets.

## Limits

- Depth is capped at 2: the root session is depth 0, its delegates depth 1,
  theirs depth 2. At depth 2, the delegate tools stay declared and fail with
  `depth_exceeded`.
- A session runs at most 10 delegates at once. The eleventh start fails with a
  message saying so; nothing is queued.
- Both limits are local. A delegate is told its depth when it starts, and each
  session counts only its own delegates, so no state is shared across the tree.
  The worst case for one root is 10 + 100 = 110 delegates.
- Measured: the owner peaked at 8 running at once in pi; 17 grandchildren were
  started, from 1.6% of children.

## Lifetime

- A delegate survives cancellation of its parent's turn, as every job does.
- A delegate that finishes its task with jobs of its own still running follows
  the rule for a session about to end (`docs/tools.md`): it is woken once, then
  waited for.
- Stopping a delegate (`jobs stop`, `job_stop`) stops its own jobs and delegates
  as its shutdown does (`docs/invocation.md`, "Shutdown"), so a stop reaches
  every descendant. Its parent waits for it to exit, up to the shutdown
  bound, before sending SIGKILL.
- Every delegate is a child process of the session that started it, whatever
  its harness. A Fiber delegate is a child `fiber serve`
  ([ADR 0009](adr/0009-each-session-is-one-process.md)): its prompt and
  commands go down the pipe, its events come back up it, and the parent is
  its client zero. The parent drives it only through the driver commands and
  events, so no delegate has a path a supervisor lacks.
- A Fiber delegate starts its own MCP servers and process extensions, as
  every session does (`docs/mcp.md`, "Where servers run").
- Stopping a Fiber delegate is a signal to its process group, as for any job
  (`docs/tools.md`, "Shell").
- If a parent's process dies without a shutdown, its delegates keep running:
  each sees its client leave, finishes its turn and jobs, and exits
  (`docs/invocation.md`, "Lifecycle"). The parent's log marks each job
  `orphaned` on resume (`docs/tools.md`, "Background jobs"), because the
  parent cannot know.
- A crash of any kind in a delegate, in Rust or in Lua's C code, ends that
  delegate `failed` and nothing else.

## Worktrees

- `isolation: worktree` makes a new branch from the parent workspace's HEAD, in
  a worktree under `~/.fiber/projects/<key>/worktrees/<id>`. Fiber runs the
  `git` program; no git library is linked in.
- At the end of a run, a worktree with nothing uncommitted and no commits beyond
  its base is removed. Otherwise it is kept. Fiber never removes a kept
  worktree.
- `delegate_finished` reports the path, the branch and whether it is dirty.
- A resume goes back into the kept worktree, or gets a fresh one from the
  current HEAD if it was removed.
- `worktree` outside a git repository fails with `invalid_arguments`.
- Measured: the owner isolated 6% of pi launches; 77% of cursor-delegate runs
  used a path the caller made.

## Forks

- `delegate_fork` starts a Fiber delegate whose history is its parent's
  conversation up to the fork, then its own events.
- It is recorded as a pointer: `forked_from { session_id, seq }` on the
  delegate's `session_started`. The fork's history is the parent's log folded to
  that `seq`. Nothing is copied: the parent's log is append-only, so the shared
  part is never written again. This is copy-on-write where the write never
  happens, as with a git branch or a ZFS clone.
- A session some fork points at cannot be deleted on its own. Deleting it
  deletes its forks or is refused, as `zfs destroy` refuses a snapshot with
  clones (`docs/state.md`).
- A fork exists to share its parent's prompt cache. Its first request must match
  the parent's byte for byte up to the new content, so:
  - It takes no model or effort. It runs on the parent's model, effort and
    thinking configuration, and every request setting that changes the prefix.
  - It sends the parent's latest preamble before its point, as logged in
    `preamble_built`, so its tool set, system prompt and request settings
    match (`docs/prompt-cache.md`, "The preamble").
  - Its identity and task go in its first user message, never the system prompt.
  - The `seq` is the position just before the assistant message that called
    `delegate_fork`, so no tool call is left without a result.
  - Every provider's cache key is the root session's id, shared by the whole
    lineage (`docs/prompt-cache.md`, "Cache markers and keys").
- Other harnesses have no fork: a Fiber conversation cannot be handed to them.

## Not settled here

- What a harness extension declares:
  [Harness extensions: running another agent as a delegate](https://github.com/aakshintala/fiber/issues/77).
- Messaging a session that was not started as a delegate:
  [Intercom: messaging a session you did not start](https://github.com/aakshintala/fiber/issues/78).
