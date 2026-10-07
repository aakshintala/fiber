# The front doors

How Fiber is started, what goes in, and what comes back out. This is what is
true now, not a plan. It is settled by
[Front doors: which invocation modes does v0.0.1 have?](https://github.com/aakshintala/fiber/issues/10)
and [Control center: one hub, headless sessions, clients over it](https://github.com/aakshintala/fiber/issues/256);
those tickets' resolutions hold the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Front door, hub, driver command, session, turn,
step boundary, steering message, watcher and driver mean what it says there
and nothing else. The events named here are `docs/events.md`; this page is
only the process contract over them.

## Two doors

Every session is headless. A person or a program reaches one through a door.

| | What it is |
|---|---|
| `fiber` | The terminal, with `fiber resume` and `fiber continue` opening it on a session. Requires a tty; without one it is a usage error naming `fiber ask`. It is a client of the hub, starting the hub if none is running ("The hub"). |
| `fiber ask` | A one-shot session for a caller outside Fiber. Its prompt is its argument or stdin, its stdout is the event stream, and it accepts no further prompts. |

The hub is not a door. It is the one process every client talks to, the
terminal included ("The hub"). A GUI, a phone, a web page or a bot is a client
of the hub, exactly as the terminal is. The terminal ships in the `fiber`
binary because a fresh machine or an SSH box needs a door that works with
nothing installed, not because it has a power another client lacks. The hub
serves no UI, and which other clients are first-party is decided by the
application that needs them, not by the hub.

A session started by the hub and a session started by `fiber ask` are the same
program. Both run the internal session command; `ask` runs it with its prompt
already supplied and no more coming. There is no second execution path, as
the archived tree also concluded in
[fiber-zig#190](https://github.com/aakshintala/fiber-zig/issues/190).

Map premise 6 governs both: every session is headless and runs one agent
loop, and the terminal, `fiber ask` and every other client see the same
session, event stream and log. No door has a privilege another lacks, and
none has a path to state another does not.

## Getting a prompt in

Fiber decides the prompt's source from `fiber ask`'s arguments alone and
never probes stdin, like a Unix command. It does not notice when stdin is
piped but unused.

```sh
fiber ask "review the diff on this branch"
fiber ask < brief.md
git diff | fiber ask "review this diff" -
```

`fiber ask "<prompt>"`: the argument is the prompt, and stdin is never read.

`fiber ask` with no prompt argument: the prompt is stdin, read to its end.

`-` as an argument means "read stdin". `fiber ask -` reads stdin as the
whole prompt. `fiber ask "<prompt>" -` appends stdin to the argument as one
prompt: the argument, a newline, then stdin. `-` is read to its end even
when stdin is a terminal, as `cat -` reads it.

`-` is accepted only as the last positional argument. `fiber ask - "<prompt>"`,
two `-` arguments, or more than one prompt is a usage error. The sentence is
`` `fiber ask` takes one prompt, then an optional `-`; quote the prompt. ``
and it ends with `` Run `fiber --help` for usage. ``

A whitespace-only part is dropped. When nothing is left, the usage error is
`` No prompt. Run `fiber ask "<prompt>"` or `fiber ask < <file>`. ``

`fiber ask` with no prompt argument and no `-`, when stdin is a terminal, is
a usage error, not a silent drop into the TUI. Checking whether stdin is a
terminal is not reading it.

A prompt whose first word is `/` followed by a skill's name runs that skill
or prompt template, as typing it in the terminal does: Fiber sends the skill's
text, then the rest of the prompt as its arguments, as the person's message
(`docs/system-prompt.md`, "Skills"). `fiber ask "/review-pr 42"` runs the
`review-pr` skill on 42. The `prompt` driver command does the same. When the
first word names no skill, the prompt is sent as written.

A large prompt goes on stdin. Linux caps a single argument at
`MAX_ARG_STRLEN`, 131072 bytes, so a long brief passed as an argument can
fail with `E2BIG` on Linux after working on macOS; stdin has no cap.

## Commands and flags

This is the whole command line. Every other doc that names a command uses the
names here.

The top level holds the commands that run a session, act on Fiber itself, or
belong to a first run. Every other command is a noun, then a verb:
`fiber extension install`, `fiber mcp add`. A command has one name. The one
exception is a pair of words both in common use for the same job: today only
`upgrade`, which runs `update`.

`fiber help` lists only the commands that are built, one line each, in
groups, then the flags and examples.

**Sessions.**

| Command | What it does |
|---|---|
| `fiber [--model <model>] [-c <key>=<value>]...` | Opens the terminal ("Two doors"). |
| `ask [--model <model>] [-c <key>=<value>]... [--resume <id> [--credential <label>]] [--worktree] [<prompt>] [-]` | Runs one session of one turn; its events go to stdout. `--resume` sends the prompt to an existing session ("Lifecycle"). `--worktree` runs it in a new worktree ("Isolation"); with `--resume` it is a usage error, because a resumed session keeps its workspace. |
| `resume [<id>] [--credential <label>]` | Opens a session in the terminal, resuming it if it has exited. With no id, opens home at the session list (`docs/tui.md`, "The session list"). |
| `continue` | Opens the most recent session in this project, live or exited, in the terminal. With none, it is a usage error naming `fiber`. |
| `sessions [--all]` | Lists sessions: id, state, the name or first prompt, what it waits on, and spend. It takes `--json`. |
| `sessions search [--all] <text>` | Searches the logs of past and running sessions for the text, as the `session_search` tool does (`docs/tools.md`, "Searching past sessions"). `--all` searches every project. It takes `--json`. |
| `sessions delete [--cascade] [--yes] <id>` | Deletes a session ("Deleting and pruning"). |
| `sessions export <id> [<path>]` | Writes the session's log and its artifacts to `<path>`, default `./<id>/` ("Deleting and pruning"). |
| `sessions prune [--older-than <duration>] [--cascade] [--force] [--dry-run] [--yes]` | Deletes exited sessions older than the duration and removes kept worktrees that hold nothing to lose, then prints the space freed ("Deleting and pruning"). |
| `models [<search>]` | Lists the models the installed providers serve: `provider/model`, context window, and price per million tokens in and out, with the configured default marked. `<search>` filters by substring. It takes `--json`. |

`sessions`, `sessions search`, `sessions prune` and `continue` use the scope of the terminal's session list. Inside
a git repository that is the repository's project, every worktree of it;
outside one it is every project. `sessions --all` lists every project.

`models` prints each provider's cached model list at once, and runs a
provider's `models()` only when it has no cached copy. A list older than
`model_lists.refresh_after` refreshes in the background for the next run
(`docs/model-routing.md`, "Model discovery").

`fiber ask` has no `--continue`. Several callers run `fiber ask` at once, so
"the most recent session" is a race.

`-c <key>=<value>` sets one configuration key for one run, the per-run layer
(`docs/configuration.md`, "Layers"). Both doors take it, as they take
`--model`, and it may be given more than once. The terminal passes it to each
session it asks the hub to start.

`--credential <label>` on a resume switches the session to another credential
label, and the log records the switch (`docs/model-routing.md`, "Which
credential a session uses"). A new session takes its label from
`providers."<name>".credential`, which `-c` can set for one run.

**Fiber itself.**

| Command | What it does |
|---|---|
| `update` | Updates the Fiber binary and every installed extension together (`docs/releasing.md`, "Updating"). `upgrade` runs the same command. A repository's extensions are not updated: each changes only through a new offer. |
| `approve [--yes]` | Run in a repository: shows every extension, hook and MCP server the repository declares, as one offer would, and approves them for this person (`docs/extensions.md`, "Code a repository ships"). `--yes` approves without asking, for a script or a machine image. It prints each approval on stderr. |
| `login [<name>] [--as <label>]` | Stores a provider's key under a credential label (`docs/model-routing.md`, "Logging in"), or a secret an installed extension declares (`docs/configuration.md`, "Secrets"). Without `--as`, a provider's label is the account's email when the login reveals one, otherwise `default`; a label already stored is refused. `--as` applies only to a provider. A name that is neither an installed provider nor a declared secret is a usage error that lists both. With no name, a terminal offers the installed providers and the declared secrets; without a terminal, it is a usage error. |
| `logout <provider> [--as <label> \| --all]` | Deletes a provider's stored key. With several labels it needs `--as` or `--all`. A key from an environment variable, a file outside Fiber home or a command is named, not removed, and the exit is non-zero. |
| `doctor` | Says whether a session can start, and how to fix it when it cannot. |
| `completion <shell>` | Prints a completion script for `bash`, `zsh` or `fish`, such as `source <(fiber completion zsh)`. It completes commands and flags, generated from the same parser definitions, and no values. |
| `help [<command>]` | Prints the menu, or a command's help. |
| `version` | Prints the version. |

`doctor` prints the version, the default model and the provider it needs, each
installed provider with one line per credential label and where its key comes
from, the selected label marked, the permission mode, the
number of MCP servers, and whether the hub is running. When it is, `doctor`
also prints the environment its sessions get: the `PATH`, the names of the
other variables and whether the login-shell capture succeeded ("A session's
environment"). A line that stops a
session from starting carries its fix, such as
`` no key for openrouter: run `fiber login openrouter` ``. It also names each
secret an enabled extension declares that is not stored, with the
`fiber login <name>` that stores it; a missing secret does not stop a
session from starting. It exits non-zero
when a session cannot start. It also prints where the diagnostic logs are, the
newest line at `error` level among them, and the newest crash file, with its
age (`docs/state.md`, "What each part holds"). When `docs/` is missing from
Fiber home, it says `` user guide not installed: run `fiber update` ``
(`docs/releasing.md`). It never touches the network, so it does not check that
a key is valid.

**Extensions.** `fiber extension` manages extensions (`docs/extensions.md`,
"Installing").

| Command | What it does |
|---|---|
| `extension install [--project] <name or path>` | Installs an extension and its dependencies. `--project` installs it for the current project only. |
| `extension update [<name>]` | Updates one extension, or every installed extension, to its newest tag. It never touches a repository's extension. |
| `extension remove <name>` | Removes an extension, the dependencies nothing else uses, and their data. Run in a project on a repository's extension, it removes that and records never for its content. |
| `extension reinstall <name>` | Removes an extension and installs it again, from its recorded source and commit, or from `<name>` as given when it is damaged (`docs/extensions.md`, "Installing"). |
| `extension list` | Lists installed extensions: name, version and commit; and each repository extension with its project, its path in the repository and the content it loads. |
| `extension test [<path>]` | Runs an extension's test cases, from its directory or the current one, against the `scripted` provider in a temporary Fiber home, and exits non-zero if any fails (`docs/testing.md`, "Testing an extension"). |

**MCP servers.** `fiber mcp` manages MCP servers (`docs/mcp.md`).

| Command | What it does |
|---|---|
| `mcp add [--project \| --repo] <name> <url>` | Declares a remote server. |
| `mcp add [--project \| --repo] <name> [-e KEY=value]... -- <command> [args]...` | Declares a stdio server. |
| `mcp remove [--project \| --repo] <name>` | Removes a server's declaration. |
| `mcp list` | Lists every declared server, the layer that declares it, and whether a repository's server is approved. |
| `mcp login <server>` | Logs in to a server that needs OAuth. |
| `mcp logout <server>` | Deletes a server's stored token. |
| `mcp serve` | The stdio MCP server a delegate on another harness uses to send session messages (`docs/delegates.md`). |

**Configuration.** `fiber config` reads and writes configuration
(`docs/configuration.md`, "When Fiber writes").

| Command | What it does |
|---|---|
| `config get <key>` | Prints the effective value and the layer or flag it came from. |
| `config set [--project \| --repo] <key> <value>` | Writes one key. |

The commands that write configuration take the same scope flags. With
neither, they write the global file in Fiber home. `--project` writes the
per-project file in Fiber home. `--repo` writes the repository's
`.fiber/config.json`. `mcp add --repo` also records the approval of that
exact declaration for the person who ran it (`docs/mcp.md`, "A repository's
servers").

**The hub.** The first seven run on the machine whose hub they manage; `add`
and `remove` run on a client ("The hub"). A paired client can also mint a
pairing code, list devices and revoke one, through the hub commands
`pairing_code`, `devices` and `revoke` ("Remote clients"):

| Command | What it does |
|---|---|
| `hub install [--port <port>]` | Registers the hub as a login service. Without `--port` it listens on its local socket only. With `--port` it also listens on `127.0.0.1:<port>`, where every connection presents a device token, and writes `hub.port`. |
| `hub uninstall` | Removes the login service. Running sessions carry on. |
| `hub status` | Prints whether the hub is running, its version, its port, the connected clients and the paired devices. It takes `--json`. |
| `hub pair <device>` | Prints a pairing code for a device of that name, and in a terminal draws it as a QR code with the hub's address. The code works once, within 10 minutes. |
| `hub token list` | Lists paired devices: name, when paired, last connection. |
| `hub token revoke <device>` | Revokes a device's token and closes its live connections. |
| `hub refresh` | Rebuilds the hub's environment for the sessions it starts from now on ("A session's environment"). |
| `hub add [--default] <name> <address>` | Adds a hub to this client's list, asks for its pairing code, and stores the device token it gets. The address is `ws://`, `wss://` or `unix:` followed by a socket path. `--default` makes it the hub this client uses without `--hub`. |
| `hub remove <name>` | Removes a hub from this client's list and deletes its device token. |

Every command that talks to the hub takes `--hub <name>` to use a hub from
the client's list instead of the default.

**Internal commands.** Fiber starts its own processes with internal
commands: the session command, the hub, the image child ("Processes") and
the `grep` and `find` that the file tools run (`docs/tools.md`). None is in
the menu, and no person or client runs them.

The flags are `-h`, `--help`, `-v` and `--version`. There is no `-V`.
`-v` and `--version` are top-level only: `fiber ask -v` is an unknown
argument.

`-h`, `--help` and `fiber help` print the menu on stdout and exit 0.
`fiber <command> --help`, `fiber <command> -h` and `fiber help <command>`
print that command's help on stdout and exit 0. `fiber ask --help` starts
no session and prints no `fiber_exited` line. It is the one exception to
"ask's stdout is the event stream".

`fiber --version`, `fiber -v` and `fiber version` print
`fiber <version> (<commit>)` on stdout and exit 0, or `fiber <version>`
from a build with no git history (`docs/releasing.md`, "Versions"). The
version is this binary's package version.

Help and version are printed before Fiber reads `FIBER_HOME`, configuration,
credentials or stdin, so they succeed when home is empty or no provider is
installed. A failed write to stdout is ignored and the exit is still 0, as
with `fiber extension list`.

```sh
fiber ask "review the diff on this branch"
fiber ask < brief.md
git diff | fiber ask "review this diff" -
fiber extension install openrouter
fiber help ask
```

A wrong invocation prints one sentence on stderr and exits 2. The sentence
is the parser's first paragraph, and where the parser has a suggestion it
includes `did you mean '<suggestion>'?`. It ends with
`` Run `fiber --help` for usage. `` A usage error from parsing an invocation
whose first argument is `ask`, including `ask`'s own argument-shape errors,
also prints one `fiber_exited` line on stdout, carrying the exit code and
`error` (`docs/errors.md`, "Before a session exists"). Any other parse
error prints the sentence on stderr only, and stdout is empty.

`fiber` with no arguments is a usage error naming `fiber ask`. Nothing is
written to stdout. The sentence is `` The terminal door is not built; run `fiber ask "<prompt>"`. Run `fiber --help` for usage. ``

`-V` is an unknown argument: `Unexpected argument '-V' found.` An unknown
command names the suggestion when there is one, as
`Unrecognized subcommand 'instal'; did you mean 'install'?`. An unknown
flag does the same, as
`Unexpected argument '--modle' found; did you mean '--model'?`. A missing
required argument names the value, as `<name or path>` for `fiber extension install`.

## Why stdin is the prompt

On `ask`, stdin is the prompt when the arguments say to read it
("Getting a prompt in"). No session takes driver commands on stdin:
every driver is a client over a socket ("Processes").

What `ask` gives up by spending its stdin on the prompt is the ability to
answer an interaction or steer mid-run. That costs nothing, because
`docs/permissions.md` already settles the unattended case — "With no client
attached and no answer possible, escalation is a block and the run continues
under the rule above until it exhausts the block budget" — and cancelling is a
signal, not a command. A program that wants to talk back is a client of the
hub, as the terminal is.

## Driver commands

The closed set a driver may send. Every command is answered with exactly one
ephemeral `command_accepted` or `command_rejected` echoing the command's id;
acknowledgements carry no `seq`, so they never reach the log.

### The command line

One JSON object per line. Its keys follow the rules of `docs/events.md`,
"Payload types": snake_case, and an optional key is absent, never `null`.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes | minted by the client from random bytes, as Fiber mints its own ids (`docs/events.md`, "Identity and ordering"). Acknowledgements and events name the command by it, as `command_id` |
| `command` | string | yes | the command's name, from the table below |
| `session_id` | string | no | on a connection to the hub, the session the command is for; the hub passes the line to that session without this key. Absent there, the command is for the hub itself ("The hub"). A session's own socket never receives it |
| `args` | object | no | the command's own keys, below; a missing `args` is read as an empty object, so a command whose keys are all optional may leave it out |

```json
{"id":"c_7f3a","command":"steer","args":{"content":[{"type":"text","text":"use the other test file"}]}}
```

A line that is not a JSON object, has no string `id` or `command`, has
`args` of the wrong type, or has a key not in this table, is rejected
`malformed`. `args` with a missing key, a key of the wrong type or a
key the command does not take is rejected `invalid_arguments`, so an older
Fiber says no to a newer client's key instead of ignoring it.

A session remembers the id of every command it accepted for as long as its
process runs, and rejects a second command with the same id
`duplicate_command`. A client that lost its connection resends every
command it had no answer for, with the same id, so a command that had
already arrived is never applied twice.

`content` is content parts (`docs/events.md`, "Content parts"). A client sends
an image part as `type`, `data`, the image's bytes in base64, and `mime_type`,
and nothing else: a remote client cannot write into the session directory.
Fiber processes the image (`docs/model-routing.md`, "Image limits") and writes
the processed file to `artifacts/`, then logs the part with its `path`,
`mime_type`, `width` and `height`. An image that cannot be read, or is over
50 megapixels, is rejected `invalid_arguments`, naming the image.

| Command | `args` |
|---|---|
| `subscribe` | `level` (string), `summary` or `full` |
| `prompt` | `content` |
| `steer` | `content` |
| `steer_drop` | `command_id` (string), the `steer` command's id |
| `message` | `from_session_id` (string), `text` (string) |
| `cancel` | none |
| `reply` | `request_id` (string) and the answer ("Replying") |
| `job_stop` | `job_id` (string) |
| `background` | none |
| `reload` | none |
| `tools` | none |
| `history` | `from_seq` (integer), `to_seq` (integer, optional) |
| `model` | `model` (string), a model reference as a person types one (`docs/model-routing.md`, "Naming a model"); `thinking` (string, optional) |
| `credential` | `label` (string), a credential label of the session model's provider |
| `name` | `text` (string); empty clears the name |
| `handoff` | `instructions` (string, optional) |
| `rewind` | `from_session_id` (string, optional), `seq` (integer, optional), `summarise` (boolean, default false), `adopt` (array of strings, default empty) |
| `shell` | `command` (string); `send` (boolean, default false) |
| `command` | `name` (string); `text` (string, optional), what the person typed after the name |
| `close` | none |

### What each command does

| Command | What it does |
|---|---|
| `subscribe` | The first command on every connection to a session, and on a connection to the hub the first command for each session. `full` receives the session's whole stream, folded from the log first; `summary` receives only the latest `session_status` and `extensions_loaded` (`docs/events.md`) and reads no log. Only a `full` connection counts in `clients`. Any other command for that session before it is rejected `not_subscribed`, and a second `subscribe` for it is rejected `invalid_arguments`. |
| `prompt` | Starts a turn. Rejected `busy` if a turn is running. |
| `steer` | Sends a steering message, which joins the running turn at its next step boundary. |
| `steer_drop` | Removes a queued steering message, so nothing is applied. Names the message by the id of the `steer` command that sent it, as `steering_queue` lists it (`docs/events.md`). |
| `message` | Delivers a session message from another session (`docs/tools.md`, "Messaging other sessions"). During a turn it is a steering message; between turns it starts a turn. Rejected `closing` after `close`. |
| `cancel` | Ends the running turn (`docs/architecture.md`, "Cancellation"), and stops a running `shell` command. Rejected `stale_request` if no turn is running and no `shell` command is. |
| `reply` | Answers an interaction the loop raised: approval, confirm, select, multi-select, text input or form ("Replying"). |
| `job_stop` | Stops a running job by `job_id`, at once, even while a model response streams (`docs/architecture.md`, "One inbox"). Rejected `stale_request` if the job is not running. |
| `background` | Moves every shell call running in the current turn to the background (`docs/tools.md`, "Shell"). Rejected `stale_request` if none is running. |
| `reload` | Re-reads configuration, restarts changed MCP servers and extensions, and declares the tool set again (`docs/mcp.md`, "Reload"). Rejected `busy` if a turn is running. |
| `history` | Answers, in its `command_accepted`, with the session's durable lines from `from_seq` to `to_seq` inclusive, or to the latest when `to_seq` is absent, at most 256 lines; a client pages for more. This is how every client pages history, the local terminal included: no client reads a session's log from disk (`docs/tui.md`, "History and paging"). Rejected `invalid_arguments` when `from_seq` is past the latest line. |
| `tools` | Answers with every declared tool: its source, whether it is full, deferred or loaded, and its approximate size (`docs/tools.md`, "Seeing the tools"). |
| `model` | Switches model or thinking level at the next turn boundary. Takes a model reference and an optional thinking level. The switch rebuilds the prompt cache, and the terminal says so with the rebuild's size first (`docs/prompt-cache.md`, "Switching model"). Rejected `invalid_arguments` for an unknown model. |
| `credential` | Switches the session's credential label at the next turn boundary (`docs/model-routing.md`, "Which credential a session uses"). The switch rebuilds the prompt cache, as a model switch does. It changes this session only; the terminal's `/credential` also saves the label. Rejected `invalid_arguments` for a label the provider does not have. |
| `name` | Sets the session's name, which pins it against the model's `name_session`. Takes the text; empty text clears the person's name and unpins it. Written as `session_named`. |
| `handoff` | Starts a handoff: the model's context restarts from a note the model writes (`docs/handoff.md`). Takes optional instructions saying what the next stretch of work focuses on. During a turn it applies at the next step boundary, as a steering message does; between turns it is a turn of its own whose input is the command. |
| `rewind` | Starts a new session process that continues a session from an earlier point (`docs/events.md`, "Rewind"), and answers with the new session's id. Takes an optional `from_session_id`, default this session; an optional `seq`, default the start of the latest turn; whether to summarise; and `adopt`, the `job_id`s of the jobs started after the point that the new session keeps, default none, so every other such job stops. Rejected `busy` if a turn is running, `stale_request` if `adopt` names a job that is not running, `not_step_boundary` if `seq` is not a step boundary, `session_held` if another process holds the session, `delegate_session` if it is a delegate, and `summary_failed` if it asked for a summary that could not be made. |
| `shell` | Runs a shell command the person typed, as `!` does in the terminal. Takes the command and `send`, default false. Answered when the command ends. Accepted during a turn. |
| `command` | Runs an extension's command by name, with the text after it as arguments, as a person typing `/name args` does (`docs/extensions.md`, "Commands and screens"). Rejected `unknown_command` for a name no extension registered. |
| `close` | Accept no more prompts; finish the turn in flight, then any running jobs (`docs/tools.md`, "Background jobs"), and exit. |

Rejection codes: `malformed`, `invalid_arguments`, `unknown_command`,
`not_subscribed`, `busy`, `stale_request`, `not_step_boundary`,
`session_held`, `delegate_session`, `summary_failed`, `closing`,
`duplicate_command`, `session_not_found`.

**`reply` answers every interaction that asks something, not just approvals.**
`docs/architecture.md` fixes the set: "Fiber ships one closed, versioned set
of interactions — approval, confirm, select, multi-select, text input and
form — carried on the same request events the loop uses to ask a human
anything, and answerable by any connected client including a headless one." The interaction kinds are
versioned and may grow; one command that carries a `request_id` does not have
to grow with them. What happens to a stale one is already `docs/events.md`'s:
"a reply naming a request that is no longer pending is rejected and does
nothing, so a late approval can never authorise a different action."

**Editing a queued steering message is drop, then steer.** If the loop
drains the queue between the two, the new text joins one step later. If it
drained before the drop, the drop is rejected `stale_request`, so the client
knows the original was applied. Nothing is lost or applied twice.

**A delegate is reached on its own connection.** A client that shows a
delegate subscribes to that delegate through the hub, and sends it `steer`
and `reply` there like any session. No session forwards a command to another
(`docs/delegates.md`).

**`job_stop` names a running `job_id`.** It is rejected `stale_request` if the
job is not running. The terminal lists jobs with `/jobs` and can stop one from
there. The list is a fold of the log, so there is no driver list command.

**`background` frees the turn without a message.** It moves every running
shell call to the background, as a person's Ctrl+B does, with nothing sent
to the model. The terminal binds it to Ctrl+B. It never kills a command: the command becomes a
job and keeps its timeout.

**`shell` runs the person's own command, and the model sees it only if asked.**
It runs in the session's workspace with the shell and output cap of the shell
tool (`docs/tools.md`, "Shell"), and `cancel` stops it. It needs no approval:
the person typed it, and a client that can attach already controls the session
([#30](https://github.com/aakshintala/fiber/issues/30)).

- With `send` false, the output goes back in the command's
  `command_accepted` to the client that sent it and is never logged. The
  terminal's side panel reads git state this way.
- With `send` true, the output is logged as `shell_command`
  (`docs/events.md`) and joins the next turn's input, never the running turn.
  The terminal sends it for `!command`, and sends `!!command` with `send`
  false.

Settled by
[#148](https://github.com/aakshintala/fiber/issues/148).

**`rewind` starts a new session process.** The session being rewound closes
with `rewound` while it still holds its lock, and the hub starts the new one,
whose `session_started` names the old session in `forked_from`. Each client
connected to the old session is sent the new session's id and subscribes to
it. A session no process holds is rewound by the hub opening it first.

**The set is a floor, not a proof.** It is what Fiber's settled semantics
require today. A later ticket may add one. Adding a command is additive and not
breaking, which is why `unknown_command` exists: an older Fiber tells a newer
client no, in words, instead of ignoring it.

A driver that needs a command Fiber does not define has found a hole in the
contract, not a reason for a private channel. Premise 5 gives the TUI "no
privilege a second GUI client would not have", so a command the terminal needs
is a command every driver gets.

### Replying

`reply`'s answer keys are the keys of the line it causes, so a client builds a
reply from the table it already reads.

An interaction (`interaction_requested`) is answered with `declined: true`, or
with the answer keys for its kind: `confirmed`, `labels`, `text`, or `answers`
with an optional `note` (`docs/events.md`, `interaction_resolved`).

An approval (`permission_requested`) is answered with these keys, which
`permission_resolved` records:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `decision` | string | yes | `allow` or `deny` |
| `feedback` | string | no | with `deny`, what the person typed; the model receives it |
| `remember` | object | no | with `allow`, on a request that offers a `rule`: `scope`, which is `session` for a session grant or `project` for a standing rule in the project's rules file, and `prefix`, which is the request's `rule.subject` or `rule.prefix` |

```json
{"id":"c_91be","command":"reply","args":{"request_id":"r_2c01","decision":"allow","remember":{"scope":"project","prefix":"npm test"}}}
```

An offer of a repository's code (`repository_code_offered`) is answered with
`decisions`: an array of strings, one per offered item in the offer's order,
each `approve`, `skip` or `never`, which `repository_code_resolved` records.

A reply is rejected `stale_request` when its request is no longer pending, and
`invalid_arguments` when its keys do not fit the request: another kind's
answer keys, `feedback` with `allow`, `remember` on a request with no `rule`
or with a prefix the request did not offer, or `decisions` whose length is
not the number of items offered. A global standing rule is added by
editing the global rules file, never from an approval.

## Lifecycle

**Every process announces itself with `fiber_started`**, carrying the Fiber
version, the `schema_version`, the `session_id`, and whether the session is
new or resumed. In a new session it follows `session_started`, the log's
first line; in a resumed process it is the process's first line
(`docs/events.md`, "Session and turn").
`fiber ask --resume <id>` and `fiber resume <id>` take the same selector: a
full session id or any prefix of one that is unique among the project's
sessions. `fiber resume` with no id opens home. `fiber ask --resume` with no
id is a usage error, because `ask` has no list to show.

**A session exits when it has been idle for `session.idle_exit_ms`**, 30 minutes
by default (`docs/configuration.md`), whoever is connected. Idle means no turn
running, no jobs running and no cache warming (`docs/prompt-cache.md`,
"Warming while idle"). Waiting on an approval or a question is idle,
because nothing is in flight. Connected clients do not keep a session alive: a
phone or a terminal left open is connected all the time. Leaving never
cancels. When the delay passes, Fiber exits.

**A session left unattended with jobs running checks them once.** Unattended
means no prompt or steer from a person or a driver for
`session.idle_exit_ms`, jobs or no jobs. A running job keeps a session from being idle, so the idle delay
never ends it; instead, when it has been unattended that long with jobs
running, Fiber wakes the model once with the jobs check: a notice listing
the running jobs and telling it to stop any that look hung or that it no
longer needs, judging from each job's output file. The session does not end.
The check fires once and is armed again only by the next prompt or steer, so a job
the model keeps does not cost a turn every delay.

**A delegate exits as soon as its run finishes**: its final answer is written
and its own jobs are done. It does not wait for `session.idle_exit_ms`. A later
`delegate_message` resumes it (`docs/delegates.md`).

**A `fiber ask` session exits when its turn ends** and its jobs are done.

**A prompt sent to an exited session through the hub resumes it**, then
delivers the prompt. A client showing a session that exits keeps showing it,
because its conversation is its log.

**A session that never got a prompt leaves nothing behind.** A session that
exits with no `turn_started` in its log deletes its own directory.

**`close` ends the session whoever else is attached.** It accepts no more
prompts, finishes the turn in flight, then any running jobs (`docs/tools.md`,
"Background jobs"), and exits.

One rule covers every case that matters. A GUI that dies mid-turn closes its
connection, and the session finishes the turn. A phone that loses its
connection mid-turn loses no work. A delegated run is spawned with its prompt
supplied, so it runs until the model's final answer, twenty minutes if it
takes twenty minutes; its caller's turn ending changes nothing.

There is no detach command. A client leaves by closing its connection. How the
terminal leaves, and how it stops one session, is `docs/tui.md`, "Quit".

**A request re-raised on resume keeps its `request_id`.** A `reply` sent
through the hub to a session that exited on a pending request makes the hub
resume the session, then deliver the reply. A late reply still cannot
authorise a different action, because a different action is a different
request.

**A pending approval or question does not keep a session alive past the idle
delay.** When the delay passes with either pending, the session exits, and
`fiber_exited` names the request it stopped on. Resuming the session raises
the request again and the turn goes on from there. `fiber ask --resume` on such
a session has no one to answer, so it refuses the request, lets the model finish
the turn, and then runs its prompt as the next turn; the prompt waits behind that
turn. The hub lists such a session as waiting on the person, from `recent.jsonl` (`docs/state.md`).
Two cases have no one to wait for, and there escalation is a block as
`docs/permissions.md` ("Headless") describes: a session started by
`fiber ask`, and a session that has been sent `close`. In both, and in every
delegate, an `ask_user` question ends the turn instead, and the driver resumes
the session with the answers (`docs/tools.md`, "Asking the person").

**A pending elicitation waits within its call's timeout.** An MCP call is in
flight, so the session stays alive for it, and no longer than the call's
timeout (`docs/mcp.md`, "Calls").

**A prompt arriving mid-turn is rejected `busy` and starts nothing.** Steering
is the mid-turn channel. Fiber holds no prompt queue that no durable event
describes; the admission-ordered steering queue is the only queue.

**Cancellation targets the turn, not the process.** What it does is the
concurrency section of `docs/architecture.md` and adds nothing here.

**Exit codes: 0 success, 1 failure, 2 usage, 129 SIGHUP, 130 SIGINT, 143
SIGTERM.** Usage is Fiber called wrongly: a bad flag, no prompt, no tty.
`fiber ask` exits 1 when its turn failed; a session the hub
started exits 1 only when the process itself failed (`docs/errors.md`, "What a caller gets").
What a signal guarantees before the process goes is "Shutdown".

## What a caller gets back

Nothing on this door is a second format. `docs/events.md`: "Filter a
non-interactive run's stdout to durable lines carrying its own `session_id` and
you have `events.jsonl`, byte for byte."

- **The verdict** is `fiber_exited`, the last line, which copies the final
  message's text so a one-shot caller reads one line and is done.
- **Questions** are `fiber_exited.questions`, when the model asked with
  `ask_user`. The run exits 0, and the caller resumes the session with the
  answers as its prompt (`docs/tools.md`, "Asking the person").
- **Finished or died** is whether `fiber_exited` is there at all. A
  `fiber_started` with no matching `fiber_exited` means the process died.
- **A failure before any session exists**, such as invalid configuration or a
  missing credential, still ends stdout with a `fiber_exited` line carrying the
  error, with no `session_id` (`docs/errors.md`, "Before a session exists").
- **Progress** is the ephemeral lines. They carry no `seq` and never reach the
  log.

Two consequences for the door: **stdout carries no terminal escape codes and
no tty is required**, because either would break that byte-for-byte equality.

## Processes

Settled by
[Process architecture: core, TUI and shared services](https://github.com/aakshintala/fiber/issues/81)
and [Control center](https://github.com/aakshintala/fiber/issues/256); the
rationale and the rejected layouts are
[ADR 0009](adr/0009-each-session-is-one-process.md).

- **Every session is one process**, running the internal session command. The
  hub starts one for a client; `fiber ask` is one; a parent starts one for each
  Fiber delegate. Each session starts its own MCP servers and process
  extensions, so nothing is shared between sessions (`docs/mcp.md`,
  `docs/extensions.md`).
- **No session has a pipe driver.** Every driver is a client over the
  session's socket. `fiber ask`'s stdout is a watcher.
- **A Fiber delegate is a child process of its parent.** The parent is an
  ordinary client over the delegate's socket. It also holds a pipe to the
  delegate's stdin that it never writes to: end of file on it means the parent
  is gone, and the delegate shuts down ("Shutdown"). A dropped socket
  connection is only a client leaving; the parent reconnects. Delegates on
  other harnesses are child processes too (`docs/delegates.md`).
- **An image child is a short-lived process, one per image.** The session
  starts it by running `fiber` again with an internal command, not a door,
  so the child always matches its parent's version. The session process
  runs no image code; the child reads the header too
  (`docs/model-routing.md`, "Image limits"). Spawning a child that does
  nothing takes 1.7 ms on macOS, measured with the probe in
  `research/image-limits/README.md`.
- **The terminal is its own process, a client of the hub.** It has a `full`
  connection to the session on screen and a `summary` connection to the rest,
  through the hub. It draws what arrives and has no path to state the stream
  does not carry. A terminal crash, or an error in a TUI extension, cannot
  interrupt a session's work.
- **Every running session listens on a local socket** at
  `~/.fiber/run/<session_id>` (`docs/state.md`), reachable only by the account
  that owns Fiber home. Being that account is the authentication. The hub, a
  delegate's parent and session messages use it; every other client reaches it
  through the hub. The process holding the session's lock owns the socket: it
  removes any old one before binding, and nothing else ever removes one.
- **Resuming a session that is still running attaches to it.** A session log
  has one writer (`docs/events.md`), so a resume never opens a second one. A
  `full` connection folds the log by `seq` first, then streams.
- **A client attaches across versions only when it can read the stream.** A
  session keeps the binary it started with through `fiber update`. A client
  reads the session's `schema_version` from `fiber_started`; an additive
  difference is fine (`docs/events.md`, "Versioning"), and on a breaking one
  the client says which version the session runs and declines, so the person
  can close it or let it exit.

The socket is the only path into a running session. The terminal, a parent
session, a GUI and a phone through the hub are the same kind of client, as map
premise 5 requires.

## Shutdown

Settled by
[Shutdown: what SIGTERM has to guarantee](https://github.com/aakshintala/fiber/issues/34);
that ticket's resolution holds the rationale and the rejected alternatives.
What reference agents do, and what a crash leaves behind, are
`research/shutdown/`.

A shutdown is Fiber stopping because a signal told it to. It is bounded, it
stops everything the session started, and it asks nobody anything. Exiting
because the work ran out ("Lifecycle") and `close` are not shutdowns: both
wait for jobs without a cap. A supervisor that wants the work finished sends
`close`, and SIGTERM when its patience runs out.

**Three signals, one path.** SIGTERM, SIGINT and SIGHUP each start a
shutdown. They differ only in the exit code: 143, 130 and 129. A second
SIGTERM or SIGINT during a shutdown skips the grace period below: every
process group still alive gets SIGKILL at once, and Fiber writes what it
knows and exits.

**What happens, all at once:**

- The model request's socket is closed.
- Every running tool call's process group, every job's and every delegate's
  gets SIGTERM. A group still alive 800 ms later gets SIGKILL, and Fiber
  reads its output for at most 2 s more: the sequence in `docs/tools.md`
  ("Stopping a command"). Groups are signalled together, never one after
  another. Six groups that ignore SIGTERM took 0.81 s in parallel and 4.85 s
  in sequence (macOS arm64, `research/shutdown/probe5_nested_kill_cost.py`).
- A Fiber delegate is a session process, and SIGTERM starts its own shutdown.
  Its parent waits for it to exit, up to the bound, rather than sending
  SIGKILL at 800 ms, so the delegate stops its own commands and writes its
  own `fiber_exited`. Depth is capped at 2 (`docs/delegates.md`) and every
  process forwards the signal before doing anything else, so every level of
  a tree counts down from almost the same moment.
- Each MCP call in flight gets `notifications/cancelled` and ends `failed`
  with code `mcp_cancel_requested`, as on a cancelled turn (`docs/mcp.md`,
  "Calls"); a pending elicitation goes with its call. Then each stdio
  server's stdin is closed and it gets SIGTERM, then SIGKILL 800 ms later.
- No model request is made, no ending notice is given, and no hook runs.
  Because no `after_tool` hook runs to redact it, a call cancelled by shutdown
  completes with no content and no artifact.

**What is written.** The turn in flight ends as the cancel key ends it
(`docs/architecture.md`, "Cancellation"): each tool call completes
`cancelled` once its group is empty, each job `cancelled`, a handoff in
flight `cancelled`, and the turn `turn_completed { outcome: interrupted }`. Two
things differ from a cancel: a pending approval or question stays pending, so
resuming raises it again (below), and queued steering messages start no turn. They
were never logged, so they are gone. A request already answered when the signal
arrives was not pending: its answer stands, and a call it allowed completes
`cancelled` without running.
Nothing is written for a call before it has stopped. Then `fiber_exited`
with the exit code and no final message, the socket is unlinked, the lock is
released, and the process exits.

A session waiting on an approval or a question when the signal arrives has nothing running
(`docs/architecture.md`: "Permission decisions are made in order, before any
of them runs"). It stops its jobs and exits with `suspended_on` naming the
request, as it does when the idle delay passes ("Lifecycle"), and resuming
raises the request again.

A signal that arrives before `fiber_started` is written exits with the code
and writes nothing. Once `fiber_started` is written, `fiber_exited` always
is, unless the process dies or passes the bound.

**The bound is 5 seconds** from the signal to exit, per process. The
command stage is at most 2.8 s (800 ms grace plus 2 s drain), the levels of
a tree run concurrently, and the rest is margin. It sits under the
supervisors Fiber runs under: Docker sends SIGKILL 10 s after SIGTERM,
Kubernetes 30 s, systemd 90 s (each one's documented default, not measured).
Past the bound, every group still
alive gets SIGKILL and the process exits at once with the same code, writing
nothing more: the loop may be stuck holding the log, so no other thread
writes to it. The log ends as a process that died leaves it, with no
`fiber_exited`, and a resume reads it as it reads any such log: a call left
open has an unknown outcome and is never re-run, and a job left open
completes `orphaned` (`docs/events.md`, "Resume").

**What a crash leaves.** A crash, a SIGKILL, or a supervisor that gives up
before the bound stops nothing. A command whose output goes to the pipe
Fiber held dies at its next write; everything else keeps running, and a job
writes to a file, so every job survives (macOS arm64,
`research/shutdown/crash-cleanup.md`). Fiber does not go looking for them: a
resumed session marks each `orphaned` and "does not touch any process"
(`docs/events.md`). Linux's `PR_SET_PDEATHSIG` reaches only the shell Fiber
starts, not what the shell starts, and a recorded process group and start
time cannot prove a group is still the one recorded, so neither is used. The
gap is stated rather than half closed.

A Fiber delegate whose parent died sees end of file on its lifeline and
shuts down within the bound, so it writes its own `fiber_exited` and its log
is complete. Its parent's log still marks the job `orphaned` on resume,
because the parent cannot know. The resumed parent reads that delegate's log
and writes any `usage_recorded` lines it is missing (`docs/loop.md`,
"Spending budget").

**The terminal and the hub.** Closing the terminal closes its connections,
and each session follows "Lifecycle". Stopping the hub ends its relays, and
each session sees its clients leave. Neither stops a session.

On SIGTERM, SIGINT or SIGHUP the hub sends each websocket a close message
saying it is stopping, closes its local connections and exits. It does not
drain, because it holds no session. A client reconnects and resends, with the
same id, every command it had no answer for; a session that already accepted
one rejects the copy `duplicate_command` ("The command line").

## The hub

Every client reaches sessions through the hub, the local terminal included.

- **The hub holds no session.** It lists sessions, starts and resumes them,
  and relays every client connection to a session's socket. A session is
  running when its socket accepts a connection; anything else is a log to
  resume. Its crash or its restart drops client connections and ends no
  session. Clients reconnect.
- **It finds every running session by its socket.** Each running session has
  a socket at `run/<session_id>`, whoever started it, a `fiber ask` run
  included. The hub reads `run/` when it starts and watches it for new
  sockets, so it also finds sessions that were running before it restarted.
- **It serves one feed.** The hub holds a `summary` connection to every
  running session and serves each client the latest `session_status` of every
  running or waiting top-level session, across all projects. When a session's
  socket closes, the hub sends `session_left` with `how`: `exited` when the
  log ends in `fiber_exited` or `rewound`, otherwise `crashed`. A process
  that died cannot add itself to `recent.jsonl`, so the hub appends the
  crashed session's row (`docs/state.md`). A crashed session stays in the
  feed, with its last `session_status`, until it is resumed or a client sends
  `dismiss`. Exited sessions
  come from a paged query over `recent.jsonl`, filterable by project, newest
  first (`docs/state.md`). Delegates are never in the feed; a client shows a
  delegate when the person opens its parent.
- **It starts sessions with the internal session command**, in the workspace
  the client names. Whoever starts a session generates its id and passes it
  on that command's line, so the starter knows the id before the process
  runs and nothing is read back.
- **A client starts it** when none is running. That hub listens on its local
  socket only, and exits once no client has been connected for
  `hub.idle_exit_ms` (`docs/configuration.md`).
- **`fiber hub install` registers it as a login service** (launchd on macOS,
  systemd on Linux) that never exits for being idle. With `--port`, it also
  listens on that port of `127.0.0.1`, and on no other address. `fiber update`
  restarts it through the service manager (`docs/releasing.md`).
- **The hub runs as the account that owns Fiber home** and is trusted as a
  session is. On its local socket, being that account is the authentication.
  Every connection to its port presents a device token, because any process
  on the machine can reach `127.0.0.1`, a proxy included.
- **Fiber does no TLS and ships no relay service.** The person wraps the port
  the way they choose: `tailscale serve`, which gives it an HTTPS name on the
  tailnet; a reverse proxy; or `ssh -L`.
- **A remote client has exactly the terminal's powers.** It receives the same
  event stream and sends the same driver commands.
- **Hubs never talk to each other.** A client may hold connections to several
  hubs ("Several hubs").

Settled by
[Remote access: what the hub speaks](https://github.com/aakshintala/fiber/issues/86).

### What the hub speaks

The hub speaks the same lines as a session's socket: one JSON object per line,
with the event envelope of `docs/events.md` and the command line of "Driver
commands". Locally the lines travel over `run/hub` (`docs/state.md`). On the
port each line is one websocket text message, and a client holds one
websocket for everything it does.

- **The hub speaks first.** Every connection opens with a `hub_hello` line
  carrying the hub's `schema_version` and Fiber version (`docs/events.md`,
  "The envelope"). A client that cannot
  read that version says which one the hub runs and disconnects, as it does
  for a session ("Processes").
- **A command with a `session_id` is for that session.** The hub passes it to
  the session's socket and passes back what the session sends, starting the
  session first when it has exited, as "Lifecycle" describes. A session never
  knows whether a line came over a websocket. Events already name their
  session, so one connection carries several sessions' streams.
- **A command without one is for the hub.** These are the hub's commands:

| Hub command | `args` | What it does |
|---|---|---|
| `authenticate` | `token` (string) | On the port, the first command: presents a device token ("Remote clients"). |
| `pair` | `code` (string) | On the port, instead of `authenticate`: exchanges a pairing code for a device token, returned in the acknowledgement. |
| `feed` | none | Subscribes the connection to the feed: the latest `session_status` of every running or waiting top-level session, and every change after it, then a `session_left` line (`docs/events.md`) when one ends. Crashed sessions not yet dismissed come first: each one's last `session_status`, then its `session_left`. |
| `dismiss` | `session` (string) | Drops a crashed session from the feed, for every client; its log stays, and it can still be resumed from `recent`. Rejected `stale_request` unless the session is crashed. |
| `recent` | `before` (string, optional), `project` (string, optional) | Answers with a page of exited sessions from `recent.jsonl`, newest first (`docs/state.md`). |
| `start` | `workspace` (string), `model` (string, optional), `content` (optional) | Starts a session in the workspace, any absolute path, and answers with its `session_id`. With `content`, its first prompt. |
| `delete` | `session` (string), `cascade` (boolean, optional) | Deletes an exited session ("Deleting and pruning"). |
| `prompt_history` | `project` (string), `before` (integer, optional) | Answers with a page of the project's prompt history, newest first (`docs/state.md`). |
| `read_file` | `session` (string), `path` (string) | Answers with one file from the session's `artifacts/` ("A session's files"). |
| `status` | none | Answers with what `fiber hub status` prints. |
| `refresh` | none | Rebuilds the hub's environment, as `fiber hub refresh` does. |
| `pairing_code` | `device` (string) | Answers with a new pairing code for that device, as `fiber hub pair` prints. |
| `devices` | none | Answers with the paired devices. |
| `revoke` | `device` (string) | Revokes the device's token, which may be the asking client's own, and closes its live connections. |

Each is answered with one `command_accepted` or `command_rejected`, as a
session's commands are. A client offers the workspaces of recent sessions
first when it asks where to start one; a way to browse directories is left to
clients.

### Remote clients

- **Pairing gives each client its own device token.** `fiber hub pair
  <device>`, run on the hub's machine, prints a short code, and in a terminal
  a QR code with the hub's address; a paired client asks for one with
  `pairing_code`. A client sends the code with `pair`
  within 10 minutes and receives a device token named after the device. The
  code then stops working. A wrong, used or old code is rejected
  `pairing_failed`.
- **A device token is presented, never placed in a URL.** A remote
  connection's first command is `authenticate` or `pair`. Anything else, or
  a token the hub does not hold, is answered `unauthenticated` and the
  connection is closed. A URL ends up in logs and history; a first message
  does not.
- **The hub keeps only a hash of each token,** in `hub/devices/<device>`
  (`docs/state.md`), so a copy of Fiber home holds no usable token.
  `fiber hub token revoke <device>` deletes the record and closes every live
  connection that presented that token.
- **A browser is held to an origin list.** A websocket opened by a web page
  carries the page's origin, and the hub accepts it only when the origin is in
  `hub.allowed_origins` (`docs/configuration.md`). Without the list, any page
  the person visits could reach a hub on their tailnet. A connection with no
  origin, from a program rather than a page, is not affected.
- **A forwarded local socket needs no token.** A person who reaches the hub's
  machine over SSH can forward `run/hub` to their own machine
  (`ssh -L /tmp/fiber-hub.sock:/home/me/.fiber/run/hub host`) and add it as a
  `unix:` address. Only their account can open either end, so being that
  account stays the authentication.
- **Any paired client manages devices, and every change is announced.**
  `pairing_code`, `devices` and `revoke` are answered on the port as on the
  local socket. Limiting them to the hub's machine would stop no one holding a
  token, since a device token is already a shell on the hub's account and a
  session's shell reaches the local socket; it would only stop a person whose
  one device is a phone from adding a second. Detection is the control
  instead. Each pairing and each revocation is written to the hub's
  diagnostic log (`docs/state.md`, "What each part holds") and sent to every
  connected client as a `device_changed` line (`docs/events.md`, "The
  envelope"), so a device paired by someone else is seen.
- **The first device is paired on the hub's machine.** Where the hub is
  deployed, `fiber hub pair` mints the first code: by hand, or from a deploy
  script or a cloud console. The same local door is the way back in after a
  stolen token revokes a person's devices.

### Several hubs

A client keeps its own list of hubs: a name and an address each, in
`hubs."<name>".address` (`docs/configuration.md`), with the device token in
`credentials/hubs/<name>` (`docs/state.md`). `fiber hub add` and
`fiber hub remove` change the list, `hub.default` names the one used without
`--hub`, and with no default a client uses the local hub, starting it when
none is running.

Hubs never talk to each other, and nothing is shared between them. A client
may hold a connection to several hubs at once. A session id is unique only
within its hub, so a client always keeps a session's id together with the hub
it came from. How a client shows several hubs is that client's design.

### A session's files

`read_file` reads one file under the session's `artifacts/`, such as an HTML
page or an image the agent wrote, so a client renders it over the connection
it already authenticated, and nothing is ever public. The answer carries the
file's bytes in base64 and its media type.

- **`artifacts/` is the only root.** The path is resolved, symbolic links
  included, and must stay inside the session's `artifacts/`; otherwise the
  command is rejected `invalid_arguments`. A missing file is `not_found`, and
  a file over 10 MiB is `too_large`.
- **Rendering an agent's HTML safely is the client's job.** The page was
  written by the model, so its scripts deserve the model's trust, no more. A
  web client puts the bytes in a frame with `sandbox="allow-scripts"` and
  without `allow-same-origin`, so the page has an opaque origin and cannot
  read the client's token or storage, and gives it a content security policy
  that blocks network requests (`connect-src 'none'`, no external scripts or
  images). Scripts, such as a chart's, still run.

### Attention

The hub tells every authenticated client when a session needs the person. It
sends an `attention` line (`docs/events.md`, "The envelope") naming the
top-level session, with a `reason` of `waiting` or `finished` and, when it
waits, the one-line summary from `session_status`. It sends one when a
session's `session_status` turns to `waiting`, and when a turn ends and the
session turns `idle`.

Delivering it to a device that is not connected, through Apple's or Google's
push service, Web Push, ntfy or anything else, happens outside the hub: a
client or an extension that holds a connection does it. The hub holds no push
credential.

### A session's environment

Every session the hub starts gets the same environment, built by the hub,
whichever client asked for the session, local or remote. The hub ignores the
environment it inherited: an installed hub inherits the service manager's
bare one, and a hub a terminal started inherits that terminal's.

- **The hub captures the person's login shell when it starts.** It runs
  `$SHELL` as an interactive login shell, from an environment holding only
  `HOME`, `USER` and `SHELL`, and takes the variables that shell ends with.
  `SHELL` is set even under launchd and systemd, so the hub finds the shell
  without being told. The result is every session's environment, so the
  `PATH` the person's startup files build reaches every command.
- **The capture sees what a fresh login terminal sees.** zsh reads all its
  startup files. An interactive login bash reads the first of
  `~/.bash_profile`, `~/.bash_login` and `~/.profile`, and reads `~/.bashrc`
  only when that file sources it.
- **The capture has 10 seconds.** If the shell fails or runs out of time, the
  hub uses the environment it inherited instead. `fiber doctor` says so, and
  every session started from it says so when it starts.
- **A session's environment is fixed when the session starts.** A tool
  installed into a directory already on `PATH` is found without a rebuild.
  A changed `PATH` in a startup file reaches new sessions after
  `fiber hub refresh` rebuilds the hub's environment.
- **A delegate gets its parent's environment**, and a session started by
  `fiber ask` gets its caller's.
- **Fiber reads no per-directory environment**, such as direnv's `.envrc`.
  A tool that reads its own per-directory file, such as Bazel's `.bazelrc`,
  works unchanged, because a command runs in its working directory.
- **The log records the variables without their secrets.** `session_started`
  carries `variables`: the `PATH`, the names of the other variables, never
  their values, and whether the login-shell capture succeeded
  (`docs/events.md`). None of it enters the opening message, so it never
  touches the prompt cache.

## Isolation

Fiber accepts a workspace path, runs in it, and records it on
`session_started`. Fiber creates a worktree when one is asked for: by a
delegate's `isolation: worktree` (`docs/delegates.md`), by the terminal's
"new worktree" switch (`docs/tui.md`, "Home"), or by `fiber ask --worktree`.
All three use the same rules (`docs/delegates.md`, "Worktrees"): a new branch
from the workspace's HEAD in a worktree under
`~/.fiber/projects/<key>/worktrees/<id>`, removed at the end when it holds
nothing uncommitted and no commits beyond its base, kept otherwise, and
`invalid_arguments` outside a git repository. Fiber runs the `git` program; no
git library is linked in. A supervisor that wants its own tree still makes it
and passes the path.

## Deleting and pruning

Nothing in Fiber deletes a session, its artifacts or a kept worktree on its
own, except a session that never got a prompt, which deletes its own
directory as it exits ("Lifecycle"). A person does, with the commands here. An idle Fiber does no work, so
there is no sweep to run, and a session log is the only record of its session
([ADR 0001](adr/0001-session-log-is-the-only-state-of-record.md)), so deleting
one cannot be undone.

**Deleting a session.** `fiber sessions delete <id>`, and the delete key on
the terminal's session list (`docs/tui.md`, "The session list"), delete one
session:

- **It goes through the hub**, as every other client action on a session does,
  so a remote client deletes exactly as the terminal does; the command starts
  a hub when none is running. The hub drops the session's row from its feed.
- **A running session is refused** with `session_held`, as is any session
  whose lock another process holds.
- **A session that other sessions point at is refused** with
  `session_has_dependents`, and the message lists them. A fork and a rewind
  each point at the session they continue (`forked_from` on their
  `session_started`; `docs/delegates.md`, "Forks"; `docs/events.md`,
  "Rewind"). `--cascade` deletes them too, and whatever points at them; it is
  refused if any of them is held. Finding them reads the first line of each
  session log, as listing does.
- **Delete is permanent.** It removes the session's directory: its log and its
  artifacts together. There is no trash. The terminal asks first, naming the
  session and everything `--cascade` adds. On the command line `--yes`
  confirms; without it the command lists what it would delete and asks, and
  with no terminal to ask on it is a usage error.
- **A kept worktree the session made stays.** Pruning removes worktrees.
- **`recent.jsonl` keeps the session's row.** Nothing rewrites that file, and
  readers skip a row whose session directory is gone (`docs/state.md`,
  "What each part holds").

**Exporting a session.** `fiber sessions export <id> [<path>]` copies the
session's `events.jsonl` and `artifacts/` into a directory, `./<id>/` by
default. A path that already exists is refused, naming it; nothing is merged
or overwritten. A running session's export holds the lines written so far. The
export is the log as recorded: text a hook redacted before it was logged is
redacted, and nothing else is (`docs/extensions.md`, "Hooks"). What the person
does with it is theirs. Another format, such as Markdown or HTML, or further
redaction, is an extension command (`docs/extensions.md`, "Commands and
screens"). An extension reads the log as any program can.

**Pruning.** `fiber sessions prune` deletes old sessions and kept worktrees
that hold nothing to lose:

- **Sessions.** With `--older-than <duration>`, such as `30d`, it deletes
  every exited session whose last line is older than that, each by the rules
  for deleting one. A session that a session it is not deleting points at is
  skipped and listed, unless `--cascade` is given. Without `--older-than` it
  deletes no session.
- **Worktrees.** It lists each kept worktree under the project's `worktrees/`
  with its branch, whether it has uncommitted changes, and its age. It
  removes a worktree and its branch only when nothing is uncommitted and every
  commit on the branch is also on another branch or a remote. Any other
  worktree is skipped, and its line names what removing it would lose:
  uncommitted files, commits found nowhere else, or both. `--force` removes
  it anyway. A worktree a running session works in is never removed.
- **Pinned copies.** It removes each copy in `pinned/` of this repository's
  code that no worktree's files match any more (`docs/extensions.md`, "Code a
  repository ships"). The worktrees that count are the repository's
  `git worktree list`. The copy's approval record stays, so the same version
  checked out again is not offered again; its copy is made again from the
  worktree.
- **Diagnostic logs and crash files.** It deletes the old ones, as the hub
  does when it starts (`docs/state.md`, "What each part holds").
- **What it frees.** `--dry-run` prints what prune would delete and the space
  it would free, and deletes nothing. Otherwise prune prints the same list,
  asks as delete does (`--yes` confirms), deletes, and prints the space freed.

## The delegation supervisor is external

Fiber ships no MCP server for delegating to Fiber, and the supervisor that manages several
outstanding delegations to Fiber lives outside Fiber. A session's own
delegates are `docs/delegates.md`. The rationale is
[ADR 0005](adr/0005-the-delegation-supervisor-is-external.md). Fiber is an
MCP client, consuming MCP servers, and that is `docs/mcp.md`.

What Fiber owes the delegation slot instead is being cleanly wrappable, and
that is the whole of it:

- start non-interactively with a prompt, with no tty,
- put no escape codes on stdout,
- announce the session id on the first line,
- emit a documented, versioned event stream,
- exit with a stable code,
- run in a workspace path it is given rather than one it makes.

Everything a supervisor does beyond that — tracking several jobs, waiting on
any or all of them, running a verification gate afterwards, reporting a git
change set — is orchestration that is identical for any agent binary, and a
supervisor that knows only about Fiber is worth less than one that does not.

## Related

- The terminal's own shape: `docs/tui.md`
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
