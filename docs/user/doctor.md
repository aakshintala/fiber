# fiber doctor

Run `fiber doctor` when a session will not start. It prints one line per check, with a fix on any line that stops a session starting. It never touches the network, so it cannot check that a key is valid. The full list of lines is [Commands and flags](../invocation.md#commands-and-flags).

## What each line means

The version line names the binary you are running ([Commands and flags]).

The model line names the default model and the provider it needs. When it says none is chosen, choose one before starting a session ([Commands and flags]).

A provider block lists that provider's credentials. A line that carries a fix names the `fiber login` command that stores the key; run it ([Commands and flags]). A provider whose credential comes from its own tool, such as Bedrock's AWS profile or Vertex's gcloud configuration, shows the label it reads, marked `(not checked)`: doctor does not run that tool, so a profile shown here can still fail when a session starts ([Commands and flags]).

The permissions and MCP server lines need no action unless a line names a server Fiber cannot use ([Commands and flags]).

The hub lines say whether the hub is running and, when it is, describe the environment its sessions get ([A session's environment](../invocation.md#a-sessions-environment)).

A missing extension secret never stops a session starting; store it with the `fiber login <name>` the line names when the extension needs it ([Commands and flags]).

The log and crash lines say where to look and what the last runs left behind; quote them when you ask for help. Where each lives is [What each part holds](../state.md#what-each-part-holds).

``user guide not installed: run `fiber update` `` means Fiber home holds no `docs/`, and the update that restores them is [Updating](../releasing.md#updating).

## Exit code

`fiber doctor` exits non-zero when a session cannot start, and zero when one can. A missing extension secret never changes the exit code ([Commands and flags]). See [Filing an issue](getting-help.md#filing-an-issue).

[Commands and flags]: ../invocation.md#commands-and-flags
