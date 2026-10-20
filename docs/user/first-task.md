# A first task

Run `fiber` to open the terminal. It opens on home: the session list, the input box at the bottom, and the chip row with the workspace, the model and the thinking level. Typing a prompt and pressing Enter starts a session in the chosen workspace. What each part of home does is [Home](../tui.md).

Pick the model first if you have one in mind: the model chip opens the model picker. `fiber models` lists the installed providers' models from the command line, with the configured default marked.

Small and concrete wins the first time: ask for a summary of the repository, or for one small change with its tests.

## One-shot runs

`fiber ask "review the diff on this branch"` runs one session of one turn and prints its events to stdout. `fiber ask --resume <id>` sends a prompt to an existing session. The whole command line is [Commands and flags](../invocation.md).

## Coming back

`fiber resume` opens a session in the terminal, resuming it if it has exited. `fiber continue` opens the most recent session in the project. With no id, `fiber resume` opens home at the session list.

## What the friends release has no plan mode, todo list or file undo

The friends release ships without plan mode, without a todo list, and without file undo. It says so here so nothing surprises you mid-task. For undo, use git: commit before the session starts, and the diff is yours to keep or revert.
