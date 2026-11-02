# The OS sandbox

This page defines the OS sandbox policy, and what it costs a session. The
issue delivers the policy in implementation parts; this page records the
Linux behavior #3027's first behavior slice ships and the macOS behavior
#3630 adds. The policy is settled by [permissions: an opt-in OS sandbox,
and whether its commands skip the
reviewer](https://github.com/aakshintala/fiber/issues/3027); that ticket's
resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`: sandbox names the feature, confined names a
call that ran inside it, and nothing else. The rule that judges calls is
`docs/permissions.md`, "Confinement"; the keys are
`docs/configuration.md`, "Keys". The `confined` event marker follows the
[surveyor ruling](https://github.com/aakshintala/fiber/issues/3027#issuecomment-6425832076).

## The rule

The sandbox is opt-in and off by default. When it is on, a shell call runs
confined on Linux under Fiber's own policy: Landlock plus seccomp; no srt,
no bubblewrap. A confined call takes the confined fast path unless the destructive-command guard matches it (`docs/permissions.md`, 'Fast paths'). A call the model marks
`unconfined: true` runs outside the sandbox and is reviewed exactly as with
the sandbox off. A confined call whose sandbox cannot be applied fails;
Fiber never reruns it unconfined. This slice adds no
`/sandbox` toggle. Each Fiber delegate inherits the parent's effective sandbox
settings at launch, including changes made during the session. Fresh spawns,
forks and resumes all take a new snapshot.

The sandbox's own read deny follows whatever the credential deny covers:
Fiber home's `credentials/`, `file` sources, and whatever #3025 adds, plus
the person's `sandbox.deny_read` paths.

## What a confined call can do

- Read any file except Fiber home's `credentials/` and the person's extra
  denies (`sandbox.deny_read`), built at spawn by the ancestor walk below.
- Write the workspace and temp: the workspace root the session launched
  against, the system temp directory, and the extra paths the person grants
  (`sandbox.allow_write`, such as `~/.pi/agent`).

## What it cannot do

- Write outside the workspace, temp and granted paths: not directly, not
  through a grandchild, not through `python -c`, not through a symlink that
  leads out. A linked worktree needs Git metadata outside its workspace:
  its git directory and the shared `objects/`, `refs/` and `logs/`
  subdirectories. These grants do not grant the shared directory as a whole
  or socket access for pushes.
- Open any socket: no internet, DNS, localhost TCP or Unix sockets, on
  Linux by the seccomp filter and on macOS by the Seatbelt policy's deny by
  default. This blocks the hub, session socket, `docker.sock` and
  ssh-agent. There is no setting that grants a socket; a command that
  needs one runs with `unconfined: true` and is reviewed.
- Change the shared `.git/config` and hooks, a worktree's `.git` pointer
  file, its git directory's `commondir` file or workspace-local `.fiber/`
  without trace. On Linux, Fiber records them before the call and restores
  them after it, reporting each change ("Protected paths" below). On macOS,
  the policy denies writes to them even within a granted Git directory.
- Signal a process outside it below Landlock ABI 6. From ABI 6, Landlock
  signal scoping covers every process outside the sandbox; below it a
  confined call can still `kill` same-user processes outside, though it
  cannot drive them. This is the measured limit of the Linux policy, stated
  here so no stronger protection is promised below ABI 6. Below ABI 6 the
  narrow supervisor-signal seccomp rule in "Protected paths" below covers
  the session and its hub instead.

## Protected paths

A confined call can still change the paths the session depends on: the
shared `.git/config` and hooks, each registered worktree's `.git` pointer
and git directory's `commondir` file, and workspace-local `.fiber/`. On Linux,
those paths stay writable inside the sandbox, so Fiber records them before
the call and restores them after it. Git grants use the recorded pointer and
`commondir` bytes, never a running call's edits. On macOS, the policy denies
writes to those paths.

Each confined call starts by checking the project's baseline. The baseline
is one JSON file per project under Fiber home, recording the protected
paths and their contents. The baseline and its two locks live at `projects/<key>/protected-paths/`,
under the project's own key. When no confined call is running, the call
compares the live paths against the baseline, restores each difference,
folds the result into its own report, and takes a fresh snapshot. When
other calls are running, the call reuses their baseline unchanged. An approved
Fiber `write` or `edit` to a protected path advances the baseline. If that
update cannot be written, the tool returns an error, but Fiber keeps the new
entries in session memory so active calls in that session cannot restore stale
bytes. Fiber refuses to start another confined call in that session until it
can persist those entries.

A session that died mid-call leaves a stale baseline behind. The next
session's first call finds no active holder, restores what the dead session
changed, reports it with its own report, and only then takes its snapshot.
A live baseline is never touched while a call holds it.

Two locks guard the baseline. The baseline lock is exclusive and short: a
call holds it only while it reads, compares or writes the file, never while
a command runs. The active lock is shared and lasts the whole call: each
call holds it from start to its final report, including after adoption, so
the baseline stays fixed while any call is active. Every probe is
non-blocking, so a call ending under the baseline lock cannot deadlock a
call starting under the active lock.

The supervisor restores paths after collecting the call's final output,
outside the call's confinement. Restoration is not a sandbox write grant.
Protection against signals from the call depends on the kernel. From
Landlock ABI 6, signal scoping covers every process outside
the sandbox and no seccomp signal rule is added. Below ABI 6, signals aimed
at the session or its hub fail with `EPERM`: `kill`, `tkill` and `tgkill`
toward either process, either process group or every process, and
`pidfd_open` of either pid. Signals to the call's own children keep working
on every kernel. A restore that fails is not retried and not hidden: it
lands in the report's `failed` list with its cause, and the call's result,
completion event and warning line carry it the same way. Each changed path
gives the person one warning line, "Protected path `<path>` changed while a
confined call ran; Fiber restored it.", or, when the restore failed,
"…; Fiber could not restore it: `<cause>`.". The terminal UI draws it with
the call; a headless run prints it on stderr as `fiber: ` and the line.

## How read denies are built

At each confined spawn, for every denied path, Fiber walks from `/` towards
it. For each existing ancestor directory on the way, it grants read on every
entry except the one leading to the denied path. A directory gets a subtree
grant and a file gets a file grant. The denied path itself gets nothing.

- A missing component stops the walk; it does not move the deny point up.
  Nothing is granted for the missing component. An entry created later in an
  enumerated ancestor is a new entry in that directory and stays unreadable
  until the next call. That fails closed.
- Entries are opened with `O_PATH | O_NOFOLLOW` and symlinks get no grant.
  Landlock judges an access through a symlink by the target's resolved path,
  so the link reaches whatever its target is granted.
- Entries created in an enumerated ancestor during the call are unreadable
  until the next call.
- A `deny_read` path inside the workspace, the temp directory or any
  `allow_write` path makes the sandbox unavailable for the session. Fiber
  shows one notice naming the path and falls back to ordinary review, as for
  an unavailable sandbox.
- Hard links to a denied file outside the denied path read as that other path
  and are out of scope.

## On both platforms

`/dev/null` is readable and writable on both platforms, outside the
ancestor walk and the write roots. No other device is granted, except the
shell's inherited terminal, which keeps working.

Commits keep working on main checkouts and on worktrees: the worktree's own
git directory stays writable, as do the shared `objects/`, `refs/` and
`logs/` directories. Deny wins over those grants, so `.git/config`,
`.git/hooks/`, a worktree's `.git` pointer file, its git directory's
`commondir` file, `.fiber/`, and the shared hooks and config stay
unwritable.

## On Linux

The policy is Landlock plus a seccomp filter, applied in the shell's
`pre_exec`. The floor is Landlock ABI 2 (Linux 5.19); below it the sandbox
reports unavailable and calls are reviewed. Non-Linux builds do not apply
this Linux policy.

Landlock grants read and execute on the entries the ancestor walk allows,
full access beneath each granted write root, read and write on granted
files, and read and write on `/dev/null`; the seccomp filter fails
`socket()` with `AF_UNIX`, `AF_INET` or `AF_INET6`, and `io_uring_setup`,
with `EPERM`.

## On macOS

The policy is Seatbelt, applied by spawning through `/usr/bin/sandbox-exec`
(hardcoded, never `$PATH`) with an inline `-p` profile and `-D` bindings.
The profile denies by default. It grants writes to the canonicalized
workspace, the system temp directory (which reads `$TMPDIR`), `/tmp`, and
`allow_write`; reads stay wide except Fiber home's `credentials/` and
`deny_read`. Every dynamic path travels only as a separately supplied
`-DNAME=<path>` binding, referenced through its fixed parameter name; no
path text is interpolated into the profile source.

Devices and commits behave as in "On both platforms" above. The macOS
mechanism is Seatbelt: a literal grant for `/dev/null`, and one file-ioctl
allow preserving the inherited pty, so `stty` on the shell's own terminal
keeps working while `ps` and `pgrep` stay unavailable. Unix sockets have
no Seatbelt operation of their own; the default deny covers their creation,
and explicit denies name the refusal for IP, DNS and launchd.

The wrapped shell prints a fixed-prefix marker line before it runs the user
program; the reader strips exactly that first line, so user output is
byte-identical past it. A rejection by `sandbox-exec` means no marker, so
the call fails closed with the wrapper's diagnostic instead of ending
normally, and no move hands the call to a job before the marker is
observed. Availability needs only the `sandbox-exec` binary plus the same
overlap and resolution rules as Linux.

## When the sandbox is unavailable

Failure goes toward review. When the sandbox cannot be applied — no
Landlock, an old kernel below ABI 2, a missing `/usr/bin/sandbox-exec` on
macOS, or an unsupported platform — the event omits
`confined`, calls are reviewed as today and the session says so once. A call
declared `confined` never runs unconfined: if applying the sandbox fails at
spawn, the call fails. The event wording is the surveyor's ruling:
`confined: the call runs only inside the sandbox; if the sandbox can't be
applied at spawn, the call fails and nothing runs.`

## Configuration

The person-only keys, defaults, and repository-config restrictions are listed
in [`docs/configuration.md`](configuration.md), "Keys".

The evidence is [research/sandbox/](../research/sandbox/), including the
opt-in probe matrices in `research/sandbox/opt-in/`.
