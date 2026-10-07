# Tools

What every tool shares: what it declares, how a call is checked and bounded,
and what its result carries on the log and to the model. This is what is true
now, not a plan. It is settled by
[The tool contract: what every tool shares](https://github.com/aakshintala/fiber/issues/14);
that ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`. Tool call, effect, artifact, participant, seam,
extension and event mean what it says there and nothing else. How effects are
judged is `docs/permissions.md`; the events themselves are `docs/events.md`.

## What a tool declares

- A name, a description, and an input schema written in JSON Schema.
- An effects function. Fiber calls it with each call's arguments before
  permission is decided; it returns the call's effects, whether it is
  reversible, and the paths it touches, in the vocabulary of
  `docs/permissions.md`. Classification is per call, not per tool
  (`docs/permissions.md`). It also returns the call's subject and the prefix
  a rule would offer (`docs/permissions.md`, "What a rule matches"). A tool
  whose section says nothing of a subject returns an empty one, and its rules
  match it by name. It may also return that the call is always reviewed
  (`always_reviewed`): such a call skips the fast path, session grants and
  standing allows, and still meets the credential deny and the standing deny
  and ask rules (`docs/permissions.md`, "The order a call is judged in"). Any
  tool may set it, an extension's included; it only makes the call stricter.
- The effects function may also return the network hosts the call contacts
  (`hosts`). A list, even an empty one, says the call contacts those hosts and
  no others; an empty list says it contacts no host the model chose, as a
  search through its fixed backend does. A call that returns no list has
  unknown hosts. A `network` call whose hosts are all known in the session
  takes the fast path (`docs/permissions.md`, "Fast paths").
- A call's result may list the hosts it surfaced (`hosts_surfaced`), such as
  the hosts of a search's result URLs. Those hosts become known in the
  session. Any tool may return either list, an MCP or extension tool included;
  the loop reads them and never names a tool.
- Optionally: guideline lines for the system prompt, for guidance that spans
  calls, such as which tool to prefer for a job (`docs/system-prompt.md`,
  "Tool guidelines").
- Optionally: how a long result is cut, and its own size cap. By default a
  cut keeps the start; a tool may instead declare how many bytes of the
  start and of the end to keep, as the shell does.
- There is no read-only flag and no parallel-safety flag. Calls in a step run
  concurrently; file safety comes from the per-path lock in
  `docs/architecture.md` ("Tool calls in a step").
- Adapting a schema to each wire protocol, and carrying images to a protocol
  that cannot take them in a tool result, is the provider module's job, not the
  tool's. It never rewrites a schema to fit strict mode. A tool is sent with `strict:
  true` only when its schema fits the vendor's strict subset, as every
  built-in tool's does (`docs/model-routing.md`, "Protocols and providers").

## Before a call runs

- Arguments the model sent are repaired first, and only where a property's
  schema names a single type, after following any `$ref` to its definition.
  Three repairs are made. A `null` sent for an optional property whose type
  does not allow `null` is dropped, and so is a `null` for any property whose
  type list includes `null`, so the tool sees it as absent. A string holding a plain JSON number, such
  as `5`, `-2.5` or `1e3`, becomes a number where the type is `number`, and
  where the type is `integer` only if its value is whole, so `5.0` becomes
  `5` and `5.5` is left as sent; the string `true` or `false` becomes a
  boolean where the type is `boolean`. A string holding JSON is parsed where
  the type is `array` or `object`, repaired the same way inside, and kept only
  if the result passes the check. Under `anyOf` or `oneOf` nothing is
  repaired, and the value passes the check as sent or fails it. A `$ref`
  that leads back to itself before reaching a type repairs nothing. Nothing
  else is repaired: JSON that does not parse, a misspelt or foreign tool name,
  and a missing or unknown property all fail the check. Repair is the same for
  every protocol, model and tool, built-in, MCP or extension. The model is not
  told, and its call is sent back to it as written. `tool_call_requested`
  records what was repaired (`docs/events.md`). Arguments a `before_tool` hook
  rewrote are never repaired. The evidence:
  [research/reply-faults](../research/reply-faults/README.md),
  [research/reply-repair-harvest](../research/reply-repair-harvest/README.md).
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
- `changes`: the lines each file gained and lost, as
  `{ path, added, removed }` per file, on a call that changed files. It is the
  one tool-neutral account of a change: a client draws a group's "+442 −12"
  and the side panel's per-file counts from it, never from `details`. Any
  tool that changes files sets it, an extension's included.
- `artifact`: the path to the full output, present when the result was cut or
  when an `after_tool` hook returned text for the artifact.
- `control`: instructions to the loop, absent on most results. Three fields
  are defined. `handoff`, a handoff note: the loop restarts the model's
  context from it at the step boundary (`docs/handoff.md`). `questions`, a
  `questions` array: once every call in the step has completed, the turn ends
  `completed` with the questions of every call that set it, in call order, on
  `turn_completed`, after any handoff in the step. `name`, a session name: the
  loop writes `session_named` just before the call's `tool_call_completed`, or
  fails the call `name_pinned` while the person's name pins it. Any tool may
  set them; the loop acts on the fields, never on which tool set them.
- Images are written to the session's `artifacts/` (see `docs/state.md`) as
  the processed file (`docs/model-routing.md`, "Image limits") and referenced
  by path, never inlined as base64 in the log.
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
  Image parts do not count toward the cap and are never cut; their limit is
  `docs/model-routing.md`, "Image limits".
  Configuration can override any tool's cap (`docs/configuration.md`). A tool may declare a larger or
  smaller cap. `read` and `web_fetch` keep the 16 KiB default ("File tools",
  "Web fetch and web search").
- A cut result keeps what its tool declares (the start by default, or both
  ends), a notice saying how many bytes were cut, and the artifact path. When
  both ends are kept, the notice sits between them. Nothing is lost, only
  moved out of the model's view. The full output is in the session's `artifacts/`. `read` is the
  exception: the file is the full output, so a cut read writes no artifact
  and its notice gives the offset to continue from.
- The model reads the rest with the ordinary `read` tool on that path. There is
  no dedicated tool for it. A read of `artifacts/` has only the `reads` effect,
  so it is never reviewed.
- There is no cap across one step's results. When they overflow the context
  window, the overflow rule in `docs/handoff.md` ("Overflow") moves them to
  the session's `artifacts/`.
- A cut read continues from an offset, a cut fetch leaves the whole page in the
  artifact, and search runs through the shell ("Search").

## Progress

While a call runs, it may stream output and progress as `tool_call_delta`,
which is ephemeral. Fiber paces updates to at most one every
`max(100 ms, encoded bytes ÷ 100 KiB/s)`; the first change after idle goes out
immediately, held changes collapse to the latest, and completion forces a final
flush.

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
How reference agents do it is
[research/file-tools/reference-agents.md](../research/file-tools/reference-agents.md);
the owner's usage is
[research/file-tools/usage.md](../research/file-tools/usage.md).

There are three: `read`, `write` and `edit`. There is no listing tool, and
`read` on a directory fails with `unsupported_file` and points to the shell.
Search runs through the shell ("Search").

A relative path is resolved against the workspace. A symbolic link is resolved
to its target, and the target is the path the call declares, so permission and
the credential deny (`docs/permissions.md`) judge where the bytes really go.

A file tool's subject is that resolved path, and the prefix it offers is the
path's directory, ending in `/`.

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
- A PNG, JPEG, GIF or WebP file is processed once and comes back as an
  image part (`docs/model-routing.md`, "Image limits"). An image that cannot
  be read, or is over 50 megapixels, fails with `unsupported_file` and the
  decoder's message (or the pixel count). A CMYK JPEG decodes and is
  accepted. For a model that cannot take images, the image is left out and
  the result says so. On a protocol that cannot carry an image inside a tool
  result, the image goes in a user message right after the run of tool
  messages that holds its result, as rendered PDF pages do on
  `openai-completions`, because nothing may come between the tool messages
  that answer one assistant turn.
- A PDF comes back as a PDF part. One of more than 10 pages needs `pages`,
  and a request takes at most 20 pages. A
  PDF of more than 10 pages without `pages`, a range of more than 20 pages,
  and `pages` on a file that is not a PDF each fail with `invalid_arguments`,
  the message saying which. The
  provider module sends the PDF natively where its protocol accepts a PDF in a
  tool result, and otherwise sends the pages rendered as images, which go
  through the same limits (`docs/model-routing.md`, "Image limits").
  `read` renders the pages once, with poppler's `pdftoppm`, as the PDF
  enters, so a later model switch has them. When `pdftoppm` is not installed,
  the result says so and names the package, and a protocol that needs pages
  gets that sentence in their place.
  `anthropic-messages` takes a `document` block inside `tool_result`,
  `openai-responses` an `input_file` in the `function_call_output` array, and
  `google-generative-ai` an `inlineData` part in `functionResponse.parts`.
  `openai-completions` rejects a file part in a tool message, so it gets the
  pages rendered as images in a user message after the tool message.
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
  bytes and lines. `changes` gives the lines added and removed: every line of
  a new file is added.
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
  dashes and spaces folded to their ASCII forms. Only the lines a
  block touches take the new text; every other line keeps its original bytes.
  A block is unique if it is unique in the form it was matched in.
- Matching sets the byte order mark aside and treats line endings as LF. The
  file is written back with its own line-ending style and byte order mark.
  A byte order mark in a block's `new_text` is kept as sent.
- Failures: a block not found fails with `no_match`; a block found more than
  once fails with `ambiguous_match`, giving the count. Both name the block by
  its index. An empty `old_text`, overlapping blocks, or edits that leave the
  file unchanged fail with `invalid_arguments`. A file that does not exist
  fails with `not_found`, and one that is not UTF-8 text with
  `unsupported_file`.
- The result gives, for each block, the lines it replaced and the lines the new
  text now occupies, and says when a block matched only after normalising.
  The diff goes in `details` for clients and is not sent to the model, and
  `changes` gives the lines added and removed.
- Effects: an irreversible `writes`, with the resolved path.

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

## Search

Settled by
[Search: built-in tools or the shell?](https://github.com/aakshintala/fiber/issues/54);
that ticket's resolution holds the rationale and the rejected alternatives.
How reference agents search is
[research/search/reference-agents.md](../research/search/reference-agents.md);
the owner's usage is [research/search/usage.md](../research/search/usage.md).

There is no tool for searching code. The model searches with `grep` and
`find` in the shell, and inside the shell tool those two names run a search built into
Fiber.

- The shell classifier already treats `grep` and `find` as read-only
  ("Shell", "Effects"), and the shell's cut bounds their output, so a tool
  would add neither.
- A plain `grep -r` walks into `.git` and build directories. In a large
  monorepo that is minutes of reading. The built-in search skips them.

### How it runs

- Before each command, the shell tool defines two shell functions, `grep`
  and `find`, that run the Fiber binary's hidden `grep` and `find`
  subcommands. The functions exist only in the model's own command line. A
  script or build the command starts gets the system tools, because shell
  functions are not passed to child processes.
- The subcommands are hidden: undocumented for people and free to change.
- `command grep`, or a full path such as `/usr/bin/grep`, runs the system
  tool. `rg` is not replaced; it already skips ignored files.
- The search is ripgrep's own crates: `ignore` walks the tree, and
  `grep-searcher` with `grep-regex` searches each file
  (`docs/dependencies.md`).

### Behaving like grep and find

- With no path and no `-r`, `grep` reads its standard input, so a pipe such
  as `cargo test | grep FAILED` works as before.
- Output is GNU `grep`'s, byte for byte: `path:line:text`, `--` between
  context groups, and the path shown when more than one file is searched
  or `-r` names a directory. `find` prints one path per line.
- `grep`'s exit codes are GNU's: 0 when something matched, 1 when nothing
  did, 2 on an error. `find` exits 0 when it finished, even with nothing
  printed, and 2 on an error.
- `xargs grep` and `find -exec grep` run the system `grep`, because shell
  functions do not reach the programs they start.

### Flags

- The built-in handles these flags: `-n`, `-r`,
  `-i`, `-v`, `-E`, `-F`, `-l`, `-c`, `-w`, `-o`, `-A`, `-B`, `-C`,
  `--include` and `--exclude` for `grep`; `-name`, `-iname`, `-path`,
  `-type`, `-maxdepth`, `-mindepth` and `-newer` for `find`.
- A call with any other flag runs the system `grep` or `find` unchanged.
  `find` with `-exec`, `-execdir`, `-ok` or `-delete`
  always runs the system `find`, and the shell classifier already declares
  those calls `executes`.
- Without `-E` or `-F`, a pattern is a basic regular expression, as in GNU
  `grep`, and is translated to ripgrep's syntax before the search. With `-E`
  it is an extended regular expression; with `-F`, a fixed string.

### What it skips

- A directory walk skips what the workspace's ignore files exclude
  (`.gitignore`, `.ignore`, and git's global and repository excludes), and
  always skips `.git`, `.svn`, `.hg`, `.bzr`, `.jj` and `.sl`.
- A path the model names is always searched, even if it is ignored, as
  ripgrep does. Ignore rules apply only to what the walk finds.
- Hidden files are searched. Binary files are skipped, as `grep -I` does.
- When a search finds nothing and skipped ignored directories on the way, it
  prints one line on standard error saying so and naming the directories
  skipped at the top level, so the model can search them by name. A search
  that finds matches prints nothing extra.

### Other command-line tools

- Every operand of a command configured at `shell.read_only` is a path. A
  flag written ending in `=`, such as `--format=`, takes a value, after the
  `=` or as the next word; any other flag takes none.
- An operand under `/proc/` makes a call not read-only, so it goes to
  review. A process's environment there can hold a key from an `env`
  credential source (`docs/configuration.md`, "Secrets").

- A tool that must stay warm between calls, such as an index kept current by
  a file watcher or a language server, is an extension that registers a tool
  (`docs/extensions.md`). A command-line tool starts cold on every call.
- Any other search tool, such as `ast-grep` or a tree-sitter query command,
  is a command-line program the person installs. It reaches the model
  through the person's instruction files (`docs/system-prompt.md`,
  "Instruction files"). Prose works best when it maps a habit the model
  already has onto the new tool.
- The person's configuration can add a command to the shell classifier's
  read-only list, with the flags it may take and stay read-only
  (`shell.read_only` in `docs/configuration.md`). A call to that command with
  only those flags declares `reads` and skips review; any other flag makes it
  `executes`, as for the built-in list. A repository cannot add to the list.

## Shell

Settled by
[Shell: running a command, and when it becomes a job](https://github.com/aakshintala/fiber/issues/53);
that ticket's resolution holds the rationale and the rejected alternatives.

### Running a command

- The command runs as `/bin/bash -c <command>`, or `sh -c` where `/bin/bash`
  does not exist. It gets the session's environment and reads no shell
  startup files (`docs/invocation.md`, "A session's environment"). That
  environment already holds the `PATH` and variables the person's startup
  files set; aliases and shell functions are not carried.
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
  (optional); `monitor` (optional; starts a monitor, "Background jobs");
  `deadline_ms` (optional; a monitor's deadline). Each call starts fresh: a `cd` does not carry over to the
  next call.
- A monitor takes neither `tty` nor `run_in_background`, and its only limit
  is `deadline_ms`, so it takes no `timeout_ms`. `deadline_ms` without
  `monitor` is refused too. Each of these fails with `invalid_arguments`,
  the message naming the argument.
- A bare wait is rejected. When the call is not `run_in_background` and
  the first part of the command is `sleep N` with N of 25 seconds or more,
  alone or followed by other commands, the call completes as `failed` with
  code `invalid_arguments` and never starts. The message points to
  `run_in_background`, `jobs wait`, or a monitor running an `until` loop.
  A `sleep` inside a loop is not the first part of the command and is
  allowed.

### Timeout

- `timeout_ms` is the one thing that kills a command for running long.
  When the model gives none it is 600,000 (10 minutes). There is no
  maximum. It counts from when the command started.
- It applies to every command except a monitor, run in the foreground or as
  a job, and it stays with a command after the command moves to the
  background. A monitor's deadline does the same for it. This is
  where a hang is bounded: "Background jobs" has no cap on waiting.
- A command that times out ends `failed` with code `timeout` and
  `process.timed_out` true.
- The unit is milliseconds.

### Moving to the background

- A command moves to the background, becoming a job ("Background jobs"),
  when any of these happens:
  - It has run for 30 seconds.
  - It was started with `run_in_background`, `tty` or `monitor`, in which
    case it moves at once.
  - A driver sends the `background` driver command (`docs/invocation.md`).
  - Its shell exits while processes it started are still running ("When a
    command ends").
- Moving never kills anything and never restarts anything. The tool call
  completes with the job's receipt, which says the command is still
  running, why it moved, the `job_id` and the output file's path. Output
  already produced and all later output go to the job's output file.

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

### Stopping a command

- A timeout, a cancellation and a `jobs stop` all stop a command the same
  way: SIGTERM to the whole process group, then SIGKILL to the group
  800 ms later if any member is still alive. 800 ms is the value from the
  archived Zig tree (fiber-zig). The grace period exists because programs
  clean up on SIGTERM: git, for example, removes its `index.lock` on
  SIGTERM and leaves it behind on SIGKILL.
- After the kill, Fiber reads output for at most 2 seconds more. If output is still held open after that, for example by a
  descendant that escaped the group, Fiber stops reading and the result is
  `failed` with code `indeterminate`, never `completed`.
- A shutdown (`docs/invocation.md`, "Shutdown") uses the same two values, on
  every group at once.

### Result and output

- Exit code 0 is `completed`. A nonzero exit is `failed` with code
  `nonzero_exit` and `process.exit_code`. A command killed by a signal
  Fiber did not send is `failed` with code `signal` and `process.signal`.
  A timeout is as above; a cancellation is `cancelled`.
- Output follows "Bounded results": the default 16 KiB cap applies, and the
  shell keeps the first 8 KiB and the last 8 KiB. The start is where a
  command reports its setup, its first error or its first matches; the end is
  where it reports how it finished. The full output is in the
  session's `artifacts/`, and output streams as `tool_call_delta` while the call runs. After a
  move to the background, output goes to the job's output file.

### Terminal (`tty`)

- With `tty: true` the command runs in a pseudo-terminal instead of pipes,
  so a program that behaves differently on a terminal, or waits for typed
  input (a REPL, `git rebase -i`, a debugger), can be driven. It moves to
  the background at once; the receipt carries whatever output arrived in
  the first 250 ms.
- The model types into it with the `jobs` action `write` ("Background
  jobs"). Output is the terminal's raw bytes. Fiber keeps no screen model
  and runs no separate terminal host process.
- Prompts do not fail at once on a terminal; that is the point of `tty`.
  The timeout still bounds the command.

### Effects

- The shell tool classifies each call (`docs/permissions.md`, "Effects").
  It splits the command on `&&`, `||`, `;` and `|`, and checks each part
  against a list of read-only commands and the flags allowed for each:
  Fiber's own list, plus any the person's configuration adds ("Search",
  "Other command-line tools"). If every part is on the list, the call
  declares `reads`, reversible, with the paths the command names, resolved
  against `workdir`. Otherwise it declares `executes`, with no paths.
- It declares `executes` whenever it finds something it cannot read
  plainly: command substitution (`$( )` or backticks), process
  substitution, a redirect, or anything else outside the list.
- Flags matter because read-only-looking commands have writing or
  executing flags: `git diff --output=<file>` writes a file,
  `rg --pre <cmd>` and `find -exec` run programs, `find -delete` deletes,
  `sort -o` writes. `grep -R` follows every link below the paths it names,
  which the credential deny never sees, so it is not on the list. The list
  and its flag rules are part of building the shell tool.
- A call declared `reads` takes the permission fast path.
- A command of one part, with nothing the classifier cannot read plainly,
  has the command as its subject. The prefix offered is its first word, and
  the second word too when it is a plain word, starting with no `-` and
  holding no `/`, `.` or `=`: `npm test -- --watch` offers `npm test`, and
  `rm -rf build` offers `rm`. Any other command has no subject, so no allow
  rule or session grant matches it, and an approval of it cannot be
  remembered.
- The credential deny (`docs/permissions.md`, "Credentials") sees paths
  only for commands the recogniser understands. A command it does not
  understand, such as `python -c` opening a file, declares no paths, so
  the deny cannot see it; it is still reviewed. Fiber does not confine the shell
  (`docs/permissions.md`, "Confinement").
- The read-only list is trusted. A command on it that writes after all, or
  an entry in `shell.read_only` that is wrong, writes without review. An
  entry is the person's statement that the command only reads.
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
  `artifacts/`. A Fiber delegate's output file is its own `events.jsonl`
  (`docs/delegates.md`, "The tools"). There is no pending status. Every provider needs a tool result
  before the model's next step, so a call held open across turns would stall
  the turn.
- A job's output streams to that file. The model reads it with the ordinary
  `read` tool ("Bounded results"). There is no output action.
- The model-facing tool is one `jobs` tool with actions `list`, `wait`,
  `stop` and `write`. `wait` blocks up to a timeout. Cancelling a wait (for
  example because the turn is cancelled) stops only the wait and leaves the
  job running. `write` sends input to a job started with `tty` and returns
  the output that arrives within a wait after the write, 250 ms by default,
  at most 30 seconds. A write with no input waits at least 5 seconds.
  `write` on a job not started with
  `tty` fails with `invalid_arguments`. A `write` call declares `executes`,
  because typed input can make the program do anything. `jobs` only sees
  and acts on jobs the calling session started, so a delegate cannot stop its
  parent's work.
- Every delivery to the model, a completion or a monitor's batch, passes
  through the `after_tool` hooks first, and so does the job's output file when
  the job ends (`docs/extensions.md`, "Hooks").
- Completion reaches the model by waking it. If the loop is idle, a finished
  job starts a new turn whose input names the job or jobs. If a turn is
  running, the news joins it at the next step boundary, the way a steering
  message does. Jobs finishing together are delivered together in one turn,
  not one turn each. If a `jobs wait` already returned a job's final state to
  the model, no completion notice is sent for it.
- A monitor is a job running a watch command. The model starts one with a
  `shell` call that sets `monitor: true`, so the command goes through the
  shell's classifier, permissions and `workdir` like any other. Lines on its standard output
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
    may set a shorter or longer one within those limits with the call's
    `deadline_ms`. At the deadline the monitor ends as `failed` with code
    `timeout`, as a timed-out shell call does (`process.timed_out`). The
    model is told the monitor expired and can start it again. The deadline
    is what stops a forgotten monitor from holding a non-interactive run
    open under the "ending with jobs running" rule in this section.
  - Testing: timing-dependent behaviour is tested under an injected clock, never by
    waiting in real time.
- A job whose output file passes 5 GB is stopped as `failed` with code
  `output_cap`.
- Stopping one job uses the same mechanism as cancelling a tool call
  ("Shell", "Stopping a command"), except a Fiber delegate, which shuts down
  as `docs/delegates.md`, "Lifetime", says. If a descendant that escaped the process
  group still holds the output pipe open past the bound, the job ends
  `failed` with code `indeterminate`, never `completed`. A stopped job ends
  `cancelled`.
- Jobs live and die with the Fiber process. Nothing reattaches to a job after
  a restart. A shutdown stops every job (`docs/invocation.md`, "Shutdown"); a
  crash stops none, and the jobs keep running unwatched.
- When a session is about to end with jobs still running — a non-interactive
  run whose model has given its final answer, `close`, or a delegate
  finishing its task — Fiber wakes the model once with a notice listing the running jobs, telling it to stop the ones it
  does not need and that the rest will be waited for. Whatever is still
  running after that is waited for, whatever its kind, and each completion
  wakes the model. The session ends when it is idle with no jobs running.
  The idle delay never ends a session with jobs running; a session left
  unattended with jobs running gets the jobs check instead
  (`docs/invocation.md`, "Lifecycle").
  There is no cap on this wait: a hang is bounded at the command that hangs
  (the shell tool's timeout, "Timeout") and by the caller's SIGTERM
  (`docs/invocation.md`, "Shutdown"),
  because a cap on the waiter cannot tell a hang from long healthy work such
  as a CI watch.
- There is no cap on running jobs, except that a session runs at most 10
  delegates at once (`docs/delegates.md`, "Limits"). Parked threads are
  measured in `docs/architecture.md` ("The threads").

## Asking the person

Settled by
[Asking the person a question](https://github.com/aakshintala/fiber/issues/55);
that ticket's resolution holds the rationale and the rejected alternatives.

The model asks with `ask_user`. The question goes to whoever drives the
session. A person answers it through a `form` interaction. A program answers
by resuming the session.

How a session started decides who drives it. A session the hub started is
driven by a person until it is sent `close`. Every other session is driven by
a program, such as a delegate (a fork included) or a `fiber ask` session.

### The call

| Argument | Rule |
|---|---|
| `questions` | 1 to 4 items |
| `question` | the full question, with its context |
| `header` | a short label, at most 12 characters |
| `options` | none, for a free-text question, or 2 to 4, each a `label` and an optional `description` |
| `multiSelect` | optional; the person may choose several options |

- There is no preview, no `required` flag and no skip flag. Any question can
  be skipped.
- The person can always type an answer, alone or with chosen options.
- The description tells the model to put a recommended option first, with
  "(Recommended)" in its label, and to ask with this tool rather than list
  choices in its reply.
- The definition is at most 1,200 bytes serialised, about 300 tokens, which a
  test checks, and counts toward the built-in budget ("Size budget in CI").
- The limits are in the schema (`minItems`, `maxItems`, `maxLength`), so the
  tool is sent with `strict: false`.
- A call that breaks these rules fails with `invalid_arguments`, as any call
  does ("Before a call runs").
- The tool declares no effect, so it never reaches a reviewer
  (`docs/permissions.md`, "Fast paths").
- It is declared in every session, including `fiber ask` sessions, forks and
  other delegates, so the tool set never differs between them.

### When a person can answer

This is a session the hub started, which any client may answer.

- One call raises one `form` interaction, with one field per question: select,
  multi-select or text input. One `reply` answers the whole form.
- It has no timeout. It lives like a pending approval: when
  `session.idle_exit_ms` passes, the session exits on it with `suspended_on`,
  and resuming raises it again (`docs/invocation.md`, "Lifecycle").
- The request and its answer are `interaction_requested` and
  `interaction_resolved` (`docs/events.md`, "Interactions").

### When a program drives the session

This is every session the hub did not start, such as a delegate (a fork
included) or a `fiber ask` session, and a session that has been sent `close`.

- The turn ends `completed`, with the questions on `turn_completed`. The
  call's result is one line saying the questions went to the driver.
  `ask_user` returns `control.questions` ("What a result carries").
- A `close` that arrives while a question is pending ends the turn the same
  way.
- The driver answers by resuming the session. The answers arrive as the next
  prompt.
- A delegate's parent reads the questions from `delegate_finished`, which puts
  them in the wake. It answers with `delegate_message`, or asks the person
  first with its own `ask_user` (`docs/delegates.md`, "Results").
- `fiber ask` exits 0 with `questions` on `fiber_exited`, and the caller
  resumes the session with the answers (`docs/invocation.md`, "What a caller
  gets back").

A child's question goes to its parent, never straight to the person. The
parent wrote the child's brief and can usually answer, and the person is never
interrupted by a session they did not start. An approval is different: no
model may answer one, so a delegate's escalation is a block
(`docs/permissions.md`, "Delegates").

### The result

For each question, one line: `<header>: ` followed by the chosen labels, the
typed text in quotes, or `skipped`. A `note:` line follows when the person
added a note to the whole form. A cancelled form is the single line
`declined`.

A declined form is an answer, not a failure: the call completes with status
`completed`. The structured answers are on `interaction_resolved`.

## Web fetch and web search

Settled by
[Web fetch and web search](https://github.com/aakshintala/fiber/issues/57);
that ticket's resolution holds the rationale and the rejected alternatives.
How reference agents do it is
[research/web-tools/reference-agents.md](../research/web-tools/reference-agents.md);
which providers host a search, and the search services a backend can call, is
[research/web-tools/providers.md](../research/web-tools/providers.md).

There are two tools. `web_fetch` is compiled in and runs on this machine for
every provider. `web_search` is a provider's hosted search where the model's
provider offers one, and otherwise Fiber's own tool over an installed search
backend.

### web_fetch

- Arguments: `url` only. There is no prompt and no second model over the
  page; the model reads the page itself.
- The result begins with one line giving the final URL after redirects, the
  HTTP status and the content type. For an HTML page it also gives the path
  of the raw page.
- HTML is converted to markdown ("HTML to markdown"). Other text, JSON and
  XML come back as they are.
- Every HTML page is saved as downloaded to the session's `artifacts/`, so
  the model can check a conversion that looks wrong with `read` or `grep`.
- A PDF, or a PNG, JPEG, GIF or WebP image, is saved to the session's
  `artifacts/` and the result gives its path. The model reads it with `read`,
  which already handles both ("File tools"). The saved file is the bytes as
  downloaded; `read` processes an image (`docs/model-routing.md`, "Image
  limits").
- Any other content type fails with `unsupported_file`, giving its type and
  size.
- The cap is the 16 KiB default ("Bounded results"). A cut keeps the start of
  the page, and the whole markdown is in the artifact.
- A download larger than 10 MiB fails with `too_large`.
- Each request times out after 60 seconds, and the whole fetch, redirects
  included, after 5 minutes. Both fail with `timeout`.
- A status other than 2xx fails with `http_error`, giving the status and the
  start of the body.
- Redirects are followed, to any host, for at most 10 hops. Fiber judges only
  the URL the model wrote, because the server chooses a redirect ("Effects").
- A URL is fetched as written: `http://` is not upgraded, so a server on
  `localhost` or an intranet host works.
- Fetch refuses link-local addresses (`169.254.0.0/16`, `fe80::/10`) and
  cloud metadata hosts such as `metadata.google.internal`, and fails with
  `blocked_host`. Without a proxy, the check runs on every hop after the name
  is resolved, so a redirect or a DNS name cannot reach them. A metadata
  server hands out the machine's cloud credentials; loopback and private
  addresses are allowed, because reaching them is why fetch runs locally.
- Through a proxy (`docs/dependencies.md`, "Proxies"), the proxy resolves the
  name, so Fiber checks what it can see. The metadata host names are always
  refused. When the name also resolves on this machine, every address it
  resolves to must pass the check, or the fetch fails with `blocked_host`. A
  name that does not resolve on this machine goes to the proxy, so an
  intranet name only the proxy knows still works. What the proxy resolves a
  name to is the proxy's responsibility.
- Writing to `artifacts/` is part of the call. It is not a `writes` effect.

#### HTML to markdown

Fiber converts a page in one pass: html5ever's tokenizer, with no document
tree, feeds Fiber's own single-pass writer (`docs/dependencies.md`, "Runtime
dependencies"). Every character reference is decoded per the HTML standard.
It handles a stated subset of HTML, not the WHATWG parsing
algorithm:

- Converted: headings, paragraphs, lists, links, images, emphasis, inline
  code, `pre` blocks, block quotes and tables.
- Dropped with everything inside them: `script`, `style`, `noscript`,
  `template`, `svg`, and the `head` apart from its `title`, which becomes a
  leading heading.
- Everything else, including markup the converter does not know, passes its
  text through.

On any input, however malformed, the converter promises three things:

- No text a reader would see is lost, except inside the dropped elements.
- Its time grows in proportion to the page's size.
- Its memory is bounded: deep nesting becomes neither recursion nor unbounded
  indentation.

Formatting may come out wrong, such as a complex table or a misnested list.
That is accepted, because the raw page is in `artifacts/`.

Before converting, Fiber decodes the page by its declared character set, from
the `Content-Type` header or the page's `<meta charset>`, with `encoding_rs`
(the WHATWG Encoding Standard). A page with no character set, or one Fiber
does not know, is read as UTF-8, and bytes that are not valid UTF-8 become
`�`.

### web_search

- The model sees one tool, `web_search`, whichever way it runs.
- A hosted search is the vendor's own tool, sent as its type and name only.
  It has no description, and its settings, such as a domain list, are fixed
  in the request; the model's call carries only the query.
- Fiber's own tool, over a backend, takes `query` (required), and either
  `allowed_domains` or `blocked_domains`, never both.
- `web_search` has a guideline line telling the model to end an answer that
  used search with a list of the sources it used, as markdown links
  (`docs/system-prompt.md`, "Tool guidelines"). It reaches the model whether
  the search is hosted or Fiber's own.
- There is no limit on searches per turn or per session.

#### Hosted by the provider

- When the model's provider hosts a search, that search is used, even when a
  backend is also installed.
- Which models host a search, and which type each takes, is the model's
  `web_search` in the provider's extension: the vendor's own tool type, such
  as `web_search_20250305` or `google_search` (`docs/model-routing.md`,
  "Hosted web search"). Reading and sending back the
  hosted search's blocks is protocol code in `anthropic-messages`,
  `openai-responses` and `google-generative-ai`.
- Measured September 27, 2026: ChatGPT/codex, muse (only on its
  `/v1/responses` endpoint), OpenRouter (its `openrouter:web_search` server tool, on `openai-completions`, unprobed) and OpenCode (by
  passing Anthropic's and OpenAI's hosted tools through) host a search.
  Databricks does not: "Code interpreter and web search tools are not
  supported by Databricks."
- The provider runs the search before Fiber sees it, so a hosted search is
  never reviewed and cannot be refused. It writes `tool_call_started` and
  `tool_call_completed` like any call, with the query and the result URLs.
- Its raw blocks are logged exactly as they arrived, as the `provider_item`
  of `tool_call_requested` (the call) and `tool_call_completed` (the result)
  (`docs/events.md`). A call with `provider_item` is one the provider ran, so
  Fiber never reviews it, runs it, or gives it a result on resume. Its
  `tool_call_completed` content is the result URLs, one per line. A search
  that fails at the provider, such as Anthropic's
  `web_search_tool_result_error`, completes `failed` with the vendor's error
  code in the message.
- The raw blocks are sent back unchanged only to the model that produced
  them, the rule for reasoning state
  (`docs/loop.md`). Anthropic refuses a request whose encrypted search content
  was changed. After a switch to another model reference, the request leaves
  them out.
- Whether a hosted search is declared is fixed when the preamble is built
  ("Which tools the model sees"). On Anthropic, turning it on or off changes
  the system prompt and so the cache.
- `usage_recorded` carries the number of searches when the provider reports
  it (`docs/events.md`), because hosted searches are billed per search.

#### Fiber's own, over a backend

- A search backend is an extension. Each search service has its own API, key
  and response shape. Fiber ships none installed.
- A backend registers with `fiber.search_backend` (`docs/extensions.md`). Its
  function takes the query and the domain filter, and returns a list of
  results, each a title, a URL and a snippet. Fiber writes the result.
- A backend's key is an extension secret, stored at `credentials/<backend>`
  and read with `host.secret`.
- With one backend installed, it is used. With more than one,
  `web_search.backend` (`docs/configuration.md`) names the one used.
- `web_search` is not declared when the provider hosts no search and no
  backend is installed, when several are installed and `web_search.backend`
  is unset, or when it names a backend that is not installed. In the last two
  cases a `notice` with code `web_search_unavailable` names the setting or the
  missing backend.
- The cap is the 16 KiB default.

### Effects

- Both tools declare `network`.
- `web_fetch` declares the host of the URL as written in `hosts`. A search
  declares an empty `hosts` list, because a query reaches only the search
  service, and its result lists the hosts of its result URLs in
  `hosts_surfaced` ("What a tool declares").
- So a search never reaches a reviewer or a person, and a fetch to a known
  host takes the same fast path. Any other fetch is reviewed
  (`docs/permissions.md`, "Fast paths").
- A host is known in a session when it was named in one of the person's
  messages or an instruction file, was listed in a result's `hosts_surfaced`
  in the session, or was the host of a fetch a reviewer or a person allowed in
  the session. A host named only inside a fetched page is not known: otherwise an
  injected page could name its own host.
- A host is named only by an `http` or `https` URL; a bare domain is not.
  Hosts match case-insensitively and exactly, with the port (the scheme's
  default when absent), so a subdomain is a different host. The result URLs of
  a hosted search make their hosts known, as `hosts_surfaced` does.
- A query string does not make a fetch suspicious. A search on a known host,
  such as a Jira query URL, needs no review.
- What the reviewer is shown does not change: the person's messages and the
  agent's tool calls, never a fetched page (`docs/permissions.md`, "What it is
  shown").
- A standing deny still applies first, so the person can deny a host.
- A fetch's subject is its URL as parsed, whose path is at least `/`, so
  `https://example.com` is `https://example.com/`. The prefix it offers is
  the URL's scheme and host, ending in `/`. A search's subject is empty.

## Naming the session

The model names the session with `name_session`, so the session list, the
terminal's header and its title show what the work is about rather than the
first prompt.

- Argument: `name`, 1 to 60 characters. An empty name fails with
  `invalid_arguments`; only the person clears a name, with the `name` command
  and empty text (`docs/invocation.md`, "What each command does").
- The description tells the model to name the session after the first
  prompt, and to rename it when the work's topic changes, a handoff included.
- A name the person set with the `name` command pins it. While pinned, the
  call fails with `name_pinned` and the message "the person named this
  session". Clearing the person's name unpins it.
- The tool declares no effect and never reaches a reviewer.
- It is declared in every session, so the tool set never differs between
  sessions. Its definition counts toward the built-in budget ("Size budget in
  CI").
- Each name is written as `session_named` (`docs/events.md`).
- The limits are `minLength` and `maxLength` in the schema, so the tool is
  sent with `strict: false`. The call returns `control.name`, and the loop
  writes `session_named` and applies the pin.

## Messaging other sessions

Settled by
[Intercom: messaging a session you did not start](https://github.com/aakshintala/fiber/issues/78);
that ticket's resolution holds the probes and the rejected alternatives.

A session sends a session message to another running session of the same
account: one it did not start, a sibling delegate, or a session in another
project. Messaging a session's own delegates is `delegate_message`
(`docs/delegates.md`).

| Tool | Arguments | What it does |
|---|---|---|
| `session_list` | none | Lists every running session of the account, delegates included. |
| `session_message` | `id`, `text` | Sends a session message to one running session. |

**`session_list` shows running sessions only.** Each entry has the session's
id, its name, its workspace, and its parent's id when it is a delegate. A
session is running when its socket accepts a connection (`docs/invocation.md`, "Processes"). The name, workspace and parent come from
each session's latest `session_status`, read on a `summary` subscription to
its socket (`docs/events.md`). The list includes the calling session, marked
as itself, so the model can see its own id and name. The tool declares
`reads`. The call connects to every session's socket at once and waits at most
2 s in all (`docs/performance.md`). A session that accepted the connection but
sent no `session_status` in time is listed by id with `answering: false`.

**`session_message` addresses a session by its full id.** The model lists
sessions first, so a name or a prefix would add ambiguity and save nothing.
The text is plain text; a session message carries no images.

**A session message reaches only a running session.** The sender connects to
the target's socket, sends the driver command `message`, reads the answer and
closes the connection at once, so it never keeps the target alive
(`docs/invocation.md`, "Lifecycle"). The target runs `before_message` before
it answers, so the call answers `delivered` only once the message is in the
target's inbox. Otherwise it fails with one of:

- `unreachable`: no running session has this id, whether it exited or never
  existed
- `closing`: the target was sent `close` (`docs/invocation.md`)
- `message_refused`: the target's `before_message` refused it, with the
  hook's reason (`docs/extensions.md`, "Hooks")
- `hook_failed`: the target's `blocking` `before_message` failed

A session may message itself. The message arrives as steering at its own next
step, which lets a test drive the whole path with one session.

A message for a session that has exited belongs on the ticket or with
whoever orchestrates the work. Fiber keeps no mailbox. An extension that runs
persistent seats replaces `session_list` and `session_message` with its own,
and keeps what mail it needs (`docs/extensions.md`, "Registering").

**The target takes it as it takes any message.** It enters the loop's inbox
(`docs/architecture.md`, "One inbox"). During a turn it is a steering message
and joins at the next step boundary. Between turns it starts a turn
(`docs/loop.md`, "Starting a turn"). `before_message` runs on it with the
sender's id and its parent's id, so an extension can rewrite or refuse it, or
refuse every message from outside its own tree. It is logged with `source` `session`
(`docs/events.md`, "Where a message came from"). A session message is not a
person's or a driver's prompt, so it never makes an unattended session
attended (`docs/invocation.md`, "Lifecycle"). The model sees it framed
with the sender's id and name. The target reads the sender's name from the
sender's latest `session_status`, as `session_list` does; when the sender has
already exited, the framing gives its id alone. The framing is a fixed
template in `crates/loop/prompt/messages.md` (`docs/system-prompt.md`, "The
texts").

**A session message is not the person's voice.** The target's reviewer never
reads one as the person's instructions (`docs/permissions.md`, "The
reviewer"). It is a request from another session, and the target's own reviewer
judges what it does about it.

**Sending declares `writes`, not reversible.** It changes what another session
does, so the sender's reviewer judges the send.

**Nothing limits how many messages sessions exchange.** Two sessions can
wake each other indefinitely while no person watches. An extension can refuse
messages in `before_message`, and `budget.usd` stops each session once it has
spent its limit (`docs/loop.md`, "Spending budget"). On a subscription the
budget counts nothing, so only the subscription's own limit ends the pair.

**Delegates on another harness receive through their parent.** The parent
binds the delegate's socket and delivers what arrives; the delegate sends
through `fiber mcp serve` (`docs/delegates.md`, "Delegates on another
harness").

Both tools are declared in every session, so they never differ between
sessions. Their definitions count toward the built-in budget ("Size
budget in CI").

## Searching past sessions

`session_search` finds text in the logs of this project's sessions, past and
running, so the model can find earlier work: what was decided, which command
ran, which file changed, or where an error appeared. `fiber sessions search`
runs the same search for a person (`docs/invocation.md`, "Commands and
flags").

- Arguments: `text` (required), matched as a literal, case-insensitive
  string; `all_projects` (optional, default false) searches every project in
  Fiber home instead of the session's own; `limit` (optional, default 20) is
  the most hits returned.
- It scans `events.jsonl` directly with the same ripgrep crates as the
  shell's `grep` ("Search"). There is no index: the answer is always
  current, and nothing is written. A line that matches is decoded, and the
  text is matched against the decoded strings, so escaped newlines and quotes
  in the JSON never hide a match. The raw pass looks for the query as the log
  escapes it (`"` as `\"`, `\` as `\\`, a newline as `\n`, other control
  characters as `\u00XX`), case-insensitively, so it selects every line whose
  decoded text could match.
- It searches everything a log holds: messages from the person and the model,
  the inputs of tool calls, and their outputs, including the full outputs in
  `artifacts/`.
- Each hit is labelled `message`, `tool_input` or `tool_output`. `message` is
  the text of `turn_started` message items, `steering_applied`,
  `text_completed`, and the answers in `interaction_resolved`. `tool_input` is
  the `arguments` of `tool_call_requested`, and of `tool_call_started` when a
  hook rewrote them, and the `command` of `shell_command`. `tool_output` is the
  `content` of `tool_call_completed`, the `output` of `shell_command`,
  `job_line`, `delegate_finished` and `job_completed`'s `output_tail`. No other
  event is searched, and reasoning is not.
- Only text artifacts are searched. A hit in one is labelled `tool_output` and
  takes the `seq` of the line that names the artifact's path (`artifact` on
  `tool_call_completed`, `shell_command` or `delegate_finished`, or the job's
  `job_started` for a job's output file), and gives that path. When the same
  line also matched in its cut result, the result gives one hit, the
  artifact's.
- Messages and tool inputs rank ahead of tool outputs, then newer before older, so a
  decision is not buried under every file read that mentioned the same name.
- Each hit carries the session id, its name as `session_status` gives it, the
  event's `seq` and time, `offset`, the hit's line in `events.jsonl` (`seq` +
  1) for `read`, the label, and a snippet of about 200 characters around the match. It never
  carries a whole event: the model reads around a hit with `read` on the log,
  whose path the result gives.
- The result is bounded like any other ("Bounded results").
- The tool declares `reads` on `projects/<key>/`, or on `projects/` with
  `all_projects`, and is never reviewed. Neither is an ancestor of
  `credentials/`. Its
  definition never changes, so it is declared in every session, in full, and
  counts toward the built-in budget ("Size budget in CI").
- Scanning the owner's 1.3 GB of pi and Claude Code logs took 0.05 to 0.35 s
  with a warm cache on macOS arm64 (2026-10-03, `rg -l -F`). Cold-cache and
  Linux timings are not measured.

## Provider quota

Settled by
[Provider quota the model can see](https://github.com/aakshintala/fiber/issues/58);
that ticket's resolution holds the rationale and the rejected alternatives.

The model sees how much quota each provider has left, so it can choose a
delegate's model with that in mind. It only reads quota. The session's own
model is the person's to choose (`docs/model-routing.md`, "Choosing the
model"), and Fiber never switches it on the model's behalf.

### Where it comes from

A provider supplies quota through an optional Lua `quota()` function
(`docs/model-routing.md`, "Quota"). It is called once per credential label,
because each label is its own account (`docs/model-routing.md`,
"Credentials"). A provider without one reports no quota. Of
the first-party providers:

| Provider | Source | Reports |
|---|---|---|
| ChatGPT/codex | `/wham/usage` | percent used of a primary and a secondary window, with reset times |
| OpenCode | `GET /zen/go/v1/usage` | percent used of Go's rolling, weekly and monthly windows, with reset times; Zen reports none |
| OpenRouter | `GET /api/v1/key` | credit remaining, and the key's limit if it has one |
| Anthropic, OpenAI, Gemini API, Databricks, muse, AWS Bedrock, Google Vertex, Azure | none | no quota reported |

muse's `x-ratelimit-remaining-*` headers are a per-minute rate limit, not
quota. No provider's quota is read from response headers.

A harness extension may declare `quota()` in the same shape
(`docs/delegates.md`, "Harness extensions"). It is the only source for quota
that no Fiber provider can see, such as the Claude subscription, which Fiber
reaches only by running Claude Code.

| Harness | Source | Reports |
|---|---|---|
| Claude Code | `/api/oauth/usage` with Claude Code's own login, and the `rate_limit_event` lines a running delegate prints | percent used of the five-hour and seven-day windows, with reset times |
| cursor-agent | none yet | no quota reported |

### What the model sees

`delegate_models` returns one quota entry per provider credential label and
per harness beside the models it lists (`docs/delegates.md`, "The tools"). There is no separate quota tool.
Each entry is one of:

- windows, each with its name, percent used and reset time where the provider
  reports one
- credit remaining, and the limit where there is one
- `no quota reported`, for a provider or harness without `quota()`
- `unreachable`, when the fetch failed or timed out, with the last value and
  its age if there is one

A window whose reset time has passed shows as reset. Quota never appears in a
tool definition or the system prompt (`docs/prompt-cache.md`, "Tools").

### The notice

When a window crosses `quota.notice_at` percent used (default 80), one line
naming the provider, the credential label, the window and its reset time goes into the model's next
turn input. It fires once per crossing, and again for that window only after
it resets. The notice is the durable event `quota_noticed`
(`docs/events.md`, "Usage and notices"). OpenRouter credit has a percentage
only when the key has a limit, so a key without one never gives a notice.

### Fetching

Each provider credential label has one cache holding its last value, when it was fetched, and
any fetch in flight. Concurrent readers share one fetch. What the person is
shown reads the same cache, and shows no age.

A fetch runs off the request path when the cache is older than 5 minutes and
one of these happens:

- a model call to that provider
- a `delegate_models` call
- a keypress, or the terminal regaining focus

`/quota` always fetches. Nothing refetches on a timer, so an idle Fiber does
no work, and a person returning to the terminal gets a fresh figure on focus.
A fetch times out after 8 seconds. A failure is reported as `unreachable`,
never as an error.

## Skills

`skill` loads one skill for the model: the name goes in, and the skill's
`SKILL.md` body comes out, read from disk at that moment. What a skill is, where
Fiber finds skills and how the listing is built are `docs/system-prompt.md`,
"Skills".

- Arguments: `name` (required), a name from the skills listing.
- The result is the body after the header, under the skill's path, so files the
  skill refers to can be read with `read`. A name that is not in the listing,
  a skill with `disable-model-invocation: true`, or a skill switched off in
  `skills.disabled` (`docs/system-prompt.md`, "Skills"), fails with
  `invalid_arguments`.
- Its definition never lists skill names. The names are in the listing, so
  adding or removing a skill never changes the tool set and the cached prefix
  holds (`docs/prompt-cache.md`).
- Fiber records each load, so it knows which skills the current context is
  working under. A handoff sends them again after the note
  (`docs/handoff.md`, "What the model sees after a handoff"), and the terminal
  shows each load as a skill, not as a file read.
- The tool declares `reads` on the skill's file and is never reviewed. It is
  declared in every session, in full, and counts toward the built-in budget
  ("Size budget in CI").

## Built in or extension

A first-party tool is compiled in unless its behaviour depends on a vendor or
on the person's environment. Read, write, edit, shell, background jobs, the
Fiber delegate harness, asking the person, web fetch, the `web_search` tool
`handoff` (`docs/handoff.md`), `name_session`, `session_search` and `skill` behave the same for everyone and are compiled in, as is
the search behind the shell's `grep` and `find` ("Search").
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
  already extensions, so the quota lookup lives in each provider's package
  ("Provider quota").
- Web search backends: each search service has its own API, key and response
  shape. The `web_search` tool itself is compiled in ("Web fetch and web
  search").
- Compact build and test output (the owner's `structured_return`): parsing
  depends on the person's toolchain, so it is an extension over an after-tool
  hook that replaces `content`, with the full log in the artifact.
- Delegate harnesses other than Fiber, such as Claude Code and cursor-agent:
  each runs another vendor's agent program (`docs/delegates.md`,
  "Harness extensions").

## Which tools the model sees

Settled by
[Which tools the model sees, and when](https://github.com/aakshintala/fiber/issues/51);
that ticket's resolution holds the rationale and the rejected alternatives.

Every tool is declared on every request, in full or deferred. A deferred tool
is declared with `defer_loading`. The model sees its name and description until
it loads the tool. The model loads the full definition when it needs it, and the definition is appended to the conversation, so the cached prefix
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
  override that for any tool (`docs/configuration.md`), and an MCP server can be marked
  `declare_in_full` (`docs/mcp.md`).
- MCP tools and `mcp_resources` are deferred by default. Every other built-in
  is declared in full.
- A built-in is deferred by default only when it is used in under about 2% of
  the owner's sessions and is not part of how they direct delegates.

### Tool search

- `tool_search` is a built-in tool the model calls to load deferred tools. It
  is declared, in full, only when the model supports deferral and at least one
  tool is deferred.
- Fiber runs the search itself: BM25 over each deferred tool's
  name, description and parameter names. It returns at most 8 tools by default;
  the model may pass `limit`.
- The result goes back in each protocol's native form: `tool_reference`
  blocks on Anthropic, `tool_search_output` on OpenAI Responses. Fiber never
  uses a provider's own search, so every provider behaves the same and every
  search is on the log. The log records the result as `loaded`, the list of
  tool names, and each protocol renders its block from that list, so a resume
  sends the same bytes.
- Its description lists the sources of the deferred tools (each MCP server's
  name), fixed when the preamble is built.
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
that disables tools. The session runs anyway.

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

## Not settled here

- Each tool's own design: the tickets indexed in
  [Epic: tools](https://github.com/aakshintala/fiber/issues/59).
- Delegates are `docs/delegates.md`, which lists what it leaves open.
- Comparisons with other tools, and the owner's usage, behind this area's rules: [research/reference-comparisons/README.md](../research/reference-comparisons/README.md)
