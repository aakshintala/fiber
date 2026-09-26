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
    worktrees/<id>/               one git worktree per delegate that asked for one
    data/<extension>/             an extension's data for this project
  extensions/<name>/              installed extensions, one directory each
  data/<extension>/               an extension's data for this machine
  approvals/<content-hash>        one file per approved extension content
  credentials/<provider>          one file per provider, mode 0600
  run/<session_id>                one local socket per running session
  cache/models/<provider>.json    discovered model list
  crashes/<session_id>-<ms>.txt   one report per panic (docs/code-quality.md)
```

## Override

One environment variable, `FIBER_HOME`, relocates all of Fiber home. It must
be an absolute path. An empty or relative value is a startup error naming the
variable; Fiber never falls back to `~/.fiber`. A supervisor that meant to
isolate a run and passed an empty value by mistake would otherwise silently
share the person's real sessions and credentials.

A missing directory is created, mode 0700. Two Fiber processes share state
only by being given the same Fiber home. There is no second, narrower
override: pi and codex each have one; Fiber has none.

pi (`~/.pi/agent`, `PI_CODING_AGENT_DIR`), codex (`~/.codex`, `CODEX_HOME`)
and Claude Code (`~/.claude`, `CLAUDE_CONFIG_DIR`) all use one root under the
home directory. That is the model here: no XDG split, no `~/Library`.

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
a fork or a rewind points at deletes them or is refused.

## What each part holds

**Standing rules.** A global file at the top level of Fiber home and the
project's `rules` file. Per-project rules live in Fiber home, never in the
repository, so cloning a repository can never grant it permissions — the same
reason `docs/model-routing.md` refuses project config that declares a
provider.

**Extensions.** Installed extensions live at `extensions/<name>/`, one
directory each. The `<name>` is the extension's git-address name, slugged the
same way as project keys. Installing or updating writes a fresh directory
and renames it into place; a running session keeps what it already loaded.

**Extension data.** Each extension has two data directories: `data/<name>/`
at the top of Fiber home for what it keeps per machine, and
`projects/<key>/data/<name>/` for what it keeps per project. `<name>` is
slugged as for `extensions/`. Fiber hands both paths to the extension and
creates each the first time the extension writes there. A memory system or
an index lives here. Nothing in them is session state, so a rewind or fork
never touches them ([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)).
`fiber remove` deletes an extension's data directories too, asking first in a
terminal.

**Approvals.** One file per approved extension content, at
`approvals/<content-hash>`. The file existing means that content is
approved; deleting it revokes the approval. Recorded per machine, so content
approved in one repository is not asked about again in another
(`docs/extensions.md`).

**Credentials.** One file per provider at `credentials/<provider>`, mode
0600, in a 0700 directory. There is no OS keychain. OAuth refresh takes a
lock on the credential file (`docs/model-routing.md`). Every tool call
touching `credentials/` is refused in every mode
([docs/permissions.md](permissions.md#credentials)).

**Cache.** `cache/` holds only what is always safe to delete;
`rm -rf ~/.fiber/cache` is a documented safe reset. Today it holds each
provider's discovered model list, one file per provider at
`cache/models/<provider>.json`, fetched and replaced whole.

**Crash reports.** `crashes/<session_id>-<ms>.txt` holds one panic's
message, thread name and backtrace, written by the panic hook before the
process aborts (`docs/code-quality.md`, "Panics"). A report describes a bug
in Fiber, not a session, so nothing reads one to decide anything, and
deleting them is always safe. Nothing prunes them.

**Worktrees.** Per project, `worktrees/<id>/`: the git worktree of a delegate
started with `isolation: worktree`. When one is removed or kept is
`docs/delegates.md` ("Worktrees").

**Sockets.** `run/<session_id>` is the local socket of a running session
(`docs/invocation.md`, "Processes"), mode 0600 in a 0700 directory. The process
holding that session's `session.lock` owns it: it removes any socket left by a
dead process before binding, unlinks its own at exit, and nothing else removes
one. It sits at the top of Fiber home
because macOS limits a socket's path to 103 bytes (`sun_path[104]` in
`sys/un.h`; binding at 104 fails, probed on Darwin 25.6.0), and a path under
`projects/<key>/sessions/<id>/` exceeds that. Linux allows 107 (`unix(7)`, not
measured here). A `FIBER_HOME` long enough to break the limit is a startup
error naming the variable.

**Prompt history.** Per project, `history.jsonl`: one JSON line per prompt,
append-only. Up-arrow recalls prompts typed anywhere in that project.

Fiber writes no derived database in v0.0.1. Listing sessions reads the logs
(`docs/events.md` has the measurement). Any future one is derived from the
logs, rebuildable, never the truth.

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
layout 1. `fiber upgrade` changes only the Fiber binary and `extensions/`;
it never touches sessions, config, rules, approvals, credentials or extension
data. It replaces the binary by renaming a new file over it, so a running
session keeps the file it launched from. How the binary is fetched and
replaced is `docs/releasing.md`.

## Not settled here

- Fiber's own debug and crash logs:
  [Observability: what Fiber records about itself outside a session](https://github.com/aakshintala/fiber/issues/60)
