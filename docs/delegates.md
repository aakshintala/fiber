# Delegates

How a session hands part of its work to another agent session, talks to it, and
gets its result. This is what is true now, not a plan. It is settled by
[Subagents and delegates: one delegate, on one stream](https://github.com/aakshintala/fiber/issues/21);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Delegate, harness, fork, job, session, steering
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
  declares is [Harness extensions](#harness-extensions).
- Fiber ships the Claude Code and cursor-agent harnesses. codex and others
  follow, unless adding them is trivial.
- A harness loads whatever the person configured for it: their hooks,
  plugins, skills, MCP servers, instructions and memory. Fiber passes only the
  flags that drive the program.
- The delegate tools belong to Fiber. Another harness's own subagents stay
  switched on, inside its process and under its own limits, and so does any
  delegation tool the person configured for it. Fiber's depth cap and
  concurrency limit do not reach inside another harness. Fiber serves no MCP
  for delegating to Fiber
  ([ADR 0005](adr/0005-the-delegation-supervisor-is-external.md)). Any
  harness can run `fiber ask` from its shell.

## The tools

| Tool | Arguments | What it does |
|---|---|---|
| `delegate_spawn` | `description`, `prompt`, `model`, `isolation`, `workspace`, `timeout_ms` | Starts a delegate. Returns a receipt with the delegate id and the output path. |
| `delegate_fork` | `description`, `prompt`, `isolation`, `timeout_ms` | Starts a Fiber delegate from the parent's conversation. No model or thinking level: see "Forks". |
| `delegate_message` | `id`, `message` | Steers a running delegate, or resumes a finished one. |
| `delegate_models` | none | Returns the configured roles, what each maps to now, the full references available, and the quota of each provider credential label and harness (`docs/tools.md`, "Provider quota"). |
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
  `harness:provider/model:level`, such as `claude:opus:high`,
  `fiber:openai/gpt-5.6:xhigh` or `cursor-agent:composer-2.5`. The last part
  is the harness's own word: for Fiber it is a thinking level
  (`docs/model-routing.md`, "Thinking"), for another harness it is passed to
  that harness's flag, such as Claude Code's `--effort` (cursor-agent writes
  it inside its model name).
- It is required and checked when the call is made. An invalid value fails with
  `invalid_arguments` and lists the valid roles and references.
- Neither the roles nor the references appear in any tool definition or the
  system prompt. They change when config changes, when a provider withdraws a
  model, or when model discovery refreshes, and a tool definition that changes
  during a session misses the whole prompt cache. `delegate_models` returns them
  as a tool result instead.
- A role is a name for a model reference, for example `deep` for
  `claude:opus:high`. A role may also name a credential label, so one
  project's delegates can run on different subscriptions of one provider
  (`docs/model-routing.md`, "Which credential a session uses"). A delegate
  started with a full reference uses the provider's configured label. The
  model sees only role names, never labels. Written instructions such as skills, prompt templates and
  AGENTS.md name roles, so they survive a model being withdrawn and work on any
  machine. Remapping a role changes nothing the model sees.
- The model is fixed when the delegate starts and never changes.

## Talking to a delegate

- `delegate_message` to a running delegate whose harness takes messages while
  running (Fiber and Claude Code) sends a steering message. It joins the delegate's turn at its next step boundary.
- To a running delegate whose harness cannot take one, the message is queued and
  delivered as a resume when the delegate finishes.
- To a finished delegate, it resumes it. A Fiber delegate exits as soon as
  its run finishes ("Lifetime"), so most messages to a finished delegate are
  resumes.
- The receipt says which of these happened.
- A person reaches a delegate on its own connection: a client that opens a
  delegate subscribes to it through the hub and sends it `steer` there, as to
  any session. No session forwards a command to another. How the terminal
  shows and steers delegates is `docs/tui.md`.
- `delegate_message` acts only on the caller's own delegates, as `jobs` does.
- Any session reaches any running delegate, a sibling included, with
  `session_message` (`docs/tools.md`, "Messaging other sessions"). A Fiber
  delegate takes it on its own socket. A delegate on another harness takes it
  through its parent ("Delegates on another harness").

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
  `cancelled`.
- The wake is built only from logged fields. Nothing in it is rendered from the
  clock.

## Events

The `job_*` kinds are unchanged. A delegate adds two kinds keyed by `job_id`, as
`job_line` is for monitors: `delegate_started`, naming the delegate's session,
harness, model, workspace and worktree, and `delegate_finished`, carrying its
final message, any questions, usage totals and worktree state. Their keys are
`docs/events.md`, "Jobs".

- `delegate_started` is written after `job_started`, for each run.
  `delegate_finished` is written just before `job_completed`.
- The usage totals are output, never a source, as `fiber_exited`'s copy of the
  final text is. For a Fiber delegate, the cost ledger is the fold over the
  delegate's own `usage_recorded` lines, and a cost that settles after the
  delegate finishes appears only in the delegate's log. Another harness has no
  Fiber log, so the parent writes one `usage_recorded` per run in its own
  log, naming the harness, from the usage the harness reports at the end of
  the run.
- A delegate's `session_started` carries `parent { session_id, delegate_id }`
  and, for a fork, `forked_from { session_id, seq }`.

## Streams

- Each delegate's stream is its own. A parent reads its Fiber delegate's
  stream as an ordinary client of the delegate's socket. Nothing a delegate
  emits is relayed onto its parent's stream, and nothing is forwarded down
  the tree.
- One kind is copied: each `usage_recorded` a parent receives from a delegate
  is copied into the parent's own log with `origin_session_id`, and a fold
  counts one line per `generation_id`, so a call is counted once. A grandchild's lines reach the root through
  its parent's log. The root's log then holds the whole tree's spend
  (`docs/loop.md`, "Spending budget").
- A client shows a delegate's card from that delegate's `session_status`,
  read over a `summary` connection, and its conversation over a `full` one
  when the person opens it (`docs/tui.md`).
- Another harness's raw stream goes to the job's output file.

## Permissions

- Every delegate runs in `auto`, as every session does (`docs/permissions.md`).
- `delegate_spawn`, `delegate_fork` and `delegate_message` declare `executes`
  and `always_reviewed` (`docs/tools.md`, "What a tool declares"), so they are
  always judged by the parent's reviewer: no fast path, session grant or
  standing allow skips them. The parent's reviewer judges what the parent's
  model tells a delegate against the parent's own person's messages, because
  the delegate's reviewer reads that prompt and those messages as the human's.
- A Fiber delegate has its own reviewer. An escalation from it is a block, as
  for any unattended run, and the delegate carries on. No model answers a
  delegate's approval, its parent's included, and nothing relays it to a
  person.
- Another harness runs in its own auto mode and is judged by its own rules
  ("Harness extensions"). A harness and model are offered as a delegate only
  when their auto mode is declared to work.

## Limits

- Depth is capped at 2: the root session is depth 0, its delegates depth 1,
  theirs depth 2. At depth 2, the delegate tools stay declared and fail with
  `depth_exceeded`.
- A session runs at most 10 delegates at once. The eleventh start fails with a
  message saying so; nothing is queued.
- Both limits are local. A delegate is told its depth when it starts, and each
  session counts only its own delegates, so no state is shared across the tree.
  The worst case for one root is 10 + 100 = 110 delegates.

## Lifetime

- A delegate survives cancellation of its parent's turn, as every job does.
  A delegate never waits on a person: an escalation is a block and an
  `ask_user` question ends its turn ("Results").
- A Fiber delegate exits as soon as its run finishes: its final answer is
  written and its own jobs are done. It does not wait for `session.idle_exit_ms`
  (`docs/invocation.md`, "Lifecycle").
- A delegate that finishes its task with jobs of its own still running follows
  the rule for a session about to end (`docs/tools.md`): it is woken once, then
  waited for.
- Stopping a delegate (`jobs stop`, `job_stop`) stops its own jobs and delegates
  as its shutdown does (`docs/invocation.md`, "Shutdown"), so a stop reaches
  every descendant. Its parent waits for it to exit, up to the shutdown
  bound, before sending SIGKILL.
- Every delegate is a child process of the session that started it, whatever
  its harness. A Fiber delegate runs the internal session command
  ([ADR 0009](adr/0009-each-session-is-one-process.md)). Its parent is an
  ordinary client over its socket and drives it only through the driver
  commands and events, so no delegate has a path a supervisor lacks.
- A Fiber delegate's stdin is a lifeline. The parent holds it open and never
  writes to it. End of file on it means the parent is gone, however it died,
  and the delegate shuts down within the bound (`docs/invocation.md`,
  "Shutdown"). A dropped socket connection is only a client leaving: the
  parent reconnects and the delegate carries on.
- A Fiber delegate starts its own MCP servers and process extensions, as
  every session does (`docs/mcp.md`, "Where servers run"). It never raises a
  repository's offer: code its repository declares loads only if a person
  already approved that content, and is otherwise skipped, or fails the
  delegate when marked `required` (`docs/extensions.md`, "Code a repository
  ships").
- How a stopped Fiber delegate is ended is the stop rule above.
- If a parent's process dies without a shutdown, each Fiber delegate sees
  end of file on its lifeline and shuts down. The parent's log marks each job
  `orphaned` on resume (`docs/tools.md`, "Background jobs"), because the
  parent cannot know, and the resumed parent reads each orphaned delegate's
  log for the `usage_recorded` lines it is missing.
- A crash of any kind in a delegate, in Rust or in Lua's C code, ends that
  delegate `failed` and nothing else.

## Worktrees

- `isolation: worktree` makes a new branch from the parent workspace's HEAD, in
  a worktree under `~/.fiber/projects/<key>/worktrees/<id>`. Fiber runs the
  `git` program; no git library is linked in.
- At the end of a run, a worktree with nothing uncommitted and no commits beyond
  its base is removed. Otherwise it is kept. Uncommitted follows git: files the
  repository ignores, such as build output, are not changes. Fiber never removes a kept
  worktree on its own; `fiber sessions prune` removes one that holds nothing
  to lose, or any with `--force` (`docs/invocation.md`, "Deleting and
  pruning").
- `delegate_finished` reports the path, the branch and whether it is dirty.
- A resume goes back into the kept worktree, or gets a fresh one from the
  current HEAD if it was removed.
- `worktree` outside a git repository fails with `invalid_arguments`.

## Forks

- `delegate_fork` starts a Fiber delegate whose history is its parent's
  conversation up to the fork, then its own events.
- It is recorded as a pointer: `forked_from { session_id, seq }` on the
  delegate's `session_started`. The fork's history is the parent's log folded to
  that `seq`. Nothing is copied: the parent's log is append-only, so the shared
  part is never written again. This is copy-on-write where the write never
  happens, as with a git branch or a ZFS clone.
- A session some fork points at cannot be deleted on its own. Deleting it is
  refused, as `zfs destroy` refuses a snapshot with clones, unless the person
  passes `--cascade`, which deletes its forks too (`docs/invocation.md`,
  "Deleting and pruning").
- A fork exists to share its parent's prompt cache. Its first request must match
  the parent's byte for byte up to the new content, so:
  - It takes no model or thinking level. It runs on the parent's model and
    thinking level, and every request setting that changes the prefix.
  - It sends the parent's latest preamble before its point, as logged in
    `preamble_built`, so its tool set, system prompt and request settings
    match (`docs/prompt-cache.md`, "The preamble").
  - Its identity and task go in its first user message, never the system prompt.
  - The `seq` is the position just before the assistant message that called
    `delegate_fork`, so no tool call is left without a result.
  - Every provider's cache key is the root session's id, shared by the whole
    lineage (`docs/prompt-cache.md`, "Cache markers and keys").
- Other harnesses have no fork: a Fiber conversation cannot be handed to them.

## Harness extensions

Settled by
[Harness extensions: running another agent as a delegate](https://github.com/aakshintala/fiber/issues/77);
that ticket's resolution holds the probes and the rejected alternatives.

A harness extension registers with `fiber.harness(name, { ... })`
(`docs/extensions.md`, "Registering"). The name is the first part of every
model reference on it, such as `claude` in `claude:opus:high`.

### What it declares

As data, for the harness:

- the flags that run the harness in its own auto mode
- whether it takes messages while running
- whether SIGTERM stops it cleanly, so it gets the shutdown wait
  (`docs/invocation.md`, "Shutdown")

As data, for each model:

- its price per token, where the harness reports no cost of its own
- whether the harness's `auto` mode works on it, declared only after a probe

As Lua, none of which runs on a model request:

- `command(spec)` returns the program, arguments and environment for a start
  or a resume. `spec` carries the prompt, the model reference, the
  workspace, and the harness session id.
- `line(text)` is called with each line the harness prints, and returns what
  that line carries, if anything: the final answer, usage, quota, or a
  failure.
- `models()` returns the harness's model references. It is cached and
  refreshed as a provider's model discovery is (`docs/model-routing.md`,
  "Model discovery").
- `quota()`, optional, returns quota in the provider shape
  (`docs/model-routing.md`, "Quota").

### How Fiber runs it

- **Configuration.** The harness loads the person's configuration for it.
  `command` passes only what drives the program: print mode, the JSON output
  stream, the session id, the model and its effort flag, the auto-mode flags, resume, and where
  the harness takes one per run, `fiber mcp serve` ("Delegates on another
  harness").
- **Session id.** Fiber fixes the harness's session id before the program
  starts, so a delegate stopped before its first line can still be resumed.
  It is the delegate's `session_id` on `delegate_started`.
- **Worktree.** `isolation: worktree` is always Fiber's own worktree
  ("Worktrees"), and the harness runs with it as its working directory. A
  harness's own worktree option is never used.
- **Output.** Every line goes to the job's output file. Fiber hands each line
  to `line` and keeps the last final answer, usage and failure it returns.
  The run ends when the program exits.
- **Messages while running.** A harness that takes them gets each
  `delegate_message` as a line on its input. Fiber closes its input after
  the final answer, so the program exits. For any other harness the message
  waits and is delivered as a resume ("Talking to a delegate").
- **Usage.** The parent writes one `usage_recorded` per run, from the usage
  `line` returned. Its cost is the harness's own where it reports one, or
  computed from the declared prices. It carries `subscription` when the
  harness ran on a subscription login, and the parent's budget then skips it
  (`docs/loop.md`, "Spending budget").
- **Quota.** `quota()` runs when `delegate_models` needs it, as a provider's
  does (`docs/tools.md`, "Provider quota"). Quota that `line` returns from a
  running delegate replaces the stored value, with no fetch.
- **Program missing.** A harness whose program is not installed lists no
  models in `delegate_models`.

### Delegates on another harness

A delegate on another harness has no socket of its own, so its parent stands
in for it in session messaging (`docs/tools.md`, "Messaging other sessions"):

- **The parent binds its socket.** It binds `~/.fiber/run/<session_id>` when
  each of the delegate's runs starts and unlinks it when the run ends. It
  removes a stale socket first, as a lock holder does (`docs/state.md`,
  "What each part holds"). `session_list` lists the delegate like any other. The socket
  accepts only `message` (`docs/invocation.md`). After the parent is sent
  `close`, it is rejected `closing`.
- **A message in is delivered as `delegate_message` delivers one.** A harness
  that takes messages while running gets it as a line on its input. For any
  other harness it waits and is delivered as a resume ("Talking to a
  delegate"). The parent frames it with the sender's id and name.
- **The delegate sends through `fiber mcp serve`.** It is a stdio MCP server
  offering `session_list` and `session_message`, which work as the built-in
  tools do, with the delegate's id as the sender. The server lists no tools
  unless its environment names a Fiber delegate, so it is inert anywhere
  else.
- **The harness judges the send.** Calling `session_message` is a tool call
  inside the harness, judged in the harness's own mode, as its other calls
  are ("Permissions"). Fiber's reviewer and `before_message` do not reach
  inside another harness. The target session applies both on its side.
- **How each harness loads the server** is in its table below.

Each tool's description says what it reaches. The harness's own tools stay
switched on: Claude Code's `SendMessage` and `ListAgents` reach its own
subagents and other Claude Code sessions, and `session_message` reaches the
sessions `session_list` shows, by a Fiber session id.

The parent stays on the receiving path because it holds the one input the harness
documents. Probed on Claude Code 2.1.285, headless:

- A message from another Claude Code session, sent with `SendMessage` to a
  session started with `crossSessionInbound` `accept`, joined the running
  turn. Its frame is one JSON line with `msgV`, `msg_id`, `type` `user`,
  `message`, `priority` and `from`.
- The same frame from a process that was not Claude Code was not delivered,
  with no token, with the `CLAUDE_CODE_MESSAGING_TOKEN` value as a first line,
  or with it as JSON. How the socket authenticates a sender is undocumented.
- An MCP server declaring `claude/channel` pushed
  `notifications/claude/channel` during a running tool call, under both
  `--channels` and `--dangerously-load-development-channels`. The model never
  saw it.

If Claude Code documents its inbound socket, a Claude Code delegate could take
session messages directly and the parent would drop out of the path.

### Claude Code

| What | How |
|---|---|
| Start | `claude -p --output-format stream-json --verbose --input-format stream-json --session-id <uuid> --model <model> --effort <effort> --permission-mode auto` |
| Resume | the same, with `--resume <session-id>` in place of `--session-id` |
| Auto mode | `--permission-mode auto`, declared per model |
| Messages while running | yes, as a `user` line on its input; it joins the turn at the next step boundary |
| Session messages out | `--mcp-config` naming `fiber mcp serve`, which adds it for the run and keeps the person's own servers |
| Final answer, usage, cost | the `result` line: `result`, `usage` and `total_cost_usd` |
| Subscription | the `system` `init` line's `apiKeySource`: `none` is the subscription login, and `total_cost_usd` is then an API-price estimate |
| Quota | `rate_limit_event` lines while running, and `quota()` from `/api/oauth/usage` with Claude Code's own stored login |
| SIGTERM | stops cleanly, so it gets the shutdown wait |

Probed on Claude Code 2.1.284:

- With no isolation flags, a delegate loaded the person's hooks, plugins, MCP
  servers, user instructions, output style and auto-memory. That is what the
  person configured, so it stays.
- With no `--permission-mode`, a Sonnet delegate ran in the person's
  configured `auto`. A Haiku delegate reported `default`, which is why `auto`
  is declared per model.
- A message sent while a Bash call was running joined the same turn, and
  the run ended with one `result`.
- `set_permission_mode` switched a running session to `plan`, confirmed by a
  `status` line.
- `--bare` and a clean `CLAUDE_CONFIG_DIR` both fail with "Not logged in",
  because neither reads the subscription login.
- On 2.1.285 with a Max login, the `init` line had `apiKeySource` `none` and
  the `result` line still had `total_cost_usd`, 0.0228 for one Haiku reply.
  The value with an API key was not probed.

### cursor-agent

| What | How |
|---|---|
| Start | `cursor-agent -p --output-format stream-json --trust --approve-mcps --model '<model>[effort=<effort>]' --auto-review` |
| Resume | the same, with `--resume <chat-id>` |
| Session id | `cursor-agent create-chat` before the first run |
| Auto mode | `--auto-review` |
| Messages while running | none: delivered as a resume |
| Session messages out | an entry for `fiber mcp serve` in `~/.cursor/mcp.json`, which has no per-run flag. Fiber asks the person once, the first time a cursor-agent delegate starts, and remembers the answer. With no entry, a cursor-agent delegate receives session messages but cannot send them |
| Final answer, usage | the `result` line: `result` and `usage`, which has tokens and no cost |
| Cost | computed from the declared prices |
| SIGTERM | not probed, so it gets SIGKILL at 800 ms like any command |

Probed on cursor-agent 2026.09.26:

- `--trust` and `--approve-mcps` answer prompts that would otherwise stop a
  run with nobody to answer them.
- It loads the person's skills, including `~/.claude/skills`, their plugins'
  MCP servers and their user rules. Its `init` line names none of them.
- It has no channel for approvals. With no mode flag it follows the person's
  `approvalMode`.

Not yet probed: `create-chat`, which is from `--help`; whether
`--auto-review` overrides the person's `approvalMode`; what SIGTERM does; and
where its quota comes from. Until its quota is known, the cursor-agent
harness declares no `quota()`.

Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md#from-docsdelegatesmd).
