# Fiber

Fiber is a terminal coding agent. This glossary is the project's vocabulary and
nothing else: it defines what words mean, never how anything is built.

It is deliberately small. Terms are added when a decision gives them meaning,
not in advance. Nothing here was inherited wholesale from the archived Zig tree;
each word was argued on its own merits.

## Language

**Session**:
The whole piece of work, from the first input to the last, surviving Fiber being
closed and reopened. A headless one-shot review started by another harness is a
session like any other.

**Turn**:
One round of the loop: input arrives, Fiber works, Fiber yields and waits.
_Avoid_: exchange, round

**Step**:
One round-trip to the model within a turn. A turn that makes four tool calls has
five steps.
_Avoid_: iteration, cycle, round

**Action**:
One thing Fiber produces inside a step that has a beginning, a middle and an end:
a message, a block of reasoning, or a tool call. Messages and reasoning are
actions as much as tool calls are.
_Avoid_: item, part, block, entry, step, move

**Event**:
One written record of something that happened. The event stream is the record.
_Avoid_: record, entry, line

**Session log**:
The events of one session, in order, as written down. It is the session's only
state of record: everything else is derived from it.
_Avoid_: journal, transcript, history file

**Durable event**:
An event a client needs in order to know the session's true state, work in
flight included. Durable events are exactly the session log.

**Ephemeral event**:
An event a later durable event makes obsolete, such as a fragment of streamed
text. Losing one costs nothing.

**Sequence number**:
A durable event's position in its session log. It is what a client stores to
say where it got to, and it is never reset or reused.
_Avoid_: offset, index, cursor position

**Fold**:
Replaying events in order to arrive at some present fact — what the model is
sent, what the screen shows, what a session has cost. A fold is never written
down as its own record.
_Avoid_: projection, snapshot, materialised view

**Tool call**:
Fiber doing something outside the conversation, such as reading a file or running
a command. It carries a request, a result, and a record of whether it actually
ran.
_Avoid_: tool use, function call

**Job**:
Work a tool call starts that outlives the call, such as a background command, a
monitor or a delegate. It has its own id and its own ending, and it survives
cancellation of the turn that started it.
_Avoid_: background session, task, process

**Delegate**:
An agent session a session starts to do part of its work, running Fiber or
another harness. It is a job, and its own session.
_Avoid_: subagent, child, worker

**Harness**:
The agent program a delegate runs: Fiber, or another vendor's, such as Claude
Code or cursor-agent.
_Avoid_: agent type, backend

**Session tree**:
A top-level session and every delegate under it. Each session in it is its own
process, with its own MCP servers and extensions. Nothing is shared between
them.
_Avoid_: process tree, job tree

**Daemon**:
The optional long-lived process that lets remote clients start and attach
sessions. It holds no session.
_Avoid_: server, gateway, broker, host

**Fork**:
A Fiber delegate whose history begins as its parent's conversation up to a
point, shared rather than copied.
_Avoid_: clone, branch

**Rewind**:
A new session that continues an existing one from an earlier point, sharing its
history rather than copying it. The existing session keeps its history.
_Avoid_: branch, backtrack, checkpoint, undo

**Handoff**:
Restarting the model's context from a handoff note, appended to the session log
so nothing before it is lost. Started automatically, by a person, or by a tool.
_Avoid_: compaction, summary, collapse, reset

**Handoff note**:
The text a handoff restarts the model's context from, written by the session's
own model.
_Avoid_: summary

**Prompt cache**:
A provider's store of the start of a request, reused by a later request whose
leading bytes match. A request that differs from some point on pays in full
for everything after it.
_Avoid_: context cache, KV cache

**Preamble**:
What every model request in a session starts with: the system prompt, the tool
definitions and the request settings. It changes only when it is built again.
_Avoid_: prefix, header

**Opening message**:
The first message of a session's conversation, written by Fiber: the
environment, the project's instruction files and the skills listing. It is
logged once and written again after each handoff. See `docs/system-prompt.md`.
_Avoid_: environment context, session context

**Instruction file**:
An `AGENTS.md`, or a `CLAUDE.md` in a directory with no `AGENTS.md`, that a
project or a person writes for the model. It applies to its directory and
everything below it.
_Avoid_: context file, project doc, memory file

**Role**:
A configured name for a delegate's model reference, so that written
instructions survive a model being withdrawn.
_Avoid_: preset, alias, tier

**Artifact**:
The full bytes behind something too large to put in the session log, such as a
cut tool result or an image, kept beside the log and referenced from it by path.
_Avoid_: attachment, blob, spill file, result store

**Watcher**:
Something that reads the event stream and cannot reply. The terminal's
rendering, a non-interactive run's stdout, a second client. A watcher can be
absent, slow or added later without changing the session.

**Driver**:
Something that sends commands to a session. The terminal's input, and stdin on
the non-interactive door. A driver may only send the commands Fiber defines.
_Avoid_: controller

**Client**:
Something attached to a running session that watches it and may drive it: the
terminal, a script on stdin, a phone through the daemon. Every client has the
same powers. A session with no client finishes its work and exits.
_Avoid_: frontend, UI, consumer

**Participant**:
Something the loop asks during a turn, whose answer may refuse or change what
happens. Tools, providers and hooks are participants. A watcher and a driver
never are.

**Seam**:
The defined surface one kind of participant is reached through. Fiber has three:
the tool seam, the provider seam and the hook seam.
_Avoid_: interface, plugin point, API

**Hook**:
A participant asked before or after something happens, which may allow it,
change it or refuse it. Distinct from a tool, which is asked to do work, and a
provider, which is asked for a model's response. A hook only changes things;
something that only needs to know what happened is a watcher. See
`docs/extensions.md`.

**Hook point**:
One of the fixed places in a session where hooks are asked, such as
`before_tool` or `turn_end`. Each runs before what it changes is logged or sent
to the model.
_Avoid_: hook event, lifecycle event

**Extension**:
A package Fiber installs and loads, registering tools, providers and hooks
through the three seams exactly as a built-in does, and watchers and commands
besides. It may also carry skills, prompt templates, themes and binaries. Its
code runs with the account's full rights, as a Lua extension, a process
extension or both. See `docs/extensions.md`.
_Avoid_: plugin, addon, module

**Lua extension**:
An extension whose code is Lua 5.4 run inside the session's own process.
_Avoid_: script, embedded extension

**Process extension**:
An extension whose code is a separate program, in any language, that the
session starts and talks to over a pipe as a client with extra rights. It is
not an MCP server.
_Avoid_: external extension, sidecar, plugin host

**TUI extension**:
The part of an extension that runs in the terminal's process and draws there.
It reaches its session only through the event stream and driver commands, like
any client.
_Avoid_: UI plugin, facet

**Extension state**:
The values an extension keeps in a session's log, by key, so they survive a
resume and follow a rewind or fork. An extension's configuration, credentials
and data directories are not extension state.
_Avoid_: extension storage, session state, entry

**Fork rule**:
What a fork or rewind gives one key of extension state: its value at the fork
point, the parent's latest value, or nothing.
_Avoid_: fork policy, inheritance

**Data directory**:
A directory in Fiber home that belongs to one extension, one per machine and
one per project, for what it keeps across sessions.
_Avoid_: storage, cache, extension home

**Extension approval**:
A person's decision to let a repository's extension load, made after seeing
what it registers and carries. It covers exact content: a changed extension
needs a new approval.
_Avoid_: review (the reviewer is the permission model), trust

**MCP server**:
A program Fiber starts or connects to that offers tools, prompts and resources
over MCP. See `docs/mcp.md`.
_Avoid_: MCP tool server, plugin

**Provider**:
An endpoint Fiber sends model requests to: a name, a credential and a list of
models. Every provider is an extension. See `docs/model-routing.md`.
_Avoid_: connection, backend, vendor

**Protocol**:
A wire format a provider speaks: how a request is shaped and how a streamed
reply is read. Protocols are built into Fiber; an extension cannot add one.
_Avoid_: API, dialect, adapter

**Fiber home**:
The one directory holding everything Fiber writes outside a repository,
`~/.fiber` unless `FIBER_HOME` moves it.
_Avoid_: state directory, configuration directory, config dir

**Configuration layer**:
One place a configuration value can come from. From lowest to highest: built-in
defaults, global, repository, per project, per run. A higher layer wins key by
key. See `docs/configuration.md`.
_Avoid_: scope, profile, level

**Project**:
A git repository, identified by git's shared directory so its worktrees are one
project and a separate clone is another; outside git, the launch directory.
Sessions, prompt history and per-project standing rules are kept per project,
distinct from the workspace, which is the root a session was launched against.
_Avoid_: repo, workspace

**Workspace**:
The root Fiber was launched against, recorded on `session_started`. Where a
project's own files live, and the boundary a permission decision turns on.
_Avoid_: project root, cwd, repo

**Effect**:
What one tool call does, in a closed vocabulary its tool declares before the
call runs: reads, writes, executes, network, plus whether it is reversible and
which paths it touches. Effects are what the loop reasons about; it never sees
the call's arguments.
_Avoid_: capability, permission, risk, category

**Reviewer**:
The model asked whether a tool call may proceed. It is shown the human's
messages and the agent's tool calls, never the model's own prose or any tool
result, and it answers allow or block with a reason.
_Avoid_: guardian, classifier, gate, judge

**Standing rule**:
A permission decision recorded in Fiber home, surviving every session until
deleted. Scoped globally or to one project.
_Avoid_: saved rule, policy, preference

**Session grant**:
A permission decision recorded in the session log, honoured for the rest of
that session and gone when it ends. Never written anywhere but the log.
_Avoid_: always-allow, remembered approval

**Step boundary**:
The point between one round-trip to the model and the next, where the loop
accepts anything that arrived while it was busy. The only moment inside a turn
at which a turn can change course.
_Avoid_: checkpoint, safe point, yield point

**Steering message**:
Input sent while a turn is running, which joins that turn at the next step
boundary rather than starting a new one. A steering message the turn ends
before applying becomes the next turn's input.
_Avoid_: follow-up, queued prompt, interjection

**Shutdown**:
Fiber stopping because a signal told it to: within a bound, stopping everything
the session started, asking nobody anything. Distinct from exiting because the
work ran out, which waits for jobs.
_Avoid_: teardown, graceful exit, termination

**Front door**:
A way of starting Fiber. There are two: the terminal, and the non-interactive
door. Which subcommand opened a door changes where its prompt comes from and
when it exits, never what the loop does.
_Avoid_: mode, entry point, interface

**Driver command**:
One thing a driver may send to a session, from a closed set Fiber defines. A
command is not an event: it goes in, it is acknowledged, and what it causes is
recorded as events like everything else.
_Avoid_: request, message, RPC

**Cancellation**:
Ending a turn early because a person asked. The model stream stops, in-flight
tool calls complete as cancelled, and the turn completes with the outcome
interrupted. Background jobs survive it, because a job outlives the turn that
started it.
_Avoid_: abort, stop, kill, interrupt (the outcome is named interrupted; the
act is cancellation)

## Deliberately unnamed

**One Fiber process, from launch to exit.** The archived Zig tree called this a
`run` and set sessions above it. Fiber does not name it: resume, event ordering
and the headless slot are all expressed in terms of sessions, and a session's
processes never overlap, so nothing has to tell two of them apart. Name it when
a decision needs it.

The session log records the boundary without naming the unit: `fiber_started`
and `fiber_exited`. A start with no matching exit is how a resumed session knows
the previous process died rather than finished.

## Reading pi's source

pi is Fiber's reference for provider and wire behaviour, not for these nouns, and
the two vocabularies collide. When reading pi:

| The thing | Fiber | pi |
|---|---|---|
| The whole piece of work | session | session |
| Input arrives, Fiber works, Fiber yields | **turn** | agent run (`agent_start` … `agent_settled`) |
| One round-trip to the model | **step** | **turn** (`turn_start` / `turn_end`) |
| One message, reasoning block or tool call | **action** | message (`message_start` / `message_update`) |
| One process, launch to exit | *(unnamed)* | *(unnamed)* |
