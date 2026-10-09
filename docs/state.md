# Fiber home

The one directory where Fiber keeps everything it writes outside a repository.
Default `~/.fiber` on macOS and Linux; `FIBER_HOME` relocates all of it.

```
~/.fiber/                         Fiber home
  config.json                     global configuration (docs/configuration.md)
  config/<extension>.json         an extension's global settings
  rules                           global standing rules
  AGENTS.md                       global instruction file (docs/system-prompt.md)
  SYSTEM.md                       replaces Fiber's system prompt text
  APPEND_SYSTEM.md                appended to the system prompt
  projects/<key>/                 one per project
    sessions/<id>/                events.jsonl, session.lock, artifacts/
    history.jsonl                 prompt history, append-only
    rules                         this project's standing rules
    SYSTEM.md, APPEND_SYSTEM.md   this project's system prompt files
    config.json                   this project's configuration
    config/<extension>.json       an extension's settings for this project
    worktrees/<id>/               one git worktree per session that asked for one
    approvals/<content-hash>      one file per extension or hook approval or never
    data/<extension>/             an extension's data for this project
  extensions/<name>/              installed extensions, one directory each
  docs/                           Fiber's docs for the installed version (docs/releasing.md)
  docs/skills/<name>/             a built-in skill (docs/system-prompt.md)
  themes/<name>.json              a theme (docs/tui.md, "Themes")
  pinned/<content-hash>/          approved copies of code a repository ships
  pinned.json                     size, modification time and hash of each declared path
  data/<extension>/               an extension's data for this machine
  approvals/<content-hash>        one file per MCP server approval or never
  credentials/<name>/<label>       one file per stored provider credential, mode 0600
  credentials/<name>              one file per extension secret, mode 0600
  credentials/hubs/<name>         one device token per hub this client paired with, mode 0600
  credentials/mcp/<url-hash>      one OAuth token per remote MCP server URL, mode 0600
  hub/devices/<device>            one record per device paired with this hub: name, when, token hash
  run/<session_id>                one local socket per running session
  run/hub                         the hub's local socket
  recent.jsonl                    recently exited sessions, a rebuildable index
  cache/models/<provider>.json    discovered model list
  cache/mcp/<server>.json         an MCP server's last tool and prompt lists
  cache/images/<session>-<file>    an image the terminal opened in the system viewer (docs/tui.md, "Images")
  crashes/<id>-<ms>.txt           one report per panic (docs/code-quality.md)
  logs/hub.log, logs/hub.log.1    the hub's diagnostic log and its previous file
  logs/<kind>-<id>.log            one other process's diagnostic log
```

## Override

One environment variable, `FIBER_HOME`, relocates all of Fiber home. It must
be an absolute path. An empty or relative value is a startup error, code `usage`,
naming the variable; Fiber never falls back to `~/.fiber`. A supervisor that meant to
isolate a run and passed an empty value by mistake would otherwise silently
share the person's real sessions and credentials.

A missing directory is created, mode 0700. Two Fiber processes share state
only by being given the same Fiber home. There is no second, narrower
override.

Fiber home is one root under the home directory: no XDG split, no
`~/Library`.

One person owns one Fiber home. There are no group permissions and no
multi-user sharing.

## Projects

A project is a git repository, identified by git's shared directory (the
common dir). Every worktree of a repository is one project; a separate clone
is a different project. Outside a git repository, the launch directory is the
project.

`~/work` and `~/work/fiber` and `~/work/lens` are three projects.
`~/work/fiber` and its worktree `~/work/fiber-wt1` are one project. Two clones
`~/work/fiber-clone1` and `~/work/fiber-clone2` are two projects.

Clones are never unified. Fiber does not key by remote URL: a clone can have
no remote, several, or a changed one. Sessions, prompt history and
per-project standing rules are kept per project.

The key is a readable slug of the project's identity path — git's shared
directory, or the launch directory outside git — after resolving symlinks.
Every `/` becomes `-`. Git shared directory `/Users/alice/work/fiber/.git`
becomes `-Users-alice-work-fiber-.git`. Launch directory `/Users/alice/work`
becomes `-Users-alice-work`.

A slug can collide (`/a-b` and `/a/b` both become `-a-b`). That is harmless:
the key is only a name for humans. `session_started` records the exact
workspace, and lookup checks it. The log stays the truth
([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)).

## Sessions and resume

Launched anywhere inside a repository — any subdirectory, any worktree —
Fiber finds that repository's project by asking git for its shared directory,
so resuming from `docs/` or from another worktree just works. Outside git,
only the exact launch directory counts.

A resumed session keeps the workspace recorded on its `session_started`.
Resuming from elsewhere never moves or narrows it. A moved repository or
directory leaves its old sessions under the old key.

The session directory is unchanged from `docs/events.md`:

```
~/.fiber/projects/<key>/sessions/<session_id>/
  events.jsonl     the log
  session.lock     one writer
  artifacts/       bytes too large to inline
```

The full output behind a bounded tool result goes in that session's
`artifacts/`.

A fork's history, and a rewind's, is a pointer into another session's log
(`docs/delegates.md`, "Forks"; `docs/events.md`, "Rewind"). Deleting a session
a fork or a rewind points at is refused unless the person asks for those to
be deleted too (`docs/invocation.md`, "Deleting and pruning"). Nothing deletes
a session on its own, except a session that never got a prompt, which
deletes its own directory as it exits (`docs/invocation.md`, "Lifecycle").

## What each part holds

**Standing rules.** A global file at the top level of Fiber home and the
project's `rules` file. Per-project rules live in Fiber home, never in the
repository, so cloning a repository can never grant it permissions — the same
reason `docs/model-routing.md` refuses project config that declares a
provider.

**Extensions.** Installed extensions live at `extensions/<name>/`, one
directory each. The `<name>` is the extension's short name when it has one
(`docs/extensions.md`, "Names"), so the memory extension lives at
`extensions/memory/`. Any other extension's `<name>` is its git address or
local path, slugged the same way as project keys. The two never collide: a
slugged git address keeps its host's dot (`github.com-…`), a slugged local
path starts with `-`, and a short name has neither. Installing or updating writes a fresh directory
and renames it into place; a running session keeps what it already loaded.

**Docs.** `docs/` holds Fiber's documentation for the installed version:
`docs/user/` for a person using Fiber, and the rest for how Fiber works,
including the extension API and the built-in skills in `docs/skills/`
(`docs/system-prompt.md`, "Skills"). `install.sh` and `fiber update` write it from
the release's docs archive, so it always matches the binary
(`docs/releasing.md`). Nothing else writes it. The system prompt names this
path (`docs/system-prompt.md`).

**Extension data.** Each extension has two data directories: `data/<name>/`
at the top of Fiber home for what it keeps per machine, and
`projects/<key>/data/<name>/` for what it keeps per project. `<name>` is
the extension's directory name in `extensions/`, so the memory extension's
store is `data/memory/`. Fiber hands both paths to the extension and
creates each the first time the extension writes there. A memory system or
an index lives here. A write of Markdown files here takes the permission
fast path (`docs/permissions.md`, "Fast paths"). Nothing in them is session state, so a rewind or fork
never touches them ([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)).
`fiber extension remove` deletes an extension's data directories too, asking first in a
terminal.

**Pinned copies.** When a person approves code a repository ships, Fiber
copies it to `pinned/<content-hash>/`: an extension's whole package, with its
dependencies and what its install step built, or the files a hook or an MCP
server declaration names. Sessions run the copy, never the repository's files.
Every approved version is kept until no worktree's files match it, when
`fiber sessions prune` removes it (`docs/extensions.md`, "Code a repository
ships"). `pinned.json` records each declared path's size, modification time
and hash, so a session hashes a file again only when one of the first two
changed. It is an index: deleting it costs a re-hash and nothing else.

**Approvals.** One file per decision about code a repository ships, named by
the content's hash; it says whether the decision is approve or never.
Deleting it withdraws the decision, and the next session offers the code
again. An extension's or a hook's is per project, at
`projects/<key>/approvals/<content-hash>`, as its install is. An MCP server's
is per machine, at `approvals/<content-hash>`, so a declaration approved in one
repository is not asked about again in another (`docs/mcp.md`, "A repository's
servers"). A repository can neither write nor read them.

**Credentials.** One file per stored provider credential at
`credentials/<name>/<label>`, one per credential label, and one file per
extension secret at `credentials/<name>`, and one file per remote MCP
server's OAuth token at `credentials/mcp/<url-hash>`, named by a SHA-256 of the
server's canonical URL and recording that URL (`docs/mcp.md`), each mode 0600,
in 0700 directories. There is no OS keychain. OAuth refresh takes a
lock on the credential file (`docs/model-routing.md`). Every tool call
touching `credentials/` is refused
([docs/permissions.md](permissions.md#credentials)).

**Cache.** `cache/` holds only what is always safe to delete;
`rm -rf ~/.fiber/cache` is a documented safe reset. Today it holds each
provider's discovered model list, one file per provider at
`cache/models/<provider>.json`, fetched and replaced whole, and each MCP
server's last tool and prompt lists at `cache/mcp/<server>.json`, keyed by a hash
of the server's declaration (`docs/mcp.md`, "Starting servers"). A writer replaces
a file by rename, so two sessions refreshing one list leave one whole file.

**Crash reports.** `crashes/<session_id>-<ms>.txt` holds one panic's
message, thread name and backtrace, named by the process id instead when the
process has no session id, written by the panic hook before the
process aborts (`docs/code-quality.md`, "Panics"). A report describes a bug
in Fiber, not a session, so nothing reads one to decide anything, and
deleting them is always safe. They are pruned with the diagnostic logs.

**Diagnostic logs.** `logs/` records what happens outside any session, where
no session log could hold it. It is always on, because the failures it exists
for, such as a startup error, happen before anyone would think to turn a log
on. Each process writes its own file, so every file has one writer, as a
session log does: the hub writes `logs/hub.log`, and any other process that
has something to record writes `logs/<kind>-<id>.log`, where `<kind>` is
`session`, `ask` or `tui` and `<id>` its session id once its session log
exists, or its process id until then.

Each line is one JSON object: `ts`, `level` (`error`, `warn`, `info` or
`debug`), `process` (`hub`, `session`, `ask` or `tui`), `session_id` when one
is known, `code` and `message`, one sentence, and on a `debug` line `data`,
an object of named values. For a failure, `code` is its code from
`docs/errors.md`. For one of the hub's operations it is the operation's name.
This shape is a contract: a program that forwards these lines elsewhere reads
these fields.

`diagnostics.level` (`docs/configuration.md`, "Keys") sets how much is
recorded. At `"info"`, the default, a process writes only the `error`, `warn`
and `info` lines below. At `"debug"` it also writes `debug` lines; `error`,
`warn` and `info` lines keep their shape and carry no `data`. A process reads
the level when it starts, so a changed value reaches new processes only. A
debug line holds no credential, token, header value, prompt, model text, tool
argument or configuration value.

What is recorded:

- **Failures with no session to hold them:** a startup error such as
  `config_invalid`, a signal that arrives before `fiber_started`, a shutdown
  that passes its bound (`docs/invocation.md`, "Shutdown"), and a background
  refresh, such as a quota or model list, that fails while no session is
  running.
- **An extension's `host.log` lines:** each is a line in the session's file
  with `process` `session`, `code` `extension_log`, the extension's name and
  the message as the extension wrote it.
- **The hub's operations:** `hub_started` and `hub_stopped`;
  `client_connected`, `client_disconnected` and `client_unauthenticated`;
  `device_paired` and `device_revoked`, naming the device and the client that
  acted; `session_started` and `session_resumed`, naming the session and the
  device that asked.
- **At `debug`, the hub's peak memory:** a `peak_memory` line just before
  each `hub_stopped`. `data.peak_kib` is the process's peak so far: peak RSS
  (`VmHWM`) on Linux, and peak physical footprint on macOS
  (`docs/performance.md`, "Measuring"). A peak that cannot be read writes no
  line.

An event inside a running session is recorded in that session's log and
nowhere else; a hook that fails in a session, for example, is a `notice` or
`hook_failed` there (`docs/extensions.md`, "When a hook fails"). Nothing in
`logs/` holds a credential or token, prompt or model text, a tool's arguments
or a configuration value, with one exception: an `extension_log` line is the
extension's own text, and Fiber records it as given. A failed hook is named
with its code, never its content. A level that records requests, their paths,
statuses and timings, is not built.

**Bounds.** `logs/hub.log` is renamed to `logs/hub.log.1` when it passes
10 MiB, replacing any older one; its single writer makes the rename safe.
Files in `logs/` and `crashes/` older than 30 days, and all but the newest 100
in each, are deleted by `fiber sessions prune` and when the hub starts. These
numbers were chosen, not measured. Fiber sends nothing from `logs/` anywhere
([ADR 0010](adr/0010-fiber-never-phones-home.md));
`fiber doctor` reads it (`docs/invocation.md`, "Commands and flags").

**Worktrees.** Per project, `worktrees/<id>/`: the git worktree of a delegate
started with `isolation: worktree`, of `fiber ask --worktree` or of the
terminal's new-worktree switch (`docs/invocation.md`, "Isolation"). When one
is removed or kept is `docs/delegates.md` ("Worktrees"). A kept worktree is
removed only by `fiber sessions prune` (`docs/invocation.md`, "Deleting and
pruning").

**Sockets.** `run/<session_id>` is the local socket of a running session
(`docs/invocation.md`, "Processes"), mode 0600 in a 0700 directory. The hub, a
delegate's parent and session messages connect to it; every other client
reaches it through the hub. `run/hub` is the hub's own socket, which local
clients connect to; the hub that binds it removes a stale one first. The process
holding that session's `session.lock` owns it: it removes any socket left by a
dead process before binding, unlinks its own at exit, and nothing else removes
one. It sits at the top of Fiber home
because macOS limits a socket's path to 103 bytes (`sun_path[104]` in
`sys/un.h`; binding at 104 fails, probed on Darwin 25.6.0), and a path under
`projects/<key>/sessions/<id>/` exceeds that. Linux allows 107 (`unix(7)`, not
measured here). A `FIBER_HOME` long enough to break the limit is a startup
error naming the variable.

**Paired devices.** `hub/devices/<device>` holds one record per device
paired with this machine's hub: its name, when it was paired, when it last
connected, and a SHA-256 hash of its device token, never the token
(`docs/invocation.md`, "Remote clients"). Revoking a device deletes its
record. On a client, `credentials/hubs/<name>` holds the device token for
each hub it paired with, mode 0600, under the same credential deny as every
other file in `credentials/` (`docs/permissions.md`, "Credentials").

**Recently exited sessions.** `recent.jsonl` at the top of Fiber home: one
JSON line per session that exited, appended by the session itself as it
exits. Its keys are `session_id`, `ts`, `project` (the project's key), `workspace`,
`name`, `how` and `status`, the session's last `session_status`, which says
what it stopped on and carries `parent` for a delegate. A session whose
process died cannot append, so the hub appends its row when it sees the
crash (`docs/invocation.md`, "The hub"). A session
appends whether or not a hub is running, and nothing rewrites it, so an
append is never lost to a rewrite. Deleting a session leaves its row, and
every reader skips a row whose session directory is gone. The hub reads its tail at start and keeps
the newest 100. It grows by about 400 to 600 bytes per exited session: 10,000
sessions is about 5 MB. It is a derived index, rebuildable from the logs, and never the
truth ([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)).

**Prompt history.** Per project, `history.jsonl`: one JSON line per prompt,
append-only. The session appends each prompt a person sends it, in a session
the hub started, so every client shares one history; a `fiber ask` prompt is
not appended. Each line holds `ts` (integer, wall-clock milliseconds),
`session_id` and `content`, the prompt's content parts as the session
accepted them. Up-arrow recalls prompts typed anywhere in that project.

Fiber writes no derived database in v0.0.1. Listing sessions asks the hub,
which answers with each running session's latest `session_status` and the
exited sessions' `recent.jsonl` rows (`docs/invocation.md`, "The hub"). Any
future database is derived from the logs, rebuildable, never the truth.

## Concurrent access

Every whole-file write is a temporary file renamed over the old one, so a
reader sees the old file or the new one, never half. A file that is read,
changed and written back — the config default, standing rules — takes a lock
file first. Appends (`history.jsonl`, `events.jsonl`) are one line per single
write. Readers never lock.

One writer per session via `session.lock` is `docs/events.md`.

## Upgrades and versioning

There is no layout version marker. The first change to this layout adds a
file `layout` at the top of Fiber home containing `2`; a missing file means
layout 1. `fiber update` changes only the Fiber binary, `extensions/` and `docs/`;
it never touches sessions, config, rules, approvals, pinned copies, credentials
or extension data. It replaces the binary by renaming a new file over it, so a running
session keeps the file it launched from. How the binary is fetched and
replaced is `docs/releasing.md`.
