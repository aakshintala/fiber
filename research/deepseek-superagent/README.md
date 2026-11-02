# DeepSeek's superagent prototype, and where Fiber sits

No recommendation is ruled here. Which gaps become tickets is the owner's call.

## What was read

- The prototype, `deepseek-ai/dsh-superagent-prototype`, at commit
  `6337fc823ce20251c8de8085948165c81aaa701e` (2026-10-31). Below, `ds:<path>:<line>`
  means that path at that commit.
- The writeup, "DeepSeek Super-agent" (21 pages, PDF supplied by the owner).
  Below, `pdf p<n>` is its page.
- Fiber as designed, at `6a0cd31d2`: `GLOSSARY.md`, `docs/architecture.md`,
  `docs/invocation.md`, `docs/tools.md` ("The keeper"), `docs/events.md`
  ("Resume"), `docs/loop.md`, `docs/errors.md`, `docs/delegates.md`,
  `docs/permissions.md`, ADR 0005 and ADR 0009, issues #1, #1453, #3878, #3879,
  #3890 to #3897, #3904 and #3942, and `research/overalls/README.md`.
- The owner's current setup, read-only: `~/.agents/claude-code/agents/` and
  scripts in `~/.agents/bin/` (`tkt-start`, `tkt-state`, `inbox-wait`,
  `lanestat`, `lanewatch`). This is evidence of how the owner works, not a
  reason for Fiber to build anything.

Nothing from the prototype was built or run. A claim the sources do not settle
is marked inferred.

## Summary

The prototype is a TypeScript plugin bundle for DeepSeek Harness (DSH). It
turns one DSH process into a coding agent that watches one GitHub repository,
and optionally a Feishu group chat, and acts without being asked (`ds:README.md`,
`ds:docs/006-deployment.md:105-124`). A main "router" session reads every event,
decides whether it is worth acting on, and opens a ticket. Each ticket gets one
worker session and one git worktree. Later events on the same thread go
straight to the worker that subscribed to it. All state lives in SQLite, and
every outside effect is stored first and retried.

The ideas that matter:

1. Two levels. A router that only routes, and workers that only execute and
   never talk to people (`pdf p4-7`; `ds:docs/001-domain.md:25-37`).
2. Event intake with a gate. Sources raise triggers, a deterministic check and
   a model assessment decide accept, stop or ignore, and a policy mode
   (`off`, `observe`, `auto`) controls whether accepted work starts on its own
   (`ds:docs/002-authorization.md:91-101`).
3. Thread subscription with pass-through. A follow-up on a thread a worker owns
   bypasses the router. The writeup's ablation shows this cut cache-read tokens
   from 31.8M to 23.2M at the same wall time (`pdf p17`).
4. Durable state, stored effects, and a recovery note. After a restart, running
   work starts again with "check your directory, git history and GitHub before
   repeating any outside action" (`ds:docs/003-execution.md:144-152`;
   `ds:src/core/assignments.ts:21`).
5. An outside control loop. The writeup describes an LLM watchdog that judges
   progress and drift (`pdf p15`). The code ships something smaller: a status
   script, and a person-created hourly agent that restarts the service when the
   status says a restart may help (`ds:docs/006-deployment.md:133-141`).

The verdict on the hypothesis, "we are already partly living this reality but
without the automatic restart on crash parts":

- It holds for the worker layer and for the owner's setup. Fiber's sessions,
  delegates, jobs, keeper, steering and session messages cover what a
  prototype worker does. The owner's long-lived orchestrator sessions, per-ticket
  lanes in worktrees, file inboxes and state files are the router and ticket
  layers, built outside Fiber.
- It is wrong about what is missing. Automatic restart is the smallest gap, and
  in the prototype it is an hourly scheduled agent running a shell script, not
  a supervisor. The larger gaps are the router layer itself: event intake, the
  accept gate and policy modes, thread subscription and pass-through, tickets,
  and replies on the channel the event came from. Fiber puts that whole layer
  outside Fiber (ADR 0005; `docs/invocation.md`, "The delegation supervisor is
  external"), and nothing in Fiber's design or tickets builds it.
- It understates one thing Fiber already does better. Fiber's keeper keeps a
  crashed session's jobs and delegates running (#3890). The prototype runs every
  session in one process, so a crash ends every worker's turn and they all
  start again (inferred from `ds:src/app/sessions.ts:89-146` and
  `ds:docs/003-execution.md:144`).
- The owner's setup already has a closer match to the writeup's progress
  watchdog than the prototype's code does: `lanestat` and `lanewatch` flag
  stalled, wedged, looping and dead-provider lanes and send them to an
  orchestrator's inbox. The prototype's code has no progress or drift check
  (a search of `ds:src/` for drift, stall and stuck finds none).

## The problem space

### What a superagent is

The writeup names four traits: continuous (one agent across chat and GitHub),
productive (several tasks in parallel, takes the next instruction before the
last finishes), proactive (watches events and acts unprompted) and persistent
(always online, "recovers automatically from anomalies") (`pdf p3`). It argues
no single agent loop gives all four, so they come from the architecture
around it, and that a person can follow only 3 to 5 sessions at once
(`pdf p2-3`). The prototype's own philosophy is minimal human attention
(`ds:docs/000-philosophy.md`).

### Topology

The router is "a global sequence point" holding the overall picture. The
writeup says it needs no shell and can run a lighter model, and that workers get
the strongest models and isolated contexts (`pdf p5-7`). In the prototype the
router preset does have bash, limited to 5 seconds before it goes to the
background (`ds:cordis.patch.yml:121-158`), so the writeup's no-shell claim is
not what ships. The router's tools are `orchestrator_*` (route, eval, read,
reply, ticket notes and status, retry). Its prompt is 16 lines and forbids
waiting by sleep or polling (`ds:prompts/orchestrator.md`). Sharding the router
is future work (`pdf p6`, `p18`).

### Intake and triggers

Sources (GitHub, Feishu) raise triggers. A fast path filters cheaply and a slow
path asks the model (`pdf p8`). Admission is a deterministic gate followed by a
model assessment, which ends in a decision: accept, stop, ignore or unavailable
(`ds:docs/002-authorization.md`). Policies are versioned, and a policy change
revalidates open work (`ds:docs/002-authorization.md:131-141`). The router
batches triggers: at 40 a minute it handles 2.8 per call, and per-decision time
falls from 6.4 to 1.6 seconds (`pdf p17-18`).

### Communication

A subscription links tickets and threads many-to-many. A follow-up on a
subscribed thread fans out straight to the worker (`pdf p8-9`). Replies go back
on the channel the event came from, with per-channel norms such as a 100-word
cap in chat (`pdf p13`). Workers never talk to people
(`ds:docs/001-domain.md`). The router hears from workers as relays and
notices, batched at its next step (`ds:docs/007-architecture.md`).

### Memory and context

"Inline and spill" puts a short form of an event in the context and keeps the
rest outside it, and "chain of recall" puts the ids of related objects in each
recalled one, so context grows by hops on demand (`pdf p10-11`). The writeup's
single-session baseline never delegated in 5 runs and took 20.4 minutes at p50,
against 12.4 for the two-level design (`pdf p17`).

### Scheduling and parallelism

There is one worker session and one directory per ticket, a slot limit on
concurrent workers, and one extra slot kept for a person's message
(`ds:docs/003-execution.md:55-61`). A prepare command, usually `git worktree`,
gets 10 minutes. A shell command moves to the background after 5 seconds in a
worker, and background work may run for 30 minutes after a turn
(`ds:src/executor/dsh.ts:28-35`). The writeup describes an asynchronous
`start()`/`stop()` loop that takes input mid-turn, which it says is implemented
but not on by default (`pdf p12-13`). Speculative work, where the agent quietly
reproduces and fixes a bug while people are still discussing it, is described
(`pdf p11`); this research found no code for it.

### Crash and restart

Three mechanisms, all in the code:

- Transient provider and network errors (408, 429, 5xx, refused or reset
  connections) are retried 3 times with a growing backoff from 15 seconds
  (`ds:docs/003-execution.md:124`; `ds:src/executor/dsh.ts:28-35`).
- Stored effects (relays, launches, deliveries, replies) are retried from 2
  seconds, doubling to a minute, and show as a system problem after 3 failures
  in a row (`ds:docs/003-execution.md:153-161`; `ds:src/app/effects.ts:1-25`).
- After a process restart, running assignments end "host restarted" and start
  again with the recovery note, oldest ticket first. Settled assignments keep
  their state. Sessions, directories, subscriptions, pending messages and
  stored work survive (`ds:docs/003-execution.md:144-152`;
  `ds:src/core/engine.ts:241-247`).

What restarts the process is not in the prototype. `scripts/dsh-superagent
status` exits 0 for healthy, 1 for "a restart may fix it", 2 for a component
error and 3 for missing setup. A person asks an agent in a different DSH
profile to run it hourly: on 1 it restarts, on 2 or 3 it tells the person, and
if a restart fails it reads the logs and fixes what it can
(`ds:docs/006-deployment.md:115-141`). A manual stop leaves a marker the check
respects. The writeup's watchdog, an agent session that judges whether each
worker is making progress or drifting and that maintainers can question
(`pdf p15`), goes beyond this.

### Durable state and resume

SQLite through `node:sqlite`, in WAL mode with full sync
(`ds:src/app/store.ts:2`, `60-64`). The writeup goes further: stateless compute
in the cloud, state in a database, and a daemon on each endpoint the user
brings (`pdf p13-15`). That split is not in the prototype.

### Human in the loop

A panel shows only what needs a person (`ds:docs/007-architecture.md`). A
person can retry a blocked or failed ticket, or talk to its worker session
directly (`ds:docs/003-execution.md:120-143`). Accept mode `observe` is the
default, so the agent proposes and a person starts. The writeup lists
"sessionless" interaction and mid-task intervention as open problems
(`pdf p18-20`).

### Sandboxing

Workers run with `danger-full-access` as the operator's own user. The docs say
the wrappers are "not a security boundary" (`ds:docs/003-execution.md:80`;
`ds:docs/006-deployment.md:13-19`). The writeup lists cloud compromise taking
over endpoints, and credentials reaching the model's context, as open problems
(`pdf p19-20`).

### Evaluation

The writeup's ablation (`pdf p16-17`), 5 runs each:

| Variant | p50 / p95 minutes | Cache-read tokens |
|---|---|---|
| Two-level with pass-through | 12.4 / 15.7 | 23.2M |
| Two-level without pass-through | 12.6 / 15.5 | 31.8M |
| One session, neutral prompt | 20.4 / 29.5 | 6.1M |
| One session, told to delegate | 13.8 / 29.0 | 10.8M |

The prototype has no evaluation harness. Its admission "eval" is a gate on
incoming events, not a test of outcomes.

## Capability table

| Capability | Prototype | Fiber as designed | Owner's current setup |
|---|---|---|---|
| Worker: one agent loop per unit of work | One DSH session per ticket (`ds:docs/003-execution.md:25-47`) | One process per session (ADR 0009); delegates (`docs/delegates.md`) | One `--bg` Claude Code or pi lane per ticket (`ticket-orchestrator.md`) |
| Isolation per unit | One directory or worktree per ticket | `--worktree`; the session event records it (`docs/invocation.md`, "Isolation") | One worktree per lane (`tkt-start --worktree`) |
| Router or orchestrator | Main session with `orchestrator_*` tools | None. The supervisor is external (ADR 0005) | Long-lived surveyor and flywheel sessions; `ticket-orchestrator` per ticket |
| Event intake from GitHub or chat | GitHub and Feishu sources (`ds:docs/001-domain.md`) | None found in the docs read | None found; work starts from issues the orchestrators pick |
| Accept gate and policy modes | Gate, assessment, `off`/`observe`/`auto`, revalidation (`ds:docs/002-authorization.md`) | None. Permissions gate tool calls, not incoming work (`docs/permissions.md`) | Owner rulings, claims and fence grants (`fence-grant`, `file-claim`) |
| Ticket as a durable unit | Ticket with mandate and assignments in SQLite | None; plans live on GitHub Issues (`AGENTS.md`) | GitHub issue plus a state file and journal (`tkt-state`) |
| Follow-ups go straight to the worker | Thread subscription and pass-through (`pdf p8-9`) | Session messages and steering reach a named session (`GLOSSARY.md`, "Session message", "Steering message"); nothing maps a thread to a session | File inboxes (`inbox-send`, `inbox-wait`); orchestrators forward by hand |
| Replies on the source channel | Channel replies with backoff (`ds:docs/003-execution.md:175-191`) | None | Agents post to GitHub with `gh` |
| Background work outliving a turn | Background shell after 5 s, 30 min after the turn (`ds:src/executor/dsh.ts`) | Jobs (`GLOSSARY.md`, "Job"); at most 10 delegates (`docs/delegates.md:212`) | `delegate run` jobs and `run_in_background` waits |
| Durable state | SQLite, WAL, full sync | The session log is the record (ADR 0001) | Session logs, state files, job records under `$TMPDIR/delegate-jobs/` |
| Work survives a process crash | No: one process, every turn ends (inferred) | Keeper keeps jobs and Fiber delegates running and writes their endings (#3890, ready); other harnesses (#3942, blocked) | Lanes are separate processes; one dying leaves the rest |
| Automatic restart of the dead process | Hourly agent runs `status` and `restart` (`ds:docs/006-deployment.md:133-141`) | None. A crashed session stays `crashed` in the hub feed until a person resumes or dismisses it (`docs/invocation.md`, "The hub") | A stopped `--bg` session restarts from its handoff file, started by a person or orchestrator (global `CLAUDE.md`) |
| Resume after a crash | Recovery note: check before repeating outside actions (`ds:src/core/assignments.ts:21`) | Resume from the log; calls with no result are sent as "outcome unknown" and never re-run (`docs/events.md`, "Resume"; `docs/loop.md`) | Handoff files, state files and the issue history |
| Transient provider errors | 3 retries, growing backoff | Retry by code: `rate_limited`, `provider_unavailable`, `connection_failed`, `stream_incomplete` (`docs/errors.md`, "A failed model call") | Harness retries; `lanestat` reports `DEAD-PROVIDER` |
| Progress or drift watchdog | Writeup only (`pdf p15`); not in code | None found | `lanestat` and `lanewatch`: STALL, WEDGED, LOOP, PARK, DEAD-PROVIDER to an inbox; they never stop a lane |
| Large content outside the context | Inline and spill, chain of recall (`pdf p10-11`) | Artifacts (`GLOSSARY.md`, "Artifact"); memory pages with links (`GLOSSARY.md`, "Memory") | Report files named in a job's last message |
| Human attention | Panel of only what needs a person | Attention lines and the hub feed (`docs/permissions.md`; `docs/invocation.md`, "The hub") | Inboxes, rulings on issues, the owner types approvals in the denied session |
| Sandbox | Full access as the operator; "not a security boundary" | Opt-in sandbox (#3027; `docs/sandbox.md`) | Worktree guard and permission hooks in Claude Code |
| Speculative work | Writeup only (`pdf p11`) | None | None |

## Gaps worth a ticket

Candidates only. None is filed, and each needs the owner's ruling on whether it
is Fiber's job or the external supervisor's (ADR 0005).

- Automatic resume of a crashed supervised session. Evidence: a crashed session
  waits for a person in the hub feed (`docs/invocation.md`, "The hub"); the
  prototype restarts in-flight work on its own (`ds:docs/003-execution.md:144-152`).
  The keeper (#3890) already keeps the jobs alive, so the remaining step is who
  resumes. It may be enough to state that a supervisor resumes through the hub.
- A recovery note on resume for a turn a crash cut off. Evidence: Fiber tells
  the model a call's outcome is unknown (`docs/loop.md`) but not to check
  outside state before repeating an outside action; the prototype's note does
  (`ds:src/core/assignments.ts:21`).
- A progress and drift signal on the hub feed. Evidence: the writeup's watchdog
  (`pdf p15`) and the owner's `lanestat` states (STALL, WEDGED, LOOP) both
  derive from the event stream; Fiber's feed shows running, exited or crashed
  but no stall state (inferred from `docs/invocation.md`, "The hub").
- Thread-to-session routing for the factory. Evidence: pass-through cut
  cache-read tokens by about 27% at equal wall time (`pdf p17`); Fiber has
  session messages but nothing maps an outside thread to a session. This
  belongs with the factory direction (#3879).
- Event intake with an accept gate and an `observe` mode. Evidence:
  `ds:docs/002-authorization.md:91-101`; nothing in Fiber's docs starts work
  from an outside event. Also factory-shaped (#3879).
- None for transient provider retries: `docs/errors.md`, "A failed model call",
  already covers what the prototype does.
