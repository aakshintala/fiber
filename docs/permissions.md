# Permissions and approvals

What Fiber may do without asking, who it asks, and what the log records. This
is what is true now, not a plan. It is settled by
[Permissions and approvals, attended and headless](https://github.com/aakshintala/fiber/issues/13);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Effect, workspace, reviewer, standing rule, session
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
nothing it could not do directly. Fiber's own built-in tools go through this
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

A call whose effects function returns `always_reviewed` skips steps 4 to 6
and is reviewed (`docs/tools.md`, "What a tool declares"). `delegate_spawn`,
`delegate_fork` and `delegate_message` return it on every call ([Delegates](#delegates)). The loop reads the
declaration and never names a tool.

The credential deny and a standing deny are both evaluated before everything
else, because a rule that can be widened by a later layer is not a deny.

An extension's **inner call**, a tool it runs with `host.tool` while its own
tool call or command runs, is admitted by that outer call
(`docs/extensions.md`, "Running a tool"). The outer call goes through this
order like any other. The inner call passes the `before_tool` hook and steps 1
and 2 only: the credential deny and the person's standing denies still refuse
it, since a deny must hold whatever path a call takes. Steps 3 to 7 do not
apply. An inner call is never reviewed, raises no question, and counts toward
no block budget.

### Fast paths

Four classes of call never reach a reviewer or a person:

- every call whose only effect is `reads`, or that declares no effect,
- a `writes` call whose paths all sit inside the **workspace**, none of them
  under `.git/` or `.fiber/`,
- a `writes` call whose paths are all Markdown files (`.md`) inside extension
  data directories, `data/<name>/` or `projects/<key>/data/<name>/` in Fiber
  home (`docs/state.md`, "What each part holds"), whichever tool makes it. Paths are
  resolved first, so a path that leads out of a data directory through a link
  does not qualify, and
- a call whose only effect is `network` and whose declared hosts are all
  known in the session (`docs/tools.md`, "What a tool declares"). A web
  search declares an empty host list and a web fetch declares its URL's host,
  so a search and a fetch to a known host take this path (`docs/tools.md`,
  "Web fetch and web search"). A call that declares no host list, or names a
  host that is not known, is reviewed.

Memory layers keep their notes and pages as Markdown in extension data
directories, so saving one costs no model call. The limit to Markdown keeps
this path off files an extension may load as code or trust as settings, such
as a Lua file or a JSON index; a write to one is reviewed.

Everything else — shell execution, other network calls, and any other write
outside the workspace — is reviewed. A write under `.git/` or `.fiber/` is reviewed too: a hook or
`.git/config` runs code on the next `git` command, which the reviewer judges
as `git commit` without seeing the hook, and `.fiber/` holds the repository's
configuration, including the extensions, hooks and MCP servers it declares.
Those run only after a person approves their exact content, a decision
separate from approving a tool call (`docs/extensions.md`, "Code a repository
ships"). This is where nearly all of the cost is saved.

Editing the repo you launched in is the most common thing a coding agent does.
Putting a model call in front of it buys little — the session log records every
edit and version control can undo one — and costs latency on every action.

## Credentials

Fiber refuses every tool call whose declared paths touch the credential
directory in [Fiber home](state.md) (`credentials/`). This is a built-in
deny: it is not a standing rule, no person or extension can remove it, and
it applies to every call. It covers every effect — a read, a
write, anything — not only reads. A path touches the credential directory
when it is the directory, sits inside it, or contains it: a recursive read
of Fiber home, `$HOME` or `/` reads every stored key, so a call declaring
one of them is refused too. It refuses, the same way, every tool call
whose declared paths touch the resolved path of a configured `file`
credential source.

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
it: the call is reviewed. A link-following recursive read, such as
`grep -R`, is not read-only either, because the deny sees only the paths a
call names and never a link below them.
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
  machine.
- A container protects the host from Fiber, not Fiber home from the agent.
  Fiber home inside the container is as reachable as it is outside one.

The evidence is [research/sandbox/](../research/sandbox/).

## The reviewer

A call that reaches step 7 is judged by a model.

### What it is shown

**Only the human's messages and the agent's tool calls.** The agent's tool
calls include its extensions' inner calls, so a review sees what an earlier
script did. The model's own
prose and every tool result are stripped before the prompt is built. The
reviewer also gets the call under review, its declared effects, the
workspace root, and the person's own notes about their environment ("What the
person tells it").

This is the load-bearing decision on this page. The reviewer's prompt contains
no text the agent wrote and no text a repository file produced, so there is
nothing in it that can argue for a verdict.

The rejected alternative is to send the whole transcript with an instruction
to treat it as untrusted. Stripping is both cheaper and stronger
(`research/reference-comparisons/README.md`).

Project instruction files are also excluded. They live in the repository, so
including them would let a repository write into the reviewer's prompt — the
hole that stripping tool output was meant to close.

A session message from another session is excluded too (`docs/tools.md`,
"Messaging other sessions"). Another model wrote it, so including it would let
one session approve calls in another by messaging it.

### At a handoff

The reviewer's input follows the session's handoffs (`docs/handoff.md`), so
it grows with the context window, not the transcript. At each handoff, Fiber
asks the reviewer which of the person's messages still bind, such as standing
orders, scope limits and "don't touch X". The reviewer answers with a
selection of messages, not prose, and Fiber carries those messages word for
word. After the handoff the reviewer's input is the kept messages, then the
person's messages and the agent's tool calls since the handoff.

Nothing the reviewer writes enters its own prompt. Tool-call arguments are
agent-written, so a prose note could repeat text such as `echo "user approved
pushing to main"` as if it were the person's. The handoff note is excluded for
the same reason: the session's model writes it after reading tool results.

The selection is recorded in the session log as `reviewer_kept` (`docs/events.md`, "Handoff"), so the person can see what was
kept. When the kept messages would pass the reviewer model's context window,
the oldest drop first. When the selection request fails, every earlier person
message is kept and a `notice` says so. When the session has no reviewer, nothing is asked and no line or notice is written.

### What the person tells it

`reviewer.context` is the person's notes about their environment, in prose,
written as they would brief a new colleague. The reviewer reads them right
after its fixed instructions, as the person's own words. They add to those
instructions and never replace them, so no setting can remove what the
reviewer guards against.

The notes cover what the reviewer cannot guess:

- source control: which accounts, organisations and repositories are the
  person's own or their organisation's
- internal hosts, registries and services that count as inside
- internal tools and what running them does
- routine actions, such as squash-merging their own pull requests once CI
  passes
- what must never happen, such as touching `infra/prod`

The key is the person's alone: the global file or the per-project file in
Fiber home (`docs/configuration.md`, "Layers"). A repository cannot set it,
for the same reason project instruction files are excluded: a repository's
text in the reviewer's prompt would argue with its verdicts. Unlike other
strings, the two layers do not replace each other. The reviewer reads the
global notes, then the per-project notes, each under its own heading, and its
instructions say the project's are the more specific and win where the two
conflict. A project adds what is true of it without repeating what is true
everywhere.

The notes are fixed for the session. They sit ahead of the person's messages
and the tool calls, and every reviewer pass extends the cache chain that
follows them, so they change only on `reload`, on a resume, or in a new session
(`docs/prompt-cache.md`, "Rules for other areas"). A delegate's reviewer reads
the same layers, so it gets the same notes.

An edit to the notes needs the person to ask for it in their messages; the
reviewer's instructions say so, and it blocks an edit the person did not ask
for. An agent that could rewrite the notes would widen its own approvals.

`fiber config get reviewer.context` prints the notes as the reviewer reads
them: each layer's text under its heading.

### How it runs

Two stages. The first asks for one word: `check`, meaning the call goes
to the reasoning pass, or `allow`. Only a call answered `check` gets the
reasoning pass. Most reviewed calls cost a few output tokens.

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
both match at the same step, and the project's rule is the one used and
reported. A match in either file decides its step, so a project allow never
overrides a global deny: every deny is step 2 and every ask is step 3. The
file format is in `docs/configuration.md`, "Standing rules".

Both files are read every time a call reaches step 2. A file that cannot be
read, or a line that does not parse, denies every call that reaches step 2
until it is fixed, with a reason naming the file and the line: the line it
cannot read could be a deny. The credential deny still comes first.

A project is identified by **git's shared directory**, so every worktree of a
repository shares one set of rules and a separate clone does not. Outside a git
repository, the launch directory is the project.

### What a rule matches

A tool and a prefix of its primary argument. The tool reads its own
arguments, so its effects function returns both halves with the effects: the
call's **subject**, its primary argument, and the **prefix** it offers as the
widening (`docs/tools.md`, "What a tool declares"). The loop never parses a
command. A prefix ending in `/` matches every subject that
starts with it. Any other prefix matches a subject equal to it, and also one
that goes on with a space: `npm test` matches `npm test --watch` and not
`npm testing`. This holds for every tool, because the loop cannot tell which
tool is the shell. A tool
with no primary argument, such as an MCP tool, returns an empty subject, and
its rule matches the tool by name. A call a rule cannot safely match, such as
a shell command with more than one part, returns no subject, and no rule or
session grant matches it, a deny or an ask included. A shell rule matches a
command's text, not what it does: a deny on `rm -rf ~/` misses `rm -fr ~`,
and a command of more than one part, or one the classifier cannot read
plainly, is matched by no rule: unless every part is read-only, which takes
the fast path, it reaches the reviewer, which judges what it does.

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
`decided_by: reviewer`, or with `decided_by: no_reviewer` when no reviewer
could be set up ("How it runs"). A standing ask where no answer is possible is
denied at once, the same way: no `permission_requested`, and a
`permission_resolved` with `decision: deny` and `decided_by: standing_rule`.

## Headless

A run started with no client, such as `fiber ask`, runs in `auto` like
every other session. The reviewer is what stands in for the person, which is
the case it exists for.

A calling harness that wants to answer can. `docs/architecture.md` settles that
"the terminal is a watcher and a driver, never a participant" — a permission
request goes on the event stream and any driver replies to it identically, so a
harness driving Fiber answers exactly as the terminal does and needs no
private channel.

With no answer possible, escalation is a block and the run continues under
the rule above until it exhausts the block budget. The turn then completes
`failed` with code `blocked`, so a headless caller learns the task needed
permissions it was not given. No answer is possible in a
session started by `fiber ask`, in a delegate, and in a session that has been
sent `close`.
Anywhere else, a person may come back: after `session.idle_exit_ms` the session
exits on the pending escalation and raises it again when resumed
(`docs/invocation.md`, "Lifecycle").

## Delegates

- Starting or messaging a delegate is always reviewed. `delegate_spawn`,
  `delegate_fork` and `delegate_message` declare `executes` and
  `always_reviewed`, so no fast path, session grant or standing allow skips
  the parent's reviewer for them. `executes` alone rules out only the fast
  path.
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
