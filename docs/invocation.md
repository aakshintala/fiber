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

A program that drives sessions, such as a delegation supervisor ("The
delegation supervisor is external"), is a client of the hub too, and does not
use `fiber ask`. It starts a supervised session with the hub's `start`
command, drives it with the commands in "Driver commands", and ends it with
`close` ("Lifecycle"). A supervised session follows the delegate rules
(`docs/permissions.md`, "Supervised sessions").

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
first word names no skill and no MCP server's prompt, the prompt is sent as
written.

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
| `continue` | Opens the most recent session in this project, the launch directory's own outside a git repository, live or exited, in the terminal. With `--hub`, it opens the most recent session on that hub in any project, since a client has no launch directory there. With none, it is a usage error naming `fiber`. |
| `sessions [--all] [--json]` | Lists sessions: id, state, the name or first prompt, what it waits on, and spend. |
| `sessions search [--all] [--json] <text>` | Searches the logs of past and running sessions for the text, as the `session_search` tool does (`docs/tools.md`, "Searching past sessions"). `--all` searches every project. |
| `sessions delete [--cascade] [--yes] <id>` | Deletes a session ("Deleting and pruning"). |
| `sessions export <id> [<path>]` | Writes the session's log and its artifacts to `<path>`, default `./<id>/` ("Deleting and pruning"). |
| `sessions prune [--older-than <duration>] [--cascade] [--force] [--dry-run] [--yes]` | Deletes exited sessions older than the duration and removes kept worktrees that hold nothing to lose, then prints the space freed ("Deleting and pruning"). |
| `models [<search>] [--json]` | Lists the models the installed providers serve, merged with the current workspace's approved repository set: `provider/model`, context window, and price per million tokens in and out, with the configured default marked. `<search>` filters by substring. A model whose per-account setting has no value is omitted from the list; one notice per provider and missing setting on stderr counts the omitted models ("A per-account host" in `docs/model-routing.md`). |

`sessions`, `sessions search` and `sessions prune` use the scope of the terminal's session list. Inside
a git repository that is the repository's project, every worktree of it;
outside one it is every project. With `--hub`, `continue`, `sessions delete`
and `sessions prune` never read this machine's directory or logs: their scope
is every project on that hub. `sessions --all` lists every project.

`models` prints each provider's cached model list at once, and runs a
provider's `models()` only when it has no cached copy. A list older than
`model_lists.refresh_after` refreshes in the background for the next run
(`docs/model-routing.md`, "Model discovery").

`fiber ask` has no `--continue`. Several callers run `fiber ask` at once, so
"the most recent session" is a race.

`-c <key>=<value>` sets one configuration key for one run, the per-run layer
(`docs/configuration.md`, "Layers"). Both doors take it, as they take
`--model`, and it may be given more than once. The terminal passes it to each
session it asks the hub to start, in `start`'s `overrides` ("The hub").

`--credential <label>` on a resume switches the session to another credential
label, and the log records the switch (`docs/model-routing.md`, "Which
credential a session uses"). A new session takes its label from
`providers."<name>".credential`, which `-c` can set for one run.

**Fiber itself.**

| Command | What it does |
|---|---|
| `update` | Updates the Fiber binary and every installed extension together (`docs/releasing.md`, "Updating"). Takes `--channel stable|nightly`, or `FIBER_CHANNEL` when the flag is absent: the flag beats the environment, and an empty setting means stable. `upgrade` runs the same command. A repository's extensions are not updated: each changes only through a new offer. |
| `setup [--provider <name>]... [--model <model>] [--thinking <level>] [--import <claude-code \| codex>]... [--completions <shell> \| --no-completions] [--hub \| --no-hub]` | Asks the questions a new install needs, and applies each answer with the command that already does it (below). `install.sh` runs it last; a person runs it again at any time. |
| `approve [--yes]` | Run in a repository: shows every extension, hook and MCP server the repository declares, as one offer would, and approves them for this person (`docs/extensions.md`, "Code a repository ships"). `--yes` approves without asking, for a script or a machine image. It prints each approval on stderr. |
| `login [<name>] [--as <label>] [--device]` | Stores a provider's key under a credential label (`docs/model-routing.md`, "Logging in"), or a secret an installed extension declares (`docs/configuration.md`, "Secrets"). Without `--as`, a provider's label is the account's email when the login reveals one, otherwise `default`; a label already stored is refused. `--as` applies only to a provider. `--device` applies only to a provider that logs in by browser, and logs in with a device code instead of the browser. A name that is neither an installed provider, nor a provider the current workspace's approved repository set registers, nor a declared secret is a usage error that names the providers and the declared secrets. With no name, a terminal offers the providers, installed and approved repository ones alike, and the declared secrets; without a terminal, it is a usage error. |
| `logout <provider> [--as <label> \| --all]` | Deletes a provider's stored key. With several labels it needs `--as` or `--all`. A key from an environment variable, a file outside Fiber home or a command is named, not removed, and the exit is non-zero. |
| `import <claude-code \| codex>` | Copies another harness's configuration into Fiber home, once (`docs/configuration.md`, "Importing"). |
| `doctor` | Says whether a session can start, and how to fix it when it cannot. |
| `stop [--hub <name>]` | Shuts down every running session, then the hub. It waits for the sockets to close, then prints what it stopped. With nothing running it prints `Nothing running.`. With `--hub`, it stops the sessions and the hub process: a login service on that machine may start the hub again, and the output does not say so. |
| `feedback [<prompt>] [-] [--session <id>]` | Writes a feedback report with the description, version, platform, `fiber doctor` output and diagnostics, and prints the issue link. |
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
`` no key for openrouter: run `fiber login openrouter` ``. A provider whose
data declares `credential_label` (`docs/model-routing.md`, "What a provider
extension declares") has one line, for its selected label, naming what that
label reads, such as `` * work  AWS profile work, read by credential() (not
checked) ``: doctor runs no extension code, so the line never stops a session
from starting, and no file stored under that provider is listed, since it
reads none. It also names each
secret an enabled extension declares that is not stored, with the
`fiber login <name>` that stores it; a missing secret does not stop a
session from starting. It exits non-zero
when a session cannot start. It also prints where the diagnostic logs are, the
newest line at `error` level among them, and the newest crash file, with its
age (`docs/state.md`, "What each part holds"). When `docs/` is missing from
Fiber home, it says `` user guide not installed: run `fiber update` ``
(`docs/releasing.md`). It never touches the network, so it does not check that
a key is valid.

`fiber feedback` takes its description as `fiber ask` takes its prompt
("Getting a prompt in"): the argument is the description, else stdin, and `-`
is accepted only as the last positional.

The report's Diagnostics section (`docs/state.md`, "What each part holds") is for
one session. `--session <id>` names it: any session in Fiber home, from any
project, by its full id, never a prefix. Without the flag it is the newest
session in the current project, as `fiber continue` picks, and a project with
none gets the hub's lines only. An id that is not `s_` and 16 lowercase hex
digits is a usage error, exit 2. An id that names no session, a session
whose log cannot be read, or a delegate fails with `session_not_found`, exit
1, and writes no report. The Diagnostics section is in the saved file and
never in the issue link, in either of its forms.

**Setting up.** `fiber setup` asks, in this order, and each answer runs the
command that already does that job:

1. which providers to sign in to: the installed providers that log in, with
   any that already have a stored credential marked and not selected. Each
   one picked runs that provider's login, as `fiber login <name>` does,
   reviewer pick included (`docs/model-routing.md`, "Logging in").
2. whether to import Claude Code's or Codex's configuration, offered for each
   source whose directory exists and has no completed receipt. Yes runs
   `fiber import` for it (`docs/configuration.md`, "Importing").
3. the default `model` and `thinking`, from the signed-in providers' models,
   with the current global values preselected. The answer is written to the
   global `config.json`; skipping leaves it unset.
4. whether to install completions for the shell `$SHELL` names. Yes appends
   one line that runs `fiber completion <shell>` to that shell's startup
   file (`~/.zshrc`, `~/.bashrc`, or `~/.config/fish/config.fish`), unless
   the line is already there.
5. whether to register the hub as a login service, default no. Yes runs
   `fiber hub install` without `--port`; a hub already registered is named,
   not asked.

Each question has a flag. A flag answers its question and the rest are still
asked. Without a terminal nothing is asked: setup does what the flags say,
skips every question without one, and prints each skipped step with the
command that does it later. It never picks a model nobody chose. Running
`fiber setup` again asks the same questions with the current answers
preselected; a step already done is shown as done, and changing it runs its
command again.

Setup prints no `model_unconfigured` notice. A model missing its per-account
host is left out of the model question. For a provider that logs in,
picking it in the first question runs its login, which asks for the missing
value (`docs/model-routing.md`, "A per-account host"). `fiber models` still
prints each such notice.

**Extensions.** `fiber extension` manages extensions (`docs/extensions.md`,
"Installing").

| Command | What it does |
|---|---|
| `extension install [--project] <name or path>` | Installs an extension and its dependencies. `--project` installs it for the current project only. |
| `extension update [<name>]` | Updates one extension, or every installed extension, to its newest tag (`docs/extensions.md`, "Versions"). It never touches a repository's extension. |
| `extension remove <name>` | Removes an extension, the dependencies nothing else uses, and their data. Run in a project on a repository's extension, it removes that and records never for its content. |
| `extension reinstall <name>` | Removes an extension and installs it again, from its recorded source and commit, or from `<name>` as given when it is damaged (`docs/extensions.md`, "Installing"). |
| `extension list` | Lists installed extensions: name, version and commit; and each repository extension with its project, its path in the repository and the content it loads. |
| `extension test [<path>]` | Runs an extension's session and provider-call cases from its directory or the current one in a temporary Fiber home, and exits non-zero if any case fails. Before any case it checks each provider's data against its Lua `credential()` registration and exits 1 on drift (`docs/testing.md`, "Testing an extension"). |

**MCP servers.** `fiber mcp` manages MCP servers (`docs/mcp.md`).

| Command | What it does |
|---|---|
| `mcp add [--project \| --repo] <name> <url>` | Declares a remote server. |
| `mcp add [--project \| --repo] <name> [-e KEY=value]... [--secret KEY]... -- <command> [args]...` | Declares a stdio server. `--secret KEY` reads the value from a hidden prompt, or stdin without a terminal, stores it at `credentials/mcp.<name>.<KEY>`, and writes `{ "secret": "mcp.<name>.<KEY>" }` as the variable's value. |
| `mcp remove [--project \| --repo] <name>` | Removes a server's declaration. |
| `mcp list` | Lists every declared server, the layer that declares it, and whether a repository's server is approved. |
| `mcp login <server>` | Logs in to a server that needs OAuth. |
| `mcp logout <server>` | Deletes a server's stored token. |
| `mcp serve` | The stdio MCP server a delegate on another harness uses to send session messages (`docs/delegates.md`). |

**Configuration.** `fiber config` reads and writes configuration
(`docs/configuration.md`, "When Fiber writes").

| Command | What it does |
|---|---|
| `config get <key>` | Prints the effective value and the layer it came from. |
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
| `hub status [--json]` | Prints whether the hub is running, its version, its port, the connected clients, the paired devices and whether the login service is installed. |
| `hub pair <device> [--address <url>]` | Prints a pairing code for a device of that name, and in a terminal draws a QR code that carries the pairing code, plus the address given with `--address`. The optional address must be a `ws://` or `wss://` URL; without it, the QR code carries the code alone. Pairing needs no `hub.port` setting. The code works once, within 10 minutes. |
| `hub token list` | Lists paired devices: name, when paired, last connection. |
| `hub token revoke <device>` | Revokes a device's token and closes its live connections. |
| `hub refresh` | Rebuilds the hub's environment for the sessions it starts from now on ("A session's environment"), then prints whether the login-shell capture succeeded. |
| `hub add [--default] <name> <address>` | Adds a hub to this client's list, asks for its pairing code, and stores the device token it gets. The address is `ws://`, `wss://` or `unix:` followed by a socket path. `--default` makes it the hub this client uses without `--hub`. |
| `hub remove <name>` | Removes a hub from this client's list, deletes its device token, and clears `hub.default` when it names that hub. |

Every command that talks to the hub takes `--hub <name>` to use a hub from
the client's list instead of the default.

**Internal commands.** Fiber starts child processes for the session command,
the hub, the case runner that `fiber extension test` starts, and the image
child ("Processes"). The release install step runs from `install.sh`
(`docs/releasing.md`, "Installing"). The file tools start `grep` and `find`
(`docs/tools.md`). These commands are not in the menu. No person or client runs
them.

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

`fiber` with no arguments opens the terminal. Without a tty on stdin and
stdout it is a usage error naming `fiber ask`, and nothing is written to
stdout. The sentence is `` The terminal needs a tty; run `fiber ask "<prompt>"`. Run `fiber --help` for usage. ``

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
hub, as the terminal is ("Two doors").

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
`malformed`. A client line longer than the framing cap (64 MiB) closes the
connection with no answer. `args` with a missing key, a key of the wrong type or a
key the command does not take is rejected `invalid_arguments`, so an older
Fiber says no to a newer client's key instead of ignoring it.

A session remembers the id of every command it accepted for as long as its
process runs, and rejects a second command with the same id
`duplicate_command`. A client that lost its connection resends each session
command it had no answer for, with the same id, so a command that had
already arrived is rejected `duplicate_command` and never applied twice. A
hub command with no answer when the connection dropped is not resent: it
settles as "No answer before the connection was lost."

`content` is content parts (`docs/events.md`, "Content parts"). A client sends
an image part as `type`, `data`, the image's bytes in base64, and `mime_type`,
and nothing else: a remote client cannot write into the session directory.
Fiber processes the image (`docs/model-routing.md`, "Image limits") and writes
the processed file to `artifacts/`, then logs the part with its `path`,
`mime_type`, `width` and `height`. An image that cannot be read, or is over
50 megapixels, is rejected `invalid_arguments`, naming the image.

| Command | `args` |
|---|---|
| `subscribe` | `level` (string), `summary` or `full`; `credential` (string, optional), a credential label of the session model's provider, or of the queued model's provider while a model switch is queued |
| `prompt` | `content` |
| `steer` | `content` |
| `steer_drop` | `command_id` (string), the `steer` command's id |
| `message` | `from_session_id` (string), `parent_session_id` (string, optional), the sender's parent session when it has one, `text` (string) |
| `cancel` | none |
| `reply` | `request_id` (string) and the answer ("Replying") |
| `call` | `call` (string), the call's name as Lua writes it, such as `fiber.tool`; `args` (array), the Lua call's arguments; `request_id` (string, optional), the request answered. Process extensions only, on their own pipe; rejected `invalid_arguments` on an ordinary client connection |
| `ready` | none, ending the extension's registrations. Process extensions only, on their own pipe; rejected `invalid_arguments` on an ordinary client connection |
| `job_stop` | `job_id` (string) |
| `job_input` | `job_id` (string), `text` (string) |
| `offered_file` | `request_id` (string), the pending offer; `path` (string), exactly one of its items' `files` |
| `background` | none |
| `reload` | none |
| `tools` | none |
| `mcp_servers` | none |
| `mcp_login` | `server` (string), the server's name |
| `mcp_reconnect` | `server` (string), the server's name |
| `commands` | none |
| `skills` | none |
| `credentials` | none |
| `quota` | `fetch` (boolean, optional, default false) |
| `history` | `from_seq` (integer, optional), `to_seq` (integer, optional), `session_id` (string, optional), default this session: this session or one its `forked_from` chain passes through, the session whose log `from_seq` and `to_seq` count in |
| `artifact_page` | `path` (string, as a result's `artifact` names it), `offset` (integer, bytes, default 0) |
| `model` | `model` (string), a model reference as a person types one (`docs/model-routing.md`, "Naming a model"); `thinking` (string, optional) |
| `credential` | `label` (string), a credential label of the session model's provider, or of the queued model's provider while a model switch is queued |
| `name` | `text` (string); empty clears the name |
| `handoff` | `instructions` (string, optional) |
| `rewind` | `from_session_id` (string, optional), `seq` (integer, optional), `summarise` (boolean, default false) |
| `shell` | `command` (string); `send` (boolean, default false) |
| `propose_notes` | `topic` (string: `source_control`, `internal_hosts`, `internal_tools`, `routine_actions` or `never`), `answer` (string, optional), `earlier` (array of strings, optional) |
| `command` | `name` (string); `text` (string, optional), what the person typed after the name |
| `close` | `now` (boolean, default false) |
| `import` | `source` (string, optional), `claude-code` or `codex`; absent means both sources |

### What each command does

| Command | What it does |
|---|---|
| `subscribe` | The first command on every connection to a session, and on a connection to the hub the first command for each session. `full` receives the session's whole stream, folded from the log first; `summary` receives only the latest `session_status` and `extensions_loaded` (`docs/events.md`) and reads no log. Only a `full` connection counts in `clients`. Any other command for that session before it is rejected `not_subscribed`. A later `subscribe` at the other level changes the connection's level: raising it to `full` folds the stream from the log as a first `full` subscribe does; lowering it to `summary` ends the connection's count in `clients`, sends every durable line written before the change, then stops the stream and sends the latest `session_status` and `extensions_loaded`. Its acknowledgement is the first line at the new level. At `full`, its `command_accepted` carries `result.last_seq`, the `seq` of the last line the fold sends before the stream goes live; `result` is absent on an empty log. A rejected change leaves the level as it was. A `subscribe` at the level the connection already holds is rejected `invalid_arguments`. Through the hub, a session that resumes is subscribed at the level the connection last held. At `full`, the client then receives `stream_reopened` (`docs/events.md`) before the new fold. Through the hub, a `full` subscribe naming a delegate whose run has finished replays that run's log instead of opening the session: `command_accepted` carries `last_seq`, then the fold follows, with no live tail after it; the kept subscription follows the delegate into its resumed run on a later `delegate_message` (`docs/delegates.md`, "Talking to a delegate"). A log that vanished is refused `session_not_found`; one that cannot be read or serialised `log_corrupt`. A `summary` subscribe, and one whose parent still runs the job, are refused as a delegate that is not running ("Lifecycle"). When the session cannot read its log to serve a connection, a line that does not parse or a failed read, it closes that connection: the client receives every line before the one that failed, then end of file. The session keeps running. Through the hub, the client receives `stream_closed` (`docs/events.md`) where a direct client receives end of file, and its next command for that session is rejected `not_subscribed` until it subscribes again. With `credential`, a subscribe that resumes an exited session starts it on that label, as `--credential` on a resume does (`docs/model-routing.md`, "Which credential a session uses"). On a running session it switches the label as the `credential` command does. A credential switch accepted while a model switch is queued applies to the queued model's provider, else the session model's provider. It applies to that subscribe only. A later subscribe, or a reconnect, carries no label unless it names one. A subscribe on a running session is accepted with one `command_accepted`, carrying only `last_seq` at `full`, even when its requested switch fails; the failure follows as an ephemeral `notice` (`docs/events.md`, "Usage and notices") carrying the switch rejection's code and message, and the session keeps its old label. |
| `prompt` | Starts a turn. Rejected `busy` if a turn is running. Rejected `invalid_arguments` or `mcp_prompt_failed` when its `/name` runs an MCP server's prompt Fiber cannot get (`docs/mcp.md`, "Prompts and resources"). |
| `steer` | Sends a steering message, which joins the running turn at its next step boundary. |
| `steer_drop` | Removes a queued steering message, so nothing is applied. Names the message by the id of the `steer` command that sent it, as `steering_queue` lists it (`docs/events.md`). |
| `message` | Delivers a session message from another session (`docs/tools.md`, "Messaging other sessions"). During a turn it is a steering message; between turns it starts a turn. Rejected `closing` after `close`. |
| `cancel` | Ends the running turn (`docs/architecture.md`, "Cancellation"), and stops a running `shell` command. A `shell` that `cancel` stops before its command starts is rejected `cancelled`. Rejected `stale_request` if no turn is running and no `shell` command is. |
| `reply` | Answers an interaction the loop raised: approval, confirm, select, multi-select, text input or form ("Replying"). |
| `call` | Sends one process-extension call: a registration, a `host` call or a `state` call, answered with `command_accepted` carrying its return as `result.value`, or `command_rejected` with the `code` and `message` a Lua call raises (`docs/extensions.md`, "Process extensions"). Process extensions only. |
| `ready` | Ends a process extension's registrations; the startup deadline is met when it arrives (`docs/extensions.md`, "Process extensions"). Process extensions only. |
| `job_stop` | Stops a running job by `job_id`, at once, even while a model response streams (`docs/architecture.md`, "One inbox"). Rejected `stale_request` if the job is not running. |
| `job_input` | Writes `text` to a running job started with `tty`, as the `jobs` action `write` does for the model (`docs/tools.md`, "Background jobs"), and is accepted once written. The job's output reaches the client on its stream as usual. Rejected `stale_request` if the job is not running, and `invalid_arguments` if it was not started with `tty`. |
| `background` | Moves every shell call running in the current turn to the background (`docs/tools.md`, "Shell"). Rejected `stale_request` if none is running. |
| `reload` | Re-reads configuration, restarts changed MCP servers and extensions, and declares the tool set again (`docs/mcp.md`, "Reload"). Rejected `busy` if a turn is running; rejected with the start's code when the reload fails as a start would, changing nothing (`docs/configuration.md`, "When Fiber reads configuration"). |
| `history` | Answers, in its `command_accepted`, with the session's durable lines from `from_seq` to `to_seq` inclusive, or to the latest when `to_seq` is absent, at most 256 lines. With no `from_seq`, it answers with the last lines up to `to_seq`, or up to the latest, at most 256; on an empty log, with none. With `session_id` naming a session this session's `forked_from` chain passes through, the set `rewind`'s `from_session_id` accepts, `from_seq` and `to_seq` count in that session's log, and the answer stops at the point the chain continues from: its lines up to the `seq` the next session on the chain names in `forked_from`, the lines the model receives (`docs/events.md`, "Rewind"). The session answers from its own read of the chain; the hub resumes no ancestor. A client pages back past this session's first line by reading `forked_from` from its `session_started`, asking `history` with that `session_id` and `to_seq` set to that `seq`, and doing the same for each further ancestor. A client pages for more. This is how every client pages history, the local terminal included: no client reads a session's log from disk (`docs/tui.md`, "History and paging"). Rejected `invalid_arguments` when `from_seq` is past the latest line, or past the point for an ancestor, and when `session_id` names any other session. |
| `artifact_page` | Answers with one page of a file under the session's `artifacts/`, starting at `offset`: at most 64 lines, the page size of history paging (`docs/tui.md`, "History and paging"), and at most 64 KiB, a longer line cut at a character boundary and continued on the next page. A client pages with `next`, so a file is never loaded whole; the terminal's call view reads a cut call's full output this way (`docs/tui.md`, "Swapped views"). The path is resolved as `read_file` resolves it ("A session's files"): outside `artifacts/` it is rejected `invalid_arguments`, and a missing file is `not_found`. An `offset` past the end of the file is rejected `invalid_arguments`. `size` is the file's size when the page is read: a file that grows is paged to that size, and one that shrinks during the read is rejected `io_failed`. `offset` and `next` count bytes; bytes that are not valid UTF-8 become `�` in `text`, and an `offset` inside a character is rejected `invalid_arguments`. |
| `offered_file` | Reads one file listed by the pending repository offer and answers with its full UTF-8 `text`, with no size cap. The connection's reader answers it at once, even while a turn streams. Rejected `stale_request` when the offer is no longer pending or the session has no repository code; `closing` when its log is gone; `invalid_arguments` when the path is not exactly listed, resolves outside the workspace, is not a regular file, or is not UTF-8 text; and `io_failed` when it cannot be opened or read. A filesystem refusal names the path. |
| `tools` | Answers with every declared tool: its source, whether it is full, deferred or loaded, and its approximate size (`docs/tools.md`, "Seeing the tools"). |
| `mcp_servers` | Answers with every configured MCP server: its name, the layer that declares it (`global`, `project` or `repository`), its state (`connecting`, `ready`, `failed` or `needs-login`), the failure's sentence when it failed, its tool count, and for a repository's server whether it is approved (`docs/mcp.md`, "Servers, their state and the per-server commands"). The terminal's `/mcp` view reads it (`docs/tui.md`, "Swapped views"). |
| `mcp_login` | Runs for one MCP server the login `fiber mcp login` runs (`docs/mcp.md`, "What Fiber does with MCP"): it opens the browser and waits for the loopback callback, then stores the token and starts the server. Accepted at once and answered when the login ends. Rejected `invalid_arguments` for a server nothing declares, `mcp_server_unapproved` for a repository's server nobody approved, `busy` when a login for that server is already in flight, and `authentication_failed` when the login fails, storing nothing. |
| `mcp_reconnect` | Restarts one MCP server and answers with its outcome (`docs/mcp.md`, "Servers, their state and the per-server commands"). Restarts only the server it names. Rejected `busy` while a turn runs, `invalid_arguments` for a server nothing declares, and `mcp_server_unapproved` for a repository's server nobody approved. |
| `skills` | Answers with every skill discovery found (`docs/system-prompt.md`, "Skills"), including those `skills.disabled` switches off and those another skill shadows, which `commands` leaves out: in discovery order, each skill followed by the skills it shadows. Each entry is the skill's name, its description, the path of its `SKILL.md`, its source and, for an extension's skill, the extension's name, whether its header and place let the model load it, whether `skills.disabled` switches it off, the paths of the skills it shadows, and, on a shadowed skill, the path of the skill that wins. The model can load a skill only when its header and place allow it, it is not switched off and no skill shadows it. A skill whose header is invalid is left out, as discovery leaves it out. The list is fixed when the session starts, again when an offer's approval loads extensions with no `reloaded` (`docs/events.md`, "`extensions_loaded`"), and again when it reloads. The terminal's `/skills` view reads it (`docs/tui.md`, "Swapped views"). |
| `quota` | Without `fetch`, answers at once with every provider credential label's cached quota and `quota.notice_at`. The first command starts loading quota providers off the request path; later commands read the same caches (`docs/tools.md`, "Fetching"). With `fetch: true`, starts a fetch for every label whose key read succeeded, whatever the cache's age, and answers once every fetch it started has ended. Each fetch keeps the 8 s timeout, so the answer comes within 8 s of the fetches starting, plus loading. A label whose fetch failed or timed out answers `unreachable`. A fetch already in flight for a label is shared (`docs/tools.md`, "Fetching"). |
| `credentials` | Answers at once with the provider a `credential` command would switch now, the queued model's when a model switch is queued, else the session model's; the credential labels it has, as a `credential_missing` names them; and the label it uses now (`docs/model-routing.md`, "Which credential a session uses"). A scripted session answers with no labels and no label. The terminal's credential picker reads it. |
| `commands` | Answers with every `/name` the session runs: each skill and prompt template that discovery keeps and `skills.disabled` does not switch off (`docs/system-prompt.md`, "Skills"), each MCP server's prompt (`docs/mcp.md`, "Prompts and resources"), and each extension's command (`docs/extensions.md`, "Commands and screens"), including a lazy extension's declared commands, which the answer lists from its manifest without starting its VM. Each entry is a name, a one-line description, the skill's `argument-hint`, or an MCP prompt's arguments as `<name>` when required and `[name]` when not, and a tag: `skill`, `template`, the extension's name, or the MCP server's name, and, on an extension's command, `extension` set to true. The list is fixed when the session starts, again when an offer's approval loads extensions with no `reloaded` (`docs/events.md`, "`extensions_loaded`"), again when it reloads, and again at each turn start that changes it. A client's `/` list reads it (`docs/tui.md`, "Slash commands"). |
| `model` | Switches model or thinking level at the next turn boundary. Takes a model reference and an optional thinking level. The switch rebuilds the prompt cache, and the terminal says so with the rebuild's size first (`docs/prompt-cache.md`, "Switching model"). Rejected `invalid_arguments` for an unknown model, and with the credential's own code, such as `credential_missing`, when the new model's credential cannot be read (`docs/model-routing.md`, "When a credential is missing or fails"). |
| `credential` | Switches the session's credential label at the next turn boundary (`docs/model-routing.md`, "Which credential a session uses"). Accepted while a model switch is queued, it applies to the queued model's provider. The switch rebuilds the prompt cache, as a model switch does. It changes this session only; the terminal's `/credential` also saves the label. Its `command_accepted` names `provider`, the provider the switch applies to, which the terminal saves the label under. Rejected `credential_missing` for a label the provider does not have, naming its labels, and with the credential's own code when the label's source cannot be read. |
| `name` | Sets the session's name, which pins it against the model's `name_session`. Takes the text; empty text clears the person's name and unpins it. Written as `session_named`. |
| `handoff` | Starts a handoff: the model's context restarts from a note the model, or an extension's `before_handoff` hook, writes (`docs/handoff.md`). Takes optional instructions saying what the next stretch of work focuses on. During a turn it applies at the next step boundary, as a steering message does; between turns it is a turn of its own whose input is the command. |
| `rewind` | Starts a new session process that continues a session from an earlier point (`docs/events.md`, "Rewind"), and answers with the new session's id, or the code the new session's start failed with, when its process could not start. Takes an optional `from_session_id`, default this session: this session or one its `forked_from` chain passes through, the session whose log `seq` counts in, and any other is rejected `invalid_arguments`, since it is rewound through the hub; an optional `seq`, default the start of the latest turn that is not a summary's (`docs/events.md`, `turn_started`, `rewind`); and whether to summarise, which first runs the old session's model in a turn of its own (`docs/events.md`, "Rewind"). Every job still running stops (`docs/events.md`, "Rewind"). Rejected `busy` if a turn is running, `not_step_boundary` if `seq` is not a step boundary, `session_held` if another process holds the session, `delegate_session` if it is a delegate, and `summary_failed` if it asked for a summary that could not be made. |
| `shell` | Runs a shell command the person typed, as `!` does in the terminal. Takes the command and `send`, default false. Answered when the command ends. Accepted during a turn. |
| `propose_notes` | Proposes reviewer-note lines for one topic on the session's model, in a request of its own with no conversation, from the evidence `docs/permissions.md` "Setting up the notes" lists, with user names and passwords removed from every URL; answers `evidence`, `proposed` and `error`; when it sends the request, writes one `usage_recorded` with no turn and no action, and no configuration; rejected `busy` while a turn runs or an approval waits, and `closing` after `close`. |
| `command` | Runs an extension's command by name, with the text after it as arguments, as a person typing `/name args` does (`docs/extensions.md`, "Commands and screens"). Rejected `unknown_command` for a name no extension registers or declares. A first-run start that fails rejects the command `extension_failed` (`docs/extensions.md`, "Commands and screens"). |
| `close` | Accept no more prompts; finish the turn in flight, then any running jobs (`docs/tools.md`, "Background jobs"), and exit. With `now`, it starts a shutdown instead ("Shutdown"): the turn in flight ends, every job and delegate stops, and the session exits 0. A `close` with `now` is accepted also after `close`. |
| `import` | Runs the same one-time import `fiber import` runs (`docs/configuration.md`, "Importing"), and reports the same imported and left-behind lists. |

Rejection codes: `malformed`, `invalid_arguments`, `unknown_command`,
`not_subscribed`, `not_found`, `busy`, `stale_request`, `not_step_boundary`,
`session_held`, `delegate_session`, `summary_failed`, `closing`, `indeterminate`,
`duplicate_command`, `session_not_found`, `message_refused`, `hook_failed`,
`io_failed`, `cancelled`, `mcp_prompt_failed`, `authentication_failed`, `config_invalid`, `connection_failed`, `credential_failed`, `credential_missing`, `extension_failed`, `extension_incompatible`, `extension_missing`, `extension_not_found`, `extension_not_installed`, `extension_required_failed`, `extension_unapproved`, `fetch_failed`, `hook_unapproved`, `log_corrupt`, `mcp_required_server_failed`, `mcp_server_unapproved`, `model_ambiguous`, `model_unconfigured`, `no_model`, `session_has_dependents`, `usage`, `version_conflict`, `from_session`.

A prompt or steer that a hook refuses is rejected `message_refused`, with the
hook's reason and extension in the message. One whose blocking hook failed is
rejected `hook_failed`. A `fiber ask` whose first prompt is rejected exits 1,
with the rejection as `fiber_exited`'s `error`.

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

**`job_input` types `text` into a running job started with `tty`.** It is
rejected `stale_request` if the job is not running, and `invalid_arguments`
if the job was not started with `tty`. A write the terminal refuses is
rejected `io_failed`.

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
- With `send` true, the loop writes the output as `shell_command`
  (`docs/events.md`) when it takes the command, after it ends, and the
  command's `command_accepted` carries no result. It joins the next turn's
  input, never the running turn. The terminal sends it for `!command`, and
  sends `!!command` with `send` false.

Settled by
[#148](https://github.com/aakshintala/fiber/issues/148).

**`rewind` starts a new session process.** The session being rewound closes
with `rewound` while it still holds its lock, and the hub starts the new one,
whose `session_started` names, in `forked_from`, the old session or the ancestor whose log the point is in. Each client
connected to the old session is sent the new session's id and subscribes to
it. When the new session's start is refused, the rewind is rejected with the start's code, and no client is moved to the new id. The hub starts it again when the old session's process exits, as it does for a rewind no client relays. Until a start succeeds, a command for that id is rejected `session_not_found` while the new session has no log; once it has one, the command resumes it as for any session ("Lifecycle"). For a session no process holds, the hub starts a session process for it, and
that process rewinds it.

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
answer keys, a typed answer that does not fit its MCP elicitation field
(`docs/mcp.md`, "Elicitation, sampling and roots"), `feedback` with `allow`,
`remember` on a request with no `rule`
or with a prefix the request did not offer, or `decisions` whose length is
not the number of items offered. A global standing rule is added by
editing the global rules file, never from an approval.

**A process extension answers Fiber's requests with `reply`.** It carries
`request_id` and exactly one of `result` (any JSON value, what the Lua
function returns) or `error` (`{ code, message }`, which fails the call as a
Lua error does). Its `command_accepted` names the reply command's own id, as
usual, and completes the named pending request (`docs/extensions.md`,
"Process extensions").

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
`--model` and `-c` on `fiber ask --resume` take effect when the resume starts
the session's process. A session still running keeps the configuration it
started with, and they are ignored.

**A session exits when it has been idle for `session.idle_exit_ms`**, 30 minutes
by default (`docs/configuration.md`), whoever is connected. Idle means no turn
running, no jobs running and no cache warming and no session message waiting
to join a turn (`docs/prompt-cache.md`,
"Warming while idle"). Waiting on an approval or a question is idle
unless a session message is waiting to join the turn,
because nothing is in flight. A question `ask_user` raised counts as idle
only once every other call of its step has its result; until then the
session waits for the answer. Connected clients do not keep a session alive: a
phone or a terminal left open is connected all the time. Leaving never
cancels. When the delay passes, Fiber exits.

**A session left unattended with jobs running checks them once.** Unattended
means no prompt or steer from a person or a driver for
`session.idle_exit_ms`, jobs or no jobs. A session message is neither. A running job keeps a session from being idle, so the idle delay
never ends it; instead, when it has been unattended that long with jobs
running, Fiber wakes the model once with the jobs check: a notice listing
the running jobs and telling it to stop any that look hung or that it no
longer needs, judging from each job's output file. The session does not end.
The check fires once and is armed again only by the next prompt or steer, so a job
the model keeps does not cost a turn every delay.
A job an extension started with `host.delegate` holds no session open and is
not in the jobs check: it does not make a session busy, and a session that
ends normally stops it.

**A delegate exits as soon as its run finishes**: its final answer is written
and its own jobs are done. It does not wait for `session.idle_exit_ms`. A later
`delegate_message` resumes it (`docs/delegates.md`).
The jobs in this rule do not include a job an extension started with
`host.delegate`: it holds no session open and is not in the jobs check.

**A `fiber ask` session exits when its turn ends** and its jobs are done.
The jobs in this rule do not include a job an extension started with
`host.delegate`: it holds no session open and is not in the jobs check.

**A prompt sent to an exited session through the hub resumes it**, then
delivers the prompt. A client showing a session that exits keeps showing it,
because its conversation is its log.

**A delegate is never resumed through the hub.** It resumes only through its
parent (`docs/delegates.md`, "Talking to a delegate"). A command for a delegate
that is not running is refused `session_not_found`, "A delegate resumes only
through its parent." A delegate whose log ends in `fiber_exited` is refused at
once, without the shutdown wait below — except a `full` `subscribe`, which replays the finished run's log instead: `command_accepted` carries `last_seq`, then the fold follows, with no live tail after it. A log that vanished is refused `session_not_found`; one that cannot be read or serialised `log_corrupt`. A `summary` subscribe, and a `full` one whose parent still runs the job, are refused as a delegate that is not running. A running delegate is reached as any session.

A session whose log ends in `fiber_exited` has exited, even while its process
is still shutting down. The hub does not pass on a `closing` answer from it: it
resumes the session and delivers the command. If that resume fails
`session_held`, the hub waits for the old process to release the lock, up to
the 5-second shutdown bound, and the client gets `session_held` only once the
bound has passed. Commands keep their order across that resume ("What the hub
speaks"). A command after `close` and before `fiber_exited` still gets
`closing`, except a `close` with `now`, which is accepted and upgrades the close. A line that arrives after the session stops reading, just before `fiber_exited`, is never read: the hub re-routes it after the clean exit, and a direct client reads end of file.

A command the hub sent to a session that then ended without answering it is
answered by the hub, by how the session ended:

- After a clean exit (the log ends in `fiber_exited`) the session never read
  the command, so the hub treats it as a `closing` answer and delivers it to
  the resumed session, in the order the hub read it.
- A command is delivered once. When the resumed session also ends without
  answering it, or answers it `closing`, the hub rejects it `closing`.
- A client that disconnected has its commands dropped, never delivered.
- After a crash (the log ends in neither `fiber_exited` nor `rewound`) the
  session may have run the command. The hub never delivers it again: it
  rejects it `indeterminate`, and the client decides whether to send it again.

**A resume whose workspace is gone and cannot be made again is refused
`io_failed`.** For a resume that needs a new session process, the hub checks
the workspace before starting one. A missing workspace is unrecoverable when
the session recorded no Fiber `worktree`, so the workspace is a plain
directory, or when the session recorded a Fiber `worktree` but its repository
is gone too. When it is unrecoverable, the hub rejects the resume with
`io_failed`, and the message names the missing path, the workspace or the
repository: "<path> no longer exists". A recorded Fiber worktree whose
repository still exists is made again, as "Isolation" says. A live session
is attached as before, without the check.

**A session that never got a prompt leaves nothing behind.** A session that
exits with no `turn_started` and no `repository_code_offered` in its log
deletes its own directory, unless its `session_started` has `forked_from`
and its process started: a rewind's new session is kept, because the old
session's `rewound` points at it.

**`close` ends the session whoever else is attached.** It accepts no more
prompts, finishes the turn in flight, then any running jobs (`docs/tools.md`,
"Background jobs"), and exits. With `now`, it starts a shutdown instead
("Shutdown"), and is accepted also after `close`.

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

**A pending approval, question or repository offer does not keep a session
alive past the idle delay** (an offer: `docs/events.md`, "Repository code").
A session message waiting to join the turn does keep it alive: the session is
not idle while one waits. An approval or question wait then ends only on its
answer, a cancel or `close`; a repository offer's wait, which comes before
the turn starts and so cannot be cancelled, ends only on its answer or
`close`. Each of these runs the message.
When the delay passes with any of them pending, the session exits, and
`fiber_exited` names the request it stopped on, the approval or question when
an offer is pending too. Resuming the session raises
the request again and the turn goes on from there. `fiber ask --resume` on such
a session has no one to answer, so it refuses the request, lets the model finish
the turn, and then runs its prompt as the next turn; the prompt waits behind that
turn. The hub lists such a session as waiting on the person, from `recent.jsonl` (`docs/state.md`).
Three cases have no one to wait for, and there escalation is a block as
`docs/permissions.md` ("Headless") describes: a session started by
`fiber ask`, a supervised session, and a session that has been sent `close`.
In all three, and in every delegate, an `ask_user` question ends the turn
instead, and the driver resumes or prompts the session with the answers
(`docs/tools.md`, "Asking the person").

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

- **Every session is one process**, running a command that hosts a session in
  that process: the internal `session` command, in-process `ask`, or the case
  runner's `extension-case` child. The hub starts one for a client; a parent
  starts one for each Fiber delegate. Each session starts its own MCP servers
  and process extensions, so nothing is shared between sessions
  (`docs/mcp.md`, `docs/extensions.md`).
- **No session has a pipe driver.** Every driver is a client over the
  session's socket. `fiber ask`'s stdout is a watcher.
- **A Fiber delegate is a child process of its parent.** The parent is an
  ordinary client over the delegate's socket. It also holds a pipe to the
  delegate's stdin that it never writes to: end of file on it means the parent
  is gone, and the delegate shuts down ("Shutdown"). A dropped socket
  connection is only a client leaving; the parent reconnects. Delegates on
  other harnesses are child processes too (`docs/delegates.md`).
- **An image child is a short-lived process, one per image or PDF.** The session
  starts it by running `fiber` again with an internal command, not a door,
  so the child always matches its parent's version. The session process
  runs no image code; the child reads the header too
  (`docs/model-routing.md`, "Image limits"). The same child, run with `pdf`,
  counts a PDF's pages and cuts a page range (`docs/tools.md`, "read").
  Spawning a child that does
  nothing takes 1.7 ms on macOS, measured with the probe in
  `research/image-limits/README.md`.
- **The terminal is its own process, a client of the hub.** It has a `full`
  connection to the session on screen and a `summary` connection to each
  session it has opened, stopped or closed, through the hub. It shows every
  other live session from the hub's feed. It draws what arrives and has no path to state the stream
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
  the client says which version the session runs and declines with
  `session_held`, so the person can close it or let it exit.

The socket is the only path into a running session. The terminal, a parent
session, a GUI and a phone through the hub are the same kind of client, as map
premise 5 requires.

## Shutdown

Settled by
[Shutdown: what SIGTERM has to guarantee](https://github.com/aakshintala/fiber/issues/34);
that ticket's resolution holds the rationale and the rejected alternatives.
What reference agents do, and what a crash leaves behind, are
`research/shutdown/`.

A shutdown is Fiber stopping because a signal, or a `close` with `now`,
told it to. It is bounded, it
stops everything the session started, and it asks nobody anything. Exiting
because the work ran out ("Lifecycle") and `close` without `now` are not shutdowns: both
wait for jobs without a cap. A supervisor that wants the work finished sends
`close` without `now`; when its patience runs out, it sends `close` with `now`,
or SIGTERM.

**Three signals, one path.** SIGTERM, SIGINT and SIGHUP each start a
shutdown. They differ only in the exit code: 143, 130 and 129. A second
SIGTERM or SIGINT during a shutdown skips the grace period below: every
process group still alive gets SIGKILL at once, and Fiber writes what it
knows and exits.

A `close` with `now` takes the same path with exit code 0: the door answers
it, then starts the shutdown a first signal would. A shutdown that already
started keeps its code: a `close` with `now` after a signal changes nothing,
and a repeat does nothing. A SIGTERM or SIGINT after it is a second signal.

**What happens, all at once:**

- The model request's socket is closed.
- Every running tool call's process group, every job's and every delegate's
  gets SIGTERM. A group still alive 800 ms later gets SIGKILL, and Fiber
  reads its output for at most 2 s more: the sequence in `docs/tools.md`
  ("Stopping a command"). Groups are signalled together, never one after
  another. Six groups that ignore SIGTERM took 0.81 s in parallel and 4.85 s
  in sequence (macOS arm64, `research/shutdown/probe5_nested_kill_cost.py`).
- Every extension callback in flight ends, and every `host.exec` process
  group it started gets SIGTERM with the rest: a group still alive 800 ms
  later gets SIGKILL, as a tool call's does.
- A Fiber delegate is a session process, and SIGTERM starts its own shutdown.
  Its parent waits for it to exit, up to the bound, rather than sending
  SIGKILL at 800 ms, so the delegate stops its own commands and writes its
  own `fiber_exited`. Depth is capped at 2 (`docs/delegates.md`) and every
  process forwards the signal before doing anything else, so every level of
  a tree counts down from almost the same moment.
- Each MCP call in flight gets `notifications/cancelled` and ends `failed`
  with code `mcp_cancel_requested`, as on a cancelled turn (`docs/mcp.md`,
  "Calls"); a pending elicitation goes with its call. Then each stdio
  server's stdin is closed. Each server runs in its own process session and
  process group, and its process group gets SIGTERM, then SIGKILL 800 ms
  later.
- A credential command a `model` or `credential` switch is reading gets SIGKILL, and the
  switch is rejected `closing`. On a foreground entry path (`fiber models`,
  `fiber refresh-model-lists`, the terminal picker) it gets SIGKILL again until its
  group is empty, within the shutdown bound: the entry point exits once every such
  group is empty, or at the bound.
- No model request is made, no ending notice is given, and no hook runs.
  Because no `after_tool` hook runs to redact it, a call cancelled by shutdown
  completes with no content and no artifact.

**What is written.** The turn in flight ends as the cancel key ends it
(`docs/architecture.md`, "Cancellation"): each tool call completes
`cancelled` once its group is empty, each job `cancelled`, a handoff in
flight `cancelled`, and the turn `turn_completed { outcome: interrupted }`. Two
things differ from a cancel: a pending approval, question or repository offer stays pending, so
resuming raises it again (below), and queued steering messages start no turn. They
were never logged, so they are gone. A question `ask_user` raised stays
pending only when no later call of its step is still without a result;
otherwise it ends as on a cancel, with `interaction_resolved` by `fiber` and
`declined: true`, so the later calls' results are written in order. An extension's pending `host.ask` is the
exception: its asker lives in a Lua VM that does not survive the exit, so
`fiber_exited` follows an `interaction_resolved` with `declined: true` and `by`
`fiber` for each of them, and resuming raises nothing. A request already answered when the signal
arrives was not pending: its answer stands, and a call it allowed completes
`cancelled` without running.
Nothing is written for a call before it has stopped. Each `host.exec` run that an ended callback started writes its `extension_exec` once its group is empty, before `fiber_exited`. Then `fiber_exited`
with the exit code and no final message, the socket is unlinked, the lock is
released, and the process exits.

A session waiting on an approval, a question or a repository offer when the signal arrives has nothing running
(`docs/architecture.md`: "Permission decisions are made in order, before any
of them runs"). It stops its jobs and exits with `suspended_on` naming the
request, as it does when the idle delay passes ("Lifecycle"), and resuming
raises the request again.

A signal that arrives before Fiber starts writing `fiber_started` exits with the code
and writes no durable event; one that arrives while it is being written takes effect once it is written, as the started process's shutdown. An ephemeral `clients` line already sent to a
client that attached stays sent. A signal that arrives before Fiber starts writing `fiber_started` also writes one diagnostic line (`docs/state.md`, "What each part holds").
Once `fiber_started` is written,
`fiber_exited` always is, unless the process dies or reaches its kill point.

**The bound is 5 seconds** from the signal to exit, per process. The
command stage is at most 2.8 s (800 ms grace plus 2 s drain), the levels of
a tree run concurrently, and the rest is margin. It sits under the
supervisors Fiber runs under: Docker sends SIGKILL 10 s after SIGTERM,
Kubernetes 30 s, systemd 90 s (each one's documented default, not measured).
At 4.9 s, every group still alive gets SIGKILL, one diagnostic line with code `timeout` goes to the process's own diagnostic log (`docs/state.md`, "What each part holds"), with at most 100 ms allowed for it, and the process exits with the same code, writing
nothing more: the loop may be stuck holding the log, so no other thread
writes to it. The log ends as a process that died leaves it, with no
`fiber_exited`, and a resume reads it as it reads any such log: a call left
open has an unknown outcome and is never re-run, and a job left open
completes `orphaned` (`docs/events.md`, "Resume"). A child started in the
instant before the bound fires may outlive Fiber.

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
same id, each session command it had no answer for; a session that already
accepted one rejects the copy `duplicate_command` ("The command line"). A
hub command with no answer is not resent: in the terminal it settles as "No
answer before the connection was lost." and its text returns to the draft.

## The hub

Every client reaches sessions through the hub, the local terminal included.

- **The hub holds no session.** It lists sessions, starts and resumes them
  (never a delegate, which resumes only through its parent:
  `docs/delegates.md`), and relays every client connection to a session's socket. A session is
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
  first (`docs/state.md`). Delegates are never in the feed or in that query;
  a client shows a delegate when the person opens its parent. A `dismiss`
  sends no line to clients already subscribed: a client showing the session
  drops it at its next subscription.
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
  The service is named after Fiber home's path, at
  `~/Library/LaunchAgents/<name>.plist` on macOS and
  `~/.config/systemd/user/<name>.service` on Linux, so each Fiber home has
  its own.
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
  carrying the hub's `schema_version`, its Fiber version and, when the build
  knows it, its commit (`docs/events.md`, "The envelope"). A client that cannot
  read that version says which one the hub runs and disconnects, as it does
  for a session ("Processes"). The terminal also compares the hub's Fiber
  version and commit with its own: when the versions differ, or both carry a
  commit and the commits differ, it shows one notice per connection naming
  both, each as `fiber --version` writes it, and saying how to stop the hub:
  `fiber stop` restarts it on the next launch for a hub on this machine, and
  `fiber stop --hub <name>` stops a listed hub (`docs/tui.md`, "Notices"). It stays connected when it can read the hub's
  `schema_version`.
- **A command with a `session_id` is for that session.** The hub passes it to
  the session's socket and passes back what the session sends, starting the
  session first when it has exited, as "Lifecycle" describes. A session never
  knows whether a line came over a websocket. Events already name their
  session, so one connection carries several sessions' streams.
- **Commands keep their order.** The hub passes one connection's commands to
  a session in the order it read them, including commands it re-routes to a
  resumed session after a `closing` answer, and the session's
  acknowledgements come back in that order. A command the session answers
  when it ends, such as `shell`, is acknowledged then, so the order holds
  for acknowledgements as the session sends them. The re-route rests on a
  guarantee on the session's side: every line a session reads gets exactly
  one answer on its connection before the session shuts it. The answer is
  the command's own, or `indeterminate` when the close grace cut it. A session
  stops reading as it ends, so a line it never read gets no answer from it. The
  close waits up to 2 seconds for the commands it is running; one still
  running then is answered `indeterminate` and never replayed, and a client
  that stops reading is cut. The grace bounds the whole close: the session stops
  waiting for a command still running at the grace, which ends when the
  process exits, and writes one `timeout` line to its diagnostic log
  (`docs/state.md`, "What each part holds") for each connection it stopped
  waiting for. The hub's relay
  threads read continuously, so the hub never meets that grace.
- **A session that closes a relayed connection ends that subscription.** When
  a running session closes the connection the hub relays for a client, the hub
  sends that client `stream_closed` (`docs/events.md`) after every line the
  session sent on it, then rejects each command it sent that the session left
  unanswered `indeterminate`, and keeps no subscription for that session: the client's
  next command for it is rejected `not_subscribed` until it subscribes again.
  The connection's other sessions are untouched. The hub reads end of file on
  a relayed connection as the session closing it while running only when the
  relay never saw the session exiting, the session's log ends in neither
  `fiber_exited` nor `rewound`, and `run/<session_id>` still accepts a
  connection. A clean exit removes the socket before it shuts its
  connections, and a crash leaves a socket that refuses, so either one is an
  exit and sends no `stream_closed`. A session that crashes and is resumed by
  another client before the hub checks its socket can give the client
  `stream_closed` for the session that ended; the client subscribes again and
  no line repeats.
- **A command without one is for the hub.** These are the hub's commands:

| Hub command | `args` | What it does |
|---|---|---|
| `authenticate` | `token` (string) | On the port, the first command: presents a device token ("Remote clients"). |
| `pair` | `code` (string) | On the port, instead of `authenticate`: exchanges a pairing code for a device token, returned in the acknowledgement. |
| `feed` | none | Subscribes the connection to the feed: the latest `session_status` of every running or waiting top-level session, and every change after it, then a `session_left` line (`docs/events.md`) when one ends. Crashed sessions not yet dismissed come first: each one's last `session_status`, then its `session_left`. When 1,024 feed lines are queued for a connection that has not read them, the hub closes the connection; the client reconnects and sends `feed` again, which resyncs it. |
| `dismiss` | `session` (string) | Drops a crashed session from the feed, for every client; its log stays, and it can still be resumed from `recent`. Rejected `stale_request` unless the session is crashed. |
| `recent` | `before` (string, optional), `project` (string, optional) | Answers with a page of exited sessions from `recent.jsonl`, newest first, skipping delegates, whose row's `status` carries `parent` (`docs/state.md`), and `total`, the number of rows the pages list for `project` (every project when absent), whatever `before` and the page hold. `project` is the project's key, the name of its `projects/<key>/` directory. |
| `sessions` | `project` (string, optional), `active` (boolean, optional) | Answers once with the latest `session_status` of every running top-level session and the `recent.jsonl` row of every exited one, newest first, delegates skipped, each session once. With `active` true, each entry carries `active`: the `ts` of the session log's last line, its opening `ts` when the last line has none, or `null` when neither can be read. Without it, no session log is opened. `project` keeps only that project's sessions, as for `recent`. A hub that has just started answers once it has read `run/` and each session it found there has sent its `session_status` or closed; a session that stays silent holds it for at most one rescan past the read. A session started since the hub last read `run/` can be missing, as it is from the feed. |
| `start` | `workspace` (string), `model` (string, optional), `overrides` (array of strings, optional), `worktree` (boolean, optional), `continues` (string, optional), `content` (optional), `supervised` (boolean, optional) | Starts a session in the workspace, any absolute path, and answers with its `session_id`. With `supervised` true, the session is driven by a program, a supervisor outside Fiber, and `session_started` records it: it follows the delegate rules (`docs/permissions.md`, "Supervised sessions") and stays top-level, in the feed. Each of `overrides` is a `key=value` passed to the session as `-c` ("Commands and flags"). With `worktree` true, the session runs in a new worktree of the workspace ("Isolation"). With `continues` naming a session id, the session runs in that session's worktree instead of a new one, and the workspace must be that worktree's path ("Isolation"). An unknown session, a missing log, or a removed worktree refuses `session_not_found`; `continues` with `worktree` true, a named session with no worktree, and a workspace that is not the worktree refuse `invalid_arguments`. With `content`, the hub sends it as the session's first prompt after answering: once the session has accepted a `full` `subscribe` for the new session from the connection that sent `start`, at once when that connection closes first, or 1 second after the answer when neither has happened. A client that subscribes `full` when it reads the answer is connected when the session takes the prompt, so it can answer the repository's offer (`docs/extensions.md`, "Code a repository ships"); for a connection that never subscribes `full`, such as a script's, the session takes the prompt after the bound, and it decides with nobody to answer unless another `full` client is connected by then. A `content` that does not fit `prompt`'s `args` is rejected `invalid_arguments` before any session starts. A rejection of the first prompt after the answer is logged, not answered: the hub writes the session id, the `start` command id and the code to `logs/hub.log`, and no client is told. |
| `delete` | `session` (string), `cascade` (boolean, optional), `expect` (array of strings, optional), `dry_run` (boolean, optional) | Deletes an exited session ("Deleting and pruning"). With `cascade`, `expect` is the sessions the person confirmed; the delete is rejected `stale_request` when the sessions it would remove differ. With `dry_run`, it answers `sessions`, the ids it would remove, and deletes nothing. |
| `prune` | `older_than` (string, optional), `cascade` (boolean, optional), `force` (boolean, optional), `dry_run` (boolean, optional), `expect` (object, optional) | Prunes the hub's own machine across every project ("Deleting and pruning"). A `prune` dry run answers `lines`, the lines prune would print, `would_free`, the space it would free, and `items`, the sessions (`{id, cascade}`), worktree paths, pinned content hashes and diagnostic paths it would remove; a confirmed run answers `freed`, the space freed, `total`, and `failures`, one entry per item that failed after execution began. The client sends the dry run's `items` back as `expect`, and the hub rejects the prune `stale_request` when what it would remove differs, deleting nothing. |
| `prompt_history` | `project` (string), `before` (integer, optional) | Answers with a page of the project's prompt history, newest first (`docs/state.md`). `project` is the project's key, as for `recent`. |
| `read_file` | `session` (string), `path` (string, relative to the session directory, as the log names the file, such as `artifacts/i_3f2a.png`) | Answers with one file from the session's `artifacts/` ("A session's files"). |
| `directories` | `path` (string, absolute, or starting with `~/` for the hub's home) | Answers with the names of the subdirectories `path` holds, sorted, one level only, and the absolute path it listed, `~/` expanded, so a client can start a session in a path it typed from `~/`. A path that is not a readable directory answers an empty list; a relative path is rejected `invalid_arguments`. The terminal's workspace picker completes a typed path with it (`docs/tui.md`, "Home"). |
| `search_files` | `workspace` (absolute string), `text` (string), `fresh` (boolean, optional) | Searches the hub's files, answering `rows` (at most 50 strings) or `error` (string), as the file panel does. |
| `device_login` | `name` (string) | Runs the provider's device-code login on the hub, answering `url` and `code` (strings). |
| `device_login_result` | none | Answers with `path` (string, relative to the hub's Fiber home) once that connection's login has stored its credential, or rejects with its failure. |
| `device_login_cancel` | none | Cancels that connection's login and answers once nothing more can be stored. No result. |
| `configure` | `operation` (string), plus that operation's arguments below | Reads or writes configuration on the hub's machine. Sent without `session_id`. |
| `read_config` | `workspace` (absolute string), `path` (string) | Reads a configuration view's file as UTF-8 `text` with a content `version`. A missing file answers empty text and version `missing`. |
| `write_config` | `workspace` (absolute string), `path`, `text`, `version` (strings) | Writes the text only while the file still has that version; otherwise rejects `stale_request` without writing. Parses no text. |
| `status` | none | Answers with `running`, `fiber_version`, `clients` and `variables`, once the hub's first login-shell capture has ended (`docs/events.md`, "`command_accepted`"). |
| `refresh` | none | Rebuilds the hub's environment, as `fiber hub refresh` does. |
| `stop` | `sessions` (boolean, optional) | Ends the hub as SIGTERM does. With `sessions` true, first shuts down every running session the hub knows of in `run/`, each through its own shutdown (`close` with `now`), bounded, then exits. Any authenticated connection may send it. |
| `placeholder_env` | `names` (array of strings) | Answers with the hub's captured values for the named placeholders, absent names omitted, so the list and the session agree (`docs/model-routing.md`, "Model discovery"). |
| `pairing_code` | `device` (string) | Answers with a new pairing code for that device, as `fiber hub pair` prints. |
| `devices` | none | Answers with the paired devices. |
| `revoke` | `device` (string) | Revokes the device's token, which may be the asking client's own, and closes its live connections. |

Each is answered with one `command_accepted` or `command_rejected`, as a
session's commands are. A client offers the workspaces of recent sessions
first when it asks where to start one, and lists directories with
`directories`.

`search_files` runs both file-panel modes (`docs/tui.md`, "Keys") on
the hub's machine. `~/` starts at the hub's home; `../` starts at `workspace`.
The connection's search worker lists git's tracked files on its first
workspace query and reuses that listing. `fresh: true` or a different
workspace replaces it. The listing ends when the connection closes or a
query replaces it. A client sends `fresh: true` when it opens a new panel.
A newer query cancels the older walk and answers it `stale_request`, even
when several queued queries were skipped. A skipped query still applies
its `fresh` and workspace change. Other hub commands do not wait behind a
walk. A base that cannot be read or a failed git listing answers `error`,
not a rejection.

A connection holds at most one device login. A new `device_login` cancels
the previous one first, and disconnecting cancels it too. Each command gets
one answer: `device_login` when the provider shows the URL and code,
`device_login_result` when it ends, and `device_login_cancel` after cancel
returns. An already ended login answers its result at once. A result or
cancel with no login, or a second waiting result command, is rejected
`stale_request`. Failure before a code is shown rejects `device_login`;
a provider that opens a browser instead is rejected `authentication_failed`,
saying it has no device-code form. Unknown arguments, wrong types and
explicit `null` on these commands or `search_files` are `invalid_arguments`.
A search worker or login thread that cannot start rejects its command
`io_failed`; a later search can try again.

The `configure` operations below use the hub's Fiber home and the project of
`workspace` on that machine. Every `workspace` is absolute. Arguments are
required unless marked optional; unknown keys, wrong types and explicit
`null` are rejected `invalid_arguments`.

| Operation | Arguments besides `operation` | Result |
|---|---|---|
| `settings` | `workspace` (string) | `rows`, the setting rows described below; client-owned `keys` and `tui` rows are omitted |
| `set` | `workspace`, `layer`, `key`, `text` (strings) | `file` (string) and `warnings` (array of strings); `layer` is `global`, `project` or `repository`; a client-owned key is rejected `invalid_arguments` without writing |
| `global_file` | none | `file` (string), the hub's global configuration file |
| `rules` | `workspace` (string) | `global` and `project`, each with `file` and either `rows` or `error`; a row holds `line` (integer), `text` (string) and `rule` (the standing rule object from `docs/configuration.md`) |
| `revoke` | `workspace`, `scope`, `text` (strings), `line` (integer) | `revoked`, either `removed` or `stale`; `scope` is `global` or `project` |
| `login_targets` | none | `targets`, objects with `name` and `kind` (`key`, `browser` or `secret`) |
| `store_key` | `name`, `key` (strings), `label` (string, optional) | `path` (string, relative to Fiber home) and `replaced` (boolean), as `fiber login` stores it; the key is never returned or logged |
| `reviewer_models` | `name` (string) | `models` (array of strings), optional `selected` (string), and `notices` (array of strings), from the hub's installed provider |
| `models` | `workspace` (string), `refresh` (string, optional: `cached`, `stale` or `every`; default `cached`) | `models`, `notices`, `lists` and `installable`, the hub's installed models as the terminal's picker lists them: a `models` element holds `reference`, `provider`, `id`, optional `name`, `levels`, optional `default_level` and `configured`, `roles`, and optional `price` (`micros_per_mtok`, `input`, `output`, `tiers`); a `lists` element holds `provider`, optional `updated_ms` and `stale`; `installable` (array of strings) names each first-party provider no installed extension declares, sorted; `cached` starts no extension, `stale` and `every` refresh the lists first (with and without the age check) |
| `install_provider` | `workspace`, `provider` (strings) | `installed` and `summary` (arrays of strings) and `login` (array of objects): the first-party package serving `provider` installed as `fiber extension install` installs it, without asking; `installed` names each extension put in place, `summary` holds the lines "What an install shows" gives (`docs/extensions.md`), and `login` holds an object per provider it registered whose key is missing (`provider`) or could not be read (`provider` and `error`, string), for `fiber login <name>`; the configuration of `workspace` must load, or nothing is fetched; a provider no first-party package serves is rejected `invalid_arguments`, and a failed install installs nothing and is rejected with its code |
| `blocking_hooks` | `workspace` (absolute string), `overrides` (array of strings, optional; absent means empty) | `names` (array of strings): sorted blocking hook names from the merged, repository-filtered configuration with the exact ordered overrides; empty when the hooks package is installed, even disabled or unloadable, or the filtered gate has no named blockers; configuration and listing errors retain their code and message |
| `install_hooks` | none | empty result (`{}`): installs `hooks` globally into the hub's home as unflagged `fiber extension install hooks`, without asking; a failed plan or commit installs nothing and retains its original error code and message |
| `set_reviewer` | `name`, `id` (strings) | no result |
| `gaps` | `name` (string) | `gaps` (array of objects): one presence gap per entry, each with `name` (string) and `env` (string or null, the declared environment variable); the hub's installed provider answers (`docs/model-routing.md`, "A per-account host") |
| `save_host` | `name`, `key`, `value` (strings) | optional `masked` (string): the project settings file hiding the saved value, when a project-layer empty masks it; saves globally (`docs/configuration.md`, "Extension settings") |
| `reviewer_notes` | `workspace` (string) | `global` and `project`, each with `file`, `text` and `version` (strings), the layer's own `reviewer.context`, empty when unset; `version` is the string `absent` when the file does not exist |
| `save_reviewer_notes` | `workspace` (string), `global` and `project` (each optional: `version` and `text`, strings) | `saved` (`written` or `stale`); on `written`, `files` (array: `file`, `saved` (boolean), `error` (error, optional), one per sent layer, global first); `stale` writes nothing; each file is written as `docs/state.md` "Concurrent access" says, and a sync failure after its rename is saved with an error |
| `tool_switches` | `workspace` (string) | `groups`, objects with `group`, `project` and `everywhere`; `group` is one of `{"mcp":"name"}` or `{"extension":"name"}`; each layer holds `disabled` (array of strings) and optional `enabled` (array of strings) |
| `switch_tool` | `workspace`, `tool`, `scope` (strings), `group` (object as above), `on` (boolean) | no result; `scope` is `project` or `everywhere` |
| `skills_disabled` | `workspace` (string) | `project` and `everywhere` (arrays of strings) |
| `switch_skill` | `workspace`, `name`, `scope` (strings), `on` (boolean) | no result; `scope` is `project` or `everywhere` |
| `skill_text` | `path` (string) | `text` (string), as the local skill view reads it |

A setting row holds `key`, `layer` and `shown` (strings), and `file` (string)
when that layer has one. `shown` is `unset`, `value`, `redacted` or `union`.
`value` and `redacted` carry `value` (string); `union` carries `names`
(`[name, layer]` pairs) and `own` (`[layer, json]` pairs).
`scope` is `any` (with `repo`, a boolean), `global_only`, `repo_only` or
`person_files`. Values are JSON text, not decoded values.

`keys` and every `tui.*` key belong to the terminal that draws, including
`/keys`, `/settings` rows for those keys and the theme picker. They stay in
the client's Fiber home. The hub serves neither `save_keys`, themes nor
`LoginShow`, which draws a login URL and code on the client.

`read_config` and `write_config` are the Ctrl+G commands, not arbitrary file
access. Their `path` must lexically equal the global, project or repository
`config.json`, the global or project `rules`, or be an absolute path named
`SKILL.md`. Both refuse text over 64 KiB with `too_large`, without cutting
it. Non-UTF-8 files fail `io_failed`, naming the path. Versions are 16-digit
lowercase hexadecimal content hashes, or `missing`. For configuration and
rules, the write holds `<path>.lock` across reading, comparing and writing;
skill files leave no lock file. A stale write says the path changed since
it was read. A matching write stores even invalid configuration or rules:
the client rereads the view to show its error. `read_file` remains limited
to session artifacts.

### Remote clients

- **Pairing gives each client its own device token.** `fiber hub pair
  <device>`, run on the hub's machine, prints a short code, and in a terminal
  a QR code that carries the pairing code, plus the address given with
  `--address`; a paired client asks for one with `pairing_code`. A client sends
  the code with `pair` within 10 minutes and receives a device token named
  after the device. The code then stops working. A wrong, used or old code is
  rejected `pairing_failed`.
  A device name is 1 to 64 ASCII letters, digits, dots, hyphens or underscores,
  does not start with a dot, and is not `local`.
  A name already paired is refused until revoked, and a second code for a name
  replaces the first.
- **A device token is presented, never placed in a URL.** A remote
  connection's first command is `authenticate` or `pair`. Anything else, or
  a token the hub does not hold, is answered `unauthenticated` and the
  connection is closed. A URL ends up in logs and history; a first message
  does not.
- **The hub keeps only a hash of each token,** in `hub/devices/<device>`
  (`docs/state.md`), so a copy of Fiber home holds no usable token.
  `fiber hub token revoke <device>` deletes the records holding its token and
  closes every live connection that presented that token.
  Revoking a device removes every record holding its token, including hand-made copies.
  Revoking a name not paired is refused `not_found`.
  Revoking a device voids the codes it minted.
  A failed directory sync after the unlink or rename, the commit point, is a
  warning in the log and the change stands.
  A record whose name the pairing rule would refuse still authenticates;
  remove it by deleting its file.
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
- **A session-owned process gets no pairing code on the local socket.** The
  hub refuses `pairing_code` with `from_session` when the kernel walk places
  the connection inside a session (`docs/permissions.md`, "Sessions on the
  sockets"). Websocket and port behavior is unchanged: a paired client
  holding a device token reaches `pairing_code` as before.
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
file's bytes in base64 and its media type. The media type comes from the
file name's extension; one Fiber does not know is `application/octet-stream`.

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
session turns `idle`, except in a supervised session, which sends no
`finished` line: its supervisor reads the turn's end from the event stream.

When 1,024 attention lines are queued for a connection that has not read
them, the hub closes the connection, as it does for the feed.

Attention covers the transitions the hub sees, so it is best effort. The hub
can miss one in four cases. A session drops `session_status` lines for a hub
that falls too far behind. The hub reads only the last 64 KiB of a session's
log when it first sees the session idle, so it misses a turn that ended
further back. A session that closes right after a turn ends may exit before
the hub sees it `idle`, and then no `finished` line is sent. It also skips a
turn whose end is stamped before the hub started, which can happen to a recent
turn when the wall clock is set back. The feed has the same exposure, and the session's next status change corrects both.

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
  without being told. A `SHELL` that is unset, empty or not an absolute
  path is a failed capture. The result, with `FIBER_HOME` set to the hub's
  own home, is every session's environment, so the `PATH` the person's
  startup files build reaches every command.
- **The capture sees what a fresh login terminal sees.** zsh reads all its
  startup files. An interactive login bash reads the first of
  `~/.bash_profile`, `~/.bash_login` and `~/.profile`, and reads `~/.bashrc`
  only when that file sources it.
- **The capture has 10 seconds.** If the shell fails or runs out of time, the
  hub uses the environment it inherited instead, with `FIBER_HOME` set the
  same way. `fiber doctor` says so, and
  every session started from it says so when it starts.
- **A session's environment is fixed when the session starts.** A tool
  installed into a directory already on `PATH` is found without a rebuild.
  A session the hub resumes gets the hub's environment at the resume.
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
nothing uncommitted and no commits beyond its base, kept otherwise. A session that ends `rewound` keeps its worktree whatever it holds: the session that continues it runs there, records the same `worktree`, and never removes it either; `fiber sessions prune` removes it later. The session a `start` `continues` names keeps its worktree the same way: the new session runs in that worktree, records the same `worktree`, and never removes it either. A resume whose worktree was removed makes it again at the same path and branch from the repository's current HEAD, before `fiber_started`, and ends it by the same rules. When the repository is gone, the resume is rejected the same way. A worktree's lock file, `~/.fiber/projects/<key>/worktrees/<id>.lock`, is held shared by every process that runs in a worktree it did not just create, exclusively while a resume checks for the worktree and makes it again, and taken exclusively without waiting by an end-of-run removal, which keeps the worktree when another process holds it; the file stays whenever its worktree stays, and goes only when a whole removal succeeds, so a dirty, never-shared worktree keeps one too, and `fiber sessions prune` leaves lock files alone. Outside a
git repository a delegate's `isolation: worktree` fails with
`invalid_arguments`, and `fiber ask --worktree` is a usage error. The session
records the worktree on `session_started` (`docs/events.md`). The `worktree`
module creates and removes worktrees, for delegates, sessions and
`fiber sessions prune` (`docs/architecture.md`). Fiber runs the `git` program; no
git library is linked in. `git worktree add` runs the repository's hooks, so
a `post-checkout` hook can set up the new tree. Every other git command the
`worktree` module runs, including those of `fiber sessions prune`, runs with
the repository's hooks off (`-c core.hooksPath=/dev/null`), so removing a
worktree can't hang on a hook. Every git `fiber sessions prune` runs, discovery
and worktree reads alike, carries the scrubbed environment as well:
`core.fsmonitor` off (`-c core.fsmonitor=false`), `GIT_OPTIONAL_LOCKS` and
`GIT_TERMINAL_PROMPT` pinned to `0`, and `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_COMMON_DIR` and `GIT_INDEX_FILE` removed, so a redirected environment
cannot move the prune to another repository. A supervisor that wants its own tree still makes it
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
  session log.
- **When `expect` is supplied, a cascade removes only the sessions the person confirmed.** A client sends them in `delete`'s `expect` ("The hub"): the session and everything `--cascade` adds. When the sessions the hub would remove differ, such as a fork made while the question was open, the delete is rejected `stale_request`, the message names the sessions it would remove now, and nothing is deleted. `fiber sessions delete` and pruning send no `expect`.
- **Delete is permanent.** It removes the session's directory: its log and its
  artifacts together. There is no trash. The terminal asks first, naming the
  session and everything `--cascade` adds. On the command line `--yes`
  confirms; without it the command lists what it would delete and asks, and
  with no terminal to ask on it is a usage error.
- **With `--hub`, the id resolves on that hub.** The command matches it
  against that hub's `sessions` answer, across every project, as
  `fiber resume --hub` does, then sends that hub `delete`. `--cascade` and
  `--yes` work as they do locally.
- **A kept worktree the session made stays.** Pruning removes worktrees.
- **`recent.jsonl` keeps the session's row.** Nothing rewrites that file, and
  readers skip a row whose session directory is gone (`docs/state.md`,
  "What each part holds").

**Exporting a session.** `fiber sessions export <id> [<path>]` copies the
session's `events.jsonl` and `artifacts/` into a directory, `./<id>/` by
default. A path that already exists is refused, naming it; nothing is merged
or overwritten. A running session's export holds the lines written so far. The
export copies a symbolic link as a link and never reads through it. Its top
directory is created with mode 0700; files keep their modes. The export is the
log as recorded: text a hook redacted before it was logged is
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
  commit on the branch is also on another branch or a remote. Uncommitted
  follows git: tracked changes, and untracked files that are not ignored.
  Ignored files are not changes, so a worktree holding only those is removed,
  and its line in prune's list names their total size and top-level paths,
  such as "target/ 3.1 GiB, .env". Any other worktree is skipped, and its line names
  what removing it would lose: uncommitted files, commits found nowhere else,
  or both. `--force` removes it anyway. A worktree a running session works in
  is never removed.
- **Pinned copies.** It removes each copy in `pinned/` of this repository's
  code that no worktree's files match any more (`docs/extensions.md`, "Code a
  repository ships"). A server copy is judged exactly as an extension copy
  is: it is a candidate when at least one project record at
  `projects/<key>/servers/<content-hash>` names it and every naming record's
  project is in scope, and it is kept when no project record names it. The worktrees that count are the repository's
  `git worktree list`. The copy's approval record stays, so the same version
  checked out again is not offered again; its copy is made again from the
  worktree.
- **Diagnostic logs and crash files.** It deletes the old ones, as the hub
  does when it starts (`docs/state.md`, "What each part holds").
- **What it frees.** `--dry-run` prints what prune would delete and the space
  it would free, and deletes nothing. Otherwise prune prints the same list,
  asks as delete does (`--yes` confirms), deletes, and prints the space freed.
- **With `--hub`, the hub prunes its own machine.** The command asks that
  hub to prune, and the hub runs everything above on its machine across
  every project, with the same flags. Without `--yes` the command first asks
  for the dry run, prints it and asks; the confirmation stays with the
  client. The dry run answers the lines prune would print, the space it
  would free, and the items it would remove. The client sends those items
  back as `expect`, and the hub rejects the prune `stale_request` when what
  it would remove differs, deleting nothing. It prints the space freed and
  each failure as a local prune does.

## The delegation supervisor is external

Fiber ships no MCP server for delegating to Fiber, and the supervisor that manages several
outstanding delegations to Fiber lives outside Fiber. A session's own
delegates are `docs/delegates.md`. The rationale is
[ADR 0005](adr/0005-the-delegation-supervisor-is-external.md). Fiber is an
MCP client, consuming MCP servers, and that is `docs/mcp.md`.

What Fiber owes the delegation slot instead is a hub a supervisor can drive
as a client ("Two doors"), and that is the whole of it:

- start a supervised session with no tty, through the hub's `start`, in a
  workspace path it is given rather than one it makes, and answer with the
  session id,
- take its prompts, steering, cancelling and replies as driver commands
  ("Driver commands"),
- serve a documented, versioned event stream to a client that subscribes
  (`docs/events.md`),
- end the session on `close`, with `fiber_exited` as the verdict
  ("Lifecycle", "What a caller gets back").

Everything a supervisor does beyond that — tracking several jobs, waiting on
any or all of them, running a verification gate afterwards, reporting a git
change set — is orchestration that is identical for any agent binary, and a
supervisor that knows only about Fiber is worth less than one that does not.

## Related

- The terminal's own shape: `docs/tui.md`
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
