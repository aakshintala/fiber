# System prompt

What the model is told before the conversation starts, and where each part
comes from. This is what is true now. It is settled by
[System prompt: what it holds and what feeds it](https://github.com/aakshintala/fiber/issues/114);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`: preamble, opening message, session log, workspace,
handoff. The cache rules this page follows are `docs/prompt-cache.md`.

## Two parts

What varies by project or by day is kept out of the system prompt:

- The **system prompt** holds Fiber's instructions, the tools' guidelines,
  the model's name and addendum, extension texts and the person's own
  additions. It is part of the
  preamble, so it is built only at start, resume, `reload` and a model switch.
- The **opening message** holds the environment, the project's instruction
  files and the skills listing. It is the first message of the conversation,
  written to the session log once and rendered from the log after that.

A request is therefore the system prompt and tools, then the opening message,
then the rest of the conversation.

Because the opening message lives in the log, a resume never reads the files
again, and the bytes the provider cached still match. Anything that changes
later, whether during a session or between two runs of it, reaches the model as
a message appended at the end ("When something changes"). What reference agents do is in
[research/system-prompt/reference-agents.md](../research/system-prompt/reference-agents.md).

## The system prompt

Built from these parts, in this order, joined by a blank line:

1. Fiber's text, `crates/loop/prompt/system.md`, or the person's `SYSTEM.md`
   in its place.
2. The guidelines of every tool declared in full, in tool-name order.
3. The session section: the model line, the docs line, the unattended line
   when nobody can answer, and the model's addendum from provider data, if it
   has one.
4. Each extension's prompt text, in extension-name order.
5. The person's `APPEND_SYSTEM.md`.

The same inputs give the same bytes. Every part is read at a preamble build and
at no other time.

### The person's files

A person replaces or extends the system prompt with files in Fiber home:

| File | Effect |
|---|---|
| `SYSTEM.md` | Replaces Fiber's text. Tool guidelines, the session section and extension texts still follow. |
| `APPEND_SYSTEM.md` | Appended at the end. |

Each can sit at the top of Fiber home or under `projects/<key>/`
(`docs/state.md`). A per-project file wins over the global file of the same
name; the two are not combined. A repository cannot replace or extend the
system prompt.

An edit to one of these files takes effect at the next preamble build: a
resume, a `reload` or a model switch. It is never appended mid-session. The
cache miss that follows is one the person started, and the rebuild is logged
(`preamble_built`).

### Fiber's text

Fiber's text says who the model is, how instruction files bind it, and how its
context restarts at a handoff. The model is an expert software engineer working
inside Fiber; it is not Fiber. Its work includes discussing, brainstorming and
planning with the person, not only changing files. The opening line is adapted
from pi's (MIT license).

Preferences about how to work, such as testing, commit habits and reply style,
are left to instruction files. Fiber's text states only what holds in every
project.

### The docs line

One line tells the model where Fiber's own documentation is, so a question
about Fiber is answered from the installed docs and not from memory. It names
`docs/` in Fiber home: `user/` for using Fiber, and the rest for how Fiber
works, including the extension API. The model reads them with its ordinary
file tools when a question is about Fiber itself.

The path is fixed for the session, so the line never changes the preamble.
When `docs/` is missing from Fiber home, as for a binary copied without the
installer, the line names the website instead, and `fiber doctor` says the
docs are not installed (`docs/invocation.md`, "Commands and flags"). The installed docs
match the installed version (`docs/releasing.md`). They can describe
behaviour the binary does not have yet, because the docs are written before
the code that follows them.

### Tool guidelines

A tool may declare guideline lines alongside its description
(`docs/tools.md`, "What a tool declares"). The system prompt carries them for
every tool declared in full, under a heading naming the tool, so guidance that
spans calls sits in one place: for example, to read files with `read` rather
than the shell, and to put every change to a file in one `edit`. A deferred
tool's guidelines are left out, because the model has not seen the tool.

The guidelines travel with the tool, not with the system prompt: `loop` never
names a tool (`docs/architecture.md`), and an extension that replaces a
built-in by name brings its own guidelines. The built-in tools' guidelines are
`crates/tools/prompt/guidelines.md`, one `##` section per tool.

The tool set is fixed per build, so the guidelines are too.

### Unattended sessions

In a session started by `fiber ask`, nobody can answer a question. There the
session section adds a line telling the model to work through to the end on its
own judgment and to state its assumptions in its final reply. A session a
person or a driver can answer carries no such line. When a `fiber ask` session
is later resumed in the terminal, the preamble is rebuilt without it.

### The model's addendum

Provider data may give a model an addendum: text appended after the model line,
for what that model needs to be told and others do not
(`docs/model-routing.md`). One prompt serves every model; the addendum is the
only variation. A model switch already rebuilds the preamble, so an addendum
costs no extra cache miss.

### Extension texts

An extension's manifest may name a prompt file in its package
(`prompt` in `extension.json`, `docs/configuration.md`). Its text goes in the
system prompt under a heading naming the extension. This is how an extension
gives standing guidance across its tools, such as which to call first, that
must survive a handoff. A tool's own description stays in its tool definition.

An extension with a prompt file is read at session start, when every enabled
Lua extension starts (`docs/extensions.md`, "Loading, and cost when nothing is
loaded"). A hook never changes the system
prompt.

## The opening message

Built from `crates/loop/prompt/opening.md`. It says it comes from Fiber, not
the person, and holds, in order:

1. The environment.
2. The instruction files.
3. The skills listing.

It is written at session start and again after every completed handoff, and
logged as `opening_message`.

### Environment

- the date, without the time
- the operating system and architecture
- the shell
- the workspace
- whether the workspace is in a git repository, and the current branch
- the path of the session log

Git state stops at the branch. A status snapshot and recent commits go stale as
soon as anything changes, and `git status` is slow in a large monorepo.

### Instruction files

Fiber reads these files, in this order:

1. `AGENTS.md` at the top of Fiber home.
2. In a git repository, each directory from the repository's top level down to
   the workspace. Outside git, the workspace alone.

In each directory Fiber reads `AGENTS.md`, or `CLAUDE.md` when there is no
`AGENTS.md`. A `CLAUDE.md` whose only content points at `AGENTS.md` is
therefore never read. `@path` imports are not followed.

Every file is sent in full, under its path, whatever its size. The system
prompt tells the model the precedence rules:

- a file applies to its own directory and everything below it
- where files disagree, the deeper file wins, and the global file loses to
  every project file
- the person's own messages win over every file

A repository's instruction files are read without asking, including in a
fresh clone. They are text the model reads, like a README. What guards the
machine is the permission decision on every call (`docs/permissions.md`), not
which files were loaded.

The evidence is in
[research/system-prompt/instruction-files.md](../research/system-prompt/instruction-files.md).

### Subdirectory files

A directory below the workspace may have its own `AGENTS.md` or `CLAUDE.md`, as
in a monorepo. Fiber sends one the first time a tool call's declared paths
(`docs/tools.md`) fall under its directory. It is appended as its own message,
once per context. After a handoff it is sent again only when a call touches
that directory again.

A call that declares no paths, such as a shell command Fiber cannot read,
never triggers a subdirectory file.

### Skills listing

The listing sits last in the opening message. It has one entry per skill the
model may load, which leaves out a skill switched off in `skills.disabled`
("Skills"): the skill's name, its description and the path of its
`SKILL.md`. Every name and every description is sent in full. The model loads a
skill with the `skill` tool (`docs/tools.md`, "Skills").

### Skills

A skill is a directory holding a `SKILL.md` file: a header, then instructions
the model reads only when a task needs them. Fiber reads the
[Agent Skills](https://agentskills.io/specification) format:

- `name` and `description` are required.
- `license`, `compatibility`, `metadata` and `allowed-tools` are optional.
- `disable-model-invocation` and `argument-hint` are also read, the only fields
  outside the standard. pi and Claude Code both honour them, so a skill written
  for either works unchanged.
- Any other field is ignored.

A skill missing `name` or `description`, or whose header does not parse, is
left out, and a `notice` with code `skill_invalid` names its path.

Both the model and the person run skills. The model loads one from the
listing. The person types `/name`, optionally followed by arguments, and the
skill's text is expanded into their message. A skill with
`disable-model-invocation: true` is left out of the listing but keeps its
`/name`. `argument-hint` is shown beside the name in the terminal's `/` list.

A prompt template is what a person runs by `/name`: a skill with
`disable-model-invocation: true`, in the same format and the same directories
as every other skill. An MCP server's prompts appear in the same `/` list,
tagged with the server's name (`docs/mcp.md`).

Fiber finds skills in these places, read once at session start:

| Source | Where |
|---|---|
| Repository | `.fiber/skills/` and `.agents/skills/` at the top of the git repository, or in the workspace outside git |
| Personal | `skills/` in Fiber home, and `~/.agents/skills/` |
| Extension | each installed extension's `skills/` and `prompts/` directories (`docs/extensions.md`, "What a package holds") |
| Built-in | skills compiled into the binary, such as the recipe that recommends `cache.warm_cap` from the person's own sessions (`docs/prompt-cache.md`) |

Each place holds one directory per skill, at a fixed depth. Fiber never walks
the tree below it, so the cost does not grow with the repository.
`~/.agents/skills/` is the directory pi reads, so skills shared with pi and
Claude Code need no copy.

When two skills share a name, the more specific source wins: the repository,
then personal, then an extension, then built-in. A `notice` with code
`skill_shadowed` names both paths.

A person can switch a skill off, for one project or everywhere, from the
terminal's `/skills` view. The view writes the skill's name to
`skills.disabled` in the project's or the global configuration
(`docs/configuration.md`). A repository cannot set that key. A switched-off
skill is left out of the listing, the `skill` tool refuses it, and `/name`
does not expand it. It still shows in `/skills`, marked off. This is how a
person turns off a repository's skill without editing the repository, or one
skill of an extension whose other skills and tools they keep.

A repository's skills load without asking, including in a fresh clone, like its
instruction files. They are text the model reads, and a repository can already
give the model instructions through `AGENTS.md`. What guards the machine is the
permission decision on every call (`docs/permissions.md`).

### Size

When the instruction text passes 10% of the model's context window, the build
is followed by a `notice` with code `instructions_large`, naming the largest
files and extension texts. Instruction text means the instruction files,
extension texts, `SYSTEM.md` and `APPEND_SYSTEM.md`. The session runs anyway.
The 10% matches `tool_definitions_large` (`docs/tools.md`).

Nothing is cut. A rules file with its end cut off breaks rules its author
thought were in force.

The skills listing follows the same rule. When it passes 10% of the context
window, the build is followed by a `notice` with code `skills_large`, naming
the sources that add the most to it. The listing is never cut.

## When something changes

Nothing sent is ever edited. A change is appended.

### Instruction files

At each turn start, Fiber checks every instruction file it has sent in this
context, and the places a new one could appear (the directories from "Instruction
files", and every subdirectory already touched). It compares size and
modification time, then content when those differ.

| Change | What the model is sent |
|---|---|
| Changed by a call in this session | Nothing. The model saw its own edit. |
| Changed from outside | A unified diff against the version the model was last given, or the full text when the diff is longer than the new file |
| Deleted | One line saying its instructions no longer apply |
| Created | The full text |

A change counts as the session's own when the file still matches what Fiber
recorded after the last call whose declared paths included it. Outside changes
are an editor, a `git pull` or another agent.

A chain of diffs lasts at most one context. A handoff writes a fresh opening
message from the current files, so the next context starts from full text.

The check runs only at turn start. An outside edit made during a turn reaches
the model at the next one.

### Added and removed skills

At each turn start, Fiber also checks the skill directories for skills added
or removed since the listing was last given. Each change is appended as one
line: an added skill's name and description, or a removed skill's name. The
opening message is never rebuilt for it.

Switching a skill off counts as removing it, and switching it back on as
adding it. The turn-start check reads `skills.disabled` with the skill
directories, so a switch made in `/skills` reaches the model at the next turn
start as one removed or added line, with no prompt-cache miss.

The check finds skills by the same rules as the listing ("Skills"). A skill
added with a missing `name` or `description`, or a header that does not parse,
is left out, and its `skill_invalid` notice is raised once, at the first turn
start that finds it. An added skill that outranks a listed skill of the same
name raises `skill_shadowed` naming both paths, and the `skill` tool loads the
winner from then on. The name is already listed, so no line is appended: the
model is given the winner's description on `reload` or after a handoff, as for
any changed description.

An edit to a skill's body needs nothing: the `skill` tool reads the file when
the skill is loaded. An edit to a description is not checked at turn start. It
reaches the model on `reload`, which sends each changed skill again as an added
line with its new description, or in the next opening message after a handoff.

### The date

When a turn starts on a later date than the model was last given, Fiber appends
one line with the new date.

## After a handoff

A handoff writes a new `opening_message` from the current files and the current
date. The model then sees the system prompt and tools, the opening message,
the context `session_start` hooks add, the turn's input, the note, the jobs
line and the skills it had loaded (`docs/handoff.md`). The cache
already misses after the tools at a handoff, so the fresh message costs nothing
extra.

## Delegates, forks and rewinds

- A delegate that is a new session writes its own opening message, from its own
  workspace. Its identity and task follow in the next message, never in the
  system prompt (`docs/delegates.md`).
- A fork or a rewind inherits the opening message from the log, with any
  changes appended before its point, so its first request matches its parent's
  byte for byte.

## The texts

Every text Fiber sends the model is a Markdown file in the `loop` crate,
compiled into the binary:

| File | Holds |
|---|---|
| `crates/loop/prompt/system.md` | Fiber's system prompt text |
| `crates/tools/prompt/guidelines.md` | the built-in tools' guidelines, one `##` section per tool |
| `crates/loop/prompt/opening.md` | the opening message |
| `crates/loop/prompt/messages.md` | everything else, one `##` section each: the tools heading, the session section, docs line and unattended line, instruction file headers, the diff, deleted and date lines, the added and removed skill lines, the session message framing, the extension heading, the nudge, the handoff note request, the handoff jobs line and the moved-result line |
| `crates/loop/prompt/reviewer.md` | the reviewer's instructions: a `shared` section, then one `##` section per stage, `first-pass` and `second-pass` (`docs/permissions.md`, "The reviewer") |

In `messages.md`, a section's text runs from its `## name` line to the next
line starting `## `, with blank lines at either end removed. Placeholders are
`{name}`. A test pins every file's bytes, so an edit is a
deliberate, reviewed change to what every session caches.

The note request says what `docs/handoff.md` requires: a fresh agent continues
the work, and the note refers by path or URL instead of copying, names the
skills to load, leaves out secrets, and gives the session log's path. The
person's `/handoff` instructions follow it. The nudge says how full the context
is, that a handoff keeps the work going, and the log's path. Its numbers come
from `context_nudged`.

The opening line is adapted from pi's. The rest is written for Fiber, and no
other text is copied from pi, codex, Claude Code, maki or the Zig tree.

## Recording

The kinds are in `docs/events.md`, "Preamble":

- `opening_message` holds the environment, each instruction file's path and
  content, and the skills listing, each entry with its name, description, path
  and source. The text is rendered from these fields.
- `skills_changed` records the skills added or removed at a turn start, as
  sent.
- `instruction_file` records one appended change: the path, the reason, the
  file's content now, and what was sent. The diff is rendered from this content
  and the content the model last had, both in the log.
- `date_changed` holds the date.

All four are durable, because the model saw them.

## Evidence

- [research/system-prompt/reference-agents.md](../research/system-prompt/reference-agents.md):
  how reference agents build their prompts, from primary sources.
- [research/system-prompt/instruction-files.md](../research/system-prompt/instruction-files.md):
  the owner's instruction files, their sizes, and how often they change.
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
