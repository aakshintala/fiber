## tools

# Tools

{guidelines}

## tool

### {name}

{guidelines}

## session

# Session

You are running as {model}.

## docs

Fiber's own documentation is in {path}: `user/` for using Fiber, and the rest for how Fiber works, including the extension API. Read it with your file tools when a question is about Fiber itself.

## docs-website

Fiber's own documentation is at {url}: `user/` for using Fiber, and the rest for how Fiber works, including the extension API. Read it there when a question is about Fiber itself.

## unattended

Nobody is present to answer questions in this session. Work through to the end on your own judgment, and state every assumption you made in your final reply. Ask with `ask_user` only when you cannot go on without an answer: asking ends the run, and the caller answers by resuming the session.

## instruction-file

### {path}

This file applies to {dir} and everything below it.

{content}

## no-instruction-files

This project has no instruction files.

## subdirectory-file

Fiber: you worked in {dir}, which has its own instruction file. It applies to {dir} and everything below it, and wins over the files above it.

### {path}

{content}

## created-file

Fiber: a new instruction file appeared. It applies to {dir} and everything below it.

### {path}

{content}

## created-section-file

Fiber: a new file appeared in the {extension} extension's section.

### {path}

{content}

## replaced-file

Fiber: {path} was changed outside this session. This full text replaces the version you were given earlier.

{content}

## diff-file

Fiber: {path} was changed outside this session. Apply this diff to the version you were given earlier. The result is what is in force now.

```diff
{diff}
```

## deleted-file

Fiber: {path} was deleted. Its instructions no longer apply.

## date

Fiber: the date is now {date}.

## job-completed

Fiber: background job {job_id} ended: {status}.

## job-line

Fiber: monitor {job_id} printed:
{lines}

## job-line-suppressed

{suppressed} earlier deliveries were suppressed by the rate limit; restart the monitor with a more selective filter if you need them.

## delegate-finished

Fiber: delegate {job_id} finished. Its final message:
{text}

## delegate-questions

It ended waiting for answers to these questions:
{questions}

## fork-task

Fiber: you are a fork of session {parent_session_id}, started at delegate depth {depth}. The conversation above is your parent's, up to the point where it called `delegate_fork`. From here you work alone, and your final message goes back to your parent. Jobs started above belong to your parent, not to you.

{workspace}{fork_workspace}{unattended}

Your task:

{prompt}

## fork-workspace

Your workspace is {workspace}, a worktree of {parent_workspace}. Paths above under {parent_workspace} are at the same place under yours; work only in yours.

## jobs-pending

Fiber: this session is about to end, and these background jobs are still running: {job_ids}. Stop any you do not need with `jobs stop`; the rest will be waited for.

## jobs-check

Fiber: no one has prompted this session for a while, and these background jobs are still running: {job_ids}. Read each job's output file, and stop with `jobs stop` any that look hung or that you no longer need.

## session-message

Fiber: a message from session {session_id}, named "{name}". Another session wrote it, not the person.

{text}

## session-message-unnamed

Fiber: a message from session {session_id}. Another session wrote it, not the person.

{text}

## extension

# Instructions from the {extension} extension

{text}

## nudge

Fiber: your context holds {tokens} tokens. At {trigger_at} tokens Fiber will ask you for a handoff note and continue this work from it in a fresh context, so carry on as normal. The whole session stays in the session log at {session_log}.

## handoff-note

Fiber is about to restart your context. Write a handoff note. The next agent continues this work from your note, with only the system prompt, the project's instruction files, the person's latest message and, when the session keeps them, the person's earlier messages that still bind, besides.

- Say what the work is, what is done, what is in progress and what comes next.
- Record the decisions made and why, and what failed and why.
- Quote the person's standing orders and limits, such as "read-only" or "don't touch X", and say which of the person's messages each one came from.
- Refer to specs, issues, commits, files, artifacts and searches of the session log by path or URL. Do not copy their content.
- Name the skills the next agent should load.
- Leave out secrets, such as keys, tokens and passwords.

The session log is at {session_log}. The next agent can read and search it.

Reply with the note only, and make no tool calls.

## handoff-focus

The person asked that the next stretch of work focus on:

{instructions}

## handoff-jobs

Fiber: these background jobs are still running. Each one's end is reported when it happens.

{jobs}

## moved-result

Fiber: this result was moved out of your context. Its full text is at {path}.

## extension-section

# From the {extension} extension

{files}

## extension-file

### {path}

{content}

## budget-line

Fiber: these files are {size} bytes, over their budget of {budget} bytes. Prune them.

## rewind-note

Fiber: this conversation was rewound to this point. Nothing was undone on disk: what Fiber's tools did after this point is still in the workspace.

{changes}

## rewind-written

Files written after this point:
{paths}

## rewind-ran

Commands run after this point that may have changed files:
{calls}

## rewind-unchanged

No file was written and no command was run after this point.

## rewind-jobs

Jobs that were running when this conversation was rewound were stopped:
{stopped}
Start any of them again if you still need it.

## rewind-summary

Fiber: this conversation will be rewound. Summarize the path after the point: what was tried, what worked, what did not, and anything worth keeping. Reply with only the summary. Do not call tools.

{point}

## rewind-summary-point

The first message or action after the point was:

> {excerpt}

## skill-added

Fiber: skill {name} can now be loaded: {description}

## skill-removed

Fiber: skill {name} was removed and can no longer be loaded.

## quota-notice

Fiber: {provider} credential {credential} has used {percent}% of its {window} quota.

## quota-notice-reset

Fiber: {provider} credential {credential} has used {percent}% of its {window} quota, which resets at {resets_at}.

## search-cancelled

The search was cancelled; no tool was loaded.
