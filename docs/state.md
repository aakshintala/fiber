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
    worktrees/<id>.lock           its lock file, held while a process runs in it
    protected-paths/               the protected-path baseline and its two locks (docs/sandbox.md)
    approvals/<content-hash>      one file per extension or hook approval or never
    servers/<content-hash>        one record per MCP server copy this project approved
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
  cache/mcp/<server>.json         an MCP server's last tool and prompt lists, and whether it offers resources
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
workspace, and the worktree's repository when Fiber made one, and lookup
checks the repository when recorded and the workspace otherwise. The log stays the truth
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

A fork's, rewind's or workspace move's history follows `forked_from` on the
child's `session_started` (`docs/delegates.md`, "Forks"; `docs/events.md`,
"Rewind" and "Change workspace"). When `forked_from.project` is present, it
names the parent's project key and the reader opens
`projects/<project>/sessions/<session_id>/` directly. When it is absent, the
parent is in the child's project. A missing parent fails closed; readers do
not scan other projects.

Deleting a session a fork, rewind or workspace move points at is refused
unless the person asks for those sessions to be deleted too
(`docs/invocation.md`, "Deleting and pruning"). For a cross-project move,
delete and prune read the parent's final `moved` event: `workspace` resolves
the child's project key, and `new_session_id` names the child. Nothing deletes
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
Approving a server declaration also writes a project record at
`projects/<key>/servers/<content-hash>`, naming this project as one that
approved that copy. Every approved version is kept until no worktree's files match it, when
`fiber sessions prune` removes it (`docs/extensions.md`, "Code a repository
ships"; `docs/invocation.md`, "Deleting and pruning"). `pinned.json` records each declared path's size, modification time
and hash, so a session hashes a file again only when one of the first two
changed. It is an index: deleting it costs a re-hash and nothing else.

**Import receipts.** `import-claude-code.json` and `import-codex.json` at
the top of Fiber home record one completed import each: the source, the time
and the counts (`docs/configuration.md`, "Importing"). Deleting one means
the next `fiber import` for that source runs again.

**Approvals.** One file per decision about code a repository ships, named by
the content's hash; it says whether the decision is approve or never.
Deleting it withdraws the decision, and the next session offers the code
again. An extension's or a hook's is per project, at
`projects/<key>/approvals/<content-hash>`, as its install is. An MCP server's
is per machine, at `approvals/<content-hash>`, so a declaration approved in one
repository is not asked about again in another (`docs/mcp.md`, "A repository's
servers"), and each approving project also holds a record at
`projects/<key>/servers/<content-hash>`, written when the approval is. A `never`
writes no project record, and removes one an earlier approval left. Only a
changed declaration or files, or a deleted record, offers approved content
again, so a server copy approved before the project record existed has no
record, and stays until a new approval writes one. A repository can neither write nor read them.

**Credentials.** One file per stored provider credential at
`credentials/<name>/<label>`, one per credential label, and one file per
extension secret at `credentials/<name>`, and one file per remote MCP
server's OAuth token at `credentials/mcp/<url-hash>`, named by a SHA-256 of the
server's [canonical URL](mcp.md#what-fiber-does-with-mcp) and recording that URL,
each mode 0600,
in 0700 directories. There is no OS keychain. OAuth refresh takes a
lock on the credential file (`docs/model-routing.md`). Every tool call
touching `credentials/` is refused
([docs/permissions.md](permissions.md#credentials)).

**Cache.** `cache/` holds only what is always safe to delete;
`rm -rf ~/.fiber/cache` is a documented safe reset. Today it holds each
provider's discovered model list, one file per provider at
`cache/models/<provider>.json`, fetched and replaced whole, and each MCP
server's last tool and prompt lists and whether it offers resources at `cache/mcp/<server>.json`, keyed by a hash
of the server's declaration (`docs/mcp.md`, "Starting servers"). A writer replaces
a file by rename, so two sessions refreshing one list leave one whole file.

**Crash reports.** `crashes/<session_id>-<ms>.txt` holds one panic's
message, thread name and backtrace, named by the process id instead when the
process has no session id, written by the panic hook before the
process aborts (`docs/code-quality.md`, "Panics"). A report describes a bug
in Fiber, not a session, so nothing reads one to decide anything, and
deleting them is always safe. `fiber doctor` names the newest one, and
`fiber feedback` copies a session's reports into its report with the panic
message left out ("What feedback reads" below). They are pruned with the diagnostic
logs.

**Diagnostic logs.** `logs/` records what happens outside any session, where
no session log could hold it. It is always on, because the failures it exists
for, such as a startup error, happen before anyone would think to turn a log
on. Each process writes its own file, so every file has one writer, as a
session log does: the hub writes `logs/hub.log`, and any other process that
has something to record writes `logs/<kind>-<id>.log`, where `<kind>` is
`session`, `ask` or `tui` and `<id>` its session id once its session log
exists, or its process id until then, except a signal before Fiber starts writing `fiber_started`, which stays pid-named with no `session_id`.

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
  that reaches its kill point (`docs/invocation.md`, "Shutdown"), and a background
  refresh, such as a quota or model list, that fails while no session is
  running.
- **An extension's `host.log` lines:** each is a line in the session's file
  with `process` `session` or `ask`, `code` `extension_log`, the extension's name and
  the message as the extension wrote it.
- **The hub's operations:** `hub_started` and `hub_stopped`;
  `environment_built`, `info` when the login-shell capture succeeded and
  `warn` with the reason it failed (`docs/invocation.md`, "A session's
  environment");
  `client_connected`, `client_disconnected` and `client_unauthenticated`;;
  `device_paired` and `device_revoked`, naming the device and the client that
  acted; `session_started` and `session_resumed`, naming the session and the
  device that asked.
- **At `debug`, a native model call's request:** one `provider_request`
  line when the call ends, with `purpose` `model_call`. `data` holds
  `provider`, `model`, `host` (the authority without userinfo), `path`
  (no query string or fragment, percent-decoded), `status` when a response
  came, `attempt` from 1, `request_bytes` as posted, `response_bytes` as
  read, and `headers_ms`, `first_token_ms` and `total_ms`, whole
  milliseconds from the send. A retried call writes one line per attempt;
  a request never sent writes none.
- **At `debug`, a Lua provider's HTTP requests:** one `provider_request`
  line when the request ends, with `purpose` `model_list` for `models()`,
  `quota` for `quota()`, `token_refresh` for `credential()`, and `cost`
  for `cost()`. `data` holds `provider`, the provider's registered name,
  `status` when a response came, `attempt` (always 1: a Lua invocation
  makes no retry), `request_bytes` as posted, `response_bytes` as read,
  and `headers_ms` and `total_ms`, whole milliseconds from the send. A
  body read that fails keeps the status, the headers and the bytes read
  before the failure. A request that validation rejects before sending
  writes no line; one that fails after writes one without `status` or
  `headers_ms` and with zero response bytes. Only these four purposes
  record: `sign()`, hooks, commands, timers, tools and the entry script's
  own requests stay silent. No other field is recorded, and never a host,
  path, model, URL, query string, credential, header value, body or token,
  as for every line in `logs/`.
- **At `debug`, the hub's peak memory:** a `peak_memory` line just before
  each `hub_stopped`. `data.peak_kib` is the process's peak so far: peak RSS
  (`VmHWM`) on Linux, and peak physical footprint on macOS
  (`docs/performance.md`, "Measuring"). A peak that cannot be read writes no
  line.
- **At `debug`, a session or ask process's peak memory:** a `peak_memory`
  line after each turn's `turn_completed` and one more at the process's
  orderly end. `data.peak_kib` is the process's peak so far, measured the
  same way. A peak that cannot be read writes no line.
- **At `debug`, how long a timed operation takes:** one `op_timing` line
  when it ends, written by the one timer every timed place goes through
  (`log::diag::timed`). `data` holds `op`, the operation's name, and `ms`,
  whole milliseconds; a tool's timers add `tool`, the tool's name. In an
  `alloc-stats` build, and only when the timer ends on the thread that
  started it, the line also carries `alloc_bytes`, bytes allocated during
  the operation, `allocs`, the allocation count during the operation, and
  `live_delta_bytes`, signed end-minus-start live bytes, negative when the
  operation freed more than it allocated; a realloc contributes one
  allocation of the new size to `allocs` and `alloc_bytes` and adjusts live
  by the signed size difference. Otherwise the three fields are absent,
  never zero, and a default build writes none. No other field is recorded,
  and never an argument, a path or any content, as for every line in
  `logs/`. At `info` the cost is one check of the level,
  except the monotonic reads taken before the configuration is read, when
  the level is not yet known: one to time `startup.config`, and two for
  each worktree created or recreated then. Those are the only extra costs
  there, and they write no line at `info`. A new module
  or top-level operation lands with its timer in the same pull request.
  Installed so far is `net.connect`: one scope around an actual connector
  attempt group for a native observed request, from the first socket attempt
  through socket admission. It excludes DNS before entry, TLS, CONNECT
  negotiation and the response body, as well as pre-stopped and empty groups
  and already-built tunnels. Also installed is `picture.run`: one scope per
  entered image-child invocation, from spawning it to its validated answer,
  kept through spawn failures, errors and cancellation after entry, while a
  call cancelled before entry starts none. A PDF count/cut call owns one
  scope, and each rendered page's own child owns its own; the renderer
  itself and file inspection stay untimed. Its `data` holds only `op` and
  `ms`. Installed for the loop are `startup.preamble`,
  `turn.request`, `turn.model_call` and `turn.handoff`. `startup.preamble`
  covers building and installing the preamble after the approved-code load,
  before the opening message, once per session start: a same-session rebuild
  writes no line. `turn.request` covers assembling the history plus the
  request, ending before the budget, window and hook checks. `turn.model_call`
  covers one admitted attempt, from entering the model call until its stream
  ends; a cancel observed before admission writes none, and a retry's backoff
  stays outside. `turn.handoff` covers one handoff end to end, from its first
  record through the restart. Installed for one tool call are
  `tool.permission`, `tool.reviewer`, `tool.hooks` and `tool.run`, each
  with `tool` naming the registered tool. `tool.permission` covers the
  permission order from admission to the decision, holding the reviewer's
  stages and a person's wait; a suspended approval's fresh segment after
  resume is a new line, never the old process's downtime. `tool.reviewer`
  covers one entered judgement stage, a re-ask inside it included; a
  stage never run, and the person's wait after an escalation, write none.
  `tool.hooks` covers one actual `before_tool` or `check_tool` dispatch,
  or one actual `after_tool` dispatch on ordinary, spilled or unsaved
  output; a call with no hooks, or a dispatch a shutdown suppresses,
  writes none. `tool.run` covers one entered execution on its worker, not
  the wait for its ordered completion. The scopes nest instead of adding up: a
  requested handoff holds its note's request and model scopes; a handoff
  whose note a hook writes has neither, and
  `provider_request`'s network share sits inside `turn.model_call`. Warming
  refreshes and reviewer judgements run through the same factories but write
  no turn scope. Also installed are the six hook chains other
  than the tool hooks: `hook.session_start`, `hook.turn_start`,
  `hook.before_message`, `hook.before_model_call`, `hook.turn_end` and
  `hook.before_handoff`, one
  scope around each entered public dispatch, however it returns, empty
  chains included. Tool hooks take no scope: `tool.hooks` covers those
  chains. Other plain agent clients stay unobserved.
  Installed too is `mcp.connect`: one scope around one actual server
  connection attempt, from starting or dialing it until its tool list is
  in hand. It covers stdio and remote servers, eager and required starts,
  lazy first use, restarts, renewals and post-approval starts, with the
  remote server's actual connector work timed as `net.connect` beside it.
  It excludes cache-only declarations, prompt discovery and tool execution.
  Installed for startup and routing are `startup.config`, from the one
  pre-load read through opening the writer; `startup.extensions`, loading
  the installed provider packages and extensions; `startup.routing`, from
  adding the Lua providers through choosing the model, reading its
  credential, connecting and choosing the reviewer, with unloading
  excluded; and `routing.switch`, one entered switch preparation,
  rejections included, with applying it excluded: the three startup lines
  are written before the session attaches, so they sit in the pid file
  with no `session_id`.
  Installed for the hub are `hub.session_start`, `hub.resume` and
  `hub.subscribe_replay`, and for listing, `sessions.list`.
  `hub.session_start` covers a start from the request until the session's
  socket accepts, excluding the summary handshake for a first prompt; a
  rejected start measures through its failure. `hub.resume` covers one
  reach for the session, including gate contention, retries and failures.
  `hub.subscribe_replay` covers one actual kept-subscription replay, from
  its write through the matching acknowledgement, or through its failure,
  cancellation or disconnect before it. Installed for the door is
  `startup.socket`: socket preparation until the bound listener is ready,
  on new and resumed sessions alike. Installed for listing is
  `sessions.list`: one listing attempt from entry through feed settlement,
  snapshot and row reads, including invalid arguments. Installed for the
  session log are `log.append` and `log.fsync`: one scope per actual
  `events.jsonl` write attempt and one per actual sync, separate and never
  nested, stamped at completion and written after the session lock is
  released. Directory, lock and artifact syncs stay untimed. Installed for
  searching is `session_search.scan`: one scope per tool search request
  from its entry, including empty, cancelled and problem-only scans;
  `fiber sessions search` writes no line. Their `data` holds only `op`
  and `ms`. Installed for jobs is `jobs.start`: one scope per background
  job or delegate run that starts, until it runs: a command started with
  `run_in_background`, `tty` or `monitor`, and a new or resumed delegate,
  from just before its spawn; a foreground command that moves, from the
  move alone, never its earlier run. It ends once the job is recorded and
  its own thread runs, before the receipt is built. A refused call, a
  spawn or registration failure, and a call cancelled before its job
  starts write none, and the job's later life adds nothing. Its `data`
  holds only `op` and `ms`. Installed for worktrees is `worktree.create`
  and `worktree.remove`. `worktree.create` covers the one `git worktree
  add` a new or resumed session runs to make its worktree, stamped before
  the configuration is read and written once the writer opens, so its `ts`
  is the writer's open time and its `ms` the stamped duration; a creation
  rollback stays inside that duration and writes no second line.
  `worktree.remove` covers the one real removal after the writer opens,
  whatever its result, excluding the exclusive take, the user scans, the
  inspection, the lock-file unlink and every kept path; a kept or shared
  worktree writes none. A failed creation or recreation, a recreation that
  finds the path present, and `fiber sessions prune` write none. Their
  `data` holds only `op` and `ms`.
  Installed for the terminal are `tui.extensions`, `tui.first_render`,
  `tui.attached_frame`, `tui.log_read`, `tui.transcript`, `tui.frame`,
  `tui.attach_summary`, `tui.tail_wait`, `tui.preview_fold` and
  `tui.tail_install`.
  `tui.extensions` covers extension loading before the first draw; when a
  loader exists it writes once per process, and without one it writes none
  (`docs/tui.md`, "How a TUI extension runs"). `tui.first_render` covers
  entry through the first draw and writes at most once per process; a setup
  failure before the draw discards it and writes none. `tui.attached_frame`
  starts at the `tui.first_render` entry and ends at the first draw holding
  attached-session history; it writes once per process. Without a stream, or
  if the draw never happens, it writes none. `tui.log_read` covers the first
  attach's first fetched
  page, the first `page_in` `hub.fetch` attempt, to completion whatever its outcome,
  and `tui.transcript` covers the first successful `app.load` folding that
  page; each is written at most once per process, a failed fetch leaves the
  transcript latch unset, and an attach with no stream writes neither.
  `tui.frame` covers one frame over its 16 ms
  budget; a frame within budget writes none, so the log stays small while
  scrolling. Their `data` holds only `op` and `ms`. The terminal's file
  stays pid-named across the attach. `tui.attach_summary` covers the
  summary subscribe from before its send until acceptance queues the
  session's commands line. `tui.tail_wait` covers the pre-frame
  tail-history fetch from the initial `tail_next` through completion,
  whatever its outcome. `tui.preview_fold` covers the first history
  answer's fold from `app.tail_lines` entry through completion, not the
  empty seed fold or installation. `tui.tail_install` covers the preview
  installation from `app.install_tail` entry through completion after the
  pre-frame tail-history fetch.
  Each is written at most once per process on the first attach; an attach
  with no stream writes none. Their `data` holds only `op` and `ms`.
  Request, model, hook and network timings may overlap as nested
  measurements, never additive phases.
- **A session close that stopped waiting for a running command:** one `warn`
  line with code `timeout` in the session's file, written once for each
  connection it stopped waiting for (`docs/invocation.md`, "What the hub
  speaks").

An event inside a running session is recorded in that session's log and
nowhere else, with one exception: a session or ask process's own diagnostic
lines, its `extension_log`, `provider_request`, `peak_memory` and `op_timing`
lines, are recorded in `logs/<kind>-<id>.log` beside the session log, never
in it; a hook that fails in a session, for example, is a `notice` or
`hook_failed` there (`docs/extensions.md`, "When a hook fails"). Nothing in
`logs/` holds a credential or token, prompt or model text, a tool's arguments
or a configuration value, with one exception: an `extension_log` line is the
extension's own text, and Fiber records it as given. A failed hook is named
with its code, never its content.

**What feedback reads.** `fiber feedback` and `/feedback` copy content-free
lines from `logs/` and `crashes/` into the Diagnostics section of the report
file, on the person's machine; the issue link never carries them. For the
selected session they read `logs/session-<id>.log`, `logs/ask-<id>.log` and
`logs/tui-<id>.log`, in that order, never a file named by process id or a
delegate's, and `logs/hub.log.1` then `logs/hub.log`. A line is copied only
when it is one JSON object with an integer `ts`, a `level` of `error`, `warn`,
`info` or `debug`, the `process` of its file, and string `code` and `message`;
a session file's line also names the selected session, and a hub line names it
or names no session. Every `extension_log` line is left out, and so is any
line that does not meet this. A hub line also needs a `ts` no later than now,
and unless it names the selected session, no more than 24 hours old. Without a
selected session, the report holds hub lines with no session id only.

A report holds the newest 500 hub lines and the newest 2,000 session lines,
oldest first, equal times in the order above. The session cap keeps every
`error` and `warn` line before any other, and if those alone pass 2,000 it
keeps their newest 2,000; the report then says how many earlier lines it left
out, counting only lines that would otherwise have been copied. Both numbers
were chosen, not measured. Each crash report named for the session is copied
without a cap, as its first line, which names the thread and the source
location, and the text after its last `stack backtrace:` line: the panic
message is dropped, because it can hold anything. A report with no such line
keeps only its first line. A session with no lines on this machine, such as
one on a remote hub, still gets its id, how to export its log privately
(`docs/invocation.md`, "Deleting and pruning") and the hub lines, with a notice
that the machine has no diagnostic lines for it.

**Bounds.** `logs/hub.log` is renamed to `logs/hub.log.1` when it passes
10 MiB, replacing any older one; its single writer makes the rename safe.
Files in `logs/` and `crashes/` older than 30 days, and all but the newest 100
in each, are deleted by `fiber sessions prune` and when the hub starts. These
numbers were chosen, not measured. Fiber sends nothing from `logs/` anywhere
([ADR 0010](adr/0010-fiber-never-phones-home.md));
`fiber doctor` and `fiber feedback` read it (`docs/invocation.md`, "Commands and
flags").

**Worktrees.** Per project, `worktrees/<id>/`: the git worktree of a delegate
started with `isolation: worktree`, of `fiber ask --worktree` or of the
terminal's new-worktree switch (`docs/invocation.md`, "Isolation"). When one
is removed or kept is `docs/delegates.md` ("Worktrees"). A kept worktree is
removed only by `fiber sessions prune` (`docs/invocation.md`, "Deleting and
pruning"). A worktree's lock file coordinates a resume's recreation, an end-of-run removal and `fiber sessions prune` against the processes running in the worktree. Every process that runs in a worktree it did not just create holds the file shared; a delegate's parent holds it shared before creation until the child records `session_started` after acquiring its own share; a recreation holds it exclusively while it checks and makes the worktree again, and a removal takes it exclusively without waiting, keeping the worktree when another process holds it. The file stays whenever its worktree stays, and goes only after a whole removal, not when only the directory is removed and the branch is kept.

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

A write that fails before its rename leaves the old file; one that fails after
it, while syncing the directory, returns an error with the new file already in
place, because every reader already sees it.

One writer per session via `session.lock` is `docs/events.md`.

## Upgrades and versioning

There is no layout version marker. Coordination files that are created on demand and carry no data, such as a worktree's `.lock` file, do not change the layout, and binaries that do not know one skip it. The first change to this layout adds a
file `layout` at the top of Fiber home containing `2`; a missing file means
layout 1. `fiber update` changes only the Fiber binary, `extensions/` and `docs/`;
it never touches sessions, config, rules, approvals, pinned copies, credentials
or extension data. It replaces the binary by renaming a new file over it, so a running
session keeps the file it launched from. How the binary is fetched and
replaced is `docs/releasing.md`.
