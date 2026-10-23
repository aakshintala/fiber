# Reporting a problem

Run `fiber feedback "<description>"` in a shell, or `/feedback <description>` in the terminal, to write a feedback report and print the issue link that files it. It carries no session content: no messages, tool calls or output. When you cannot sign in to GitHub, the printed link still carries the whole report except its diagnostics; when the report is too long for a link, the command says to attach the file instead.

## What the report holds

The version and platform, how Fiber was installed, the description, the `fiber doctor` output verbatim, and a Diagnostics section. What each part means is [Commands and flags](../invocation.md#commands-and-flags).

## Diagnostics

The Diagnostics section helps with a problem in one session. It holds:

- the session's id, and the command that exports its log, `fiber sessions export <id>`. The export holds your prompts and the model's replies, so send it privately to whoever asked, never in an issue;
- the session's diagnostic log lines, which record failures and timings and never your prompts, the model's text, a tool's arguments, a credential or a setting's value. An extension's own `extension_log` lines are left out, because Fiber cannot vouch for what an extension wrote;
- the hub's lines from the last day, and any older hub line that names the session;
- each crash report for the session, with the thread, the source location and the backtrace, and without the panic message.

`fiber feedback` uses the newest session in the current project. Name another session, from any project, with `--session <id>`; an id that names no session, or a delegate, fails with `session_not_found` and writes no report. With no session, the section holds the hub's lines only. In the terminal, `/feedback` uses the session on screen; a session on another machine's hub has no diagnostic lines here, and the report says so.

The report holds at most the newest 500 hub lines and 2,000 session lines. Errors and warnings are kept first, and the report says how many earlier session lines it left out. Both limits were chosen, not measured ([Fiber home](../state.md#what-each-part-holds)). Anything Fiber is unsure of is left out. The Diagnostics section is in the saved file only, not in the issue link, so read the file before you attach it.

## Where it goes

The report is written to `fiber-feedback-<time>.md` in the current directory, and the command prints its path with the issue link. The terminal prints both as a notice ([Slash commands](../tui.md#slash-commands)).

## Exit code

`fiber feedback` exits 0 once the report is written, even when the doctor output it carries reports a blocker. An empty description is a usage error, as on [`fiber ask`](../invocation.md#getting-a-prompt-in).
