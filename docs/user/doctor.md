# fiber doctor

Run `fiber doctor` when a session will not start. It prints one line per check, with a fix on any line that stops a session starting. It never touches the network, so it cannot check that a key is valid. The full list of lines is [Commands and flags](../invocation.md#commands-and-flags).

## What each line means

`fiber <version> (<commit>)` names the binary and its commit ([Commands and flags]).

`model: <choice> (provider <name>)` names the default model and the provider it needs. When no model is chosen, the line says so instead, and a session cannot start ([Commands and flags]).

Each installed provider gets one block: one line per credential label, with where its key comes from. `*` marks the selected label. A selected key that is missing, empty, or unreadable carries its fix, such as ``no key for openrouter: run `fiber login openrouter` `` ([Commands and flags]).

`permissions: auto` names the permission mode ([Commands and flags]).

`MCP servers: <count>` counts the declared MCP servers. A required server Fiber cannot use adds a line naming it ([Commands and flags]).

`hub: running` or `hub: not running` says whether the hub is running. When it is, the next lines describe the environment its sessions get: `hub environment` names the login-shell capture or its fallback, `hub PATH` gives the `PATH`, and `hub variables` names the other variables ([A session's environment](../invocation.md#a-sessions-environment)).

A `secret <name> (from <extension>) is not stored` line names an enabled extension's secret with the `fiber login <name>` that stores it. A missing secret never stops a session starting ([Commands and flags]).

`logs:` names the diagnostic log directory. `newest error:` gives the newest `error` line across those logs, with its file, age, code, and message. `newest crash:` gives the newest crash report with its age. Where each lives is [What each part holds](../state.md#what-each-part-holds).

``user guide not installed: run `fiber update` `` means Fiber home holds no `docs/`, and the update that restores them is [Updating](../releasing.md#updating).

## Exit code

`fiber doctor` exits non-zero when a session cannot start, and zero when one can. A missing extension secret never changes the exit code ([Commands and flags]). See [Filing an issue](getting-help.md#filing-an-issue).

[Commands and flags]: ../invocation.md#commands-and-flags
