## read

- Read files with `read`. It returns the file's exact text, ready to copy into an edit, so keep the shell for running commands.

## edit

- Change an existing file with `edit`: one call per file, one block per change.
- Every `old_text` matches the file as it stood before the call, so blocks never overlap. Merge nearby changes into one block.
- Keep each `old_text` to the fewest lines that are unique in the file.

## write

- Use `write` for new files and complete rewrites. Replacing an existing file needs a `read` of it earlier in this context.

## shell

- Commands run with no terminal and empty input, so use each command's non-interactive form.

## jobs

- A finished job wakes you. Carry on with other work meanwhile, and use `jobs wait` only when nothing else is left.

## tool_search

- When a tool you need is missing, find it with `tool_search`.

## handoff

- Call `handoff` with your note to restart your context from it, for example when one piece of work ends and an unrelated one begins.
