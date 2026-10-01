# Permissions and approvals

What Fiber may do without asking, who it asks, and what the log records. This
is what is true now, not a plan. It is settled by
[Permissions and approvals, attended and headless](https://github.com/aakshintala/fiber/issues/13);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Effect, workspace, reviewer, standing rule, session
grant, watcher, driver, participant, tool call and event mean what it says
there and nothing else. The request and reply events themselves are
`docs/events.md`; this page is only the policy over them.

## The rule everything else follows from

**Nothing in Fiber decides what is dangerous by looking at a command.** A tool
is asked what a specific call does, in a closed vocabulary, and every later
decision is made from that answer. The loop never sees the command string and
never learns which tool produced the call, which is what
`docs/architecture.md` requires: "`loop` never names a tool, a vendor or a
provider. It reasons about what a tool is allowed to do, never about which
tool it is."

The consequence worth stating first: **adding a tool never means editing the
permission policy.** A tool arrives already able to describe itself.

## Effects

Before a tool call runs, its tool declares that call's **effects**:

| Effect | Meaning |
|---|---|
| `reads` | observes something without changing it |
| `writes` | changes something that persists after the call |
| `executes` | runs a program |
| `network` | sends or receives outside this machine |

A call declares every effect that applies, plus two qualifiers: whether it is
**reversible**, and the **paths** it touches where it has any. A shell tool
asked to run `git status` declares `reads`, reversible. Asked to run
`rm -rf build/`, it declares `executes`, not reversible, with no paths,
because the shell only recognises read-only commands (`docs/tools.md`,
"Shell"). An edit to an existing file is an irreversible write; creating a new
one is a reversible write.

Classification is per call, not per tool. A tool that classified itself once,
at registration, would make every shell call as dangerous as the worst shell
call, and the only escape from that noise would be no protection at all.

A tool registered by an extension classifies its own calls and is believed.
An extension runs with the account's full rights, so misdeclaring buys it
nothing it could not do directly. pi states the same boundary in its own
security documentation: extensions "run with the same permissions" as the
process, and that is outside its security boundary. Fiber's own built-in tools go through this
seam identically.

An MCP tool declares its effects from the hints its server gives, per tool
rather than per call. How the hints map to effects is `docs/mcp.md`
("Effects").

## One mode

Every session runs in `auto`, interactive or headless, a delegate included.
There is no other mode and no way to change it. Calls the person wants decided
differently are standing rules ([Remembering a decision](#remembering-a-decision)).

| What answers | When |
|---|---|
| a fast path, a session grant or a standing rule | the call matches one |
| the reviewer | everything else, escalating to a human on repetition or failure |

There is no setting in which a person answers every call. A session offline,
or with the reviewer's provider down, is still gated: a reviewer that fails
hands the call to a person ([What happens on a block](#what-happens-on-a-block)),
and the person can switch the reviewer to another model. A call a person
wants to see every time is a standing ask.

## The order a call is judged in

A `before_tool` hook runs first. It may refuse a call or rewrite its
arguments, and a rewritten call is classified again before this order judges
it. A hook can never approve a call (`docs/extensions.md`, "Hooks").

1. **The credential deny**: a call whose paths touch Fiber home's
   `credentials/` is refused. See [Credentials](#credentials).
2. **A standing deny** matching this call: refused. No model call, no question.
3. **A standing ask** matching this call: a human is asked.
4. **A fast path** — see below: allowed, with no model call.
5. **A session grant** matching this call: allowed.
6. **A standing allow** matching this call: allowed.
7. Otherwise **reviewed** by the reviewer.

A call to `delegate_spawn`, `delegate_fork` or `delegate_message` skips steps
4 to 6 and is always reviewed ([Delegates](#delegates)).

The credential deny and a standing deny are both evaluated before everything
else, because a rule that can be widened by a later layer is not a deny.

### Fast paths

Three classes of call never reach a reviewer or a person:

- every call whose only effect is `reads`, or that declares no effect,
- a `writes` call whose paths all sit inside the **workspace**, none of them
  under `.git/` or `.fiber/`, and
- a web search, and a web fetch to a host known in the session
  (`docs/tools.md`, "Web fetch and web search"). A fetch to any other host
  is reviewed.

Everything else — shell execution, other network calls, and any write outside the
workspace — is reviewed. A write under `.git/` or `.fiber/` is reviewed too: a hook or
`.git/config` runs code on the next `git` command, which the reviewer judges
as `git commit` without seeing the hook, and `.fiber/` holds the repository's
configuration and MCP declarations. This is where nearly all of the cost is saved, and it
is the line Claude Code draws: a fixed allowlist of state-free tools, plus
"file writes and edits inside the project directory are allowed without a
classifier call."

Editing the repo you launched in is the most common thing a coding agent does.
Putting a model call in front of it buys little — the session log records every
edit and version control can undo one — and costs latency on every action.

## Credentials

Fiber refuses every tool call whose declared paths touch the credential
directory in [Fiber home](state.md) (`credentials/`). This is a built-in
deny: it is not a standing rule, no person or extension can remove it, and
it applies to every call. It covers every effect — a read, a
write, anything — not only reads.

Fiber does not confine tools ([Confinement](#confinement)), so without this
deny an agent's file read could retrieve the stored tokens. The macOS Keychain is not an
alternative protection: any process running as the person can read a keychain
item the same way Fiber would. Source: the archived tree's
[Deny tool reads of the credential store](https://github.com/aakshintala/fiber-zig/issues/97).

Paths are canonicalised before matching (symlinks resolved, `..` removed), so
no spelling of a declared path escapes it. The deny follows the resolved Fiber home,
so it still protects the credentials when `FIBER_HOME` moves Fiber home. If
Fiber home cannot be resolved, Fiber does not start ([Fiber home](state.md)
already makes a bad `FIBER_HOME` a startup error), so the deny can never be
silently narrowed.

Like every other decision on this page, it relies on declared paths. The
shell tool declares paths only for commands it recognises as read-only
(`docs/tools.md`, "Shell"). A command it does not recognise, such as
`python -c` opening a token file, declares no paths and the deny does not see
it: the call is reviewed.
An extension tool that misdeclares its paths gets nothing it could not do
directly, the same boundary the Effects section already states for
extensions.

## Confinement

Settled by
[Does Fiber confine what tools can touch?](https://github.com/aakshintala/fiber/issues/30);
that ticket's resolution holds the rationale and the rejected alternatives.

Fiber does not confine what a tool call can reach at the operating-system
level. A program a tool starts runs with the account's full rights, on every
platform. Everything on this page decides whether a call
runs; nothing limits what it does once it runs.

- Approving an `executes` call grants whatever the account can do. That
  includes Fiber home: an approved command can read `credentials/`, edit
  `rules`, or write an extension. Approving a call is trusting it, the same
  boundary as installing an extension.
- Isolation comes from outside Fiber: run it in a container or a virtual
  machine. pi takes the same position in its security documentation: "Real
  isolation needs to come from the operating system or a
  virtualization/container boundary."
- A container protects the host from Fiber, not Fiber home from the agent.
  Fiber home inside the container is as reachable as it is outside one.

codex confines every command by default and asks only to escape. Claude Code
ships a sandbox that is off by default, and its path denies are enforced by
the operating system only while that sandbox is on. How each works, and what
a fence would have broken in the owner's sessions, is
[research/sandbox/](../research/sandbox/).

## The reviewer

A call that reaches step 7 is judged by a model.

### What it is shown

**Only the human's messages and the agent's tool calls.** The model's own
prose and every tool result are stripped before the prompt is built. The
reviewer also gets the call under review, its declared effects, and the
workspace root.

This is the load-bearing decision on this page. The reviewer's prompt contains
no text the agent wrote and no text a repository file produced, so there is
nothing in it that can argue for a verdict. Anthropic's published rationale
for the same design is exactly this: the classifier is "reasoning-blind by
design" so that "the agent can't talk the classifier into making a bad call."

The rejected alternative is Codex's: send the whole transcript with an
instruction to treat it as "untrusted evidence, not as instructions to
follow." That label is enforced by asking the model nicely, and the prompt
grows for the life of the session, which is why Codex needs token budgeting
and compaction around its reviewer. Stripping is both cheaper and stronger.

Project instruction files are also excluded. They live in the repository, so
including them would let a repository write into the reviewer's prompt — the
hole that stripping tool output was meant to close.

A session message from another session is excluded too (`docs/tools.md`,
"Messaging other sessions"). Another model wrote it, so including it would let
one session approve calls in another by messaging it.

### How it runs

Two stages. The first asks for a single token: `check`, meaning the call goes
to the reasoning pass, or `allow`. Only a call answered `check` gets the
reasoning pass. Most reviewed calls cost one token.

The reviewer's model is chosen separately from the session's, because a
review on every effectful action at the session model's price and latency is a
cost nobody chose. It is `reviewer.model` when set (`docs/configuration.md`).
Otherwise it is the reviewer model the session's provider names in its data
(`docs/model-routing.md`, "What a provider extension declares"). When neither
names one, every reviewed call escalates as `reviewer_failed` with the error
`no_model`, and a `notice` with code `no_model` says to set `reviewer.model`.
Fiber never reviews with the session's own model.

### Its verdict

`allow` or `block`, plus a reason. There is no risk scale: a number a model
assigns is only as calibrated as the model, and nothing here reads one.

## What happens on a block

**A block is not the end of the turn.** The refusal is returned to the model as
the tool call's result, with the reason and an instruction to respect the
boundary and find another way. The turn continues.

A human is interrupted when the agent keeps hitting the wall: **3 consecutive
blocks, or 20 in a session**, both configurable. The block that reaches the count
is not returned to the model: the call goes to a person instead, as a
`permission_requested` whose `escalation` gives the cause and the reviewer's
reason. The person's answer ends the run of consecutive blocks. Escalating every block would
send a person every call under load, and a person answering a stream of
questions stops reading them.

**A reviewer that fails is a block, never an allow.** A verdict that cannot
be read, after the argument repair in `docs/tools.md` ("Before a call runs"),
is asked for once more with the error attached. An unreachable provider,
a timeout, a verdict still unreadable on that second ask, a missing
credential: each escalates to a human, and blocks where no answer is possible ("Headless"). There is no
path on this page where an error results in an action running.

## Remembering a decision

Two layers, and they are different kinds of thing.

A **session grant** is an event in the session log. It is honoured for the rest
of that session, survives a resume because the log does, is visible to every
client, and is gone when the session ends.

A **standing rule** is a line in [Fiber home](state.md). It survives
everything until deleted, and it is a file a person can read and revoke.

This split is forced. `docs/events.md` says "No sidecar. A session directory
holds the log, a lock, and directories for bytes too big to inline." A rule
that outlives its session cannot be session state, so it is configuration, and
`docs/architecture.md` says "Only `config` reads the configuration files in
Fiber home."

### Scope

Standing rules come from two files in [Fiber home](state.md): `rules` at the
top level, and the project's `rules` file. The project's rules win where
both match.

A project is identified by **git's shared directory**, so every worktree of a
repository shares one set of rules and a separate clone does not. Outside a git
repository, the launch directory is the project.

### What a rule matches

A tool and a prefix of its primary argument. The tool reads its own
arguments, so its effects function returns both halves with the effects: the
call's **subject**, its primary argument, and the **prefix** it offers as the
widening (`docs/tools.md`, "What a tool declares"). The loop never parses a
command. A prefix ending in `/` matches every subject that
starts with it. Any other prefix matches a subject equal to it, and for the
shell also one that goes on with a space: `npm test` matches
`npm test --watch` and not `npm testing`. A tool
with no primary argument, such as an MCP tool, returns an empty subject, and
its rule matches the tool by name. A call a rule cannot safely match, such as
a shell command with more than one part, returns no subject, and no rule or
session grant matches it, a deny or an ask included. A shell rule matches a
command's text, not what it does: a deny on `rm -rf ~/` misses `rm -fr ~`,
and a command of more than one part, or one the classifier cannot read
plainly, always reaches the reviewer, which judges what it does.

Approving `npm test -- --watch` can remember the subject, or the prefix
`npm test`; the terminal offers the prefix, and shows it. The widening is an explicit, separate choice at the moment
of approval, so a rule never grants more than what was read when it was
written.

An approval remembers in one of two scopes, the person's choice: a session
grant, or a standing rule appended to the project's rules file. A global rule
is added by editing the global rules file. Only a request raised at step 7
offers to remember: a standing ask comes before every allow, so an allow rule
could never answer it.

A deny rule and an ask rule are matched the same way and are evaluated first.

## What the log records

Both events are defined in `docs/events.md` and are durable. This page fixes
their contents.

`permission_requested` carries the tool call's `action_id`, the call's declared
effects, its paths, and which step of the order above sent it here. It also
says why a person is asked: on a standing ask, the rule that asked; on a
review, the escalation's cause; and on a review, the rule an allow
can remember.

`permission_resolved` carries the decision, the reason, and **what decided it**:
the credential deny, a human, a standing rule, a session grant or the
reviewer. A reviewer decision also carries the reviewer's model and which
stage decided.

A blocked call ends as `tool_call_completed` with `status: denied` and a
`reason`, both already in the contract. The proof that nothing ran is also
already there: `docs/events.md` guarantees that `tool_call_requested` with no
`tool_call_started` means "provably never ran; safe to run or discard". A
reviewer's verdict never causes a `tool_call_started`, so a caller auditing a
session can establish that a denied call had no effect without trusting this
page at all.

A session grant is recorded the same way, as a `permission_resolved` whose
decision says it applies to later matching calls. Nothing else is written down:
the grant is a fold of the log, like every other derived fact. A standing rule
added from an approval is written to the project's rules file, and its
`permission_resolved` names it too, so the log shows where the rule came from.

An escalation where no answer is possible writes no `permission_requested`.
It is the reviewer's block, recorded as a `permission_resolved` with
`decided_by: reviewer`.

## Headless

A run started with no client, such as `fiber ask`, runs in `auto` like
every other session. The reviewer is what stands in for the person, which is
the case it exists for.

A calling harness that wants to answer can. `docs/architecture.md` settles that
"the terminal is a watcher and a driver, never a participant" — a permission
request goes on the event stream and any driver replies to it identically, so a
harness driving Fiber answers exactly as the terminal does and needs no
private channel. Claude Code makes the same distinction with
`--permission-prompts host|none`.

With no answer possible, escalation is a block and the run continues under
the rule above until it exhausts the block budget. The turn then completes
`failed` with code `blocked`, so a headless caller learns the task needed
permissions it was not given. No answer is possible in a
session started by `fiber ask`, and in a session that has been sent `close`.
Anywhere else, a person may come back: after `session.idle_exit_ms` the session
exits on the pending escalation and raises it again when resumed
(`docs/invocation.md`, "Lifecycle").

## Delegates

- Starting or messaging a delegate is always reviewed. `delegate_spawn`,
  `delegate_fork` and `delegate_message` declare `executes`, and no fast
  path, session grant or standing allow skips the parent's reviewer for them.
  A delegate's reviewer reads its parent's prompt and messages as the human's,
  so without this a parent's model could talk a delegate's reviewer into an
  action the parent's own reviewer never judged. The parent's reviewer judges
  the prompt or message against the parent's own person's messages.
- Each Fiber delegate has its own reviewer, which judges that delegate's
  calls.
- An escalation from a delegate is a block, as for any headless run, and the
  delegate carries on. It is never relayed up the tree.
- A model never answers a delegate's approval, the parent's model included.

A delegate on another harness is judged by that harness, in its own auto
mode (`docs/delegates.md`, "Harness extensions"). The credential deny and
standing rules do not reach inside it: the harness's own rules, as the person
configured them, apply there. A harness and model are offered as a delegate
only when their auto mode is declared to work.

## Not settled here

- Which tools exist is indexed in
  [Epic: tools](https://github.com/aakshintala/fiber/issues/59); how a tool
  declares effects is `docs/tools.md`.
- How the reviewer's model is named and routed:
  [Provider and model routing](https://github.com/aakshintala/fiber/issues/12).
