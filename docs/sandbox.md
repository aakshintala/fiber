# The OS sandbox

This page defines the Linux policy for #3027's first behavior slice, and
what it costs a session. The issue delivers the policy in implementation
parts; this page records only the Linux behavior this slice ships. The policy
is settled by [permissions: an opt-in OS sandbox, and whether its commands
skip the reviewer](https://github.com/aakshintala/fiber/issues/3027); that
ticket's resolution holds the rationale and the rejected alternatives.

Vocabulary is `GLOSSARY.md`: sandbox names the feature, confined names a
call that ran inside it, and nothing else. The rule that judges calls is
`docs/permissions.md`, "Confinement"; the keys are
`docs/configuration.md`, "Keys". The `confined` event marker follows the
[surveyor ruling](https://github.com/aakshintala/fiber/issues/3027#issuecomment-6425832076).

## The rule

The sandbox is opt-in and off by default. When it is on, a shell call runs
confined on Linux under Fiber's own policy: Landlock plus seccomp; no srt,
no bubblewrap. Calls remain reviewed; this slice adds no reviewer fast path,
no `unconfined` argument, no `/sandbox` toggle, and no delegate inheritance.
A delegate does not inherit its parent's setting in this slice.

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

- Write outside the workspace and temp: not directly, not through a
  grandchild, not through `python -c`, not through a symlink that leads out.
- Open any socket: no internet, DNS, localhost TCP or Unix sockets. This
  blocks the hub, session socket, `docker.sock` and ssh-agent. There is no
  `sandbox.allow_unix_sockets` key in this slice; all sockets are denied.
- Ask the sandbox to protect `.git/config`, hooks, a worktree's `.git` file
  or `.fiber/` on Linux. This slice does not claim that protection; slice 4
  owns it. A confined call in this slice can write those paths when they sit
  inside a writable root.
- Signal a process outside it below Landlock ABI 6. From ABI 6, Landlock
  signal scoping covers every process outside the sandbox; below it a
  confined call can still `kill` same-user processes outside, though it
  cannot drive them. This is the measured limit of the Linux policy, stated
  here so no stronger protection is promised below ABI 6. This slice adds no
  supervisor-signal seccomp rule.

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

## On Linux

The policy is Landlock plus a seccomp filter, applied in the shell's
`pre_exec`. The floor is Landlock ABI 2 (Linux 5.19); below it the sandbox
reports unavailable and calls are reviewed. Non-Linux builds do not apply
this Linux policy.

Landlock grants read and execute on the entries the ancestor walk allows,
full access beneath each granted write root, and read and write on granted
files; the seccomp filter fails `socket()` with `AF_UNIX`, `AF_INET` or
`AF_INET6`, and `io_uring_setup`, with `EPERM`.

## When the sandbox is unavailable

Failure goes toward review. When the sandbox cannot be applied — no
Landlock, an old kernel below ABI 2, a non-Linux build — the event omits
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
