# Tools

What every tool shares: what it declares, how a call is checked and bounded,
and what its result carries on the log and to the model. This is what is true
now, not a plan. It is settled by
[The tool contract: what every tool shares](https://github.com/aakshintala/fiber/issues/14);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `CONTEXT.md`. Tool call, effect, artifact, participant, seam,
extension and event mean what it says there and nothing else. How effects are
judged is `docs/permissions.md`; the events themselves are `docs/events.md`.

## What a tool declares

- A name, a description, and an input schema written in JSON Schema.
- An effects function. Fiber calls it with each call's arguments before
  permission is decided; it returns the call's effects, whether it is
  reversible, and the paths it touches, in the vocabulary of
  `docs/permissions.md`. Classification is per call, not per tool
  (`docs/permissions.md`).
- Optionally: guideline lines for the system prompt, for guidance that spans
  calls, such as which tool to prefer for a job (`docs/system-prompt.md`,
  "Tool guidelines").
- Optionally: which end of long output to keep (`head` by default; a shell-like
  tool declares `tail`), and its own size cap.
- There is no read-only flag and no parallel-safety flag. Calls in a step run
  concurrently; file safety comes from the per-path lock in
  `docs/architecture.md` ("Tool calls in a step").
- Adapting a schema to each wire protocol, and carrying images to a protocol
  that cannot take them in a tool result, is the provider module's job, not the
  tool's.

## Before a call runs

- Arguments are checked against the input schema before the effects function is
  called. A call that fails the check completes as `failed` with code
  `invalid_arguments` and never writes `tool_call_started`, so the log proves
  it never ran. The model is told what is wrong, one line per bad field.
- A call naming a tool that does not exist completes as `failed` with code
  `unknown_tool`, never starts, and the model is told which names exist.
- An effects function that itself errors (for example a bug in an extension)
  fails closed: the call completes as `failed` with code `tool_error` and never
  runs.
- A `before_tool` hook then sees the call and may rewrite its arguments or
  refuse it (`docs/extensions.md`, "Hooks"). Rewritten arguments go through
  the schema check and the effects function again. The model's call is sent
  back to it as written, and the result's `content` begins with a line giving
  the arguments that ran.

## What a result carries

`tool_call_completed` carries, beside `status`, `reason`, `error` and `process`
(defined in `docs/events.md`):

- `content`: text and image parts. It is exactly what the model is sent,
  including after an `after_tool` hook has rewritten it. The hook runs on the
  full output before the cut, so the completed line and the artifact both hold
  what the hook returned (`docs/extensions.md`, "Hooks").
- `details`: JSON for clients, such as an edit's diff for the terminal to draw.
  It is never sent to the model and the loop never reads it. A client that does
  not recognise a tool's `details` shows `content` instead: tool identity is an
  opaque name and an extension can replace any tool, so no client may depend
  on one tool's `details`.
- `artifact`: the path to the full output, present when the result was cut or
  when an `after_tool` hook returned text for the artifact.
- `control`: instructions to the loop, absent on most results. The one field
  defined is `handoff`, a handoff note: the loop restarts the model's context
  from it at the step boundary (`docs/handoff.md`). Any tool may set it; the
  loop acts on the field, never on which tool set it.
- Images are written to the session's `artifacts/` (see `docs/state.md`) and
  referenced by path, never inlined as base64 in the log.
- The loop reads `status`, `error.code`, `control` and the declared effects,
  never `content` or `details`. A tool's prose cannot steer control flow.
- A `failed` status is sent to the provider as that protocol's error flag on
  the tool result.

## What a result proves

Content states what was observed, never "success": the lines an edit changed, a
command's exit code, the bytes written. Each tool's specification carries this
as an acceptance criterion; nothing can check it mechanically. Whether a call
ran at all is proved by the log structure (`docs/events.md`, resume table), not
by content.

## Bounded results

- A tool that declares no cap is cut at 16 KiB of model-facing content.
  Configuration can override any tool's cap (`docs/configuration.md`). A tool may declare a larger or
  smaller cap: web fetch sets its own in
  [Web fetch and web search](https://github.com/aakshintala/fiber/issues/57).
  `read` keeps the 16 KiB default ("File tools").
- A cut result keeps the declared end (head or tail), a notice saying it was
  cut, and the artifact path. Nothing is lost, only moved out of the model's
  view. The full output is in the session's `artifacts/`. `read` is the
  exception: the file is the full output, so a cut read writes no artifact
  and its notice gives the offset to continue from.
- The model reads the rest with the ordinary `read` tool on that path. There is
  no dedicated tool for it. A read of `artifacts/` has only the `reads` effect,
  so it is never reviewed.
- There is no cap across one step's results. When they overflow the context
  window, the overflow rule in `docs/handoff.md` ("Overflow") moves them to
  the session's `artifacts/`.
- In the owner's 648 pi sessions (measured 2026-09-22 with
  `research/tool-result-sizes/sizes.py`; sizes, so they do not depend on the
  platform), 16 KiB cuts 1.2% of 26,829 shell results and almost no result of
  any other tool except file reads (16.8% of 5,070), search (about 9%) and web
  fetch (24% of 21). Those are the tools where the model asked for exactly the
  content, which is why they declare their own cap.

## Progress

While a call runs, it may stream output and progress as `tool_call_delta`,
which is ephemeral. Fiber paces updates to at most one every
`max(100 ms, encoded bytes ÷ 100 KiB/s)`; the first change after idle goes out
immediately, held changes collapse to the latest, and completion forces a final
flush. These are pi's numbers, not measured for Fiber.

## Cancellation

`cancelled` means the tool stopped. Fiber interrupts every kind of tool itself
rather than asking it to stop: it kills a process's process group ("Shell",
"Stopping a command"); it raises an error inside running Lua through mlua's
instruction hook (`Lua::set_hook`, whose
documentation says the error "will be propagated through the Lua code that was
executing"); it closes the socket of a network call made through the host, as it
does for a model request; and a built-in Rust tool checks for cancellation
between chunks of work. The loop writes `tool_call_completed` with
`status: cancelled` only after the tool has returned, so the log never calls a
call cancelled while it can still change something. The one wait it cannot cut
short is a read blocked in the kernel, such as on a hung network filesystem.
The second is an MCP call, which Fiber can only ask to stop: it ends `failed`
with code `mcp_cancel_requested`, never `cancelled` (`docs/mcp.md`, "Calls").

## File tools

Settled by
[File tools: read, write and edit](https://github.com/aakshintala/fiber/issues/52);
that ticket's resolution holds the rationale and the rejected alternatives.
How pi, codex, Claude Code and fiber-zig do it is
[research/file-tools/reference-agents.md](../research/file-tools/reference-agents.md);
the owner's usage is
[research/file-tools/usage.md](../research/file-tools/usage.md).

There are three: `read`, `write` and `edit`. There is no listing tool, and
`read` on a directory fails with `unsupported_file` and points to the shell.
Search is
[Search: built-in tools or the shell?](https://github.com/aakshintala/fiber/issues/54).

A relative path is resolved against the workspace. A symbolic link is resolved
to its target, and the target is the path the call declares, so permission and
the credential deny (`docs/permissions.md`) judge where the bytes really go.

### read

- Arguments: `path` (required); `offset`, the first line to return, counted
  from 1; `limit`, the number of lines; `pages`, a page range such as `1-5`,
  for a PDF only.
- Text comes back as it is in the file, without line numbers and without a
  byte order mark. Nothing needs stripping before it is copied into an edit.
- The cap is the 16 KiB default ("Bounded results"). A cut falls at a line
  boundary, and the notice gives the lines shown, the file's total and the
  `offset` to continue from. No artifact is written.
- A single line longer than the cap is cut inside the line. The notice gives
  the byte where it was cut and says the rest needs a byte-range read through
  the shell, such as `cut -c` or `dd`.
- An `offset` past the end of the file fails with `invalid_arguments` and
  gives the file's line count.
- A PNG, JPEG, GIF or WebP file comes back as an image part. The provider
  module resizes it to the provider's limit. For a model that cannot take
  images, the image is left out and the result says so.
- A PDF comes back as a PDF part. One of more than 10 pages needs `pages`,
  and a request takes at most 20 pages; these are Claude Code's numbers. The
  provider module sends the PDF natively where its protocol accepts a PDF in a
  tool result, and otherwise sends the pages rendered as images. Rendering uses
  poppler's `pdftoppm`; when it is not installed, the call fails with
  `tool_error` and a message naming the package. Which protocols accept a PDF
  is
  [Probe: which protocols accept a PDF in a tool result](https://github.com/aakshintala/fiber/issues/121).
- A path that does not exist fails with `not_found`.
- Any other file that is not UTF-8 text, and any directory or device, fails
  with `unsupported_file`, giving its size and detected type.
- Effects: `reads`, reversible, with the resolved path.

### write

- Arguments: `path` and `content`, both required.
- It creates the file, and any missing parent directories, or replaces an
  existing file.
- Replacing a file keeps that file's line-ending style (CRLF or LF) and its
  byte order mark. A new file is stored as given.
- The result says whether the file was created or replaced, with its size in
  bytes and lines.
- Effects: creating a file is a reversible `writes`; replacing one is an
  irreversible `writes`. Both carry the resolved path.

### edit

- Arguments: `path` and `edits`, a list of one or more blocks, each
  `{ old_text, new_text }`. There is no replace-all.
- Every block is matched against the file as it was before the call. Each
  `old_text` must occur exactly once, and blocks may not overlap. Either every
  block applies and the file is written once, or nothing is written.
- Matching is exact first. When a block's exact text is not found, it is
  matched again with trailing spaces on each line ignored and Unicode quotes,
  dashes and spaces folded to their ASCII forms (pi's rule). Only the lines a
  block touches take the new text; every other line keeps its original bytes.
  A block is unique if it is unique in the form it was matched in.
- Matching sets the byte order mark aside and treats line endings as LF. The
  file is written back with its own line-ending style and byte order mark.
- Failures: a block not found fails with `no_match`; a block found more than
  once fails with `ambiguous_match`, giving the count. Both name the block by
  its index. An empty `old_text`, overlapping blocks, or edits that leave the
  file unchanged fail with `invalid_arguments`. A file that does not exist
  fails with `not_found`, and one that is not UTF-8 text with
  `unsupported_file`.
- The result gives, for each block, the lines it replaced and the lines the new
  text now occupies, and says when a block matched only after normalising.
  The diff goes in `details` for clients and is not sent to the model.
- Effects: an irreversible `writes`, with the resolved path.
- In the owner's pi sessions, 32.5% of 3,837 edits carried more than one
  block, and up to 27. Claude Code's replace-all was used in none of 505
  edits.

### Stale files

- An edit carries its own check: every block must still match the current
  file, and everything outside the blocks is kept.
- A `write` that would replace an existing file is refused with `stale_file`
  unless this session has seen the file's current content. Seen means a
  `read` of the file (any range) or the session's own `write` or `edit`.
  Fiber compares a hash of the whole file, taken when it was read or written,
  with a hash of the file now. Modification times are not used.
- The message says the file changed or was never read, and to read it first.
  A file changed by the session's own shell command, such as a formatter,
  counts as changed.
- What was seen is held in memory for the live context. It is cleared at a
  handoff, because the model's context no longer holds the file
  (`docs/handoff.md`), and it starts empty after a resume, a fork or a rewind.
  Nothing is logged for it.
- In the owner's Claude Code sessions, Claude Code's refusal to write a file
  the model had not read fired 26 times; its refusal of a file modified since
  it was read never fired. 86% to 94% of writes created files never read in
  the session.

### How a change lands

- `write` and `edit` write a temporary file in the same directory and rename
  it over the target, so a crash leaves the old file or the new one, never
  half of either. The file's permission bits are kept.
- A file with more than one name on disk (a hard link) is written in place
  instead, so every name sees the change. A crash during that write can leave
  it partly written.
- Under the per-path lock (`docs/architecture.md`, "Tool calls in a step"),
  just before writing, the path is resolved again. If a symbolic link now
  points somewhere other than the path permission judged, the call fails with
  `path_changed` and nothing is written.

## Shell

Settled by
[Shell: running a command, and when it becomes a job](https://github.com/aakshintala/fiber/issues/53);
that ticket's resolution holds the rationale and the rejected alternatives.

### Running a command

- The command runs as `/bin/bash -c <command>`, or `sh -c` where `/bin/bash`
  does not exist. It gets the environment Fiber was launched with and reads
  no shell startup files.
- It runs in a new process session and its own process group, with no
  controlling terminal, and standard input connected to nothing
  (`/dev/null`). Standard output and standard error are one stream, in the
  order they arrive.
- With no terminal and nothing on standard input, a command that prompts
  fails at once instead of waiting forever. Probed on macOS (2026-09-23):
  `read` returned an empty answer, opening `/dev/tty` failed with "Device
  not configured", `sudo` failed with "a terminal is required", git asking
  for HTTPS credentials failed in 1.9 s, and Python's `input()` raised
  EOFError. There is therefore no warning for a command waiting on a
  prompt.
- Arguments: `command` (required); `workdir` (optional; defaults to the
  workspace; a relative path is resolved against the workspace);
  `timeout_ms` (optional); `run_in_background` (optional); `tty`
  (optional). Each call starts fresh: a `cd` does not carry over to the
  next call.
- A bare wait is rejected. When the call is not `run_in_background` and
  the first part of the command is `sleep N` with N of 25 seconds or more,
  alone or followed by other commands, the call completes as `failed` with
  code `invalid_arguments` and never starts. The message points to
  `run_in_background`, `jobs wait`, or a monitor running an `until` loop.
  A `sleep` inside a loop is not the first part of the command and is
  allowed. This is Claude Code 2.1.280's rule, read from its binary.

### Timeout

- `timeout_ms` is the one thing that kills a command for running long.
  When the model gives none it is 600,000 (10 minutes). There is no
  maximum. It counts from when the command started.
- It applies to every command, run in the foreground or as a job, and it
  stays with a command after the command moves to the background. This is
  where a hang is bounded: "Background jobs" has no cap on waiting.
- A command that times out ends `failed` with code `timeout` and
  `process.timed_out` true.
- The unit is milliseconds, as in Claude Code and codex. In the owner's pi
  sessions (pi's timeout is in seconds), 1,347 of 9,372 timeouts the model
  set (14%) were 10,000 or more, milliseconds written into a seconds field,
  which made a wait 1,000 times longer; the opposite mistake in a
  milliseconds field kills the command within a second, so the model sees
  it straight away.
- Of 26,687 foreground shell commands in the owner's pi sessions, 4 ran
  past 10 minutes without a timeout the model set, and 3 of those 4 were
  hangs (115 minutes, 291 minutes and 11.2 hours).

### Moving to the background

- A command moves to the background, becoming a job ("Background jobs"),
  when any of these happens:
  - It has run for 30 seconds.
  - It was started with `run_in_background` or `tty`, in which case it
    moves at once.
  - A steering message arrives while it runs, so the message reaches the
    model at the next step boundary instead of waiting for the command.
  - A driver sends the `background` driver command (`docs/invocation.md`).
  - Its shell exits while processes it started are still running ("When a
    command ends").
- Moving never kills anything and never restarts anything. The tool call
  completes with the job's receipt, which says the command is still
  running, why it moved, the `job_id` and the output file's path. Output
  already produced and all later output go to the job's output file.
- 5.1% of the owner's foreground pi commands ran longer than 30 s (8.6%
  longer than 10 s, 2.9% longer than 2 minutes). Claude Code moves a
  command at its 2-minute timeout; codex returns a still-running command
  after 10 s.

### When a command ends

- A command runs until its process group is empty, not until its shell
  exits and not until its output pipe closes. Fiber checks whether the
  group still has members (`kill(-pgid, 0)`).
- If the shell exits and processes it started are still in the group, the
  command moves to the background at once. The receipt gives the shell's
  exit code and names what was left running (command names and process
  ids). The job ends when the group is empty, and keeps the command's
  timeout.
- A process that left the group on purpose (for example with `nohup` or
  `setsid`) is not tracked.
- The longest call in the owner's pi sessions, 11.2 hours, started a local
  server with `&`; the server held the output pipe open and pi waited for
  the pipe to close.

### Stopping a command

- A timeout, a cancellation and a `jobs stop` all stop a command the same
  way: SIGTERM to the whole process group, then SIGKILL to the group
  800 ms later if any member is still alive. 800 ms is the value from the
  archived Zig tree (fiber-zig). The grace period exists because programs
  clean up on SIGTERM: git, for example, removes its `index.lock` on
  SIGTERM and leaves it behind on SIGKILL.
- After the kill, Fiber reads output for at most 2 seconds more (codex's
  drain bound). If output is still held open after that, for example by a
  descendant that escaped the group, Fiber stops reading and the result is
  `failed` with code `indeterminate`, never `completed`.
- A shutdown (`docs/invocation.md`, "Shutdown") uses the same two values, on
  every group at once.

### Result and output

- Exit code 0 is `completed`. A nonzero exit is `failed` with code
  `nonzero_exit` and `process.exit_code`. A command killed by a signal
  Fiber did not send is `failed` with code `signal` and `process.signal`.
  A timeout is as above; a cancellation is `cancelled`.
- Output follows "Bounded results": the shell declares `tail`, the default
  16 KiB cap applies, the full output is in the session's `artifacts/`,
  and output streams as `tool_call_delta` while the call runs. After a
  move to the background, output goes to the job's output file.

### Terminal (`tty`)

- With `tty: true` the command runs in a pseudo-terminal instead of pipes,
  so a program that behaves differently on a terminal, or waits for typed
  input (a REPL, `git rebase -i`, a debugger), can be driven. It moves to
  the background at once; the receipt carries whatever output arrived in
  the first 250 ms.
- The model types into it with the `jobs` action `write` ("Background
  jobs"). Output is the terminal's raw bytes. Fiber keeps no screen model
  and runs no separate terminal host process. codex works the same way
  (opt-in `tty`, raw bytes, typed input through a timed wait).
- Prompts do not fail at once on a terminal; that is the point of `tty`.
  The timeout still bounds the command.

### Effects

- The shell tool classifies each call (`docs/permissions.md`, "Effects").
  It splits the command on `&&`, `||`, `;` and `|`, and checks each part
  against a fixed list of read-only commands and the flags allowed for
  each. If every part is on the list, the call declares `reads`,
  reversible, with the paths the command names, resolved against
  `workdir`. Otherwise it declares `executes`, with no paths.
- It declares `executes` whenever it finds something it cannot read
  plainly: command substitution (`$( )` or backticks), process
  substitution, a redirect, or anything else outside the list.
- Flags matter because read-only-looking commands have writing or
  executing flags: `git diff --output=<file>` writes a file,
  `rg --pre <cmd>` and `find -exec` run programs, `find -delete` deletes,
  `sort -o` writes. The list and its flag rules are part of building the
  shell tool.
- A call declared `reads` takes the permission fast path and is allowed in
  `readonly` mode.
- The credential deny (`docs/permissions.md`, "Credentials") sees paths
  only for commands the recogniser understands. A command it does not
  understand, such as `python -c` opening a file, declares no paths, so
  the deny cannot see it; in `auto` and `ask` it is still reviewed, and in
  `yolo` nothing stops it. Closing that gap needs confinement:
  [Does Fiber confine what tools can touch?](https://github.com/aakshintala/fiber/issues/30).
- If Fiber confines tools, a shell call declared `reads` runs confined to
  read-only access, so a command wrongly on the list fails instead of
  writing.
- The built-in shell is trusted to classify because it is compiled in. An
  extension that replaces the shell classifies its own calls and is
  believed, as `docs/permissions.md` already states.

## Background jobs

Settled by
[Background jobs: one killable object](https://github.com/aakshintala/fiber/issues/20);
that ticket's resolution holds the rationale and the rejected alternatives.
The kinds are `docs/events.md`.

- One object: one `job_id`, one lifecycle, stopped the same way whatever runs
  inside it. A job is a shell command, a monitor or a delegate
  (`docs/delegates.md`).
- The call that starts a job completes in its own turn with a receipt naming
  the `job_id` and the path of the job's output file in the session's
  `artifacts/`. There is no pending status. Every provider needs a tool result
  before the model's next step, so a call held open across turns would stall
  the turn.
- A job's output streams to that file. The model reads it with the ordinary
  `read` tool ("Bounded results"). There is no output action.
- The model-facing tool is one `jobs` tool with actions `list`, `wait`,
  `stop` and `write`. `wait` blocks up to a timeout. Cancelling a wait (for
  example because the turn is cancelled) stops only the wait and leaves the
  job running. `write` sends input to a job started with `tty` and returns
  the output that arrives within a wait after the write, 250 ms by default,
  at most 30 seconds. A write with no input waits at least 5 seconds
  (codex's floor; [fiber-zig#8](https://github.com/aakshintala/fiber-zig/issues/8)
  found shorter empty polls burned turns). `write` on a job not started with
  `tty` fails with `invalid_arguments`. A `write` call declares `executes`,
  because typed input can make the program do anything. `jobs` only sees
  and acts on jobs the calling session started, so a delegate cannot stop its
  parent's work.
- Completion reaches the model by waking it. If the loop is idle, a finished
  job starts a new turn whose input names the job or jobs. If a turn is
  running, the news joins it at the next step boundary, the way a steering
  message does. Jobs finishing together are delivered together in one turn,
  not one turn each. If a `jobs wait` already returned a job's final state to
  the model, no completion notice is sent for it.
- A monitor is a job running a watch command. Lines on its standard output
  are delivered to the model in batches, the same way as a completion.
  Standard error goes to a separate file and never reaches the model. It
  ends when its command exits, it is stopped, or its deadline passes.
  - Batching: lines are joined into one delivery. Each line is cut at 500
    characters and each delivery at 3,000 characters, with a marker saying
    it was cut. The full output stays in the job's output file.
  - Rate: deliveries draw from a budget of 10, refilled one every 2 seconds.
    A delivery that finds the budget empty is dropped, and the model is
    later told how many were suppressed and that it should restart the
    monitor with a more selective filter. The monitor keeps running.
  - Flood stop: after 30 seconds of continuous suppression the monitor is
    stopped as `failed` with code `flooded`, and the model is told to
    restart it with a more selective source.
  - Deadline: every monitor has a deadline, 5 minutes by default, at most
    30 minutes, and at most 10 minutes in a non-interactive run. The model
    may set a shorter or longer one within those limits when it starts the
    monitor. At the deadline the monitor ends as `failed` with code
    `timeout`, as a timed-out shell call does (`process.timed_out`). The
    model is told the monitor expired and can start it again. The deadline
    is what stops a forgotten monitor from holding a non-interactive run
    open under the "ending with jobs running" rule in this section.
  - Source and testing: these are Claude Code's numbers, taken as they are.
    Timing-dependent behaviour is tested under an injected clock, never by
    waiting in real time.
- A job whose output file passes 5 GB is stopped as `failed` with code
  `output_cap` (Claude Code's documented kill threshold).
- Stopping one job uses the same mechanism as cancelling a tool call
  ("Shell", "Stopping a command"). If a descendant that escaped the process
  group still holds the output pipe open past the bound, the job ends
  `failed` with code `indeterminate`, never `completed`. A stopped job ends
  `cancelled`.
- Jobs live and die with the Fiber process. Nothing reattaches to a job after
  a restart. A shutdown stops every job (`docs/invocation.md`, "Shutdown"); a
  crash stops none, and the jobs keep running unwatched.
- When a session is about to end with jobs still running — a non-interactive
  run whose model has given its final answer, `close` or stdin EOF on
  `fiber serve`, or a delegate finishing its task — Fiber wakes the model
  once with a notice listing the running jobs, telling it to stop the ones it
  does not need and that the rest will be waited for. Whatever is still
  running after that is waited for, whatever its kind, and each completion
  wakes the model. The session ends when it is idle with no jobs running.
  There is no cap on this wait: a hang is bounded at the command that hangs
  (the shell tool's timeout, "Timeout") and by the caller's SIGTERM
  (`docs/invocation.md`, "Shutdown"),
  because a cap on the waiter cannot tell a hang from long healthy work such
  as a CI watch.
- There is no cap on running jobs, except that a session runs at most 10
  delegates at once (`docs/delegates.md`, "Limits"). Parked threads are
  measured in `docs/architecture.md` ("The threads").

## Built in or extension

A first-party tool is compiled in unless its behaviour depends on a vendor or
on the person's environment. Read, write, edit, shell, background jobs, the
Fiber delegate harness, the task list, asking the person, web fetch and
`handoff` (`docs/handoff.md`) behave the same for everyone and are compiled in, as is search if
[Search: built-in tools or the shell?](https://github.com/aakshintala/fiber/issues/54)
keeps it as a tool.
The default tool set therefore never needs a Lua VM, and a headless run never
fails with `extension_missing` for one of them.
Built-ins register through the tool seam exactly as an extension does and can be
replaced by name (`docs/architecture.md`, "Tool seam").

The MCP client is compiled in too. Each tool an MCP server offers registers
through the tool seam as `mcp__<server>__<tool>` and can be replaced by name
like any built-in. How MCP tools are named, declare effects and fail is
`docs/mcp.md`.

Four kinds ship as extensions:

- Provider quota: each provider reports it differently, and providers are
  already extensions, so the quota lookup lives in each provider's package.
  Detail belongs to
  [Provider quota the model can see](https://github.com/aakshintala/fiber/issues/58).
- Web search backends: each search service has its own API, key and response
  shape. How the web search tool splits from its backend is
  [Web fetch and web search](https://github.com/aakshintala/fiber/issues/57)'s.
- Compact build and test output (the owner's `structured_return`): parsing
  depends on the person's toolchain, so it is an extension over an after-tool
  hook that replaces `content`, with the full log in the artifact.
- Delegate harnesses other than Fiber, such as Claude Code and cursor-agent:
  each runs another vendor's agent program (`docs/delegates.md`,
  "Harnesses"). What a harness extension declares belongs to
  [Harness extensions: running another agent as a delegate](https://github.com/aakshintala/fiber/issues/77).

## Which tools the model sees

Settled by
[Which tools the model sees, and when](https://github.com/aakshintala/fiber/issues/51);
that ticket's resolution holds the rationale and the rejected alternatives.

Every tool is declared on every request, in full or deferred. A deferred tool
is sent as its name only. The model loads its full definition when it needs
it, and the definition is appended to the conversation, so the cached prefix
holds (`docs/prompt-cache.md`, "Deferred tools"). The tool set, and which of
its tools are deferred, change only when the preamble is built.

### Deferral is a property of the model

- A model's provider data says whether deferral works for it
  (`docs/model-routing.md`). It says so only after a probe shows a deferred
  tool loading through that provider, because support varies within a
  protocol: probed on September 24, 2026, Muse's `openai-responses` endpoint
  deferred and kept the cache, while OpenRouter's accepted the same request for
  GPT-6 Luna, sent every definition in full and let the model call a deferred
  tool without loading it.
- Anthropic and OpenAI Responses (gpt-5.4 and later) both defer natively.
  `openai-completions` and `google-generative-ai` do not.
- On a model without deferral, every tool is declared in full and
  `tool_search` is not declared.

### What is deferred by default

- Every tool declares whether it is deferred by default. Configuration can
  override that for any tool (`docs/configuration.md`), and an MCP server can be marked eager
  (`docs/mcp.md`).
- MCP tools and `mcp_resources` are deferred by default. Every other built-in
  is declared in full.
- A built-in is deferred by default only when it is used in under about 2% of
  the owner's sessions and is not part of how they direct delegates. Measured
  on September 25, 2026, across 686 pi sessions, 171 Claude Code sessions and
  143 Claude Code subagent sessions, no built-in except `mcp_resources` meets
  that. Web fetch, the rarest candidate, is used in 1% of pi sessions but 11%
  of Claude Code sessions.
- Deferring has a cost. Claude Code defers widely, and its tool search runs in
  48% of those Claude Code sessions.

### Tool search

- `tool_search` is a built-in tool the model calls to load deferred tools. It
  is declared, in full, only when the model supports deferral and at least one
  tool is deferred.
- Fiber runs the search itself, as codex does: BM25 over each deferred tool's
  name, description and parameter names. It returns at most 8 tools by default;
  the model may pass `limit`.
- The result goes back in each protocol's native form: `tool_reference`
  blocks on Anthropic, `tool_search_output` on OpenAI Responses. Fiber never
  uses a provider's own search, so every provider behaves the same and every
  search is on the log.
- Its description lists the sources of the deferred tools (each MCP server's
  name and description), fixed when the preamble is built.
- A search is an ordinary tool call. A resume or fork re-sends its result
  byte for byte.
- A loaded tool stays loaded until a handoff, which restarts the conversation
  after the preamble (`docs/handoff.md`); after that the model searches again.
- A call to a deferred tool that was never loaded runs like any other call,
  after the schema check.

### Size warning

When the definitions declared in full take more than 10% of the model's
context window, the preamble build is followed by a `notice` with code
`tool_definitions_large`. It names the largest sources and the configuration
that disables tools. The session runs anyway. 10% is the threshold at which
Claude Code's opt-in automatic mode starts deferring tools.

### Seeing the tools

- `/tools` in the terminal, and the `tools` driver command
  (`docs/invocation.md`), list every declared tool with:
  - its source: built-in, extension or MCP server
  - its state: full, deferred or loaded
  - its approximate size in tokens
- A tool's size in tokens is estimated from its size in bytes. The
  bytes-to-tokens rate comes from the last preamble build: the tokens its first
  request wrote to the cache (`usage_recorded`), divided by the preamble's size
  in bytes. Before a first request, sizes are shown in bytes.
- `preamble_built` records, for each tool definition, whether it was deferred
  (`docs/events.md`).

### Size budget in CI

- CI fails the build when the built-in tool definitions, serialised as sent,
  grow past a total budget in bytes. CI counts bytes because it cannot count
  tokens without calling a provider.
- The budget is set from the total when the built-ins are first written.
  Raising it is an explicit change in the same pull request that grows a
  definition.
- Every CI run prints the size of each built-in definition, so the tool that
  grew can be seen without reproducing the build.
- The owner's pi setup spent about 13,800 tokens a request on 31 tools
  ([pi-extensions#1](https://github.com/aakshintala/pi-extensions/issues/1)).

## Not settled here

- Whether a call that started but never finished may be re-run after a crash:
  [Revisit: may a tool that never finished be re-run after a crash?](https://github.com/aakshintala/fiber/issues/40)
- Each tool's own design: the tickets indexed in
  [Epic: tools](https://github.com/aakshintala/fiber/issues/59).
- Confinement:
  [Does Fiber confine what tools can touch?](https://github.com/aakshintala/fiber/issues/30)
- Delegates are `docs/delegates.md`, which lists what it leaves open.
