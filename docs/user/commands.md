# Command reference

Generated from `fiber help`. Do not edit by hand; run `FIBER_UPDATE_COMMANDS=1 cargo nextest run -p main --bins command_reference` to regenerate.

## fiber

```text
Fiber, a coding agent.

Usage: fiber <command> [arguments]

Sessions:
  fiber [--model <model>] [-c <key>=<value>]...                                                                   Open the terminal
  ask [--model <model>] [-c <key>=<value>]... [--resume <id> [--credential <label>]] [--worktree] [<prompt>] [-]  Run one session of one turn; its events go to stdout
  resume [<id>] [--credential <label>]                                                                            Open a session in the terminal, or home at the session list
  continue [--hub <name>]                                                                                           Open the most recent session in this project
  sessions [--all] [--json]                                                                                       List sessions: id, state, name, what it waits on, spend
  sessions search [--all] [--json] <text>                                                                         Search the logs of past and running sessions for the text
  sessions delete [--hub <name>] [--cascade] [--yes] <id>                                                         Delete a session, and with --cascade the sessions that continue it
  sessions export <id> [<path>]                                                                                   Write the session's log and its artifacts to <path>
  sessions prune [--hub <name>] [--older-than <duration>] [--cascade] [--force] [--dry-run] [--yes]               Delete old sessions, worktrees and diagnostic logs
  models [<search>] [--json]                                                                                      List the models the installed providers serve

Fiber itself:
  setup [--provider <name>]... [--model <model>] [--thinking <level>] [--import <claude-code | codex>]... [--completions <shell> | --no-completions] [--hub | --no-hub]  Ask the questions a new install needs, applying each answer
  update                                    Update the Fiber binary and every installed extension together; `upgrade` runs the same command, with `--channel stable|nightly`
  approve [--yes]                           Show what this repository ships and approve it
  login [<name>] [--as <label>] [--device]   Store a provider's key or an extension's secret
  logout <provider> [--as <label> | --all]  Delete a provider's stored key
  doctor                                    Says whether a session can start, and how to fix it when it cannot
  stop [--hub <name>]                       Shut down every running session, then the hub
  feedback [<prompt>] [-] [--session <id>]  Write a feedback report and print the issue link.
  completion <shell>                        Print a completion script for bash, zsh or fish
  help [<command>]                          Print this menu, or a command's help
  version                                   Print the version

Extensions:
  extension install [--project] <name or path>  Install an extension and its dependencies
  extension update [<name>]         Update one extension, or every installed extension, to its newest tag
  extension remove <name>           Remove an extension, the dependencies nothing else uses, and their data
  extension reinstall <name>        Remove an extension and install it again from its recorded source and commit
  extension list                    List installed extensions: name, version and commit
  extension test [<path>]           Run an extension's test cases against the scripted provider

MCP servers:
  mcp add [--project | --repo] <name> <url>                                                      Declare a remote server
  mcp add [--project | --repo] <name> [-e KEY=value]... [--secret KEY]... -- <command> [args]...  Declare a stdio server
  mcp remove [--project | --repo] <name>                                                         Remove a server's declaration
  mcp list                                                                                       List declared servers, their layers and approvals
  mcp login <server>                                                                             Log in to a server that needs OAuth
  mcp logout <server>                                                                            Delete a server's stored token

Configuration:
  config get <key>                              Print the effective value and the layer it came from
  config set [--project | --repo] <key> <value>  Write one key in one layer's file
  import <claude-code | codex>                   Copy another harness's configuration into Fiber home, once

The hub:
  hub add [--default] <name> <address>  Add a hub to this client's list
  hub remove <name>                    Remove a hub from this client's list
  hub install [--port <port>]          Register the hub as a login service
  hub uninstall                        Remove the hub's login service; running sessions carry on
  hub status [--json]                  Print the hub's state: running, version, port, clients, devices, installed
  hub refresh                          Rebuild the hub's environment for the sessions it starts from now on
  hub pair <device> [--address <url>]  Print a pairing code for a device of that name
  hub token list                       List paired devices: name, when paired, last connection
  hub token revoke <device>            Revoke a device's token and close its live connections

Flags:
  -h, --help     Print this menu
  -v, --version  Print the version

Examples:
  fiber ask "review the diff on this branch"
  fiber ask < brief.md
  git diff | fiber ask "review this diff" -
  fiber extension install openrouter
  fiber login openrouter
  fiber help ask
```

## fiber update

```text
Update the Fiber binary and every installed extension together; `upgrade` runs the same command

Usage: fiber update [OPTIONS]

Options:
      --channel <channel>  The release channel: `stable` or `nightly`. Absent reads `FIBER_CHANNEL`; an empty or unset variable means stable [possible values: stable, nightly]
  -h, --help               Print help
```

## fiber ask

```text
Run one session of one turn; its events go to stdout

Usage: fiber ask [OPTIONS] [prompt]...

Arguments:
  [prompt]...  The prompt. A final `-` reads stdin

Options:
      --model <model>       The model for this run, as a person types it
  -c <key>=<value>          One configuration key for this run, `<key>=<value>`: the value is JSON, or a bare string when it does not parse as JSON. May repeat; a later one wins, and every one wins over `--model`
      --resume <id>         Resume the session: sends the prompt to an existing session instead of starting a new one (`docs/invocation.md`, "Lifecycle")
      --credential <label>  On a resume, the credential label to switch the session to
      --worktree            Run the session in a new worktree (`docs/invocation.md`, "Isolation")
  -h, --help                Print help
```

## fiber resume

```text
Open a session in the terminal, or home at the session list

Usage: fiber resume [OPTIONS] [id]

Arguments:
  [id]  The session: a full id or a unique prefix of one. With none, home opens at the session list

Options:
      --credential <label>  The credential label to switch the session to (`docs/model-routing.md`, "Which credential a session uses")
      --hub <name>          Use a hub from this client's list
  -h, --help                Print help
```

## fiber continue

```text
Open the most recent session in this project

Usage: fiber continue [OPTIONS]

Options:
      --hub <name>  Use a hub from this client's list
  -h, --help        Print help
```

## fiber sessions

```text
List sessions, or delete, export or prune them

Usage: fiber sessions [OPTIONS]
       fiber sessions <COMMAND>

Commands:
  delete  Delete a session, and with --cascade the sessions that continue it
  export  Write the session's log and its artifacts to <path>
  prune   Delete old sessions, worktrees and diagnostic logs
  search  Search the logs of past and running sessions for the text

Options:
      --hub <name>  Use a hub from this client's list
      --all         List every project, not only this repository's
      --json        Print each session as one JSON object per line
  -h, --help        Print help
```

## fiber sessions delete

```text
Delete a session, and with --cascade the sessions that continue it

Usage: fiber sessions delete [OPTIONS] <id>

Arguments:
  <id>  The session: a full id or a unique prefix of one

Options:
      --hub <name>  Use a hub from this client's list
      --cascade     Also delete every session that continues it: its forks and rewinds, and theirs
      --yes         Delete without asking
  -h, --help        Print help
```

## fiber sessions export

```text
Write the session's log and its artifacts to <path>

Usage: fiber sessions export <id> [path]

Arguments:
  <id>    The session: a full id or a unique prefix of one
  [path]  The directory to write, from the current directory when relative; the session id when absent

Options:
  -h, --help  Print help
```

## fiber sessions prune

```text
Delete old sessions, worktrees and diagnostic logs

Usage: fiber sessions prune [OPTIONS]

Options:
      --hub <name>             Use a hub from this client's list
      --older-than <duration>  Delete every exited session whose last line is older than this, such as `30d`: a whole number and `s`, `m`, `h` or `d`. Without it no session is deleted
      --cascade                Also delete every session that continues a deleted one
      --dry-run                Print what would be deleted and the space it would free, and delete nothing
      --yes                    Prune without asking
      --force                  Remove a worktree even when removing it would lose something. A worktree a running session works in is never removed
  -h, --help                   Print help
```

## fiber sessions search

```text
Search the logs of past and running sessions for the text

Usage: fiber sessions search [OPTIONS] <text>

Arguments:
  <text>  The text to find, as a literal

Options:
      --all   Search every project, not only this repository's
      --json  Print each hit as one JSON object per line
  -h, --help  Print help
```

## fiber models

```text
List the models the installed providers serve

Usage: fiber models [OPTIONS] [search]

Arguments:
  [search]  Only list models whose `provider/model` holds this text

Options:
      --json  Print one JSON object per model
  -h, --help  Print help
```

## fiber extension

```text
Manage extensions

Usage: fiber extension <COMMAND>

Commands:
  install    Install an extension and its dependencies
  update     Update one extension, or every installed extension, to its newest tag
  remove     Remove an extension, the dependencies nothing else uses, and their data
  reinstall  Remove an extension and install it again from its recorded source and commit
  list       List installed extensions: name, version and commit
  test       Run an extension's test cases against the scripted provider

Options:
  -h, --help  Print help
```

## fiber extension install

```text
Install an extension and its dependencies

Usage: fiber extension install [OPTIONS] <name or path>

Arguments:
  <name or path>  The extension's name, or a path to its package

Options:
      --project  Enable the extension only for the current project
  -h, --help     Print help
```

## fiber extension update

```text
Update one extension, or every installed extension, to its newest tag

Usage: fiber extension update [name]

Arguments:
  [name]  The installed extension's name

Options:
  -h, --help  Print help
```

## fiber extension remove

```text
Remove an extension, the dependencies nothing else uses, and their data

Usage: fiber extension remove <name>

Arguments:
  <name>  The installed extension's name

Options:
  -h, --help  Print help
```

## fiber extension reinstall

```text
Remove an extension and install it again from its recorded source and commit

Usage: fiber extension reinstall <name>

Arguments:
  <name>  The installed extension's name

Options:
  -h, --help  Print help
```

## fiber extension list

```text
List installed extensions: name, version and commit

Usage: fiber extension list

Options:
  -h, --help  Print help
```

## fiber extension test

```text
Run an extension's test cases against the scripted provider

Usage: fiber extension test [path]

Arguments:
  [path]  The extension package; defaults to the current directory

Options:
  -h, --help  Print help
```

## fiber mcp

```text
Manage MCP servers

Usage: fiber mcp <COMMAND>

Commands:
  add     Declare a remote or stdio server
  remove  Remove a server's declaration
  list    List declared servers, their layers and approvals
  login   Log in to a server that needs OAuth
  logout  Delete a server's stored token

Options:
  -h, --help  Print help
```

## fiber mcp add

```text
Declare a remote or stdio server

Usage: fiber mcp add [OPTIONS] <name> [url] [-- <command>...]

Arguments:
  <name>        The server's name
  [url]         The server's URL: a remote server. With none, `-- <command>` declares a stdio server
  [command]...  The program and its arguments: a stdio server

Options:
      --project       Write the per-project file in Fiber home
      --repo          Write the repository's `.fiber/config.json`
  -e <KEY=value>      A clear variable, `KEY=value`; only with `-- <command>`. May repeat
      --secret <KEY>  A secret variable's name, stored beside the configuration; only with `-- <command>`. May repeat
  -h, --help          Print help
```

## fiber mcp remove

```text
Remove a server's declaration

Usage: fiber mcp remove [OPTIONS] <name>

Arguments:
  <name>  The server's name

Options:
      --project  Remove from the per-project file in Fiber home
      --repo     Remove from the repository's `.fiber/config.json`
  -h, --help     Print help
```

## fiber mcp list

```text
List declared servers, their layers and approvals

Usage: fiber mcp list

Options:
  -h, --help  Print help
```

## fiber mcp login

```text
Log in to a server that needs OAuth

Usage: fiber mcp login <server>

Arguments:
  <server>  The server's name

Options:
  -h, --help  Print help
```

## fiber mcp logout

```text
Delete a server's stored token

Usage: fiber mcp logout <server>

Arguments:
  <server>  The server's name

Options:
  -h, --help  Print help
```

## fiber approve

```text
Show what this repository ships and approve it

Usage: fiber approve [OPTIONS]

Options:
      --yes   Approve without asking, for a script or a machine image
  -h, --help  Print help
```

## fiber config

```text
Print a configuration value, or write one

Usage: fiber config <COMMAND>

Commands:
  get  Print the effective value and the layer it came from
  set  Write one key in one layer's file

Options:
  -h, --help  Print help
```

## fiber config get

```text
Print the effective value and the layer it came from

Usage: fiber config get <key>

Arguments:
  <key>  The dotted key, such as `model` or `handoff.tokens`

Options:
  -h, --help  Print help
```

## fiber config set

```text
Write one key in one layer's file

Usage: fiber config set [OPTIONS] <key> <value>

Arguments:
  <key>    The dotted key, such as `model`
  <value>  The value as JSON, or a bare string when it does not parse as JSON

Options:
      --project  Write the per-project file in Fiber home
      --repo     Write the repository's `.fiber/config.json`
  -h, --help     Print help
```

## fiber import

```text
Copy another harness's configuration into Fiber home, once

Usage: fiber import <source>

Arguments:
  <source>  The harness to import from: `claude-code` or `codex` [possible values: claude-code, codex]

Options:
  -h, --help  Print help
```

## fiber login

```text
Store a provider's key or an extension's secret

Usage: fiber login [OPTIONS] [name]

Arguments:
  [name]  The installed provider to store a key for, or the declared secret to store. With none, a terminal offers the installed providers and the declared secrets

Options:
      --as <label>  The credential label to store a provider's key under. Without it the label is the account's email when the login reveals one, otherwise `default`
      --device      Log in with a device code instead of the browser, for a provider that logs in by browser
  -h, --help        Print help
```

## fiber setup

```text
Ask the questions a new install needs, applying each answer

Usage: fiber setup [OPTIONS]

Options:
      --provider <name>               Sign in to this provider, as `fiber login <name>` does. May repeat
      --model <model>                 The default model, written to the global `config.json`
      --thinking <level>              The default thinking level for the model, written beside it
      --import <claude-code | codex>  Import this source's configuration, as `fiber import` does. May repeat [possible values: claude-code, codex]
      --completions <shell>           Install completions for this shell in its startup file [possible values: bash, zsh, fish]
      --no-completions                Leave the shell's startup file alone
      --hub                           Register the hub as a login service, as `fiber hub install` does
      --no-hub                        Leave the hub's login service alone
  -h, --help                          Print help
```

## fiber logout

```text
Delete a provider's stored key

Usage: fiber logout [OPTIONS] [provider]

Arguments:
  [provider]  The provider whose stored key to delete. A missing provider is a usage error `fiber` words itself, so it matches the other sentences

Options:
      --as <label>  Delete this credential label alone
      --all         Delete every stored label
  -h, --help        Print help
```

## fiber doctor

```text
Say whether a session can start, and how to fix it when it cannot

Usage: fiber doctor

Options:
  -h, --help  Print help
```

## fiber stop

```text
Shut down every running session, then the hub

Usage: fiber stop [OPTIONS]

Options:
      --hub <name>  Use a hub from this client's list
  -h, --help        Print help
```

## fiber feedback

```text
Write a feedback report and print the issue link

Usage: fiber feedback [OPTIONS] [prompt]...

Arguments:
  [prompt]...  The description. A final `-` reads stdin

Options:
      --session <session_id>  The session whose diagnostics the report carries: any session in Fiber home. Without it, the newest session in this project
  -h, --help                  Print help
```

## fiber completion

```text
Print a completion script for bash, zsh or fish

Usage: fiber completion <shell>

Arguments:
  <shell>  The shell to complete in [possible values: bash, zsh, fish]

Options:
  -h, --help  Print help
```

## fiber version

```text
Print the version

Usage: fiber version

Options:
  -h, --help  Print help
```

## fiber help

```text
Print this menu, or a command's help

Usage: fiber help [command]...

Arguments:
  [command]...  The command to describe, a word per level: `extension install`. Absent prints the menu

Options:
  -h, --help  Print help
```

## fiber hub

```text
Manage this client's hub list, the hub's service and paired devices

Usage: fiber hub <COMMAND>

Commands:
  add        Add a hub to this client's list
  remove     Remove a hub from this client's list
  install    Register the hub as a login service
  uninstall  Remove the hub's login service; running sessions carry on
  status     Print the hub's state: running, version, port, clients, devices, installed
  refresh    Rebuild the hub's environment for the sessions it starts from now on
  pair       Print a pairing code for a device of that name
  token      Manage paired devices

Options:
  -h, --help  Print help
```

## fiber hub add

```text
Add a hub to this client's list

Usage: fiber hub add [OPTIONS] <name> <address>

Arguments:
  <name>     The hub's name in this client's list
  <address>  A ws://, wss:// or unix: address

Options:
      --default  Use this hub when no --hub is given
  -h, --help     Print help
```

## fiber hub remove

```text
Remove a hub from this client's list

Usage: fiber hub remove <name>

Arguments:
  <name>  The hub's name in this client's list

Options:
  -h, --help  Print help
```

## fiber hub install

```text
Register the hub as a login service

Usage: fiber hub install [OPTIONS]

Options:
      --port <port>  Also listen on this port of `127.0.0.1`, where every connection presents a device token. Without it the hub listens on its local socket only
  -h, --help         Print help
```

## fiber hub uninstall

```text
Remove the hub's login service; running sessions carry on

Usage: fiber hub uninstall

Options:
  -h, --help  Print help
```

## fiber hub status

```text
Print the hub's state: running, version, port, clients, devices, installed

Usage: fiber hub status [OPTIONS]

Options:
      --json  Print one JSON object
  -h, --help  Print help
```

## fiber hub refresh

```text
Rebuild the hub's environment for the sessions it starts from now on

Usage: fiber hub refresh [OPTIONS]

Options:
      --hub <name>  Use a hub from this client's list
  -h, --help        Print help
```

## fiber hub pair

```text
Print a pairing code for a device of that name

Usage: fiber hub pair [OPTIONS] <device>

Arguments:
  <device>  The device to pair

Options:
      --hub <name>     Use a hub from this client's list
      --address <url>  The websocket address carried in the QR code
  -h, --help           Print help
```

## fiber hub token

```text
Manage paired devices

Usage: fiber hub token <COMMAND>

Commands:
  list    List paired devices: name, when paired, last connection
  revoke  Revoke a device's token and close its live connections

Options:
  -h, --help  Print help
```

## fiber hub token list

```text
List paired devices: name, when paired, last connection

Usage: fiber hub token list [OPTIONS]

Options:
      --hub <name>  Use a hub from this client's list
  -h, --help        Print help
```

## fiber hub token revoke

```text
Revoke a device's token and close its live connections

Usage: fiber hub token revoke [OPTIONS] <device>

Arguments:
  <device>  The device to revoke

Options:
      --hub <name>  Use a hub from this client's list
  -h, --help        Print help
```

