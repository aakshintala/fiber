## tools

# Tools

{guidelines}

## tool

### {name}

{guidelines}

## session

# Session

You are running as {model}.

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

## jobs-pending

Fiber: this session is about to end, and these background jobs are still running: {job_ids}. Stop any you do not need with `jobs stop`; the rest will be waited for.

## jobs-check

Fiber: no one has prompted this session for a while, and these background jobs are still running: {job_ids}. Read each job's output file, and stop with `jobs stop` any that look hung or that you no longer need.

## extension

# Instructions from the {extension} extension

{text}

## nudge

Fiber: your context holds {tokens} tokens. At {trigger_at} tokens Fiber will ask you for a handoff note and continue this work from it in a fresh context, so carry on as normal. The whole session stays in the session log at {session_log}.

## handoff-note

Fiber is about to restart your context. Write a handoff note. The next agent continues this work from your note, with only the system prompt, the project's instruction files and the person's latest message besides.

- Say what the work is, what is done, what is in progress and what comes next.
- Record the decisions made and why, and what failed and why.
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

## skill-added

Fiber: skill {name} can now be loaded: {description}

## skill-removed

Fiber: skill {name} was removed and can no longer be loaded.
