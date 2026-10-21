# Reporting a problem

Run `fiber feedback "<description>"` in a shell, or `/feedback <description>` in the terminal, to write a feedback report and print the issue link that files it. It carries no session content: no messages, tool calls or output. When you cannot sign in to GitHub, the printed link still carries the whole report; when the report is too long for a link, the command says to attach the file instead.

## What the report holds

The version and platform, how Fiber was installed, the description, and the `fiber doctor` output verbatim. What each part means is [Commands and flags](../invocation.md#commands-and-flags).

## Where it goes

The report is written to `fiber-feedback-<time>.md` in the current directory, and the command prints its path with the issue link. The terminal prints both as a notice ([Slash commands](../tui.md#slash-commands)).

## Exit code

`fiber feedback` exits 0 once the report is written, even when the doctor output it carries reports a blocker. An empty description is a usage error, as on [`fiber ask`](../invocation.md#getting-a-prompt-in).
